//! Read-only video inspection and preview. Source frame rates can be averages
//! (VFR); never use source frame indices as timestamps.

use crate::timeline::{validate_clips, Clip};
use serde_json::Value;
use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VideoInfo {
    pub duration: f64,
    /// Average source rate, or nominal rate when no average is available.
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    /// Reported frame count, or a decoded count if the container lacks one.
    pub frames: u64,
    pub has_audio: bool,
}

pub struct PreviewFrame {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

pub struct PlaybackFrame {
    pub seconds: f64,
    pub image: PreviewFrame,
}

/// Sequential, bounded decoding of a read-only video. Dropping the stream stops
/// and reaps FFmpeg, including when its output or the frame queue is blocked.
pub struct Playback {
    child: Child,
    frames: Option<Receiver<Result<PlaybackFrame, String>>>,
    reader: Option<thread::JoinHandle<()>>,
    errors: Option<thread::JoinHandle<String>>,
    pending: Option<PlaybackFrame>,
    finished: bool,
}

impl Playback {
    pub fn start(path: &Path, info: &VideoInfo, seconds: f64) -> Result<Self, String> {
        if !seconds.is_finite()
            || seconds < 0.0
            || seconds >= info.duration
            || !info.duration.is_finite()
            || !valid_fps(info.fps)
        {
            return Err("Invalid video playback time or frame rate.".into());
        }
        let mut child = spawn(
            Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-ss"])
                .arg(seconds.to_string())
                .arg("-i").arg(path)
                .args(["-map", "0:V:0", "-an", "-sn", "-dn", "-vf"])
                .arg(format!("fps={}:start_time=0,scale=w='min(640,360*dar)':h='min(360,640/dar)',setsar=1,pad=640:360:(ow-iw)/2:(oh-ih)/2", info.fps))
                .args(["-pix_fmt", "rgb24", "-f", "rawvideo", "pipe:1"])
                .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()),
            "ffmpeg",
        )?;
        let mut stdout = child.stdout.take().unwrap();
        let errors = error_reader(child.stderr.take().unwrap());
        let (sender, frames) = mpsc::sync_channel(6);
        let fps = info.fps;
        let duration = info.duration;
        let reader = thread::spawn(move || {
            for index in 0_u64.. {
                let mut rgb = vec![0; 640 * 360 * 3];
                match stdout.read_exact(&mut rgb[..1]) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(error) => {
                        let _ = sender.send(Err(format!("Cannot read video playback: {error}")));
                        break;
                    }
                }
                if let Err(error) = stdout.read_exact(&mut rgb[1..]) {
                    let _ = sender.send(Err(format!("Incomplete playback frame: {error}")));
                    break;
                }
                let at = seconds + index as f64 / fps;
                if at >= duration {
                    continue;
                }
                if sender
                    .send(Ok(PlaybackFrame {
                        seconds: at,
                        image: PreviewFrame {
                            width: 640,
                            height: 360,
                            rgb,
                        },
                    }))
                    .is_err()
                {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            frames: Some(frames),
            reader: Some(reader),
            errors: Some(errors),
            pending: None,
            finished: false,
        })
    }

    /// Present the most recent due frame, keeping the next future frame queued.
    pub fn frame_at(&mut self, seconds: f64) -> Result<Option<PlaybackFrame>, String> {
        if !seconds.is_finite() {
            return Err("Playback position must be finite.".into());
        }
        let mut latest = None;
        loop {
            let frame = if let Some(frame) = self.pending.take() {
                frame
            } else if self.finished {
                break;
            } else {
                match self.frames.as_ref().unwrap().try_recv() {
                    Ok(frame) => frame?,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.finished = true;
                        let status = self
                            .child
                            .wait()
                            .map_err(|error| format!("Cannot wait for playback: {error}"))?;
                        let diagnostic =
                            self.errors.take().unwrap().join().map_err(|_| {
                                "The playback diagnostic task stopped unexpectedly."
                            })?;
                        if !status.success() {
                            return Err(format!("Video playback failed: {diagnostic}"));
                        }
                        break;
                    }
                }
            };
            if frame.seconds > seconds + 1e-9 {
                self.pending = Some(frame);
                break;
            }
            latest = Some(frame);
        }
        Ok(latest)
    }

