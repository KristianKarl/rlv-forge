use rlv_forge::timeline::Clip;
use rlv_forge::video::{pairing_xml, preview, probe, render, Playback, PlaybackFrame, VideoInfo};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn workspace() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("video-pipeline-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap()
}

fn synthetic(directory: &Path, audio: bool, vfr: bool) -> PathBuf {
    let path = directory.join(if vfr {
        "variable rate source.mkv"
    } else {
        "source with spaces & 'quotes'.mp4"
    });
    let mut command = Command::new("ffmpeg");
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-n",
        "-f",
        "lavfi",
        "-i",
        "color=red:size=160x90:rate=12:duration=4,drawbox=color=blue:t=fill:enable='gte(t,2)'",
    ]);
    if audio {
        command.args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=4",
            "-c:a",
            "aac",
            "-shortest",
        ]);
    }
    if vfr {
        command.args(["-vf", "select='not(eq(mod(n,3),1))'", "-fps_mode", "vfr"]);
    }
    let result = command
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&path)
        .output()
        .expect("FFmpeg is a required test dependency; run inside nix develop");
    assert!(
        result.status.success(),
        "Synthetic video generation failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    path
}

fn assert_close(actual: f64, expected: f64, tolerance: f64) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{actual} differs from {expected} by more than {tolerance}"
    );
}

