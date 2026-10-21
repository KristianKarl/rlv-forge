use crate::{
    gpx::Track,
    timeline::{self, Clip, EditedTimeline},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    Normal,
    BeforeCut,
    AfterCut,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Landmark",
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
                Kind::Normal => true,
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
                Kind::Normal if kept.iter().any(|clip| point.gpx == clip.end && clip.end < source_duration) =>
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
            self.rate(false)
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

    pub fn export(
        &self,
        source: &Track,
        name: &str,
        description: &str,
    ) -> Result<EditedTimeline, String> {
        self.validate_source(source)?;
        let start = self.edited_at(0.0);
        let end = self.edited_at(self.video_duration);
        let kept = timeline::kept_intervals(self.source_duration, &self.cuts)?;
        let retained_duration: f64 = kept.iter().map(|clip| clip.duration()).sum();
        if start < -1e-6 || end > retained_duration + 1e-6 || end <= start {
            return Err("Sync points do not provide GPX coverage for the entire video. Add or correct landmarks near both ends.".into());
        }
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
        let times: Vec<_> = result
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
        if times.first().is_none_or(|time| time.abs() > 0.0005)
            || times
                .last()
                .is_none_or(|time| (time - self.video_duration).abs() > 0.0005)
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
        result.gpx = timeline::retime_gpx(&result.gpx, &times)?;
        result.duration = self.video_duration;
        Ok(result)
    }
}