    pub fn finished(&self) -> bool {
        self.finished
    }

    pub fn process_id(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.frames.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some(errors) = self.errors.take() {
            let _ = errors.join();
        }
    }
}

struct Source {
    info: VideoInfo,
    video_index: u64,
    audio_index: Option<u64>,
}

const ERROR_LIMIT: usize = 64 * 1024;

fn error_reader(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut tail = Vec::new();
        let mut buffer = [0_u8; 8192];
        while let Ok(count) = pipe.read(&mut buffer) {
            if count == 0 {
                break;
            }
            tail.extend_from_slice(&buffer[..count]);
            if tail.len() > ERROR_LIMIT {
                tail.drain(..tail.len() - ERROR_LIMIT);
            }
        }
        String::from_utf8_lossy(&tail).trim().to_owned()
    })
}

fn spawn(command: &mut Command, program: &str) -> Result<Child, String> {
    command.spawn().map_err(|error| {
        format!("Could not start {program}: {error}. Ensure FFmpeg and ffprobe are installed.")
    })
}

fn capture(command: &mut Command, program: &str, limit: usize) -> Result<Vec<u8>, String> {
    let mut child = spawn(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
        program,
    )?;
    let errors = error_reader(child.stderr.take().unwrap());
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .unwrap()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() > limit {
        let _ = child.kill();
        let _ = child.wait();
        let _ = errors.join();
        return Err(format!("{program} returned malformed or excessive data"));
    }
    let status = child.wait().map_err(|error| {
        let _ = child.kill();
        let _ = child.wait();
        format!("Could not wait for {program}: {error}")
    });
    let diagnostic = errors.join().unwrap_or_default();
    if !status?.success() {
        return Err(format!("{program} failed: {diagnostic}"));
    }
    Ok(bytes)
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse::<f64>().ok())
        .filter(|number| number.is_finite())
}

fn positive(value: &Value) -> Option<f64> {
    number(value).filter(|value| *value > 0.0)
}

fn integer(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse::<u64>().ok())
        .filter(|value| *value > 0)
}

fn frame_rate(value: &Value) -> Option<f64> {
    let text = value.as_str()?;
    let rate = if let Some((numerator, denominator)) = text.split_once('/') {
        let numerator = numerator.parse::<f64>().ok()?;
        let denominator = denominator.parse::<f64>().ok()?;
        if !numerator.is_finite() || !denominator.is_finite() || denominator <= 0.0 {
            return None;
        }
        numerator / denominator
    } else {
        text.parse::<f64>().ok()?
    };
    valid_fps(rate).then_some(rate)
}

fn valid_fps(fps: f64) -> bool {
    fps.is_finite() && fps > 0.0 && fps <= 1000.0
}

