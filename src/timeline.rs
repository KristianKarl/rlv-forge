use crate::gpx::Track;
use roxmltree::{Document, Node};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Clip {
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CutRange {
    Time(Clip),
    Distance([f64; 2]),
}

impl Clip {
    pub fn duration(self) -> f64 {
        self.end - self.start
    }
}

pub fn validate_clips(clips: &[Clip], duration: f64) -> Result<(), String> {
    if !duration.is_finite() || duration <= 0.0 || clips.is_empty() {
        return Err("Keep at least one interval from a video with a valid duration.".into());
    }
    let mut previous = 0.0;
    for clip in clips {
        if !clip.start.is_finite()
            || !clip.end.is_finite()
            || clip.start < previous
            || clip.end <= clip.start
            || clip.end > duration + 1e-6
        {
            return Err(
                "Kept intervals must be ordered, non-overlapping, and within the video.".into(),
            );
        }
        previous = clip.end;
    }
    Ok(())
}

pub fn frame_aligned_clips(clips: &[Clip], duration: f64, fps: f64) -> Result<Vec<Clip>, String> {
    validate_clips(clips, duration)?;
    if !fps.is_finite() || !(1.0..=120.0).contains(&fps) {
        return Err("Choose an output frame rate between 1 and 120 fps.".into());
    }
    let clips: Vec<_> = clips
        .iter()
        .map(|clip| Clip {
            start: (clip.start * fps).round() / fps,
            end: ((clip.end * fps).round() / fps).min((duration * fps).floor() / fps),
        })
        .collect();
    validate_clips(&clips, duration)?;
    if clips.iter().any(|clip| clip.duration() * fps < 2.0 - 1e-6) {
        return Err("Each kept interval must contain at least two output frames.".into());
    }
    Ok(clips)
}

pub fn shared_interval(
    video_duration: f64,
    gpx_duration: f64,
    offset: f64,
    fps: f64,
) -> Result<Clip, String> {
    if !offset.is_finite()
        || !gpx_duration.is_finite()
        || gpx_duration <= 0.0
        || !fps.is_finite()
        || !(1.0..=120.0).contains(&fps)
    {
        return Err(
            "Shared coverage requires valid durations, offset and output frame rate.".into(),
        );
    }
    let clip = Clip {
        start: ((-offset).max(0.0) * fps).ceil() / fps,
        end: ((gpx_duration - offset).min(video_duration) * fps).floor() / fps,
    };
    frame_aligned_clips(&[clip], video_duration, fps)?;
    Ok(clip)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Timing {
    pub seconds: Vec<f64>,
    pub duration: f64,
    pub start: String,
}

fn named(node: Node<'_, '_>, name: &str, ns: Option<&str>) -> bool {
    node.is_element() && node.tag_name().name() == name && node.tag_name().namespace() == ns
}

fn track_nodes<'a, 'input>(doc: &'a Document<'input>) -> Result<Vec<Node<'a, 'input>>, String> {
    let root = doc.root_element();
    let ns = root.tag_name().namespace();
    let tracks: Vec<_> = root
        .children()
        .filter(|node| named(*node, "trk", ns))
        .collect();
    if tracks.len() != 1 || root.children().any(|node| named(node, "rte", ns)) {
        return Err("Incyclist video editing requires one GPX track; multiple track segments are supported.".into());
    }
    let points: Vec<_> = tracks[0]
        .children()
        .filter(|node| named(*node, "trkseg", ns))
        .flat_map(|group| {
            group
                .children()
                .filter(move |node| named(*node, "trkpt", ns))
        })
        .collect();
    if points.len() < 2 {
        return Err("Video synchronization requires at least two timestamped GPX points.".into());
    }
    Ok(points)
}

fn read_timing(nodes: &[Node<'_, '_>]) -> Result<Timing, String> {
    let mut seconds = Vec::with_capacity(nodes.len());
    let mut start = None;
    for (i, node) in nodes.iter().enumerate() {
        let text = node.children()
            .find(|child| named(*child, "time", node.tag_name().namespace()))
            .and_then(|child| child.text())
            .ok_or_else(|| format!("GPX point {} has no timestamp. Video synchronization needs a timestamp on every point.", i + 1))?;
        let stamp = OffsetDateTime::parse(text.trim(), &Rfc3339)
            .map_err(|error| format!("Invalid timestamp at GPX point {}: {error}", i + 1))?;
        let epoch = *start.get_or_insert(stamp);
        let elapsed = (stamp - epoch).as_seconds_f64();
        if seconds.last().is_some_and(|previous| elapsed <= *previous) {
            return Err(format!(
                "GPX timestamps must increase strictly; check point {}.",
                i + 1
            ));
        }
        seconds.push(elapsed);
    }
    Ok(Timing {
        duration: *seconds.last().unwrap(),
        seconds,
        start: start
            .unwrap()
            .format(&Rfc3339)
            .map_err(|error| error.to_string())?,
    })
}

pub fn timing(track: &Track) -> Result<Timing, String> {
    let xml = track.export(&track.name, &track.description)?;
    let doc = Document::parse(&xml).map_err(|error| error.to_string())?;
    read_timing(&track_nodes(&doc)?)
}

fn samples_at(timing: &Timing, track: &Track, seconds: f64) -> Option<(usize, usize, f64)> {
    if !seconds.is_finite() || seconds < 0.0 || seconds > timing.duration {
        return None;
    }
    let right = timing.seconds.partition_point(|time| *time < seconds);
    if right == 0 {
        return Some((0, 0, 0.0));
    }
    if right == timing.seconds.len() {
        return Some((right - 1, right - 1, 0.0));
    }
    if timing.seconds[right] == seconds {
        return Some((right, right, 0.0));
    }
    if track.points[right - 1].segment != track.points[right].segment {
        return None;
    }
    let fraction =
        (seconds - timing.seconds[right - 1]) / (timing.seconds[right] - timing.seconds[right - 1]);
    Some((right - 1, right, fraction))
}

pub fn distance_at(timing: &Timing, track: &Track, seconds: f64) -> Option<f64> {
    let (left, right, fraction) = samples_at(timing, track, seconds)?;
    Some(
        track.points[left].distance
            + fraction * (track.points[right].distance - track.points[left].distance),
    )
}

/// Inverse riding-distance lookup. At repeated distances, stay nearest the
/// current GPX time without selecting a gap between disconnected segments.
pub fn time_at_distance(
    timing: &Timing,
    track: &Track,
    distance: f64,
    current: f64,
) -> Option<f64> {
    if !distance.is_finite()
        || !current.is_finite()
        || distance < 0.0
        || distance > track.total_distance()
    {
        return None;
    }
    let right = track
        .points
        .partition_point(|point| point.distance < distance);
    let end = track
        .points
        .partition_point(|point| point.distance <= distance);
    if right < end {
        let seconds = current.clamp(timing.seconds[right], timing.seconds[end - 1]);
        if position_at(timing, track, seconds).is_some() {
            return Some(seconds);
        }
        let after = timing.seconds.partition_point(|time| *time < seconds);
        let before = after - 1;
        return Some(
            if seconds - timing.seconds[before] <= timing.seconds[after] - seconds {
                timing.seconds[before]
            } else {
                timing.seconds[after]
            },
        );
    }
    let left = right.checked_sub(1)?;
    if track.points[left].segment != track.points[right].segment {
        return None;
    }
    let fraction = (distance - track.points[left].distance)
        / (track.points[right].distance - track.points[left].distance);
    Some(timing.seconds[left] + fraction * (timing.seconds[right] - timing.seconds[left]))
}

/// Horizontal metres per second within a connected, timestamped GPX interval.
pub fn speed_at(timing: &Timing, track: &Track, seconds: f64) -> Option<f64> {
    let (mut left, mut right, _) = samples_at(timing, track, seconds)?;
    if left == right {
        if right + 1 < track.points.len()
            && track.points[right].segment == track.points[right + 1].segment
        {
            right += 1;
        } else if left > 0 && track.points[left - 1].segment == track.points[left].segment {
            left -= 1;
        } else {
            return None;
        }
    }
    Some(
        (track.points[right].distance - track.points[left].distance)
            / (timing.seconds[right] - timing.seconds[left]),
    )
}

/// Longitude/latitude on the current GPX clock. Unlike distance, time distinguishes
/// the two endpoints of a cut, which share the same cumulative riding distance.
pub fn position_at(timing: &Timing, track: &Track, seconds: f64) -> Option<[f64; 2]> {
    let (left, right, fraction) = samples_at(timing, track, seconds)?;
    let a = &track.points[left];
    let b = &track.points[right];
    let longitude_delta = (b.longitude - a.longitude + 180.0).rem_euclid(360.0) - 180.0;
    Some([
        (a.longitude + fraction * longitude_delta + 180.0).rem_euclid(360.0) - 180.0,
        a.latitude + fraction * (b.latitude - a.latitude),
    ])
}

#[derive(Debug)]
pub struct EditedTimeline {
    pub gpx: String,
    pub cuts: Vec<f64>,
    pub duration: f64,
    pub points: usize,
    pub source_indices: Vec<Option<usize>>,
    pub source_times: Vec<f64>,
}

fn tag<'a>(xml: &'a str, node: Node<'_, '_>) -> &'a str {
    xml[node.range().start + 1..]
        .split([' ', '\n', '\t', '\r', '/', '>'])
        .next()
        .unwrap()
}

