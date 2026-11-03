use crate::{
    gpx::Track,
    timeline::{self, Clip, EditedTimeline},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    Normal,
    VideoStart,
    VideoEnd,
    BeforeCut,
    AfterCut,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Landmark",
            Self::VideoStart => "Video start",
            Self::VideoEnd => "Video end",
            Self::BeforeCut => "Before cut",
            Self::AfterCut => "After cut",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SyncPoint {
    pub video: f64,
    pub gpx: f64,
    pub kind: Kind,
}

#[derive(Clone, Debug)]
pub struct SyncMap {
    points: Vec<SyncPoint>,
    edited: Vec<f64>,
    cuts: Vec<Clip>,
    source_duration: f64,
    retained_duration: f64,
    video_duration: f64,
    unpaired_boundaries: bool,
}

impl SyncMap {
    pub fn new(
        points: &[SyncPoint],
        manual_cuts: &[Clip],
        source_duration: f64,
        video_duration: f64,
    ) -> Result<Self, String> {
        if points.is_empty()
            || !source_duration.is_finite()
            || source_duration <= 0.0
            || !video_duration.is_finite()
            || video_duration <= 0.0
        {
            return Err(
                "Synchronization needs a timestamped GPX, a video and at least one sync point."
                    .into(),
            );
        }
        let mut points = points.to_vec();
        points.sort_by(|a, b| a.video.total_cmp(&b.video));
        let mut unpaired_boundaries = false;
        let landmarks = points.clone();
        for (index, point) in points.iter_mut().enumerate() {
            let paired = match point.kind {
                Kind::BeforeCut => landmarks
                    .get(index + 1)
                    .is_some_and(|next| next.kind == Kind::AfterCut),
                Kind::AfterCut => index > 0 && landmarks[index - 1].kind == Kind::BeforeCut,
                Kind::Normal | Kind::VideoStart | Kind::VideoEnd => true,
            };
            if !paired {
                // A single cut boundary aligns the timelines until its partner is picked.
                point.kind = Kind::Normal;
                unpaired_boundaries = true;
            }
        }
        let mut cuts = manual_cuts.to_vec();
        for (index, point) in points.iter().enumerate() {
            if !point.video.is_finite()
                || !point.gpx.is_finite()
                || point.video < 0.0
                || point.video > video_duration
                || point.gpx < 0.0
                || point.gpx > source_duration
            {
                return Err(format!(
                    "Sync point {} is outside the video or original GPX timeline.",
                    index + 1
                ));
            }
            if index > 0
                && (point.video <= points[index - 1].video || point.gpx <= points[index - 1].gpx)
            {
                return Err(
                    "Sync points must advance strictly in both video and original GPX time.".into(),
                );
            }
            match point.kind {
                Kind::VideoStart => {
                    if index != 0 || point.video != 0.0 || point.gpx >= source_duration {
                        return Err("The video start must anchor video time zero to a GPX position before the end of the track.".into());
                    }
                    if point.gpx > 0.0 {
                        cuts.push(Clip {
                            start: 0.0,
                            end: point.gpx,
                        });
                    }
                }
                Kind::VideoEnd => {
                    if index + 1 != points.len() || point.video <= 0.0 || point.gpx <= 0.0 {
                        return Err("The video end must be the last sync point and retain a positive GPX interval.".into());
                    }
                    if point.gpx < source_duration {
                        cuts.push(Clip {
                            start: point.gpx,
                            end: source_duration,
                        });
                    }
                }
                Kind::BeforeCut => {
                    let after = points
                        .get(index + 1)
                        .filter(|next| next.kind == Kind::AfterCut)
                        .ok_or("Each Before cut point must be followed by an After cut point.")?;
                    cuts.push(Clip {
                        start: point.gpx,
                        end: after.gpx,
                    });
                }
                Kind::AfterCut if index == 0 || points[index - 1].kind != Kind::BeforeCut => {
                    return Err(
                        "Each After cut point needs an immediately preceding Before cut point."
                            .into(),
                    )
                }
                _ => {}
            }
        }
        let kept = timeline::kept_intervals(source_duration, &cuts)?;
        for point in &points {
            if !kept
                .iter()
                .any(|clip| point.gpx >= clip.start && point.gpx <= clip.end)
            {
                return Err("A sync point lies inside an omitted GPX interval. Revise the cut or the sync point.".into());
            }
            match point.kind {
                Kind::BeforeCut if !kept.iter().any(|clip| point.gpx == clip.end) =>
                    return Err("A Before cut point must end a retained GPX section. Manual cuts must not cover or extend its boundary.".into()),
                Kind::AfterCut if !kept.iter().any(|clip| point.gpx == clip.start) =>
                    return Err("An After cut point must start a retained GPX section. Manual cuts must not cover or extend its boundary.".into()),
                Kind::Normal if kept.iter().any(|clip| point.gpx == clip.end && clip.end < kept.last().unwrap().end) =>
                    return Err("Use a Before cut / After cut pair for a sync point at the beginning of an omitted interval.".into()),
                _ => {}
            }
        }
        let edited: Vec<_> = points
            .iter()
            .map(|point| timeline::edited_time(point.gpx, source_duration, &cuts))
            .collect::<Result<_, _>>()?;
        for index in 1..points.len() {
            let jump =
                points[index - 1].kind == Kind::BeforeCut && points[index].kind == Kind::AfterCut;
            if (!jump && edited[index] <= edited[index - 1])
                || (jump && (edited[index] - edited[index - 1]).abs() > 1e-6)
            {
                return Err("Sync points do not define a forward retained timeline. Use paired cut boundaries for a jump.".into());
            }
        }
        Ok(Self {
            points,
            edited,
            cuts,
            source_duration,
            retained_duration: kept.iter().map(|clip| clip.duration()).sum(),
            video_duration,
            unpaired_boundaries,
        })
    }

    pub fn has_unpaired_boundaries(&self) -> bool {
        self.unpaired_boundaries
    }

    pub fn cuts(&self) -> &[Clip] {
        &self.cuts
    }

    pub fn validate_source(&self, source: &Track) -> Result<(), String> {
        let timing = timeline::timing(source)?;
        if (timing.duration - self.source_duration).abs() > 1e-6 {
            return Err(
                "The synchronization mapping belongs to a different original GPX timeline.".into(),
            );
        }
        if self
            .points
            .iter()
            .any(|point| timeline::position_at(&timing, source, point.gpx).is_none())
        {
            return Err("A sync point falls between disconnected GPX segments. Choose a recorded route location.".into());
        }
        Ok(())
    }

    fn rate(&self, first: bool) -> f64 {
        let mut spans = self.points.windows(2).zip(self.edited.windows(2));
        let span = if first {
            spans.find(|(_, gpx)| gpx[1] > gpx[0])
        } else {
            spans.rfind(|(_, gpx)| gpx[1] > gpx[0])
        };
        span.map_or(1.0, |(video, gpx)| {
            (gpx[1] - gpx[0]) / (video[1].video - video[0].video)
        })
    }

    /// Edited GPX seconds per video second, including zero during a cut hold.
    pub fn edited_rate_at(&self, video: f64) -> f64 {
        let right = self.points.partition_point(|point| point.video <= video);
        if right == 0 {
            self.rate(true)
        } else if right == self.points.len() {
            if self.points[right - 1].kind == Kind::VideoEnd {
                0.0
            } else {
                self.rate(false)
            }
        } else {
            (self.edited[right] - self.edited[right - 1])
                / (self.points[right].video - self.points[right - 1].video)
        }
    }

    pub fn edited_at(&self, video: f64) -> f64 {
        let right = self.points.partition_point(|point| point.video <= video);
        if right == 0 {
            return self.edited[0] + (video - self.points[0].video) * self.rate(true);
        }
        if right == self.points.len() {
            if self.points[right - 1].kind == Kind::VideoEnd {
                return self.edited[right - 1];
            }
            return self.edited[right - 1]
                + (video - self.points[right - 1].video) * self.rate(false);
        }
        let fraction = (video - self.points[right - 1].video)
            / (self.points[right].video - self.points[right - 1].video);
        self.edited[right - 1] + fraction * (self.edited[right] - self.edited[right - 1])
    }

    pub fn original_at(&self, video: f64) -> Result<f64, String> {
        let right = self.points.partition_point(|point| point.video <= video);
        if right > 0
            && right < self.points.len()
            && self.points[right - 1].kind == Kind::BeforeCut
            && self.points[right].kind == Kind::AfterCut
        {
            return Ok(self.points[right - 1].gpx);
        }
        timeline::original_time(
            self.edited_at(video),
            self.source_duration,
            &self.cuts,
            false,
        )
    }

    pub fn video_at(&self, edited: f64) -> f64 {
        let right = self.edited.partition_point(|time| *time <= edited);
        if right == 0 {
            return self.points[0].video + (edited - self.edited[0]) / self.rate(true);
        }
        if right == self.points.len() {
            return self.points[right - 1].video
                + (edited - self.edited[right - 1]) / self.rate(false);
        }
        let fraction =
            (edited - self.edited[right - 1]) / (self.edited[right] - self.edited[right - 1]);
        self.points[right - 1].video
            + fraction * (self.points[right].video - self.points[right - 1].video)
    }

    pub fn coverage_error(&self) -> Option<String> {
        let start = self.edited_at(0.0);
        let end = self.edited_at(self.video_duration);
        if start >= -1e-6 && end <= self.retained_duration + 1e-6 && end > start {
            return None;
        }
        let mut details = vec![format!(
            "Sync points do not provide GPX coverage for the entire video.\n\
             Video: 0.000 to {:.3} s. Retained GPX: 0.000 to {:.3} s after cuts.\n\
             Video boundaries map to retained GPX {start:.3} and {end:.3} s.",
            self.video_duration, self.retained_duration,
        )];
        if start < -1e-6 {
            let first = self.points[0];
            details.push(format!(
                "Start: video 0.000 to {:.3} s is uncovered ({:.3} s of video; {:.3} s before retained GPX start).\n\
                 First sync point: video {:.3} s / original GPX {:.3} s; extrapolated rate {:.6} GPX s/video s.",
                self.video_at(0.0).clamp(0.0, self.video_duration),
                self.video_at(0.0).clamp(0.0, self.video_duration),
                -start, first.video, first.gpx, self.rate(true),
            ));
        }
        if end > self.retained_duration + 1e-6 {
            let last = self.points.last().unwrap();
            let boundary = self
                .video_at(self.retained_duration)
                .clamp(0.0, self.video_duration);
            details.push(format!(
                "End: video {boundary:.3} to {:.3} s is uncovered ({:.3} s of video; {:.3} s beyond retained GPX end).\n\
                 Last sync point: video {:.3} s / original GPX {:.3} s; extrapolated rate {:.6} GPX s/video s.",
                self.video_duration, self.video_duration - boundary,
                end - self.retained_duration, last.video, last.gpx, self.rate(false),
            ));
        }
        if end <= start {
            details.push("The mapping has no forward GPX duration across the video. Revise the cut boundary pairs.".into());
        }
        details.push("Verify landmarks at the affected end(s). Boundary suggestions assume the first/last retained GPX location matches the video boundary; check the footage before applying. The video is never trimmed or changed.".into());
        Some(details.join("\n"))
    }

    /// Suggest only missing endpoints, preserving all existing landmarks and cut pairs.
    pub fn suggest_coverage_points(&self) -> Result<Vec<SyncPoint>, String> {
        if self.coverage_error().is_none() {
            return Ok(Vec::new());
        }
        let kept = timeline::kept_intervals(self.source_duration, &self.cuts)?;
        let mut additions = Vec::new();
        if self.edited_at(0.0) < -1e-6 {
            additions.push(SyncPoint {
                video: 0.0,
                gpx: kept.first().unwrap().start,
                kind: Kind::Normal,
            });
        }
        if self.edited_at(self.video_duration) > self.retained_duration + 1e-6 {
            additions.push(SyncPoint {
                video: self.video_duration,
                gpx: kept.last().unwrap().end,
                kind: Kind::Normal,
            });
        }
        if additions.is_empty() {
            return Err("No safe endpoint addition is available. Correct the existing cut boundary pairs using the footage.".into());
        }
        let mut points = self.points.clone();
        points.extend_from_slice(&additions);
        let candidate = Self::new(
            &points,
            &self.cuts,
            self.source_duration,
            self.video_duration,
        )
        .map_err(|error| format!("Cannot add boundary landmarks without conflicting with existing sync points: {error} Verify the affected landmarks manually."))?;
        if let Some(error) = candidate.coverage_error() {
            return Err(error);
        }
        Ok(additions)
    }

    pub fn export(
        &self,
        source: &Track,
        name: &str,
        description: &str,
    ) -> Result<EditedTimeline, String> {
        self.validate_source(source)?;
        if let Some(error) = self.coverage_error() {
            return Err(error);
        }
        let kept = timeline::kept_intervals(self.source_duration, &self.cuts)?;
        let source_start = self.original_at(0.0)?;
        let source_end = self.original_at(self.video_duration)?;
        let clips: Vec<_> = kept
            .iter()
            .filter_map(|clip| {
                let start = clip.start.max(source_start);
                let end = clip.end.min(source_end);
                (end > start).then_some(Clip { start, end })
            })
            .collect();
        let samples: Vec<_> = self.points.iter().map(|point| point.gpx).collect();
        let edges: Vec<_> = self
            .points
            .iter()
            .filter(|point| point.kind == Kind::BeforeCut)
            .map(|point| point.gpx)
            .collect();
        let mut result =
            timeline::edit_gpx_samples(source, name, description, &clips, &samples, &edges)?;
        if self.points.iter().any(|point| {
            !result
                .source_times
                .iter()
                .any(|time| (time - point.gpx).abs() < 1e-7)
        }) {
            return Err("A sync point has no retained GPX coverage at an export boundary. Keep footage on both sides of each jump.".into());
        }
        let mut times: Vec<_> = result
            .source_times
            .iter()
            .map(|seconds| {
                if let Some(point) = self
                    .points
                    .iter()
                    .find(|point| (point.gpx - seconds).abs() < 1e-7)
                {
                    return Ok(point.video);
                }
                Ok(self.video_at(timeline::edited_time(
                    *seconds,
                    self.source_duration,
                    &self.cuts,
                )?))
            })
            .collect::<Result<_, String>>()?;
        let end_hold = self
            .points
            .last()
            .is_some_and(|point| point.kind == Kind::VideoEnd && point.video < self.video_duration);
        if times.first().is_none_or(|time| time.abs() > 0.0005)
            || times
                .last()
                .is_none_or(|time| !end_hold && (time - self.video_duration).abs() > 0.0005)
        {
            return Err("The synchronized GPX does not cover the full prepared video. Revise the first and last sync points.".into());
        }
        let exported_timing = timeline::timing(&Track::parse(result.gpx.clone())?)?;
        // Keep cut markers attached to their samples across rounding and cut holds.
        result.cuts = result
            .cuts
            .iter()
            .map(|cut| {
                let index = exported_timing
                    .seconds
                    .iter()
                    .position(|time| (time - cut).abs() < 1e-7)
                    .ok_or("A geographic cut has no matching GPX sample before synchronization.")?;
                Ok(times[index])
            })
            .collect::<Result<_, String>>()?;
        if end_hold {
            // The final frame still occupies time up to the video's end boundary.
            result.gpx = timeline::append_last_point(&result.gpx)?;
            result.source_times.push(
                *result
                    .source_times
                    .last()
                    .ok_or("The synchronized GPX has no final sample.")?,
            );
            result.source_indices.push(
                *result
                    .source_indices
                    .last()
                    .ok_or("The synchronized GPX has no final source index.")?,
            );
            result.points += 1;
            times.push(self.video_duration);
        }
        result.gpx = timeline::retime_gpx(&result.gpx, &times)?;
        result.duration = self.video_duration;
        Ok(result)
    }
}