fn probe_source(path: &Path, count_frames: bool) -> Result<Source, String> {
    let metadata = path
        .metadata()
        .map_err(|error| format!("Cannot read video {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(format!("Video {} is empty or not a file", path.display()));
    }
    let bytes = capture(
        Command::new("ffprobe").args([
            OsStr::new("-v"),
            OsStr::new("error"),
            OsStr::new("-show_entries"),
            OsStr::new("stream=index,codec_type,width,height,avg_frame_rate,r_frame_rate,duration,nb_frames:stream_disposition=attached_pic:format=duration"),
            OsStr::new("-of"),
            OsStr::new("json"),
            OsStr::new("-i"),
            path.as_os_str(),
        ]),
        "ffprobe",
        1024 * 1024,
    )?;
    let json: Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("Invalid ffprobe JSON: {error}"))?;
    let streams = json["streams"]
        .as_array()
        .ok_or("ffprobe returned no streams")?;
    let video = streams
        .iter()
        .find(|stream| {
            stream["codec_type"] == "video" && stream["disposition"]["attached_pic"] != 1
        })
        .ok_or("Unsupported input: no video stream")?;
    let video_index = video["index"]
        .as_u64()
        .ok_or("Video stream has no valid index")?;
    let audio_index = streams
        .iter()
        .find(|stream| stream["codec_type"] == "audio")
        .and_then(|stream| stream["index"].as_u64());
    let dimension = |key: &str| -> Result<u32, String> {
        integer(&video[key])
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| format!("Video has invalid {key}"))
    };
    let duration = positive(&video["duration"])
        .or_else(|| positive(&json["format"]["duration"]))
        .ok_or("Video duration is unknown or invalid")?;
    let fps = frame_rate(&video["avg_frame_rate"])
        .or_else(|| frame_rate(&video["r_frame_rate"]))
        .ok_or("Video frame rate is unknown or invalid")?;
    let frames = if let Some(frames) = integer(&video["nb_frames"]) {
        frames
    } else if !count_frames {
        // Only private preview metadata may omit the count. Public VideoInfo
        // always comes from a full probe and never contains an estimate.
        0
    } else {
        // Counting is slower, but duration * average FPS is not an exact count
        // for variable-frame-rate or partially indexed sources.
        let counted = capture(
            Command::new("ffprobe").args([
                OsStr::new("-v"),
                OsStr::new("error"),
                OsStr::new("-select_streams"),
                OsStr::new(&video_index.to_string()),
                OsStr::new("-count_frames"),
                OsStr::new("-show_entries"),
                OsStr::new("stream=nb_read_frames"),
                OsStr::new("-of"),
                OsStr::new("json"),
                OsStr::new("-i"),
                path.as_os_str(),
            ]),
            "ffprobe",
            1024 * 1024,
        )?;
        let counted: Value = serde_json::from_slice(&counted)
            .map_err(|error| format!("Invalid ffprobe frame count JSON: {error}"))?;
        integer(&counted["streams"][0]["nb_read_frames"])
            .ok_or("Video frame count is unknown or zero")?
    };
    Ok(Source {
        info: VideoInfo {
            duration,
            fps,
            width: dimension("width")?,
            height: dimension("height")?,
            frames,
            has_audio: audio_index.is_some(),
        },
        video_index,
        audio_index,
    })
}

pub fn probe(path: &Path) -> Result<VideoInfo, String> {
    probe_source(path, true).map(|source| source.info)
}

/// Decode a source timestamp, not a source frame number. No preview files are
/// created; the exact RGB buffer is returned directly from FFmpeg's stdout.
pub fn preview(path: &Path, seconds: f64) -> Result<PreviewFrame, String> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err("Preview time must be finite and nonnegative".into());
    }
    let source = probe_source(path, false)?;
    if seconds >= source.info.duration {
        return Err("Preview time must be before the end of the video".into());
    }
    let width = 640;
    let height = 360;
    let length = width * height * 3;
    let rgb = capture(
        Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-ss"])
            .arg(seconds.to_string())
            .arg("-i")
            .arg(path)
            .arg("-map")
            .arg(format!("0:{}", source.video_index))
            .args([
                "-frames:v",
                "1",
                "-vf",
                "scale=w='min(640,360*dar)':h='min(360,640/dar)',setsar=1,pad=640:360:(ow-iw)/2:(oh-ih)/2",
                "-pix_fmt",
                "rgb24",
                "-f",
                "rawvideo",
                "pipe:1",
            ]),
        "ffmpeg",
        length,
    )?;
    if rgb.len() != length {
        return Err(format!(
            "Incomplete preview frame: expected {length} RGB bytes, received {}",
            rgb.len()
        ));
    }
    Ok(PreviewFrame { width, height, rgb })
}