fn playback_frame(playback: &mut Playback, seconds: f64) -> PlaybackFrame {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(frame) = playback.frame_at(seconds).unwrap() {
            return frame;
        }
        assert!(!playback.finished(), "Playback ended before {seconds}");
        assert!(Instant::now() < deadline, "Playback stalled at {seconds}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn continuous_playback_delivers_every_sixty_fps_frame_and_reaps_a_buffered_decoder() {
    let directory = workspace();
    let path = directory.path().join("sixty fps.mp4");
    let result = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=60:duration=2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let original = std::fs::read(&path).unwrap();
    let info = probe(&path).unwrap();
    assert_eq!(info.fps, 60.0);
    let mut playback = Playback::start(&path, &info, 0.0).unwrap();
    let process = playback.process_id();
    for index in 0..120 {
        let seconds = f64::from(index) / 60.0;
        let frame = playback_frame(&mut playback, seconds);
        assert_close(frame.seconds, seconds, 1e-9);
        assert_eq!((frame.image.width, frame.image.height), (640, 360));
        assert_eq!(frame.image.rgb.len(), 640 * 360 * 3);
        assert_eq!(playback.process_id(), process);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !playback.finished() {
        assert!(playback.frame_at(2.0).unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(playback);
    let buffered = Playback::start(&path, &info, 0.5).unwrap();
    let process = buffered.process_id();
    std::thread::sleep(Duration::from_millis(150));
    let started = Instant::now();
    drop(buffered);
    assert!(started.elapsed() < Duration::from_secs(3));
    #[cfg(target_os = "linux")]
    assert!(!Path::new(&format!("/proc/{process}")).exists());
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn playback_seeks_in_seconds_preserves_vfr_timing_and_reports_decode_errors() {
    let directory = workspace();
    for vfr in [false, true] {
        let path = synthetic(directory.path(), false, vfr);
        let info = probe(&path).unwrap();
        for (seconds, red) in [(0.357, true), (2.251, false)] {
            let mut playback = Playback::start(&path, &info, seconds).unwrap();
            let frame = playback_frame(&mut playback, seconds);
            assert_close(frame.seconds, seconds, 1e-9);
            let center = (180 * 640 + 320) * 3;
            let pixel = &frame.image.rgb[center..center + 3];
            assert!(
                if red {
                    pixel[0] > 200 && pixel[2] < 40
                } else {
                    pixel[2] > 200 && pixel[0] < 40
                },
                "{pixel:?}"
            );
            assert!(playback.frame_at(seconds - 0.1).unwrap().is_none());
        }
        for seconds in [-1.0, f64::NAN, f64::INFINITY, info.duration] {
            assert!(Playback::start(&path, &info, seconds).is_err());
        }
        let mut bad_info = info.clone();
        bad_info.fps = 0.0;
        assert!(Playback::start(&path, &bad_info, 0.0).is_err());
        let mut missing =
            Playback::start(&directory.path().join("missing.mp4"), &info, 0.0).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Err(error) = missing.frame_at(0.0) {
                assert!(error.contains("playback failed"));
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

fn audio_duration(path: &Path) -> f64 {
    let result = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=duration",
            "-of",
            "json",
            "-i",
        ])
        .arg(path)
        .output()
        .expect("ffprobe is a required test dependency");
    assert!(result.status.success());
    let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    json["streams"][0]["duration"]
        .as_str()
        .expect("Rendered output must contain audio with known duration")
        .parse()
        .unwrap()
}

fn assert_color(path: &Path, seconds: f64, red: bool) {
    let frame = preview(path, seconds).unwrap();
    assert_eq!((frame.width, frame.height), (640, 360));
    assert_eq!(frame.rgb.len(), 640 * 360 * 3);
    let center = (180 * 640 + 320) * 3;
    let pixel = &frame.rgb[center..center + 3];
    if red {
        assert!(pixel[0] > 200 && pixel[2] < 40, "{pixel:?}");
    } else {
        assert!(pixel[2] > 200 && pixel[0] < 40, "{pixel:?}");
    }
}

#[test]
fn probes_previews_and_renders_disjoint_intervals_with_synchronized_audio() {
    let directory = workspace();
    let source = synthetic(directory.path(), true, false);
    let info = probe(&source).unwrap();
    assert_eq!((info.width, info.height, info.frames), (160, 90, 48));
    assert!(info.has_audio);
    assert_close(info.duration, 4.0, 0.001);
    assert_close(info.fps, 12.0, 0.001);
    assert_color(&source, 0.75, true);
    assert_color(&source, 2.5, false);

    let output = directory.path().join("kept ride.mp4");
    let clips = [
        Clip {
            start: 0.5,
            end: 1.0,
        },
        Clip {
            start: 2.0,
            end: 3.0,
        },
    ];
    let rendered = render(&source, &output, &clips, 12.0, &AtomicBool::new(false)).unwrap();
    assert_eq!(rendered.frames, 18);
    assert!(rendered.has_audio);
    assert_close(rendered.duration, 1.5, 1.0 / 12.0);
    assert_close(rendered.fps, 12.0, 0.001);
    assert_close(audio_duration(&output), 1.5, 0.05);
    assert_color(&output, 0.1, true);
    assert_color(&output, 0.75, false);
    let names: Vec<_> = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 2, "Staging files must be cleaned up");
}

#[test]
fn no_audio_and_variable_frame_rate_sources_render_as_cfr() {
    let directory = workspace();
    let source = synthetic(directory.path(), false, true);
    let info = probe(&source).unwrap();
    assert!(!info.has_audio);
    assert_eq!(info.frames, 32, "Missing container counts must be decoded");
    assert_close(info.duration, 4.0, 0.1);
    assert_color(&source, 2.5, false);
    let output = directory.path().join("constant.mp4");
    let rendered = render(
        &source,
        &output,
        &[Clip {
            start: 0.0,
            end: 3.0,
        }],
        10.0,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(!rendered.has_audio);
    assert_close(rendered.fps, 10.0, 0.001);
    assert_eq!(rendered.frames, 30);
    assert_close(rendered.duration, 3.0, 0.001);
}

#[test]
fn mixed_frame_rates_and_disjoint_clips_have_exact_output_frame_counts() {
    let directory = workspace();
    for vfr in [false, true] {
        let source = directory.path().join(if vfr {
            "source-vfr.mkv"
        } else {
            "source-25.mp4"
        });
        let mut command = Command::new("ffmpeg");
        command.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-n",
            "-f",
            "lavfi",
            "-i",
            "color=red:size=160x90:rate=25:duration=3,drawbox=color=blue:t=fill:enable='gte(t,2)'",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=3",
        ]);
        if vfr {
            command.args(["-vf", "select='not(eq(mod(n,3),1))'", "-fps_mode", "vfr"]);
        }
        let generated = command
            .args(["-c:v", "libx264", "-c:a", "aac"])
            .arg(&source)
            .output()
            .expect("FFmpeg is a required test dependency");
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
        let output = directory
            .path()
            .join(if vfr { "vfr-cuts.mp4" } else { "25-to-30.mp4" });
        let clips = [
            Clip {
                start: 0.1,
                end: 0.7,
            },
            Clip {
                start: 1.1,
                end: 1.8,
            },
            Clip {
                start: 2.1,
                end: 2.8,
            },
        ];
        let info = render(&source, &output, &clips, 30.0, &AtomicBool::new(false)).unwrap();
        assert_eq!(info.frames, 60, "vfr={vfr}");
        assert_close(info.duration, 2.0, 0.001);
        assert_close(info.fps, 30.0, 0.001);
        assert_close(audio_duration(&output), 2.0, 0.025);
        assert_color(&output, 0.1, true);
        assert_color(&output, 1.4, false);
    }
}

#[test]
fn invalid_files_times_and_intervals_are_explicit_errors() {
    let directory = workspace();
    let missing = directory.path().join("missing.mp4");
    assert!(probe(&missing).unwrap_err().contains("Cannot read"));
    let empty = directory.path().join("empty.mp4");
    std::fs::write(&empty, []).unwrap();
    assert!(probe(&empty).unwrap_err().contains("empty"));
    std::fs::write(&empty, b"not a video").unwrap();
    assert!(probe(&empty).unwrap_err().contains("ffprobe"));
    let source = synthetic(directory.path(), false, false);
    for seconds in [-1.0, f64::NAN, f64::INFINITY, 4.0, 5.0] {
        assert!(preview(&source, seconds).is_err(), "{seconds}");
    }
    let output = directory.path().join("invalid.mp4");
    for clips in [
        vec![],
        vec![Clip {
            start: -0.1,
            end: 1.0,
        }],
        vec![Clip {
            start: 1.0,
            end: 1.0,
        }],
        vec![Clip {
            start: 1.0,
            end: 0.5,
        }],
        vec![Clip {
            start: 0.0,
            end: 4.1,
        }],
        vec![Clip {
            start: f64::NAN,
            end: 1.0,
        }],
        vec![Clip {
            start: 0.0,
            end: f64::INFINITY,
        }],
        vec![
            Clip {
                start: 1.0,
                end: 2.0,
            },
            Clip {
                start: 0.0,
                end: 1.0,
            },
        ],
        vec![
            Clip {
                start: 0.0,
                end: 2.0,
            },
            Clip {
                start: 1.0,
                end: 3.0,
            },
        ],
    ] {
        assert!(render(&source, &output, &clips, 12.0, &AtomicBool::new(false)).is_err());
        assert!(!output.exists());
    }
    let clips = [Clip {
        start: 0.0,
        end: 1.0,
    }];
    for fps in [0.0, -1.0, f64::NAN, f64::INFINITY, 1001.0] {
        assert!(render(&source, &output, &clips, fps, &AtomicBool::new(false)).is_err());
    }
}

#[test]
fn cancellation_and_nonoverwrite_preserve_existing_files() {
    let directory = workspace();
    let source = synthetic(directory.path(), true, false);
    let original = std::fs::read(&source).unwrap();
    let output = directory.path().join("existing.mp4");
    std::fs::write(&output, b"do not replace").unwrap();
    let clips = [Clip {
        start: 0.0,
        end: 1.0,
    }];
    assert!(render(&source, &source, &clips, 12.0, &AtomicBool::new(false)).is_err());
    assert_eq!(std::fs::read(&source).unwrap(), original);
    assert!(render(&source, &output, &clips, 12.0, &AtomicBool::new(false)).is_err());
    assert_eq!(std::fs::read(&output).unwrap(), b"do not replace");
    let cancelled = directory.path().join("cancelled.mp4");
    let error = render(&source, &cancelled, &clips, 12.0, &AtomicBool::new(true)).unwrap_err();
    assert!(error.contains("cancelled"));
    assert!(!cancelled.exists());
}

#[test]
fn cancellation_reaps_an_active_encoder_and_removes_staging_files() {
    let directory = workspace();
    let source = synthetic(directory.path(), false, false);
    let output = directory.path().join("cancel-in-progress.mp4");
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let result = scope.spawn(|| {
            render(
                &source,
                &output,
                &[Clip {
                    start: 0.0,
                    end: 4.0,
                }],
                1000.0,
                &cancel,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut saw_encoder = false;
        while Instant::now() < deadline {
            // The MP4 is created after FFmpeg starts, not during source probing.
            saw_encoder = std::fs::read_dir(directory.path())
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".gpx-video-")
                        && entry.path().join("render.mp4").exists()
                });
            if saw_encoder || result.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        cancel.store(true, Ordering::Relaxed);
        let error = result.join().unwrap().unwrap_err();
        assert!(saw_encoder, "Expected to cancel a running FFmpeg process");
        assert!(error.contains("cancelled"), "{error}");
    });
    assert!(!output.exists());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn non_video_inputs_are_rejected_and_previews_are_letterboxed() {
    let directory = workspace();
    let audio = directory.path().join("audio.m4a");
    let result = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-n",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=0.25",
            "-c:a",
            "aac",
        ])
        .arg(&audio)
        .output()
        .expect("FFmpeg is a required test dependency");
    assert!(result.status.success());
    assert!(probe(&audio).unwrap_err().contains("no video stream"));
    let square = directory.path().join("square.mp4");
    let result = Command::new("ffmpeg")
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
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&square)
        .output()
        .unwrap();
    assert!(result.status.success());
    let frame = preview(&square, 0.0).unwrap();
    let side = (180 * 640 + 10) * 3;
    assert!(
        frame.rgb[side..side + 3].iter().all(|value| *value < 5),
        "Square source must be padded with black side bars"
    );
    let center = (180 * 640 + 320) * 3;
    assert!(frame.rgb[center] > 200);
}

#[test]
fn audio_start_offset_is_preserved_as_silence_not_shifted_early() {
    let directory = workspace();
    let source = directory.path().join("delayed-audio.mp4");
    let generated = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-n",
            "-f",
            "lavfi",
            "-i",
            "color=red:size=160x90:rate=12:duration=2",
            "-itsoffset",
            "0.5",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1.5",
            "-c:v",
            "libx264",
            "-c:a",
            "aac",
        ])
        .arg(&source)
        .output()
        .unwrap();
    assert!(generated.status.success());
    let output = directory.path().join("delayed-render.mp4");
    render(
        &source,
        &output,
        &[Clip {
            start: 0.0,
            end: 1.0,
        }],
        12.0,
        &AtomicBool::new(false),
    )
    .unwrap();
    let decoded = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(&output)
        .args([
            "-map", "0:a:0", "-ar", "48000", "-ac", "1", "-f", "s16le", "pipe:1",
        ])
        .output()
        .unwrap();
    assert!(decoded.status.success());
    let samples: Vec<_> = decoded
        .stdout
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect();
    let energy = |start: usize, end: usize| {
        samples[start..end]
            .iter()
            .map(|sample| f64::from(*sample).abs())
            .sum::<f64>()
            / (end - start) as f64
    };
    assert!(energy(4800, 14_400) < 10.0, "Leading silence was lost");
    assert!(energy(33_600, 43_200) > 1000.0, "Expected audible tone");
}