fn opening(xml: &str, node: Node<'_, '_>) -> String {
    let mut quote = None;
    for (i, ch) in xml[node.range()].char_indices() {
        match (quote, ch) {
            (None, '\'' | '"') => quote = Some(ch),
            (Some(delimiter), _) if delimiter == ch => quote = None,
            (None, '>') => return xml[node.range().start..=node.range().start + i].to_owned(),
            _ => {}
        }
    }
    unreachable!("Parsed XML has a complete opening tag")
}

fn stamp(seconds: f64) -> Result<String, String> {
    let epoch = OffsetDateTime::parse("2000-01-01T00:00:00Z", &Rfc3339).unwrap();
    let millis = (seconds * 1000.0).round();
    if !millis.is_finite() || !(0.0..=1e12).contains(&millis) {
        return Err("Edited GPX timestamp is out of range.".into());
    }
    (epoch + time::Duration::milliseconds(millis as i64))
        .format(&Rfc3339)
        .map_err(|error| error.to_string())
}

struct OutputPoint {
    xml: String,
    group: usize,
    latitude: f64,
    longitude: f64,
    time: f64,
    source_index: Option<usize>,
}

fn point_at(
    track: &Track,
    timing: &Timing,
    nodes: &[Node<'_, '_>],
    xml: &str,
    source_time: f64,
    output_time: f64,
) -> Result<OutputPoint, String> {
    let right = timing
        .seconds
        .partition_point(|time| *time < source_time - 1e-7);
    let rounded_time = (output_time * 1000.0).round() / 1000.0;
    let timestamp = stamp(rounded_time)?;
    if right < timing.seconds.len() && (timing.seconds[right] - source_time).abs() < 1e-6 {
        let node = nodes[right];
        let time = node
            .children()
            .find(|child| named(*child, "time", node.tag_name().namespace()))
            .unwrap();
        let node_xml = format!(
            "{}<{}>{timestamp}</{}>{}",
            &xml[node.range().start..time.range().start],
            tag(xml, time),
            tag(xml, time),
            &xml[time.range().end..node.range().end]
        );
        return Ok(OutputPoint {
            source_index: track.points[right].source_index,
            xml: node_xml,
            group: track.points[right].segment,
            latitude: track.points[right].latitude,
            longitude: track.points[right].longitude,
            time: rounded_time,
        });
    }
    if right == 0 || right == timing.seconds.len() {
        return Err("A cut boundary falls outside the timestamped GPX recording.".into());
    }
    let left = right - 1;
    let a = &track.points[left];
    let b = &track.points[right];
    if a.segment != b.segment || (a.longitude - b.longitude).abs() > 180.0 {
        return Err("A cut boundary falls inside a disconnected GPX segment gap. Move it to a recorded point.".into());
    }
    let t = (source_time - timing.seconds[left]) / (timing.seconds[right] - timing.seconds[left]);
    let latitude = a.latitude + t * (b.latitude - a.latitude);
    let longitude = a.longitude + t * (b.longitude - a.longitude);
    let elevation = a.elevation + t * (b.elevation - a.elevation);
    let group_tag = tag(xml, nodes[left].parent().unwrap());
    let prefix = group_tag
        .rsplit_once(':')
        .map_or(String::new(), |(prefix, _)| format!("{prefix}:"));
    let point_tag = format!("{prefix}trkpt");
    Ok(OutputPoint {
        source_index: None,
        xml: format!("<{point_tag} lat=\"{latitude:.9}\" lon=\"{longitude:.9}\"><{prefix}ele>{elevation:.6}</{prefix}ele><{prefix}time>{timestamp}</{prefix}time></{point_tag}>"),
        group: a.segment, latitude, longitude, time: rounded_time,
    })
}