/// Render kept time intervals into a new CFR H.264/AAC MP4. A private staging
/// directory prevents races from overwriting or removing someone else's output.
pub fn render(
    input: &Path,
    output: &Path,
    clips: &[Clip],
    fps: f64,
    cancel: &AtomicBool,
) -> Result<VideoInfo, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Video rendering cancelled".into());
    }
    if !valid_fps(fps) {
        return Err("Output frame rate must be finite, positive, and at most 1000 fps".into());
    }
    if output.symlink_metadata().is_ok() {
        return Err(format!("Output already exists: {}", output.display()));
    }
    if clips.is_empty() {
        return Err("At least one kept video interval is required".into());
    }
    let source = probe_source(input, true)?;
    validate_clips(clips, source.info.duration)?;
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let staging = tempfile::Builder::new()
        .prefix(".gpx-video-")
        .tempdir_in(parent)
        .map_err(|error| format!("Cannot create video staging directory: {error}"))?;
    let encoded = staging.path().join("render.mp4");
    let mut filters = Vec::new();
    // Resample on the source clock before cutting: resetting each source clip's
    // first decoded frame to zero would shift cuts between source frame times.
    let branches = (0..clips.len())
        .map(|index| format!("[source{index}]"))
        .collect::<String>();
    filters.push(format!(
        "[0:{}]fps=fps={fps}:start_time=0:round=near,tpad=stop_mode=clone:stop=-1,split={}{branches}",
        source.video_index,
        clips.len(),
    ));
    let mut inputs = String::new();
    let mut duration = 0.0;
    let mut total_frames = 0_u64;
    for (index, clip) in clips.iter().enumerate() {
        // Carry fractional-frame remainders across cuts instead of accumulating
        // the error from rounding every duration independently.
        duration += clip.end - clip.start;
        let target_frames = (duration * fps).round();
        let first_frame = (clip.start * fps).round();
        if !target_frames.is_finite()
            || target_frames >= i64::MAX as f64
            || first_frame >= i64::MAX as f64
        {
            return Err("Video intervals exceed the supported frame count".into());
        }
        let target_frames = target_frames as u64;
        let frames = target_frames.saturating_sub(total_frames);
        let first_frame = first_frame as u64;
        if frames == 0 {
            return Err("Each kept video interval must contain at least one output frame".into());
        }
        let last_frame = first_frame
            .checked_add(frames)
            .filter(|frame| *frame < i64::MAX as u64)
            .ok_or("Video intervals exceed the supported frame count")?;
        total_frames = target_frames;
        filters.push(format!(
            "[source{index}]trim=start_frame={first_frame}:end_frame={last_frame},setpts=N/({fps}*TB)[v{index}]",
        ));
        inputs.push_str(&format!("[v{index}]"));
        if let Some(audio_index) = source.audio_index {
            filters.push(format!(
                "[0:{audio_index}]atrim=start={}:end={},asetpts=PTS-{}/TB,aresample=async=1:first_pts=0,apad,atrim=duration={}[a{index}]",
                clip.start,
                clip.end,
                clip.start,
                frames as f64 / fps,
            ));
            inputs.push_str(&format!("[a{index}]"));
        }
    }
    let audio_count = usize::from(source.info.has_audio);
    filters.push(format!(
        "{inputs}concat=n={}:v=1:a={audio_count}[joined]{}",
        clips.len(),
        if source.info.has_audio { "[audio]" } else { "" }
    ));
    filters.push(format!(
        "[joined]setpts=N/({fps}*TB),scale=trunc(iw/2)*2:trunc(ih/2)*2[video]"
    ));
    let mut command = Command::new("ffmpeg");
    command
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-n", "-i"])
        .arg(input)
        .arg("-filter_complex")
        .arg(filters.join(";"))
        .args(["-map", "[video]"]);
    if source.info.has_audio {
        command.args(["-map", "[audio]", "-c:a", "aac", "-b:a", "192k"]);
    } else {
        command.arg("-an");
    }
    command
        .args([
            "-c:v",
            "libx264",
            "-crf",
            "18",
            "-preset",
            "fast",
            "-pix_fmt",
            "yuv420p",
            "-fps_mode",
            "cfr",
            "-r",
        ])
        .arg(fps.to_string())
        .args(["-movflags", "+faststart", "-f", "mp4"])
        .arg(&encoded)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if cancel.load(Ordering::Relaxed) {
        return Err("Video rendering cancelled".into());
    }
    let mut child = spawn(&mut command, "ffmpeg")?;
    let errors = error_reader(child.stderr.take().unwrap());
    let result = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            break Err("Video rendering cancelled".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("Could not wait for ffmpeg: {error}"));
            }
        }
    };
    let diagnostic = errors.join().unwrap_or_default();
    if !result?.success() {
        return Err(format!("ffmpeg rendering failed: {diagnostic}"));
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("Video rendering cancelled".into());
    }
    let info = probe(&encoded)?;
    if info.frames != total_frames {
        return Err(format!(
            "Rendered video has {} frames; expected {total_frames}",
            info.frames
        ));
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("Video rendering cancelled".into());
    }
    // Both paths are in the same directory/filesystem. Linking is atomic and
    // fails if even a dangling symlink has appeared at the destination.
    std::fs::hard_link(&encoded, output)
        .map_err(|error| format!("Cannot publish video without overwriting output: {error}"))?;
    Ok(info)
}