#[test]
fn pairing_xml_matches_schema_and_escapes_metadata() {
    let mut info = VideoInfo {
        duration: 1.5,
        fps: 12.0,
        width: 160,
        height: 90,
        frames: 18,
        has_audio: true,
    };
    let name = "Montée <test> & \"ride\" 'one'";
    let xml = pairing_xml(name, "FR & CH", "ride & video.mp4", "track's.gpx", &info).unwrap();
    let document = roxmltree::Document::parse(&xml).unwrap();
    assert_eq!(document.root_element().tag_name().name(), "gpx-import");
    let field = |name: &str| {
        document
            .root_element()
            .children()
            .find(|node| node.has_tag_name(name))
            .unwrap()
            .text()
            .unwrap()
    };
    assert_eq!(field("name"), name);
    assert_eq!(field("title"), name);
    assert_eq!(field("country"), "FR & CH");
    assert_eq!(field("video-file-path"), "ride & video.mp4");
    assert_eq!(field("gpx-file-path"), "track's.gpx");
    assert_eq!(field("framerate"), "12");
    assert_eq!(field("start-frame"), "1");
    assert_eq!(field("end-frame"), "18");
    assert_eq!(
        uuid::Uuid::parse_str(field("id"))
            .unwrap()
            .get_version_num(),
        4
    );
    for invalid in ["bad\0name", "bad\u{1f}name", "bad\u{fffe}name"] {
        assert!(pairing_xml(invalid, "FR", "video.mp4", "track.gpx", &info).is_err());
        assert!(pairing_xml("ride", invalid, "video.mp4", "track.gpx", &info).is_err());
        assert!(pairing_xml("ride", "FR", invalid, "track.gpx", &info).is_err());
        assert!(pairing_xml("ride", "FR", "video.mp4", invalid, &info).is_err());
    }
    for fps in [0.0, f64::NAN, f64::INFINITY] {
        info.fps = fps;
        assert!(pairing_xml("ride", "FR", "video.mp4", "track.gpx", &info).is_err());
    }
    info.fps = 12.0;
    info.frames = 0;
    assert!(pairing_xml("ride", "FR", "video.mp4", "track.gpx", &info).is_err());
}