fn separation(a: &OutputPoint, b: &OutputPoint) -> f64 {
    let lat1 = a.latitude.to_radians();
    let lat2 = b.latitude.to_radians();
    let h = ((lat2 - lat1) / 2.0).sin().powi(2)
        + lat1.cos()
            * lat2.cos()
            * ((b.longitude - a.longitude).to_radians() / 2.0)
                .sin()
                .powi(2);
    6_371_008.8 * 2.0 * h.clamp(0.0, 1.0).sqrt().asin()
}

/// Offset is GPX elapsed seconds at video time zero. Cuts use source-video seconds.
pub fn edit(
    track: &Track,
    name: &str,
    description: &str,
    clips: &[Clip],
    video_duration: f64,
    offset: f64,
    fps: f64,
) -> Result<EditedTimeline, String> {
    let clips = frame_aligned_clips(clips, video_duration, fps)?;
    edit_intervals(
        track,
        name,
        description,
        &clips,
        EditOptions {
            offset,
            boundary_step: 1.0 / fps,
            level: false,
            samples: &[],
            exact_ends: &[],
        },
    )
}

/// Edit GPX only, without snapping boundaries to video frames.
pub fn edit_gpx(
    track: &Track,
    name: &str,
    description: &str,
    clips: &[Clip],
    offset: f64,
) -> Result<EditedTimeline, String> {
    let duration = timing(track)?.duration - offset;
    validate_clips(clips, duration)?;
    edit_intervals(
        track,
        name,
        description,
        clips,
        EditOptions {
            offset,
            boundary_step: 0.001,
            level: true,
            samples: &[],
            exact_ends: &[],
        },
    )
}