fn xml_text(text: &str) -> Result<String, String> {
    let mut escaped = String::new();
    for character in text.chars() {
        if !matches!(character, '\u{9}' | '\u{A}' | '\u{D}' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
        {
            return Err("Pairing metadata contains an invalid XML character".into());
        }
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    Ok(escaped)
}

pub fn pairing_xml(
    name: &str,
    country: &str,
    video_file: &str,
    gpx_file: &str,
    info: &VideoInfo,
) -> Result<String, String> {
    if !valid_fps(info.fps) || info.frames == 0 {
        return Err("Pairing metadata requires a valid frame rate and positive frame count".into());
    }
    let name = xml_text(name)?;
    let country = xml_text(country)?;
    let video = xml_text(video_file)?;
    let gpx = xml_text(gpx_file)?;
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gpx-import>\n  <name>{name}</name>\n  <id>{}</id>\n  <country>{country}</country>\n  <title>{name}</title>\n  <video-file-path>{video}</video-file-path>\n  <gpx-file-path>{gpx}</gpx-file-path>\n  <framerate>{}</framerate>\n  <start-frame>1</start-frame>\n  <end-frame>{}</end-frame>\n</gpx-import>\n",
        uuid::Uuid::new_v4(),
        info.fps,
        info.frames,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_metadata_does_not_decode_missing_container_frame_counts() {
        let directory = tempfile::Builder::new()
            .prefix("video-preview-metadata-")
            .tempdir_in(env!("CARGO_MANIFEST_DIR"))
            .unwrap();
        let path = directory.path().join("unindexed.mkv");
        capture(
            Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-nostdin",
                    "-n",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=red:size=96x96:rate=12:duration=1",
                    "-c:v",
                    "libx264",
                ])
                .arg(&path),
            "ffmpeg",
            0,
        )
        .unwrap();

        let metadata = probe_source(&path, false).unwrap();
        assert_eq!(metadata.info.frames, 0, "Preview must not count frames");
        assert_eq!(metadata.video_index, 0);
        assert_eq!(metadata.info.duration, 1.0);
        assert_eq!(preview(&path, 0.5).unwrap().rgb.len(), 640 * 360 * 3);
        assert_eq!(probe(&path).unwrap().frames, 12, "Public counts stay exact");
    }
}