pub(crate) fn edit_gpx_samples(
    track: &Track,
    name: &str,
    description: &str,
    clips: &[Clip],
    samples: &[f64],
    exact_ends: &[f64],
) -> Result<EditedTimeline, String> {
    let duration = timing(track)?.duration;
    validate_clips(clips, duration)?;
    edit_intervals(
        track,
        name,
        description,
        clips,
        EditOptions {
            offset: 0.0,
            boundary_step: 0.001,
            level: true,
            samples,
            exact_ends,
        },
    )
}

pub fn retime_gpx(xml: &str, seconds: &[f64]) -> Result<String, String> {
    let doc = Document::parse(xml).map_err(|error| error.to_string())?;
    let nodes = track_nodes(&doc)?;
    if nodes.len() != seconds.len() {
        return Err("Synchronized timestamps do not match the GPX point count.".into());
    }
    let mut previous = None;
    let mut edits = Vec::new();
    for (node, seconds) in nodes.iter().zip(seconds) {
        let rounded = (seconds * 1000.0).round() / 1000.0;
        if previous.is_some_and(|previous| rounded <= previous) {
            return Err("Synchronization maps GPX samples to the same millisecond. Remove redundant samples or revise the sync points.".into());
        }
        let timestamp = stamp(rounded)?;
        let time = node
            .children()
            .find(|child| named(*child, "time", node.tag_name().namespace()))
            .ok_or("A synchronized GPX sample has no timestamp.")?;
        edits.push((
            time.range(),
            format!("<{}>{timestamp}</{}>", tag(xml, time), tag(xml, time)),
        ));
        previous = Some(rounded);
    }
    let mut output = xml.to_owned();
    for (range, replacement) in edits.into_iter().rev() {
        output.replace_range(range, &replacement);
    }
    Ok(output)
}

/// Remove a range on the current GPX clock and close its elevation gap.
pub fn cut(track: &Track, cut: Clip) -> Result<Track, String> {
    cut_ranges(track, &[cut])
}

pub fn kept_intervals(duration: f64, cuts: &[Clip]) -> Result<Vec<Clip>, String> {
    let mut cuts = cuts.to_vec();
    for cut in &cuts {
        validate_clips(&[*cut], duration)?;
    }
    cuts.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut kept = Vec::new();
    let mut cursor = 0.0;
    for cut in cuts {
        if cut.start > cursor {
            kept.push(Clip {
                start: cursor,
                end: cut.start,
            });
        }
        cursor = cursor.max(cut.end);
    }
    if cursor < duration {
        kept.push(Clip {
            start: cursor,
            end: duration,
        });
    }
    validate_clips(&kept, duration)?;
    Ok(kept)
}

pub fn original_time(
    seconds: f64,
    duration: f64,
    cuts: &[Clip],
    end_boundary: bool,
) -> Result<f64, String> {
    if !seconds.is_finite() {
        return Err("GPX time must be finite.".into());
    }
    let kept = kept_intervals(duration, cuts)?;
    let mut elapsed = 0.0;
    for clip in &kept {
        let end = elapsed + clip.duration();
        if seconds < end || (end_boundary && (seconds - end).abs() < 1e-7) {
            return Ok(clip.start + seconds - elapsed);
        }
        elapsed = end;
    }
    Ok(kept.last().unwrap().end + seconds - elapsed)
}

pub fn edited_time(seconds: f64, duration: f64, cuts: &[Clip]) -> Result<f64, String> {
    if !seconds.is_finite() {
        return Err("GPX time must be finite.".into());
    }
    let kept = kept_intervals(duration, cuts)?;
    let mut removed = 0.0;
    let mut cursor = 0.0;
    for clip in kept {
        removed += (seconds - cursor).clamp(0.0, clip.start - cursor);
        cursor = clip.end;
    }
    removed += (seconds - cursor).clamp(0.0, duration - cursor);
    Ok(seconds - removed)
}

pub fn rebuild_cuts(track: &Track, cuts: &[CutRange]) -> Result<Track, String> {
    if cuts.iter().all(|cut| matches!(cut, CutRange::Time(_))) {
        let times: Vec<_> = cuts
            .iter()
            .filter_map(|cut| match cut {
                CutRange::Time(clip) => Some(*clip),
                _ => None,
            })
            .collect();
        cut_ranges(track, &times)
    } else if cuts.iter().all(|cut| matches!(cut, CutRange::Distance(_))) {
        let mut result = track.clone();
        for cut in cuts {
            if let CutRange::Distance(range) = cut {
                result = result.cut_distance(*range)?;
            }
        }
        Ok(result)
    } else {
        Err("A cut list cannot mix original GPX times and untimed distance ranges.".into())
    }
}

pub fn cut_ranges(track: &Track, cuts: &[Clip]) -> Result<Track, String> {
    if cuts.is_empty() {
        return Ok(track.clone());
    }
    let duration = timing(track)?.duration;
    let clips = kept_intervals(duration, cuts)?;
    let edited = edit_gpx(track, &track.name, &track.description, &clips, 0.0)?;
    let current = Track::parse(edited.gpx)?;
    let mut original = track.clone();
    original.reset();
    let baseline = edit_gpx(&original, &track.name, &track.description, &clips, 0.0)?;
    let mut result = Track::parse(baseline.gpx)?;
    for ((point, current), source_index) in result
        .points
        .iter_mut()
        .zip(current.points)
        .zip(edited.source_indices)
    {
        point.elevation = current.elevation;
        point.source_index = source_index;
    }
    Ok(result)
}

pub fn validate_incyclist(edited: &EditedTimeline) -> Result<(), String> {
    let track = Track::parse(edited.gpx.clone())?;
    let timing = timing(&track)?;
    let mut moving = Vec::new();
    let mut last: Option<OutputPoint> = None;
    for (point, time) in track.points.iter().zip(timing.seconds) {
        let sample = OutputPoint {
            source_index: point.source_index,
            xml: String::new(),
            group: point.segment,
            latitude: point.latitude,
            longitude: point.longitude,
            time,
        };
        if last
            .as_ref()
            .is_none_or(|last| separation(last, &sample) >= 1.0)
        {
            moving.push(time);
            last = Some(sample);
        }
    }
    if moving.len() < 2 {
        return Err("The GPX has fewer than two moving samples at least 1 m apart; Incyclist cannot build a ride from it.".into());
    }
    for cut in &edited.cuts {
        // GPX timestamps and pairing cut markers are both serialized to milliseconds.
        let cut = (cut * 1000.0).round() / 1000.0;
        let index = moving
            .iter()
            .position(|time| (time - cut).abs() < 1e-6)
            .ok_or("A geographic cut has no matching moving sample.")?;
        if index == 0 || index + 1 == moving.len() {
            return Err("A moving cut needs moving GPX samples before and after it. Keep a longer interval around the edit.".into());
        }
    }
    Ok(())
}

struct EditOptions<'a> {
    offset: f64,
    boundary_step: f64,
    level: bool,
    samples: &'a [f64],
    exact_ends: &'a [f64],
}

fn edit_intervals(
    track: &Track,
    name: &str,
    description: &str,
    clips: &[Clip],
    options: EditOptions<'_>,
) -> Result<EditedTimeline, String> {
    let EditOptions {
        offset,
        boundary_step,
        level,
        samples: extra_samples,
        exact_ends,
    } = options;
    if !offset.is_finite() {
        return Err("The GPX/video offset must be finite.".into());
    }
    let xml = track.export(name, description)?;
    let doc = Document::parse(&xml).map_err(|error| error.to_string())?;
    let nodes = track_nodes(&doc)?;
    let timing = read_timing(&nodes)?;
    if clips
        .iter()
        .any(|clip| clip.start + offset < -1e-6 || clip.end + offset > timing.duration + 1e-6)
    {
        return Err("A kept interval has no matching GPX data. Adjust the offset or trim to the shared recording interval.".into());
    }
    let ns = doc.root_element().tag_name().namespace();
    let container = nodes[0].parent().unwrap().parent().unwrap();
    let groups: Vec<_> = container
        .children()
        .filter(|node| named(*node, "trkseg", ns))
        .collect();
    let mut output = String::new();
    let mut elapsed = 0.0;
    let mut count = 0;
    let mut cuts = Vec::new();
    let mut previous: Option<OutputPoint> = None;
    let mut group_open: Option<usize> = None;
    let mut last_moving: Option<OutputPoint> = None;
    let mut moving_times = Vec::new();
    let mut elevation_shift = 0.0;
    let mut last_elevation = None;
    let mut elevation_edits = Vec::new();
    let mut source_indices = Vec::new();
    let mut source_times = Vec::new();
    for (clip_index, clip) in clips.iter().enumerate() {
        let start = (clip.start + offset).max(0.0);
        let end = (clip.end + offset).min(timing.duration);
        let exact_end =
            clip_index + 1 < clips.len() && exact_ends.iter().any(|time| (time - end).abs() < 1e-7);
        let last_time = if clip_index + 1 == clips.len() || exact_end {
            end
        } else {
            end - boundary_step
        };
        if last_time <= start {
            return Err("A retained GPX interval is too short; keep more than one millisecond around the cut.".into());
        }
        let mut samples = vec![start];
        samples.extend(
            timing
                .seconds
                .iter()
                .copied()
                .filter(|time| *time > start + 1e-7 && *time < last_time - 1e-7),
        );
        samples.extend(
            extra_samples
                .iter()
                .copied()
                .filter(|time| *time > start + 1e-7 && *time < last_time - 1e-7),
        );
        samples.push(last_time);
        samples.sort_unstable_by(f64::total_cmp);
        samples.dedup_by(|a, b| (*a - *b).abs() < 1e-7);
        for source_time in samples {
            let point = point_at(
                track,
                &timing,
                &nodes,
                &xml,
                source_time,
                elapsed + source_time
                    - start
                    - if exact_end && source_time == end {
                        boundary_step
                    } else {
                        0.0
                    },
            )?;
            if level {
                let right = timing.seconds.partition_point(|time| *time < source_time);
                let elevation = if right == 0 || timing.seconds[right] == source_time {
                    track.points[right].elevation
                } else {
                    let left = right - 1;
                    let fraction = (source_time - timing.seconds[left])
                        / (timing.seconds[right] - timing.seconds[left]);
                    track.points[left].elevation
                        + fraction * (track.points[right].elevation - track.points[left].elevation)
                };
                if source_time == start && clip_index > 0 {
                    elevation_shift = last_elevation.unwrap() - elevation;
                }
                last_elevation = Some(elevation + elevation_shift);
                elevation_edits.push(elevation + elevation_shift);
            }
            if previous
                .as_ref()
                .is_some_and(|last| point.time <= last.time)
            {
                return Err("Two retained GPX points map to the same millisecond. Use a lower output frame rate or repair sub-millisecond timestamps.".into());
            }
            let new_group = previous
                .as_ref()
                .is_none_or(|last| last.group != point.group)
                || (source_time - start).abs() < 1e-7;
            if new_group {
                if let Some(index) = group_open.take() {
                    close_group(&mut output, &xml, groups[index], ns);
                }
                let group = groups[point.group - 1];
                output.push_str(&opening(&xml, group));
                group_open = Some(point.group - 1);
                // Incyclist flattens segments; its cut markers remove cross-cut distance jumps.
                if last_moving
                    .as_ref()
                    .is_some_and(|last| separation(last, &point) >= 1.0)
                {
                    cuts.push(point.time);
                }
            }
            if last_moving
                .as_ref()
                .is_none_or(|last| separation(last, &point) >= 1.0)
            {
                last_moving = Some(point.clone_without_xml());
                moving_times.push(point.time);
            }
            output.push_str(&point.xml);
            source_indices.push(point.source_index);
            source_times.push(source_time);
            count += 1;
            previous = Some(point);
        }
        elapsed += clip.duration();
    }
    if let Some(index) = group_open {
        close_group(&mut output, &xml, groups[index], ns);
    }
    if !level && moving_times.len() < 2 {
        return Err("The kept GPX recording has fewer than two moving samples at least 1 m apart; Incyclist cannot build a ride from it.".into());
    }
    for cut in cuts.iter().filter(|_| !level) {
        let index = moving_times.iter().position(|time| time == cut).unwrap();
        if index == 0 || index + 1 == moving_times.len() {
            return Err("A moving cut needs a moving GPX sample before and after the cut point. Keep a longer interval around this edit.".into());
        }
    }
    let first_group = groups[0].range().start;
    let mut gpx = xml[..first_group].to_owned();
    gpx.push_str(&output);
    let mut cursor = first_group;
    for group in &groups {
        gpx.push_str(&xml[cursor..group.range().start]);
        cursor = group.range().end;
    }
    gpx.push_str(&xml[cursor..]);
    if level {
        let mut track = Track::parse(gpx)?;
        for (point, elevation) in track.points.iter_mut().zip(elevation_edits) {
            point.elevation = elevation;
        }
        gpx = track.export(name, description)?;
    }
    Ok(EditedTimeline {
        gpx,
        cuts,
        duration: elapsed,
        points: count,
        source_indices,
        source_times,
    })
}

fn close_group(output: &mut String, xml: &str, group: Node<'_, '_>, ns: Option<&str>) {
    for child in group
        .children()
        .filter(|child| child.is_element() && !named(*child, "trkpt", ns))
    {
        output.push_str(&xml[child.range()]);
    }
    output.push_str(&format!("</{}>", tag(xml, group)));
}

impl OutputPoint {
    fn clone_without_xml(&self) -> Self {
        Self {
            xml: String::new(),
            group: self.group,
            latitude: self.latitude,
            longitude: self.longitude,
            time: self.time,
            source_index: self.source_index,
        }
    }
}

pub fn add_cut_markers(pairing: &str, cuts: &[f64]) -> Result<String, String> {
    let mut tags = String::new();
    for seconds in cuts {
        if !seconds.is_finite() || *seconds <= 0.0 {
            return Err("Invalid Incyclist cut time.".into());
        }
        let millis = (seconds * 1000.0).round() as u64;
        // Incyclist adds H:M:S floats, then matches GPX milliseconds / 1000 exactly.
        // Keep all seconds in one component to avoid rounding during that addition.
        tags.push_str(&format!(
            "    <cut time=\"0:00:{:02}.{:03}\"/>\n",
            millis / 1000,
            millis % 1000
        ));
    }
    if !pairing.contains("</gpx-import>") {
        return Err("Invalid Incyclist pairing document.".into());
    }
    Ok(if tags.is_empty() {
        pairing.to_owned()
    } else {
        pairing.replace(
            "</gpx-import>",
            &format!("  <cuts>\n{tags}  </cuts>\n</gpx-import>"),
        )
    })
}
