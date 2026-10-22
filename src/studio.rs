use eframe::egui::{self, vec2, Color32, TextureHandle, TextureOptions};
use rlv_forge::{
    bundle,
    gpx::Track,
    sync::{Kind, SyncMap, SyncPoint},
    timeline::{self, Clip, CutRange, Timing},
    video::{self, VideoInfo},
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
        Arc,
    },
    time::{Duration, Instant},
};

enum Job {
    Loaded(PathBuf, VideoInfo),
    Exported(bundle::ExportedBundle),
    Cancelled,
}

pub enum Edit {
    Cut(Clip),
    ReplaceCuts(Vec<CutRange>),
    Sync(Vec<SyncPoint>),
}

pub struct Tracks<'a> {
    pub current: Option<&'a Arc<Track>>,
    pub source: Option<&'a Arc<Track>>,
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    path: Option<PathBuf>,
    info: Option<VideoInfo>,
    offset: f64,
    anchor: f64,
    marked: Clip,
    folder: String,
    country: String,
    cuts: Vec<CutRange>,
    source_duration: Option<f64>,
    #[serde(skip)]
    source_timing: Option<Arc<Timing>>,
    sync_points: Vec<SyncPoint>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ProjectState {
    settings: Settings,
    position: f64,
    cut_draft: Option<Vec<CutRange>>,
    sync_draft: Option<Vec<SyncPoint>>,
}

impl ProjectState {
    pub fn resolve_video(&mut self, directory: &std::path::Path) {
        if let Some(path) = &mut self.settings.path {
            if path.is_relative() {
                *path = directory.join(&*path);
            }
        }
    }
}

pub struct Studio {
    settings: Settings,
    cut_draft: Option<Vec<CutRange>>,
    sync_draft: Option<Vec<SyncPoint>>,
    sync_map: Option<SyncMap>,
    pick_kind: Option<Kind>,
    pick_candidates: Vec<f64>,
    pick_time: f64,
    timing: Result<Timing, String>,
    worker: Option<Receiver<Result<Job, String>>>,
    preview_worker: Option<Receiver<(f64, Result<video::PreviewFrame, String>)>>,
    cancel: Arc<AtomicBool>,
    texture: Option<TextureHandle>,
    decoded_at: Option<f64>,
    playback: Option<video::Playback>,
    playback_clock: Option<(Instant, f64)>,
    profile: crate::ui::LinkedProfile,
    position: f64,
    playing: bool,
    pub dirty: bool,
    status: String,
    error: Option<String>,
    exporting: bool,
}

impl Default for Studio {
    fn default() -> Self {
        Self {
            settings: Settings {
                path: None,
                info: None,
                offset: 0.0,
                anchor: 0.0,
                marked: Clip {
                    start: 0.0,
                    end: 0.0,
                },
                folder: "incyclist-ride".into(),
                country: String::new(),
                cuts: Vec::new(),
                source_duration: None,
                source_timing: None,
                sync_points: Vec::new(),
            },
            cut_draft: None,
            sync_draft: None,
            sync_map: None,
            pick_kind: None,
            pick_candidates: Vec::new(),
            pick_time: 0.0,
            timing: Err("Open a timestamped GPX track first.".into()),
            worker: None,
            preview_worker: None,
            cancel: Arc::new(AtomicBool::new(false)),
            texture: None,
            decoded_at: None,
            playback: None,
            playback_clock: None,
            profile: Default::default(),
            position: 0.0,
            playing: false,
            dirty: false,
            status: "Open a prepared video for reference. Only the GPX will be edited.".into(),
            error: None,
            exporting: false,
        }
    }
}

impl Studio {
    pub fn position(&self) -> f64 {
        self.position
    }

    pub fn project_state(&self) -> Result<ProjectState, String> {
        if self.pick_kind.is_some() {
            return Err("Finish or cancel landmark picking before saving the project.".into());
        }
        let mut settings = self.settings.clone();
        settings.path = settings
            .path
            .as_deref()
            .map(std::path::absolute)
            .transpose()
            .map_err(|error| format!("Cannot save the video's reference path: {error}"))?;
        Ok(ProjectState {
            settings,
            position: self.position,
            cut_draft: self.cut_draft.clone(),
            sync_draft: self.sync_draft.clone(),
        })
    }

    pub fn from_project(
        state: ProjectState,
        source: &Track,
        track: &Track,
    ) -> Result<Self, String> {
        let mut studio = Self {
            settings: state.settings,
            ..Default::default()
        };
        let timing = timeline::timing(source).ok().map(Arc::new);
        studio.settings.source_duration = timing.as_ref().map(|timing| timing.duration);
        studio.settings.source_timing = timing;
        if !studio.settings.offset.is_finite()
            || !studio.settings.anchor.is_finite()
            || !studio.settings.marked.start.is_finite()
            || !studio.settings.marked.end.is_finite()
            || !state.position.is_finite()
            || state.position < 0.0
        {
            return Err("Invalid project timeline settings.".into());
        }
        match (&studio.settings.path, &studio.settings.info) {
            (Some(_), Some(info))
                if info.duration.is_finite()
                    && info.duration > 0.0
                    && info.fps.is_finite()
                    && info.fps > 0.0
                    && info.width > 0
                    && info.height > 0
                    && state.position <= info.duration => {}
            (None, None) if state.position == 0.0 => {}
            _ => return Err("Invalid project video reference or metadata.".into()),
        }
        studio.sync_map = studio.plan_for_points(&studio.settings.sync_points)?;
        if let Some(mapping) = &studio.sync_map {
            mapping.validate_source(source)?;
        } else {
            timeline::rebuild_cuts(source, &studio.settings.cuts)?;
        }
        studio.refresh_track(Some(track));
        if let Some(duration) = studio.settings.source_duration {
            let cuts = match &studio.sync_map {
                Some(mapping) => mapping.cuts().to_vec(),
                None => Self::time_cuts(&studio.settings.cuts)?,
            };
            let expected: f64 = timeline::kept_intervals(duration, &cuts)?
                .iter()
                .map(|clip| clip.duration())
                .sum();
            let actual = studio.timing.as_ref().map_err(Clone::clone)?.duration;
            if (actual - expected).abs() > 0.002 {
                return Err("The project's current GPX does not match its applied cuts.".into());
            }
        }
        studio.position = state.position;
        studio.cut_draft = state.cut_draft;
        studio.sync_draft = state.sync_draft;
        studio.status = "Project restored. Playback is paused.".into();
        if studio
            .settings
            .path
            .as_ref()
            .is_some_and(|path| !path.is_file())
        {
            studio.error = Some("The referenced video was not found. Use Open prepared video to locate it; GPX edits and sync points have been restored.".into());
        }
        Ok(studio)
    }

    pub fn settings(&self) -> Settings {
        self.settings.clone()
    }

    pub fn video_path(&self) -> Option<PathBuf> {
        self.settings.path.clone()
    }

    pub fn elapsed_at(&self, index: usize) -> Option<f64> {
        self.timing.as_ref().ok()?.seconds.get(index).copied()
    }

    fn gpx_time(&self) -> f64 {
        if self.pick_kind.is_some() {
            self.pick_time
        } else {
            self.gpx_at(self.position)
        }
    }

    fn gpx_at(&self, video: f64) -> f64 {
        self.sync_map
            .as_ref()
            .map_or(video + self.settings.offset, |map| map.edited_at(video))
    }

    pub fn mapping(&self) -> Option<&SyncMap> {
        self.sync_map.as_ref()
    }

    pub fn sync_points(&self) -> &[SyncPoint] {
        &self.settings.sync_points
    }

    pub fn plan_for_points(&self, points: &[SyncPoint]) -> Result<Option<SyncMap>, String> {
        self.plan(points, &self.settings.cuts)
    }

    pub fn plan_for_cuts(&self, cuts: &[CutRange]) -> Result<Option<SyncMap>, String> {
        self.plan(&self.settings.sync_points, cuts)
    }

    fn plan(&self, points: &[SyncPoint], cuts: &[CutRange]) -> Result<Option<SyncMap>, String> {
        if points.is_empty() {
            return Ok(None);
        }
        let duration = self
            .settings
            .source_duration
            .ok_or("Sync points need a timestamped original GPX.")?;
        let video = self
            .settings
            .info
            .as_ref()
            .ok_or("Open a prepared video before adding sync points.")?;
        Ok(Some(SyncMap::new(
            points,
            &Self::time_cuts(cuts)?,
            duration,
            video.duration,
        )?))
    }

    pub fn commit_sync(&mut self, track: &Track, mut points: Vec<SyncPoint>) -> Result<(), String> {
        points.sort_by(|a, b| a.video.total_cmp(&b.video));
        let mapping = self.plan_for_points(&points)?;
        self.settings.sync_points = points;
        self.sync_map = mapping;
        self.sync_draft = None;
        self.pick_kind = None;
        self.pick_candidates.clear();
        self.refresh_track(Some(track));
        self.settings.marked = Clip {
            start: 0.0,
            end: self.timing.as_ref().map_or(0.0, |timing| timing.duration),
        };
        self.pause();
        self.dirty = true;
        Ok(())
    }

    fn route_position(&self, track: Option<&Track>) -> Option<[f64; 2]> {
        timeline::position_at(self.timing.as_ref().ok()?, track?, self.gpx_time())
    }

    fn speed_kmh(&self, source: Option<&Track>) -> Option<f64> {
        if !self.playing || self.pick_kind.is_some() {
            return None;
        }
        let source = source?;
        let timing = self.settings.source_timing.as_deref()?;
        let (seconds, rate) = if let Some(mapping) = &self.sync_map {
            (
                mapping.original_at(self.position).ok()?,
                mapping.edited_rate_at(self.position),
            )
        } else {
            (
                timeline::original_time(
                    self.gpx_time(),
                    timing.duration,
                    &Self::time_cuts(self.cuts()).ok()?,
                    false,
                )
                .ok()?,
                1.0,
            )
        };
        if rate == 0.0 {
            return timeline::position_at(timing, source, seconds).map(|_| 0.0);
        }
        Some(timeline::speed_at(timing, source, seconds)? * rate * 3.6)
    }

    fn speed_control(&self, ui: &mut egui::Ui, source: Option<&Track>) -> egui::Response {
        let text = self.speed_kmh(source).map_or_else(
            || "Speed: -- km/h".into(),
            |speed| format!("Speed: {speed:.1} km/h"),
        );
        ui.add(egui::Label::new(text).truncate())
            .on_hover_text("Riding speed at the synchronized video position, including timing drift correction. Available during playback within GPX coverage; cut jumps add no distance.")
    }

    fn sync_tools(&mut self, ui: &mut egui::Ui, source: Option<&Track>) -> Option<Vec<SyncPoint>> {
        let mut apply = None;
        ui.heading("Sync video frames to the original route");
        ui.label("Press Pick Landmark to freeze the video. Position the GPX marker using the map or its timeline, then press Pick Landmark again to create and automatically apply the sync point.");
        ui.small("Landmarks correct alignment and drift. Use consecutive Before cut / After cut points on the last/first frames around a video edit to skip omitted GPX sections.");
        ui.add_enabled_ui(!self.busy() && self.settings.source_timing.is_some() && self.cut_draft.is_none(), |ui| {
            ui.horizontal_wrapped(|ui| {
                for (label, kind) in [("Pick Landmark", Kind::Normal), ("Pick before cut", Kind::BeforeCut), ("Pick after cut", Kind::AfterCut)] {
                    let active = self.pick_kind == Some(kind);
                    if ui.add_enabled(active || (self.pick_kind.is_none() && self.preview_ready()), egui::Button::new(label).selected(active)).clicked() {
                        if active {
                            if let (Some(source), Some(timing)) = (source, &self.settings.source_timing) {
                                if timeline::position_at(timing, source, self.pick_time).is_some() {
                                    self.append_sync_point(self.pick_time);
                                    apply = self.sync_draft.clone();
                                } else {
                                    self.error = Some("Choose a recorded GPX location, not a gap between segments.".into());
                                }
                            } else {
                                self.error = Some("The original GPX is unavailable for picking.".into());
                            }
                        } else if let Err(error) = self.begin_picking(kind) {
                            self.error = Some(error);
                        }
                    }
                }
                if self.pick_kind.is_some() && ui.button("Cancel picking").clicked() {
                    self.pick_kind = None;
                    self.pick_candidates.clear();
                }
            });
            if let Some(kind) = self.pick_kind {
                ui.add(egui::Label::new(format!("Video frozen at {:.3} s | Original GPX {:.3} s | Press {} again to apply.", self.position, self.pick_time, kind.label())).truncate());
            }
            if self.pick_kind.is_some() {
                ui.add(egui::Label::new("For repeated route locations, choose the correct occurrence or adjust the original GPX time.").truncate());
                ui.horizontal_wrapped(|ui| {
                    ui.add_enabled_ui(!self.pick_candidates.is_empty(), |ui| {
                    egui::ComboBox::from_id_salt("route-occurrence")
                        .width(160.0)
                        .selected_text(format!("Original GPX {:.3} s", self.pick_time))
                        .show_ui(ui, |ui| {
                            for &time in &self.pick_candidates {
                                ui.selectable_value(&mut self.pick_time, time, format!("{time:.3} s"));
                            }
                        });
                    });
                    ui.add_sized(vec2(110.0, ui.spacing().interact_size.y), egui::DragValue::new(&mut self.pick_time).speed(0.1).fixed_decimals(3)
                        .range(0.0..=self.settings.source_duration.unwrap_or(0.0)).suffix(" s"));
                });
            }
        });
        if self.cut_draft.is_some() {
            ui.small("Apply or cancel manual cut changes before adding sync points.");
        }
        if self.settings.source_timing.is_none() {
            ui.small("Sync points require timestamps on every GPX point.");
        }
        ui.small("Pick tools require the current video frame. Pause and wait for it to load before picking.");
        if self
            .sync_map
            .as_ref()
            .is_some_and(|mapping| mapping.has_unpaired_boundaries())
        {
            ui.small("An unpaired cut boundary acts as an alignment landmark. Add its Before/After partner to cut and level the GPX section automatically.");
        }
        apply
    }

    fn preview_ready(&self) -> bool {
        self.texture.is_some()
            && self.preview_worker.is_none()
            && self
                .decoded_at
                .is_some_and(|time| (time - self.position).abs() < 0.0005)
    }

    fn begin_picking(&mut self, kind: Kind) -> Result<(), String> {
        let duration = self
            .settings
            .source_duration
            .ok_or("Landmarks require original GPX timestamps.")?;
        let time = if let Some(mapping) = &self.sync_map {
            mapping.original_at(self.position)?
        } else {
            timeline::original_time(
                self.gpx_at(self.position),
                duration,
                &Self::time_cuts(self.cuts())?,
                false,
            )?
        };
        self.pause();
        self.pick_time = time.clamp(0.0, duration);
        self.pick_kind = Some(kind);
        self.pick_candidates.clear();
        self.error = None;
        Ok(())
    }

    fn cancel_picking(&mut self) {
        self.pick_kind = None;
        self.pick_candidates.clear();
    }

    fn append_sync_point(&mut self, gpx: f64) {
        let Some(kind) = self.pick_kind else { return };
        self.sync_draft
            .get_or_insert_with(|| self.settings.sync_points.clone())
            .push(SyncPoint {
                video: self.position,
                gpx,
                kind,
            });
        self.sync_draft
            .as_mut()
            .unwrap()
            .sort_by(|a, b| a.video.total_cmp(&b.video));
        self.pick_kind = None;
        self.pick_candidates.clear();
        self.error = None;
        self.dirty = true;
    }

    fn receive_picks(&mut self, picks: Vec<crate::map::RoutePick>) {
        let Some(timing) = &self.settings.source_timing else {
            return;
        };
        let mut candidates: Vec<_> = picks
            .iter()
            .map(|pick| {
                timing.seconds[pick.left]
                    + pick.fraction * (timing.seconds[pick.right] - timing.seconds[pick.left])
            })
            .collect();
        candidates.sort_by(f64::total_cmp);
        candidates.dedup_by(|a, b| (*a - *b).abs() < 0.001);
        match candidates.as_slice() {
            [] => {
                self.error =
                    Some("Click closer to the original GPX route to select a sync location.".into())
            }
            [time] => {
                self.pick_time = *time;
                self.pick_candidates.clear();
                self.error = None;
            }
            [first, ..] => {
                self.pick_time = *first;
                self.pick_candidates = candidates;
                self.error = None;
            }
        }
    }

    fn sync_overlay(&self, source: Option<&Track>) -> crate::map::SyncOverlay {
        let mut overlay = crate::map::SyncOverlay {
            picking: self.pick_kind.is_some() && !self.busy() && self.preview_ready(),
            ..Default::default()
        };
        if let (Some(source), Some(timing)) = (source, &self.settings.source_timing) {
            for (index, point) in self
                .sync_draft
                .as_ref()
                .unwrap_or(&self.settings.sync_points)
                .iter()
                .enumerate()
            {
                if let Some(position) = timeline::position_at(timing, source, point.gpx) {
                    overlay.markers.push(crate::map::SyncMarker {
                        position,
                        label: (index + 1).to_string(),
                    });
                }
            }
            if !self.pick_candidates.is_empty() {
                if let Some(position) = timeline::position_at(timing, source, self.pick_time) {
                    overlay.markers.push(crate::map::SyncMarker {
                        position,
                        label: "Pick".into(),
                    });
                }
            }
            let original = self
                .sync_map
                .as_ref()
                .and_then(|map| map.original_at(self.position).ok());
            overlay.hint = original.map(|time| {
                timing
                    .seconds
                    .partition_point(|seconds| *seconds < time)
                    .min(source.points.len() - 1)
            });
        }
        overlay
    }

    fn show_sync_points(&mut self, ui: &mut egui::Ui) -> Option<Vec<SyncPoint>> {
        ui.heading("Sync points");
        let mut points = self
            .sync_draft
            .as_deref()
            .unwrap_or(self.sync_points())
            .to_vec();
        let before = points.clone();
        let mut remove = None;
        let mut seek = None;
        ui.add_enabled_ui(self.cut_draft.is_none(), |ui| {
            egui::Grid::new("sync-point-table")
                .striped(true)
                .show(ui, |ui| {
                    for heading in ["#", "Type", "Video seconds", "Original GPX seconds", ""] {
                        ui.strong(heading);
                    }
                    ui.end_row();
                    for (index, point) in points.iter_mut().enumerate() {
                        ui.label((index + 1).to_string());
                        egui::ComboBox::from_id_salt(("sync-kind", index))
                            .selected_text(point.kind.label())
                            .show_ui(ui, |ui| {
                                for kind in [Kind::Normal, Kind::BeforeCut, Kind::AfterCut] {
                                    ui.selectable_value(&mut point.kind, kind, kind.label());
                                }
                            });
                        ui.add(
                            egui::DragValue::new(&mut point.video)
                                .speed(0.01)
                                .fixed_decimals(3)
                                .suffix(" s"),
                        );
                        ui.add(
                            egui::DragValue::new(&mut point.gpx)
                                .speed(0.1)
                                .fixed_decimals(3)
                                .suffix(" s"),
                        );
                        ui.horizontal(|ui| {
                            if ui.small_button("Seek").clicked() {
                                seek = Some(point.video);
                            }
                            if ui.small_button("Remove").clicked() {
                                remove = Some(index);
                            }
                        });
                        ui.end_row();
                    }
                });
        });
        if let Some(index) = remove {
            points.remove(index);
        }
        if points != before {
            self.sync_draft = Some(points);
            self.dirty = true;
        }
        if let (Some(seconds), Some(info)) = (seek, &self.settings.info) {
            self.seek_video(seconds, (info.duration - 1.0 / info.fps).max(0.0));
        }
        if self.settings.sync_points.is_empty() && self.sync_draft.is_none() {
            ui.small("No sync points applied. Manual offset and GPX cuts remain available.");
        }
        let mut apply = None;
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    self.sync_draft.is_some() && self.cut_draft.is_none(),
                    egui::Button::new("Apply sync points"),
                )
                .clicked()
            {
                apply = self.sync_draft.clone();
            }
            if ui
                .add_enabled(
                    self.sync_draft.is_some(),
                    egui::Button::new("Cancel sync changes"),
                )
                .clicked()
            {
                self.sync_draft = None;
                self.dirty = true;
                self.pick_kind = None;
                self.pick_candidates.clear();
            }
            if ui
                .add_enabled(
                    !self.settings.sync_points.is_empty() && self.cut_draft.is_none(),
                    egui::Button::new("Remove all sync points"),
                )
                .clicked()
            {
                self.sync_draft = Some(Vec::new());
                self.dirty = true;
            }
        });
        if self.sync_draft.is_some() {
            ui.small("Unapplied sync changes: the sliders, red marker and exports still use the applied mapping.");
        }
        apply
    }

    pub fn restore(&mut self, settings: Settings, track: Option<&Track>) {
        let video_changed = self.settings.path != settings.path;
        self.dirty = settings.path.is_some() && (self.dirty || self.settings != settings);
        self.settings = settings;
        self.cut_draft = None;
        self.sync_draft = None;
        self.pick_kind = None;
        self.pick_candidates.clear();
        self.refresh_track(track);
        self.pause();
        if video_changed {
            self.position = 0.0;
            self.texture = None;
            self.decoded_at = None;
            self.preview_worker = None;
        }
        self.error = None;
        match self.plan_for_points(&self.settings.sync_points) {
            Ok(mapping) => self.sync_map = mapping,
            Err(error) => {
                self.sync_map = None;
                self.error = Some(error);
            }
        }
    }

    pub fn adjust_after_cut(&mut self, cut: Clip) {
        for time in [&mut self.settings.offset, &mut self.settings.anchor] {
            if *time > cut.start {
                *time -= (*time - cut.start).min(cut.duration());
            }
        }
    }

    pub fn busy(&self) -> bool {
        self.worker.is_some()
    }

    pub fn pause(&mut self) {
        self.playing = false;
        self.playback = None;
        self.playback_clock = None;
    }

    fn toggle_play(&mut self) {
        if self.pick_kind.is_some() {
            return;
        }
        if self.playing {
            self.pause();
        } else if let Some(info) = &self.settings.info {
            if !self.playing && self.position >= info.duration - 1.0 / info.fps {
                self.position = 0.0;
            }
            self.playing = true;
        }
    }

    pub fn open_video(&mut self, ctx: &egui::Context, path: Option<PathBuf>) {
        self.status = "Reading prepared video metadata...".into();
        self.spawn(ctx, move || {
            let path = path.or_else(|| {
                rfd::FileDialog::new()
                    .add_filter("Video", &["mp4", "mov", "mkv", "avi", "webm"])
                    .pick_file()
            });
            let Some(path) = path else {
                return Ok(Job::Cancelled);
            };
            let info = video::probe(&path)?;
            Ok(Job::Loaded(path, info))
        });
    }

    pub fn refresh_track(&mut self, track: Option<&Track>) {
        self.timing = track.map_or_else(
            || Err("Open a timestamped GPX track first.".into()),
            timeline::timing,
        );
    }

    pub fn set_track(&mut self, track: &Track) {
        self.refresh_track(Some(track));
        self.settings.offset = 0.0;
        self.settings.anchor = 0.0;
        self.settings.cuts.clear();
        self.settings.source_duration = self.timing.as_ref().ok().map(|timing| timing.duration);
        self.settings.source_timing = self
            .timing
            .as_ref()
            .ok()
            .map(|timing| Arc::new(timing.clone()));
        self.settings.sync_points.clear();
        self.sync_map = None;
        self.sync_draft = None;
        self.pick_kind = None;
        self.pick_candidates.clear();
        self.cut_draft = None;
        self.settings.marked = Clip {
            start: 0.0,
            end: self.timing.as_ref().map_or(0.0, |timing| timing.duration),
        };
        self.dirty = false;
    }

    pub fn track_edited(
        &mut self,
        track: &Track,
        cut_start: Option<f64>,
        selection: [f64; 2],
    ) -> Result<(), String> {
        if self.has_cut_draft() {
            return Err("Apply or cancel the cut table changes before adding another cut.".into());
        }
        let timing = timeline::timing(track);
        if let (Ok(before), Ok(after), Some(start)) = (&self.timing, &timing, cut_start) {
            let cut = Clip {
                start,
                end: start + before.duration - after.duration,
            };
            let duration = self
                .settings
                .source_duration
                .ok_or("The original GPX duration is unavailable.")?;
            let cuts = if let Some(map) = &self.sync_map {
                map.cuts().to_vec()
            } else {
                Self::time_cuts(self.cuts())?
            };
            let original = Clip {
                start: timeline::original_time(cut.start, duration, &cuts, false)?,
                end: timeline::original_time(cut.end, duration, &cuts, true)?,
            };
            let mut revised = self.settings.cuts.clone();
            revised.push(CutRange::Time(original));
            let mapping = self.plan_for_cuts(&revised)?;
            self.settings.cuts = revised;
            self.sync_map = mapping;
            self.adjust_after_cut(cut);
        } else {
            self.settings.cuts.push(CutRange::Distance(selection));
        }
        self.timing = timing;
        self.settings.marked = Clip {
            start: 0.0,
            end: self.timing.as_ref().map_or(0.0, |timing| timing.duration),
        };
        self.pause();
        self.dirty = true;
        Ok(())
    }

    pub fn has_cut_draft(&self) -> bool {
        self.cut_draft.is_some() || self.sync_draft.is_some()
    }

    pub fn cuts(&self) -> &[CutRange] {
        &self.settings.cuts
    }

    pub fn cuts_with_interval(&self, cut: Clip) -> Result<Vec<CutRange>, String> {
        let timing = self.timing.as_ref().map_err(Clone::clone)?;
        timeline::kept_intervals(timing.duration, &[cut])?;
        let duration = self
            .settings
            .source_duration
            .ok_or("The original GPX duration is unavailable.")?;
        let effective = match &self.sync_map {
            Some(mapping) => mapping.cuts().to_vec(),
            None => Self::time_cuts(self.cuts())?,
        };
        let mut cuts = self.settings.cuts.clone();
        cuts.push(CutRange::Time(Clip {
            start: timeline::original_time(cut.start, duration, &effective, false)?,
            end: timeline::original_time(cut.end, duration, &effective, true)?,
        }));
        self.plan_for_cuts(&cuts)?;
        Ok(cuts)
    }

    fn time_cuts(cuts: &[CutRange]) -> Result<Vec<Clip>, String> {
        cuts.iter()
            .map(|cut| match cut {
                CutRange::Time(clip) => Ok(*clip),
                CutRange::Distance(_) => {
                    Err("The cut list uses untimed distance ranges, not GPX times.".into())
                }
            })
            .collect()
    }

    pub fn commit_cuts(&mut self, track: &Track, cuts: Vec<CutRange>) -> Result<(), String> {
        let mapping = self.plan_for_cuts(&cuts)?;
        let mut offset = self.settings.offset;
        let mut anchor = self.settings.anchor;
        if let Some(duration) = self.settings.source_duration {
            let old = Self::time_cuts(&self.settings.cuts)?;
            let new = Self::time_cuts(&cuts)?;
            offset = timeline::edited_time(
                timeline::original_time(offset, duration, &old, false)?,
                duration,
                &new,
            )?;
            anchor = timeline::edited_time(
                timeline::original_time(anchor, duration, &old, false)?,
                duration,
                &new,
            )?;
        }
        self.settings.offset = offset;
        self.settings.anchor = anchor;
        self.settings.cuts = cuts;
        self.sync_map = mapping;
        self.cut_draft = None;
        self.refresh_track(Some(track));
        self.settings.marked = Clip {
            start: 0.0,
            end: self.timing.as_ref().map_or(0.0, |timing| timing.duration),
        };
        self.pause();
        self.dirty = true;
        Ok(())
    }

    fn mark_from_video(&mut self, start: bool) {
        if start {
            self.settings.marked.start = self.position;
        } else {
            self.settings.marked.end = self.position;
        }
    }

    fn video_position_control(&mut self, ui: &mut egui::Ui, end: f64) -> egui::Response {
        ui.add_enabled_ui(self.pick_kind.is_none(), |ui| {
            let value = ui
                .horizontal(|ui| {
                    ui.label("Prepared video seconds:");
                    ui.add_sized(
                        vec2(110.0, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut self.position)
                            .range(0.0..=end)
                            .speed(0.1)
                            .fixed_decimals(3),
                    )
                })
                .inner;
            let slider = ui
                .scope(|ui| {
                    ui.spacing_mut().slider_width = ui.available_width();
                    ui.add(egui::Slider::new(&mut self.position, 0.0..=end).show_value(false))
                })
                .inner;
            if value.changed() || slider.changed() {
                self.pause();
                self.cancel_picking();
            }
            slider
        })
        .inner
    }

    fn linked_video_range(&self, video_end: f64) -> Option<[f64; 2]> {
        let duration = self.timing.as_ref().ok()?.duration;
        if let Some(map) = &self.sync_map {
            let start = map.video_at(0.0).max(0.0);
            let end = map.video_at(duration).min(video_end);
            return (start <= end).then_some([start, end]);
        }
        let offset = self.settings.offset;
        if !offset.is_finite() || !video_end.is_finite() || video_end < 0.0 {
            return None;
        }
        let start = 0.0_f64.max(-offset);
        let end = video_end.min(duration - offset);
        (start <= end).then_some([start, end])
    }

    fn seek_gpx(&mut self, seconds: f64, video_end: f64) -> Result<(), String> {
        if !seconds.is_finite() {
            return Err("Choose a finite GPX timeline position.".into());
        }
        if self.pick_kind.is_some() {
            let duration = self
                .settings
                .source_duration
                .ok_or("The original GPX timeline is unavailable.")?;
            self.pick_time = seconds.clamp(0.0, duration);
            self.pick_candidates.clear();
            return Ok(());
        }
        let [start, end] = self
            .linked_video_range(video_end)
            .ok_or("The GPX and video timelines do not overlap. Adjust the alignment first.")?;
        self.position = self
            .sync_map
            .as_ref()
            .map_or(seconds - self.settings.offset, |map| map.video_at(seconds))
            .clamp(start, end);
        self.pause();
        self.cancel_picking();
        Ok(())
    }

    fn seek_profile(
        &mut self,
        distance: f64,
        tracks: &Tracks<'_>,
        video_end: f64,
    ) -> Result<(), String> {
        let (track, timing) = if self.pick_kind.is_some() {
            (
                tracks.source,
                self.settings
                    .source_timing
                    .as_deref()
                    .ok_or("The original GPX timeline is unavailable.")?,
            )
        } else {
            (tracks.current, self.timing.as_ref().map_err(Clone::clone)?)
        };
        let track = track.ok_or("Open a GPX track before seeking its elevation profile.")?;
        let seconds = timeline::time_at_distance(timing, track, distance, self.gpx_time())
            .ok_or("Choose a recorded GPX location in the elevation profile.")?;
        self.seek_gpx(seconds, video_end)
    }

    fn seek_video(&mut self, seconds: f64, end: f64) {
        if self.pick_kind.is_some() {
            return;
        }
        self.position = seconds.clamp(0.0, end);
        self.pause();
        self.cancel_picking();
    }

    fn gpx_position_control(
        &mut self,
        ui: &mut egui::Ui,
        video_end: f64,
    ) -> Option<egui::Response> {
        let picking = self.pick_kind.is_some();
        let duration = if picking {
            self.settings.source_timing.as_ref()?.duration
        } else {
            match &self.timing {
                Ok(timing) => timing.duration,
                Err(error) => {
                    ui.label(format!("GPX timeline unavailable: {error}"));
                    return None;
                }
            }
        };
        let elapsed = self.gpx_time();
        let mut seconds = elapsed.clamp(0.0, duration);
        let range = if picking {
            Some([0.0, duration])
        } else {
            self.linked_video_range(video_end)
        };
        let response = ui.add_enabled_ui(!self.busy() && range.is_some(), |ui| {
            let value = ui
                .horizontal(|ui| {
                    ui.label(if picking {
                        "Original GPX seconds:"
                    } else {
                        "GPX seconds:"
                    });
                    ui.add_sized(
                        vec2(110.0, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut seconds)
                            .range(0.0..=duration)
                            .speed(0.1)
                            .fixed_decimals(3),
                    )
                })
                .inner;
            let slider = ui
                .scope(|ui| {
                    ui.spacing_mut().slider_width = ui.available_width();
                    ui.add(egui::Slider::new(&mut seconds, 0.0..=duration).show_value(false))
                })
                .inner;
            if value.changed() || slider.changed() {
                if let Err(error) = self.seek_gpx(seconds, video_end) {
                    self.error = Some(error);
                }
            }
            ui.horizontal(|ui| {
                for (label, step) in [("-10 s", -10.0), ("+10 s", 10.0)] {
                    if ui.small_button(label).clicked() {
                        if let Err(error) = self.seek_gpx(self.gpx_time() + step, video_end) {
                            self.error = Some(error);
                        }
                    }
                }
            });
            slider
        });
        let coverage = if picking {
            "Video locked. Move the original GPX timeline or click the route.".into()
        } else if let Some([start, end]) = range {
            format!(
                "Linked GPX seek range: {:.3}-{:.3} s.",
                self.gpx_at(start),
                self.gpx_at(end)
            )
        } else {
            "No shared timeline coverage. Adjust the GPX/video alignment.".into()
        };
        ui.add(egui::Label::new(egui::RichText::new(coverage).small()).truncate());
        let outside = elapsed < 0.0 || elapsed > duration;
        let hint = if picking {
            "Press the active pick button again to apply the sync point."
        } else if outside {
            "Video is outside GPX coverage; the handle shows the nearest boundary."
        } else {
            "Seeking updates both the video and GPX position."
        };
        let mut hint = egui::RichText::new(hint).small();
        if !picking && outside {
            hint = hint.color(Color32::from_rgb(180, 90, 0));
        }
        ui.add(egui::Label::new(hint).truncate());
        Some(response.inner)
    }

    pub fn show_cuts(&mut self, ui: &mut egui::Ui) -> Option<Vec<CutRange>> {
        ui.heading("GPX cuts");
        if self.settings.cuts.is_empty() {
            ui.label("No GPX cuts yet.");
            return None;
        }
        ui.small(
            "Time bounds use the ORIGINAL GPX clock, in whole seconds. Overlapping time cuts are combined. Apply changes to rebuild the track.",
        );
        let mut cuts = self
            .cut_draft
            .clone()
            .unwrap_or_else(|| self.settings.cuts.clone());
        let mut changed = false;
        ui.add_enabled_ui(self.sync_draft.is_none(), |ui| {
            egui::Grid::new("gpx_cuts").striped(true).show(ui, |ui| {
                for heading in ["#", "Range basis", "In", "Out", "Span"] {
                    ui.strong(heading);
                }
                ui.end_row();
                for (index, cut) in cuts.iter_mut().enumerate() {
                    let (basis, mut start, mut end, unit, decimals, speed) = match cut {
                        CutRange::Time(clip) => {
                            ("Original GPX time", clip.start, clip.end, "s", 0, 1.0)
                        }
                        CutRange::Distance(range) => (
                            "Sequential GPX distance",
                            range[0] / 1000.0,
                            range[1] / 1000.0,
                            "km",
                            3,
                            0.01,
                        ),
                    };
                    ui.label((index + 1).to_string());
                    ui.label(basis);
                    let start_changed = ui
                        .add(
                            egui::DragValue::new(&mut start)
                                .speed(speed)
                                .fixed_decimals(decimals)
                                .suffix(format!(" {unit}")),
                        )
                        .changed();
                    let end_changed = ui
                        .add(
                            egui::DragValue::new(&mut end)
                                .speed(speed)
                                .fixed_decimals(decimals)
                                .suffix(format!(" {unit}")),
                        )
                        .changed();
                    if start_changed || end_changed {
                        match cut {
                            CutRange::Time(clip) => {
                                if start_changed {
                                    clip.start = start.round();
                                }
                                if end_changed {
                                    clip.end = end.round();
                                }
                            }
                            CutRange::Distance(range) => *range = [start * 1000.0, end * 1000.0],
                        }
                        changed = true;
                    }
                    ui.label(format!("{:.decimals$} {unit}", end - start));
                    ui.end_row();
                }
            });
        });
        if changed {
            self.cut_draft = Some(cuts);
            self.dirty = true;
        }
        let mut request = None;
        if self.cut_draft.is_some() {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.busy() && self.sync_draft.is_none(),
                        egui::Button::new("Apply cut changes"),
                    )
                    .clicked()
                {
                    request = self.cut_draft.clone();
                }
                if ui.button("Cancel cut changes").clicked() {
                    self.cut_draft = None;
                    self.dirty = true;
                }
            });
        }
        request
    }

    fn spawn(
        &mut self,
        ctx: &egui::Context,
        work: impl FnOnce() -> Result<Job, String> + Send + 'static,
    ) {
        let (sender, receiver) = mpsc::channel();
        self.worker = Some(receiver);
        self.error = None;
        self.pause();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = sender.send(work());
            ctx.request_repaint();
        });
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        let result = match self.worker.as_ref().map(Receiver::try_recv) {
            Some(Ok(result)) => Some(result),
            Some(Err(TryRecvError::Disconnected)) => {
                Some(Err("The video task stopped unexpectedly.".into()))
            }
            _ => None,
        };
        if let Some(result) = result {
            self.worker = None;
            self.exporting = false;
            match result {
                Ok(Job::Loaded(path, info)) => {
                    let mapping = if self.settings.sync_points.is_empty() {
                        Ok(None)
                    } else {
                        self.settings
                            .source_duration
                            .ok_or_else(|| "The original GPX duration is unavailable.".to_owned())
                            .and_then(|duration| {
                                SyncMap::new(
                                    &self.settings.sync_points,
                                    &Self::time_cuts(&self.settings.cuts)?,
                                    duration,
                                    info.duration,
                                )
                                .map(Some)
                            })
                    };
                    let mapping = match mapping {
                        Ok(mapping) => mapping,
                        Err(error) => {
                            self.status = "Video replacement rejected; remove or revise sync points first. The current video and GPX are unchanged.".into();
                            self.error = Some(error);
                            return;
                        }
                    };
                    self.settings.path = Some(path);
                    self.settings.info = Some(info);
                    self.settings.offset = 0.0;
                    self.sync_map = mapping;
                    self.sync_draft = None;
                    self.pick_kind = None;
                    self.pick_candidates.clear();
                    self.position = 0.0;
                    self.texture = None;
                    self.decoded_at = None;
                    self.preview_worker = None;
                    self.dirty = true;
                    self.status = "Prepared video ready. Applied sync points are retained; verify them against the new video. Video always stays unchanged.".into();
                }
                Ok(Job::Exported(result)) => {
                    self.dirty = false;
                    self.status = format!(
                        "Exported unchanged video ({:.3} s, {} frames), {} GPX points and ride.xml to {}.",
                        result.duration, result.frames, result.points, result.directory.display()
                    );
                }
                Ok(Job::Cancelled) => self.status = "File dialog cancelled.".into(),
                Err(error) => {
                    self.status = "Operation failed; source files and edits are unchanged.".into();
                    self.error = Some(error);
                }
            }
        }
        let preview = match self.preview_worker.as_ref().map(Receiver::try_recv) {
            Some(Ok(result)) => Some(result),
            Some(Err(TryRecvError::Disconnected)) => Some((
                self.position,
                Err("The video preview task stopped unexpectedly.".into()),
            )),
            _ => None,
        };
        if let Some((at, result)) = preview {
            self.preview_worker = None;
            let current = (at - self.position).abs() < 0.0005;
            // Show intermediate frames while seeking, without replacing an exact
            // playhead frame or reporting errors from an obsolete seek.
            if self.playing
                || (!current
                    && (self.pick_kind.is_some() || self.preview_ready() || result.is_err()))
            {
                return;
            }
            self.decoded_at = Some(at);
            match result {
                Ok(frame) => self.update_texture(ctx, &frame),
                Err(error) => {
                    self.pause();
                    self.texture = None;
                    self.error = Some(error);
                }
            }
        }
    }

    fn decode_preview(&mut self, ctx: &egui::Context) {
        if self.pick_kind.is_some()
            || self.playing
            || self.busy()
            || self.preview_worker.is_some()
            || self
                .decoded_at
                .is_some_and(|at| (at - self.position).abs() < 0.0005)
        {
            return;
        }
        let Some(path) = self.settings.path.clone() else {
            return;
        };
        let at = self.position;
        let ctx = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        self.preview_worker = Some(receiver);
        std::thread::spawn(move || {
            let _ = sender.send((at, video::preview(&path, at)));
            ctx.request_repaint();
        });
    }

    fn update_texture(&mut self, ctx: &egui::Context, frame: &video::PreviewFrame) {
        let image = egui::ColorImage::from_rgb([frame.width, frame.height], &frame.rgb);
        if let Some(texture) = &mut self.texture {
            texture.set(image, TextureOptions::LINEAR);
        } else {
            self.texture = Some(ctx.load_texture("video-preview", image, TextureOptions::LINEAR));
        }
    }

    fn advance_playback(&mut self, ctx: &egui::Context) {
        if !self.playing {
            return;
        }
        if self.playback.is_none() {
            let Some((path, info)) = self.settings.path.as_ref().zip(self.settings.info.as_ref())
            else {
                self.pause();
                self.error = Some("Open a prepared video before playing.".into());
                return;
            };
            match video::Playback::start(path, info, self.position) {
                Ok(playback) => self.playback = Some(playback),
                Err(error) => {
                    self.pause();
                    self.error = Some(error);
                    return;
                }
            }
        }
        let target = self
            .playback_clock
            .map_or(self.position, |(started, origin)| {
                origin + started.elapsed().as_secs_f64()
            });
        let playback = self.playback.as_mut().unwrap();
        let frame = playback.frame_at(target);
        let finished = playback.finished();
        match frame {
            Ok(Some(frame)) => {
                self.playback_clock
                    .get_or_insert((Instant::now(), frame.seconds));
                self.position = frame.seconds;
                self.decoded_at = Some(frame.seconds);
                self.update_texture(ctx, &frame.image);
            }
            Ok(None) => {}
            Err(error) => {
                self.pause();
                self.error = Some(error);
                return;
            }
        }
        if finished {
            self.pause();
        } else {
            ctx.request_repaint_after(Duration::from_millis(1));
        }
    }

    fn export(&mut self, ctx: &egui::Context, track: &Track, name: &str, description: &str) {
        if self.settings.folder.is_empty()
            || self.settings.folder == "."
            || self.settings.folder == ".."
            || self
                .settings
                .folder
                .chars()
                .any(|ch| ch == '/' || ch == '\\' || ch.is_control())
        {
            self.error = Some("Choose a single folder name, without path separators.".into());
            return;
        }
        let Some(path) = self.settings.path.clone() else {
            return;
        };
        let track = track.clone();
        let settings = self.settings.clone();
        let mapping = self.sync_map.clone();
        let name = name.to_owned();
        let description = description.to_owned();
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        self.status =
            "Copying the prepared video unchanged and exporting GPX / Incyclist XML...".into();
        self.spawn(ctx, move || {
            let Some(parent) = rfd::FileDialog::new()
                .set_title("Choose the parent folder for a NEW Incyclist bundle")
                .pick_folder()
            else {
                return Ok(Job::Cancelled);
            };
            let result = bundle::export(
                bundle::Export {
                    track: &track,
                    video: &path,
                    offset: settings.offset,
                    name: &name,
                    description: &description,
                    country: &settings.country,
                    sync: mapping.as_ref(),
                },
                &parent.join(settings.folder),
                &cancel,
            )?;
            Ok(Job::Exported(result))
        });
        self.exporting = true;
    }

    /// Returns a GPX-clock interval to cut; never edits the video timeline.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        tracks: Tracks<'_>,
        name: &str,
        description: &str,
        map: &mut crate::map::TrackMap,
    ) -> Option<Edit> {
        let before = self.settings.clone();
        let track = tracks.current.map(Arc::as_ref);
        let source = tracks.source.map(Arc::as_ref);
        let previous_position = self.position;
        let previous_pick_time = self.pick_time;
        let mut cut = None;
        let ctx = ui.ctx().clone();
        if !self.busy()
            && self.pick_kind.is_none()
            && self.settings.info.is_some()
            && !ctx.egui_wants_keyboard_input()
            && ctx.input(|input| input.key_pressed(egui::Key::Space))
        {
            self.toggle_play();
        }
        self.advance_playback(&ctx);
        ui.heading("Prepared video & GPX editing");
        ui.label("Video is read-only. Match frames to locations on the original GPX route; only the GPX is edited.");
        ui.add_enabled_ui(!self.busy() && self.pick_kind.is_none(), |ui| {
            if ui.button("Open prepared video...").clicked() {
                self.open_video(&ctx, None);
            }
            if let Some(path) = &self.settings.path {
                ui.label(path.display().to_string());
            }
        });
        if let Some(info) = self.settings.info.clone() {
            if let Some(points) = self.sync_tools(ui, source) {
                cut = Some(Edit::Sync(points));
            }
            let position = if self.pick_kind.is_some() {
                tracks
                    .source
                    .map(Arc::as_ref)
                    .zip(self.settings.source_timing.as_ref())
                    .and_then(|(source, timing)| {
                        timeline::position_at(timing, source, self.pick_time)
                    })
            } else if let (Some(mapping), Some(source), Some(timing)) =
                (&self.sync_map, source, &self.settings.source_timing)
            {
                mapping
                    .original_at(self.position)
                    .ok()
                    .and_then(|time| timeline::position_at(timing, source, time))
            } else {
                self.route_position(track)
            };
            let distance = if self.pick_kind.is_some() {
                tracks
                    .source
                    .map(Arc::as_ref)
                    .zip(self.settings.source_timing.as_ref())
                    .and_then(|(source, timing)| {
                        timeline::distance_at(timing, source, self.pick_time)
                    })
            } else {
                match (&self.timing, track) {
                    (Ok(timing), Some(track)) => {
                        timeline::distance_at(timing, track, self.gpx_time())
                    }
                    _ => None,
                }
            };
            let width = ((ui.available_width() - ui.spacing().item_spacing.x) / 2.0).max(1.0);
            ui.horizontal_top(|ui| {
                let size = vec2(width, width * 9.0 / 16.0);
                ui.allocate_ui_with_layout(
                    vec2(width, width * 9.0 / 16.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        let header_height = (ui.text_style_height(&egui::TextStyle::Button)
                            + 2.0 * ui.spacing().button_padding.y)
                            .max(ui.spacing().interact_size.y);
                        ui.allocate_ui_with_layout(
                            vec2(width, header_height),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.strong("Prepared video");
                            },
                        );
                        if let Some(texture) = &self.texture {
                            ui.add(
                                egui::Image::new(texture)
                                    .fit_to_exact_size(vec2(width, width * 9.0 / 16.0)),
                            );
                        } else {
                            let (rect, _) = ui.allocate_exact_size(
                                vec2(width, width * 9.0 / 16.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
                        }
                    },
                );
                ui.allocate_ui_with_layout(
                    vec2(width, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(width);
                        let selection =
                            track.map_or([0.0, 0.0], |track| [0.0, track.total_distance()]);
                        let mut overlay = self.sync_overlay(source);
                        overlay.viewport = Some(size);
                        if let Some(picks) =
                            map.show_at_position(ui, track, selection, position, overlay)
                        {
                            self.receive_picks(picks);
                        }
                        self.speed_control(ui, source);
                        self.gpx_position_control(ui, (info.duration - 1.0 / info.fps).max(0.0));
                    },
                );
            });
            ui.label(format!(
                "{} x {} | {:.3} fps | {:.3} s | playback is silent",
                info.width, info.height, info.fps, info.duration
            ));
            ui.add_enabled_ui(!self.busy() && self.pick_kind.is_none(), |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .button(if self.playing { "Pause" } else { "Play" })
                        .clicked()
                    {
                        self.toggle_play();
                    }
                    for (label, amount) in [
                        ("-10 s", -10.0),
                        ("-1 s", -1.0),
                        ("Previous frame", -1.0 / info.fps),
                        ("Next frame", 1.0 / info.fps),
                        ("+1 s", 1.0),
                        ("+10 s", 10.0),
                    ] {
                        if ui.button(label).clicked() {
                            self.seek_video(
                                self.position + amount,
                                (info.duration - 1.0 / info.fps).max(0.0),
                            );
                        }
                    }
                });
                self.video_position_control(ui, (info.duration - 1.0 / info.fps).max(0.0));
            });
            ui.add_enabled_ui(!self.busy(), |ui| {
                let _ = self.profile.show(
                    ui,
                    if self.pick_kind.is_some() {
                        tracks.source
                    } else {
                        tracks.current
                    },
                    distance,
                    map.ground_span(width, position.map_or(0.0, |position| position[1])),
                );
            });
            if let Some(distance) = self.profile.take_seek() {
                if let Err(error) =
                    self.seek_profile(distance, &tracks, (info.duration - 1.0 / info.fps).max(0.0))
                {
                    self.error = Some(error);
                }
            }
            ui.add_enabled_ui(!self.busy() && self.pick_kind.is_none(), |ui| {
                if let Some(points) = self.show_sync_points(ui) { cut = Some(Edit::Sync(points)); }
                let location = if let Some(distance) = distance {
                    format!("{} GPX elapsed: {:.3} s | {:.3} km",
                        if self.pick_kind.is_some() { "Original" } else { "Current" },
                        self.gpx_time(), distance / 1000.0)
                } else { "Video time is outside GPX coverage.".into() };
                ui.add(egui::Label::new(location).truncate());
                match self.timing.clone() {
                    Ok(timing) => {
                        ui.label(format!("Current GPX duration: {:.3} s | video: {:.3} s",
                            timing.duration, info.duration));
                        ui.add_enabled_ui(self.sync_map.is_none(), |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label("GPX elapsed at video start:");
                            ui.add(egui::DragValue::new(&mut self.settings.offset).speed(0.1).suffix(" s"));
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label("This video frame matches GPX elapsed:");
                            ui.add(egui::DragValue::new(&mut self.settings.anchor).speed(0.1)
                                .range(0.0..=timing.duration).suffix(" s"));
                            if ui.button("Set alignment").clicked() {
                                self.settings.offset = self.settings.anchor - self.position;
                            }
                        });
                        });
                        if self.sync_map.is_some() { ui.small("Sync points control alignment. Remove all sync points and apply to restore manual offset controls."); }
                        ui.separator();
                        ui.heading("Remove a section from the smoothed GPX");
                        ui.label("Times refer to the CURRENT GPX, not the prepared video. Later GPX times close up after each cut.");
                        ui.horizontal_wrapped(|ui| {
                            ui.label("GPX in:");
                            if ui.add_enabled(self.position <= timing.duration,
                                egui::Button::new("Use video time").small())
                                .on_hover_text("Copy prepared video seconds into GPX in, without applying the alignment offset. Available within the current GPX duration.")
                                .clicked() {
                                self.mark_from_video(true);
                            }
                            ui.add(egui::DragValue::new(&mut self.settings.marked.start).speed(0.1)
                                .range(0.0..=timing.duration).suffix(" s").fixed_decimals(3));
                            ui.label("GPX out:");
                            if ui.add_enabled(self.position <= timing.duration,
                                egui::Button::new("Use video time").small())
                                .on_hover_text("Copy prepared video seconds into GPX out, without applying the alignment offset. Available within the current GPX duration.")
                                .clicked() {
                                self.mark_from_video(false);
                            }
                            ui.add(egui::DragValue::new(&mut self.settings.marked.end).speed(0.1)
                                .range(0.0..=timing.duration).suffix(" s").fixed_decimals(3));
                            if ui.add_enabled(!self.has_cut_draft(),
                                egui::Button::new("Cut GPX section & level elevation")).clicked() {
                                cut = Some(Edit::Cut(self.settings.marked));
                            }
                        });
                        ui.label("The following section is shifted vertically to meet the preceding section. Its slopes are preserved.");
                        ui.small("For visual selection on the route and elevation chart, use GPX smoothing > Cut selected GPX section.");
                        if (timing.duration - self.gpx_at(info.duration)).abs() > 1.0 / info.fps {
                            ui.colored_label(Color32::from_rgb(180, 90, 0),
                                "GPX/video durations differ. Check cuts, alignment and landmarks near both ends before export.");
                        }
                    }
                    Err(error) => { ui.colored_label(Color32::from_rgb(180, 35, 35), error); }
                }
                if let Some(cuts) = self.show_cuts(ui) { cut = Some(Edit::ReplaceCuts(cuts)); }
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.label("Country (optional):");
                    ui.text_edit_singleline(&mut self.settings.country);
                    ui.label("New bundle folder:");
                    ui.text_edit_singleline(&mut self.settings.folder);
                });
                if ui.add_enabled(track.is_some() && self.timing.is_ok(),
                    egui::Button::new("Export Incyclist bundle...")).clicked() {
                    let export_track = if self.sync_map.is_some() { source } else { track };
                    if let Some(track) = export_track { self.export(&ctx, track, name, description); }
                }
                ui.label("Export copies the entire video byte-for-byte (including audio), plus matching GPX and ride.xml. No video cuts, frame-rate conversion or re-encoding.");
            });
            self.decode_preview(&ctx);
        }
        if self.settings.info.is_none() {
            if let Some(cuts) = self.show_cuts(ui) {
                cut = Some(Edit::ReplaceCuts(cuts));
            }
        }
        if self.settings != before {
            self.dirty = true;
        }
        if self.position != previous_position
            || self.settings.offset != before.offset
            || self.pick_time != previous_pick_time
        {
            ctx.request_repaint();
        }
        if self.busy() {
            ui.horizontal(|ui| {
                ui.spinner();
                if self.exporting && ui.button("Cancel export").clicked() {
                    self.cancel.store(true, Ordering::Relaxed);
                }
            });
        }
        ui.label(&self.status);
        if let Some(error) = &self.error {
            ui.colored_label(Color32::from_rgb(180, 35, 35), error);
        }
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_preview(studio: &mut Studio, ctx: &egui::Context) {
        studio.texture = Some(ctx.load_texture(
            "test-preview",
            egui::ColorImage::from_rgb([160, 90], &vec![0; 160 * 90 * 3]),
            TextureOptions::LINEAR,
        ));
        studio.decoded_at = Some(studio.position);
    }

    #[test]
    fn profile_seeking_uses_video_sync_and_cuts_but_keeps_landmark_video_frozen() {
        let source = Arc::new(timeline_track());
        let mut studio = prepared_studio(&source, 42.0);
        let points = vec![
            SyncPoint {
                video: 0.0,
                gpx: 0.0,
                kind: Kind::Normal,
            },
            SyncPoint {
                video: 10.0,
                gpx: 10.0,
                kind: Kind::BeforeCut,
            },
            SyncPoint {
                video: 12.0,
                gpx: 30.0,
                kind: Kind::AfterCut,
            },
            SyncPoint {
                video: 42.0,
                gpx: 60.0,
                kind: Kind::Normal,
            },
        ];
        let mapping = studio.plan_for_points(&points).unwrap().unwrap();
        let track = Arc::new(timeline::cut_ranges(&source, mapping.cuts()).unwrap());
        studio.commit_sync(&track, points).unwrap();
        let tracks = Tracks {
            current: Some(&track),
            source: Some(&source),
        };
        let distance =
            timeline::distance_at(studio.timing.as_ref().unwrap(), &track, 20.0).unwrap();
        studio.playing = true;
        studio.seek_profile(distance, &tracks, 41.96).unwrap();
        assert!(!studio.playing);
        assert!((studio.position - 22.0).abs() < 1e-9);
        assert!(
            (studio
                .mapping()
                .unwrap()
                .original_at(studio.position)
                .unwrap()
                - 40.0)
                .abs()
                < 1e-9
        );
        studio.begin_picking(Kind::Normal).unwrap();
        let distance = timeline::distance_at(
            studio.settings.source_timing.as_ref().unwrap(),
            &source,
            25.0,
        )
        .unwrap();
        studio.seek_profile(distance, &tracks, 41.96).unwrap();
        assert_eq!(studio.position, 22.0);
        assert!((studio.pick_time - 25.0).abs() < 1e-9);
        assert!(studio.seek_profile(f64::NAN, &tracks, 41.96).is_err());
    }

    #[test]
    fn speed_follows_alignment_and_drift_and_never_counts_cut_jumps() {
        let source = timeline_track();
        let baseline = source.points[1].step / 30.0 * 3.6;
        let mut studio = prepared_studio(&source, 60.0);
        assert!(studio.speed_kmh(Some(&source)).is_none());
        studio.playing = true;
        studio.position = 5.0;
        studio.settings.offset = 10.0;
        assert!((studio.speed_kmh(Some(&source)).unwrap() - baseline).abs() < 1e-9);
        studio.position = 55.0;
        assert!(studio.speed_kmh(Some(&source)).is_none());
        studio.settings.offset = 0.0;
        studio.position = 5.0;
        studio
            .commit_sync(
                &source,
                vec![
                    SyncPoint {
                        video: 0.0,
                        gpx: 0.0,
                        kind: Kind::Normal,
                    },
                    SyncPoint {
                        video: 30.0,
                        gpx: 60.0,
                        kind: Kind::Normal,
                    },
                ],
            )
            .unwrap();
        studio.playing = true;
        assert!((studio.speed_kmh(Some(&source)).unwrap() - baseline * 2.0).abs() < 1e-9);
        let points = vec![
            SyncPoint {
                video: 0.0,
                gpx: 0.0,
                kind: Kind::Normal,
            },
            SyncPoint {
                video: 10.0,
                gpx: 10.0,
                kind: Kind::BeforeCut,
            },
            SyncPoint {
                video: 12.0,
                gpx: 30.0,
                kind: Kind::AfterCut,
            },
            SyncPoint {
                video: 42.0,
                gpx: 60.0,
                kind: Kind::Normal,
            },
        ];
        let mapping = studio.plan_for_points(&points).unwrap().unwrap();
        let cut = timeline::cut_ranges(&source, mapping.cuts()).unwrap();
        studio.commit_sync(&cut, points).unwrap();
        studio.playing = true;
        for seconds in [10.0, 11.0, 11.999] {
            studio.position = seconds;
            assert_eq!(studio.speed_kmh(Some(&source)), Some(0.0));
        }
        for seconds in [9.999, 12.0, 12.001] {
            studio.position = seconds;
            assert!((studio.speed_kmh(Some(&source)).unwrap() - baseline).abs() < 1e-9);
        }
        studio.pick_kind = Some(Kind::Normal);
        assert!(studio.speed_kmh(Some(&source)).is_none());
        let mut studio = prepared_studio(&source, 40.0);
        let cuts = vec![CutRange::Time(Clip {
            start: 20.0,
            end: 40.0,
        })];
        let cut = timeline::rebuild_cuts(&source, &cuts).unwrap();
        studio.commit_cuts(&cut, cuts).unwrap();
        studio.playing = true;
        for seconds in [19.999, 20.0, 20.001] {
            studio.position = seconds;
            assert!((studio.speed_kmh(Some(&source)).unwrap() - baseline).abs() < 1e-9);
        }
        assert!(studio.speed_kmh(None).is_none());
    }

    #[test]
    fn speed_readout_reserves_the_same_row_when_playing_or_paused() {
        let source = timeline_track();
        let mut studio = prepared_studio(&source, 60.0);
        studio.position = 5.0;
        let ctx = egui::Context::default();
        let draw = |studio: &Studio| {
            let mut rect = egui::Rect::NOTHING;
            let output = crate::test_ui(&ctx, Default::default(), |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    rect = studio.speed_control(ui, Some(&source)).rect;
                    ui.button("Timeline below speed").clicked();
                });
            });
            (rect, output)
        };
        let (paused, output) = draw(&studio);
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.job.text == "Speed: -- km/h")));
        let below = button(&output, "Timeline below speed");
        studio.playing = true;
        let (playing, output) = draw(&studio);
        let expected = format!(
            "Speed: {:.1} km/h",
            studio.speed_kmh(Some(&source)).unwrap()
        );
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.job.text == expected)));
        assert_eq!(paused.min, playing.min);
        assert_eq!(paused.height(), playing.height());
        assert_eq!(button(&output, "Timeline below speed"), below);
    }

    #[test]
    fn scrubbing_keeps_the_player_map_profile_and_edit_controls_in_fixed_positions() {
        for width in [760.0, 1200.0] {
            let source = Arc::new(timeline_track());
            let mut studio = prepared_studio(&source, 120.0);
            let mut map = crate::map::TrackMap::offline();
            map.set_reference_track(source.clone());
            let ctx = egui::Context::default();
            let mut style = (*ctx.global_style()).clone();
            style.spacing.item_spacing = vec2(9.0, 9.0);
            style.spacing.button_padding = vec2(11.0, 6.0);
            style
                .text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(16.0));
            style
                .text_styles
                .insert(egui::TextStyle::Button, egui::FontId::proportional(16.0));
            ctx.set_global_style(style);
            ready_preview(&mut studio, &ctx);
            let mut draw = |studio: &mut Studio| {
                let output = crate::test_ui(
                    &ctx,
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            vec2(width, 2600.0),
                        )),
                        ..Default::default()
                    },
                    |root| {
                        egui::CentralPanel::default().show(root, |ui| {
                            egui::ScrollArea::vertical()
                                .scroll_bar_visibility(
                                    egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                                )
                                .show(ui, |ui| {
                                    studio.show(
                                        ui,
                                        Tracks {
                                            current: Some(&source),
                                            source: Some(&source),
                                        },
                                        "",
                                        "",
                                        &mut map,
                                    );
                                });
                        });
                    },
                );
                let mut positions: Vec<_> = [
                    "Pick Landmark",
                    "Play",
                    "Previous frame",
                    "Apply sync points",
                    "Cut GPX section & level elevation",
                    "Export Incyclist bundle...",
                ]
                .into_iter()
                .map(|label| button(&output, label))
                .collect();
                positions.extend(output.shapes.iter().filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect)
                        if rect.fill == Color32::from_rgb(235, 241, 244)
                            || (rect.fill == Color32::WHITE
                                && (rect.rect.height() - 177.0).abs() < 0.1) =>
                    {
                        Some(rect.rect.min)
                    }
                    _ => None,
                }));
                positions
            };
            draw(&mut studio);
            let expected = draw(&mut studio);
            let (_sender, receiver) = mpsc::channel();
            studio.preview_worker = Some(receiver);
            for seconds in [0.999, 9.999, 10.0, 59.999, 60.001, 99.999, 100.0, 119.0] {
                studio.seek_video(seconds, 119.96);
                assert_eq!(
                    draw(&mut studio),
                    expected,
                    "Video {seconds} s at width {width}"
                );
            }
            studio.position = 5.0;
            studio.begin_picking(Kind::Normal).unwrap();
            let expected = draw(&mut studio);
            for seconds in [0.0, 9.999, 10.0, 59.999, 60.0] {
                studio.pick_candidates = vec![5.0, 35.0];
                assert_eq!(draw(&mut studio), expected);
                studio.seek_gpx(seconds, 119.96).unwrap();
                assert_eq!(
                    draw(&mut studio),
                    expected,
                    "GPX {seconds} s at width {width}"
                );
            }
        }
    }

    #[test]
    fn stale_preview_results_cannot_replace_the_current_playhead_frame() {
        let mut studio = prepared_studio(&timeline_track(), 60.0);
        let ctx = egui::Context::default();
        ready_preview(&mut studio, &ctx);
        studio.seek_video(10.0, 59.96);
        ready_preview(&mut studio, &ctx);
        let texture = studio.texture.as_ref().unwrap().id();
        let (sender, receiver) = mpsc::channel();
        sender
            .send((
                0.0,
                Ok(video::PreviewFrame {
                    width: 1,
                    height: 1,
                    rgb: vec![255, 0, 0],
                }),
            ))
            .unwrap();
        studio.preview_worker = Some(receiver);
        studio.poll(&ctx);
        assert!(studio.preview_worker.is_none());
        assert_eq!(studio.position, 10.0);
        assert_eq!(studio.decoded_at, Some(10.0));
        assert_eq!(studio.texture.as_ref().unwrap().id(), texture);
    }

    #[test]
    fn completed_previews_update_while_either_timeline_is_still_scrubbing() {
        for gpx in [false, true] {
            let mut studio = prepared_studio(&timeline_track(), 60.0);
            let ctx = egui::Context::default();
            ready_preview(&mut studio, &ctx);
            for position in [10.0, 20.0, 15.0, 5.0] {
                let requested = studio.position;
                let (sender, receiver) = mpsc::channel();
                studio.preview_worker = Some(receiver);
                if gpx {
                    studio.seek_gpx(position, 59.96).unwrap();
                } else {
                    studio.seek_video(position, 59.96);
                }
                sender
                    .send((
                        requested,
                        Ok(video::PreviewFrame {
                            width: 1,
                            height: 1,
                            rgb: vec![255, 0, 0],
                        }),
                    ))
                    .unwrap();
                studio.poll(&ctx);
                assert_eq!(studio.position, position);
                assert_eq!(studio.decoded_at, Some(requested));
                assert_eq!(studio.texture.as_ref().unwrap().size(), [1, 1]);
                assert!(studio.preview_worker.is_none());
                assert!(!studio.preview_ready());
                assert!(!studio.playing);
            }
            let (sender, receiver) = mpsc::channel();
            studio.preview_worker = Some(receiver);
            sender
                .send((
                    studio.position,
                    Ok(video::PreviewFrame {
                        width: 2,
                        height: 1,
                        rgb: vec![0; 6],
                    }),
                ))
                .unwrap();
            studio.poll(&ctx);
            assert_eq!(studio.decoded_at, Some(studio.position));
            assert_eq!(studio.texture.as_ref().unwrap().size(), [2, 1]);
            assert!(studio.preview_ready());
        }
    }

    #[test]
    fn obsolete_preview_errors_do_not_interrupt_scrubbing() {
        let mut studio = prepared_studio(&timeline_track(), 60.0);
        let ctx = egui::Context::default();
        ready_preview(&mut studio, &ctx);
        let (sender, receiver) = mpsc::channel();
        studio.preview_worker = Some(receiver);
        studio.seek_video(10.0, 59.96);
        sender
            .send((0.0, Err("Obsolete seek failed.".into())))
            .unwrap();
        studio.poll(&ctx);
        assert_eq!(studio.decoded_at, Some(0.0));
        assert!(studio.texture.is_some());
        assert!(studio.error.is_none());
        assert!(studio.preview_worker.is_none());
    }

    #[test]
    fn pressed_landmark_button_unlocks_only_the_original_gpx_timeline_until_released() {
        let source = timeline_track();
        let mut studio = prepared_studio(&source, 40.0);
        let cuts = vec![CutRange::Time(Clip {
            start: 20.0,
            end: 30.0,
        })];
        let track = timeline::rebuild_cuts(&source, &cuts).unwrap();
        studio.commit_cuts(&track, cuts).unwrap();
        studio.position = 5.0;
        let ctx = egui::Context::default();
        ready_preview(&mut studio, &ctx);
        let texture = studio.texture.as_ref().unwrap().id();
        let draw = |studio: &mut Studio, events| {
            let mut controls = None;
            let output = crate::test_ui(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        vec2(1200.0, 900.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        if let Some(points) = studio.sync_tools(ui, Some(&source)) {
                            let mapping = studio.plan_for_points(&points).unwrap().unwrap();
                            let track = timeline::cut_ranges(&source, mapping.cuts()).unwrap();
                            studio.commit_sync(&track, points).unwrap();
                        }
                        let gpx = studio.gpx_position_control(ui, 39.96).unwrap();
                        let video = studio.video_position_control(ui, 39.96);
                        controls = Some((gpx, video));
                    });
                },
            );
            (output, controls.unwrap())
        };
        let (output, _) = draw(&mut studio, vec![]);
        let pick_button = button(&output, "Pick Landmark");
        draw(&mut studio, click(pick_button, true));
        let (_, (gpx, video)) = draw(&mut studio, click(pick_button, false));
        assert_eq!(studio.pick_kind, Some(Kind::Normal));
        assert!(gpx.enabled());
        assert!(!video.enabled());
        assert_eq!(studio.pick_time, 5.0);
        let point = egui::pos2(
            gpx.rect.left() + gpx.rect.width() * 0.9,
            gpx.rect.center().y,
        );
        draw(&mut studio, click(point, true));
        draw(&mut studio, click(point, false));
        let chosen = studio.pick_time;
        assert!(chosen > 50.0 && chosen < 60.0);
        assert_eq!(studio.position, 5.0);
        assert!(studio.sync_draft.is_none());
        studio.seek_gpx(25.0, 39.96).unwrap();
        assert_eq!(studio.gpx_time(), 25.0);
        assert_eq!(studio.position, 5.0);
        studio.receive_picks(vec![crate::map::RoutePick {
            left: 1,
            right: 2,
            fraction: 0.8,
        }]);
        let chosen = studio.pick_time;
        assert_eq!(chosen, 54.0);
        assert!(studio.sync_draft.is_none());
        assert_eq!(studio.pick_kind, Some(Kind::Normal));
        draw(&mut studio, click(video.rect.center(), true));
        draw(&mut studio, click(video.rect.center(), false));
        studio.seek_video(25.0, 39.96);
        studio.toggle_play();
        studio.decode_preview(&ctx);
        assert_eq!(studio.position, 5.0);
        assert_eq!(studio.decoded_at, Some(5.0));
        assert_eq!(studio.texture.as_ref().unwrap().id(), texture);
        assert!(studio.preview_worker.is_none());
        assert!(!studio.playing);
        let (output, _) = draw(&mut studio, vec![]);
        let pick_button = button(&output, "Pick Landmark");
        draw(&mut studio, click(pick_button, true));
        let (_, (_, video)) = draw(&mut studio, click(pick_button, false));
        assert!(studio.pick_kind.is_none());
        assert!(video.enabled());
        assert_eq!(
            studio.sync_points(),
            &[SyncPoint {
                video: 5.0,
                gpx: chosen,
                kind: Kind::Normal,
            }]
        );
        assert_eq!(studio.position, 5.0);
        let (output, _) = draw(&mut studio, vec![]);
        let pick_button = button(&output, "Pick Landmark");
        draw(&mut studio, click(pick_button, true));
        let (output, _) = draw(&mut studio, click(pick_button, false));
        let cancel = button(&output, "Cancel picking");
        draw(&mut studio, click(cancel, true));
        draw(&mut studio, click(cancel, false));
        assert!(studio.pick_kind.is_none());
        assert!(studio.sync_draft.is_none());
        assert_eq!(studio.sync_points().len(), 1);
    }

    #[test]
    fn every_pick_button_requests_automatic_application_and_cut_pair_completion_is_undoable() {
        let source = Arc::new(timeline_track());
        let mut track = source.clone();
        let mut studio = prepared_studio(&source, 40.04);
        let mut map = crate::map::TrackMap::offline();
        map.set_reference_track(source.clone());
        let ctx = egui::Context::default();
        let mut history = rlv_forge::history::History::default();
        let mut draw = |studio: &mut Studio, track: &Arc<Track>, events| {
            let mut request = None;
            let output = crate::test_ui(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        vec2(1200.0, 2400.0),
                    )),
                    events,
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        request = studio.show(
                            ui,
                            Tracks {
                                current: Some(track),
                                source: Some(&source),
                            },
                            "",
                            "",
                            &mut map,
                        );
                    });
                },
            );
            (output, request)
        };
        for (label, kind, video, gpx) in [
            ("Pick Landmark", Kind::Normal, 0.0, 0.0),
            ("Pick before cut", Kind::BeforeCut, 10.0, 10.0),
            ("Pick after cut", Kind::AfterCut, 10.04, 30.0),
        ] {
            studio.position = video;
            ready_preview(&mut studio, &ctx);
            let before = (studio.settings(), track.clone());
            let (output, request) = draw(&mut studio, &track, vec![]);
            assert!(request.is_none());
            let pick = button(&output, label);
            draw(&mut studio, &track, click(pick, true));
            let (_, request) = draw(&mut studio, &track, click(pick, false));
            assert!(request.is_none());
            assert_eq!(studio.pick_kind, Some(kind));
            studio.seek_gpx(gpx, 40.0).unwrap();
            let (output, _) = draw(&mut studio, &track, vec![]);
            let pick = button(&output, label);
            draw(&mut studio, &track, click(pick, true));
            let (_, request) = draw(&mut studio, &track, click(pick, false));
            let Some(Edit::Sync(points)) = request else {
                panic!("Releasing {label} must apply automatically")
            };
            assert_eq!(points.last().unwrap().kind, kind);
            let mapping = studio.plan_for_points(&points).unwrap().unwrap();
            track = Arc::new(timeline::cut_ranges(&source, mapping.cuts()).unwrap());
            studio.commit_sync(&track, points).unwrap();
            history.record(before, &(studio.settings(), track.clone()), None);
            assert!(studio.sync_draft.is_none());
            assert_eq!(studio.position, video);
            assert_eq!(
                studio.mapping().unwrap().has_unpaired_boundaries(),
                kind == Kind::BeforeCut
            );
        }
        assert_eq!(studio.sync_points().len(), 3);
        assert_eq!(
            studio.mapping().unwrap().cuts(),
            &[Clip {
                start: 10.0,
                end: 30.0
            }]
        );
        assert_eq!(studio.timing.as_ref().unwrap().duration, 40.0);
        let completed = (studio.settings(), track.clone());
        let (settings, uncut) = history.undo(completed.clone()).unwrap();
        studio.restore(settings, Some(&uncut));
        assert_eq!(studio.sync_points().len(), 2);
        assert!(studio.mapping().unwrap().has_unpaired_boundaries());
        assert!(studio.mapping().unwrap().cuts().is_empty());
        assert_eq!(studio.timing.as_ref().unwrap().duration, 60.0);
        let (settings, cut) = history.redo((studio.settings(), uncut)).unwrap();
        studio.restore(settings, Some(&cut));
        assert_eq!(studio.sync_points().len(), 3);
        assert_eq!(studio.mapping().unwrap().cuts().len(), 1);
        assert_eq!(cut, completed.1);
    }

    #[test]
    fn video_and_map_viewports_have_matching_dimensions_and_top_alignment() {
        for width in [600.0, 1200.0] {
            let source = Arc::new(timeline_track());
            let mut studio = prepared_studio(&source, 40.0);
            let mut map = crate::map::TrackMap::offline();
            let ctx = egui::Context::default();
            ready_preview(&mut studio, &ctx);
            map.set_reference_track(source.clone());
            let texture = studio.texture.as_ref().unwrap().id();
            let output = crate::test_ui(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        vec2(width, 2000.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        studio.show(
                            ui,
                            Tracks {
                                current: Some(&source),
                                source: Some(&source),
                            },
                            "",
                            "",
                            &mut map,
                        );
                    });
                },
            );
            let video = output
                .shapes
                .iter()
                .find_map(|shape| {
                    if let egui::epaint::Shape::Rect(rect) = &shape.shape {
                        if rect
                            .brush
                            .as_ref()
                            .is_some_and(|brush| brush.fill_texture_id == texture)
                        {
                            return Some(rect.rect);
                        }
                    }
                    None
                })
                .unwrap();
            let map = output
                .shapes
                .iter()
                .find_map(|shape| {
                    if let egui::epaint::Shape::Rect(rect) = &shape.shape {
                        if rect.fill == Color32::from_rgb(235, 241, 244) {
                            return Some(rect.rect);
                        }
                    }
                    None
                })
                .unwrap();
            assert!(
                (video.width() - map.width()).abs() < 1.0,
                "{video:?} vs {map:?}"
            );
            assert!(
                (video.height() - map.height()).abs() < 1.0,
                "{video:?} vs {map:?}"
            );
            assert!(
                (video.top() - map.top()).abs() < 1.0,
                "{video:?} vs {map:?}"
            );
        }
    }

    #[test]
    fn projects_keep_cut_drafts_editable_and_require_finishing_active_landmark_picks() {
        let track = timeline_track();
        let mut studio = prepared_studio(&track, 60.0);
        let current = timeline::cut_ranges(
            &track,
            &[Clip {
                start: 10.0,
                end: 20.0,
            }],
        )
        .unwrap();
        studio
            .commit_cuts(
                &current,
                vec![CutRange::Time(Clip {
                    start: 10.0,
                    end: 20.0,
                })],
            )
            .unwrap();
        studio.cut_draft = Some(vec![CutRange::Time(Clip {
            start: 40.0,
            end: 30.0,
        })]);
        let state = studio.project_state().unwrap();
        let restored = Studio::from_project(state, &track, &current).unwrap();
        assert_eq!(restored.cut_draft, studio.cut_draft);
        assert_eq!(restored.cuts(), studio.cuts());
        studio.pick_kind = Some(Kind::Normal);
        assert!(studio
            .project_state()
            .err()
            .unwrap()
            .contains("Finish or cancel"));
    }

    fn prepared_studio(track: &Track, duration: f64) -> Studio {
        let mut studio = Studio::default();
        studio.set_track(track);
        studio.settings.path = Some(PathBuf::from("prepared.mp4"));
        studio.settings.info = Some(VideoInfo {
            duration,
            fps: 25.0,
            width: 160,
            height: 90,
            frames: (duration * 25.0).round() as u64,
            has_audio: true,
        });
        studio
    }

    #[test]
    fn saved_project_resumes_smoothed_cuts_sync_drafts_and_video_time_without_source_gpx() {
        let points: String = (0..=60).map(|i| format!(
            "<trkpt lat=\"0\" lon=\"{}\"><ele>{}</ele><time>2026-01-01T00:{:02}:{:02}Z</time><extensions><power>{}</power></extensions></trkpt>",
            f64::from(i) * 0.0001, 100 + i, i / 60, i % 60, 200 + i,
        )).collect();
        let mut source =
            Track::parse(format!("<gpx><trk><trkseg>{points}</trkseg></trk></gpx>")).unwrap();
        source.points[15].elevation += 0.4;
        let mut studio = prepared_studio(&source, 45.04);
        let cuts = vec![CutRange::Time(Clip {
            start: 5.0,
            end: 10.0,
        })];
        let manual = timeline::rebuild_cuts(&source, &cuts).unwrap();
        studio.commit_cuts(&manual, cuts.clone()).unwrap();
        let points = vec![
            SyncPoint {
                video: 0.0,
                gpx: 0.0,
                kind: Kind::Normal,
            },
            SyncPoint {
                video: 25.0,
                gpx: 30.0,
                kind: Kind::BeforeCut,
            },
            SyncPoint {
                video: 25.04,
                gpx: 40.0,
                kind: Kind::AfterCut,
            },
            SyncPoint {
                video: 45.04,
                gpx: 60.0,
                kind: Kind::Normal,
            },
        ];
        let mapping = studio.plan_for_points(&points).unwrap().unwrap();
        let mut track = timeline::cut_ranges(&source, mapping.cuts()).unwrap();
        let before = track.clone();
        track
            .points
            .iter_mut()
            .find(|point| point.source_index == Some(15))
            .unwrap()
            .elevation += 0.2;
        source.retain_elevation_changes(&before, &track).unwrap();
        studio.commit_sync(&track, points.clone()).unwrap();
        studio.position = 35.04;
        studio.settings.country = "SE".into();
        studio.settings.folder = "my-ride".into();
        studio.sync_draft = Some(vec![SyncPoint {
            video: 10.0,
            gpx: 15.0,
            kind: Kind::Normal,
        }]);
        let directory = tempfile::tempdir().unwrap();
        let original_gpx = directory.path().join("source.gpx");
        let video = directory.path().join("prepared.mp4");
        std::fs::write(&original_gpx, source.source_xml()).unwrap();
        std::fs::write(&video, b"unchanged video").unwrap();
        studio.settings.path = Some(video.clone());
        let map = crate::map::TrackMap::offline();
        let mut project = crate::project::Project::new(
            &track,
            &source,
            studio.project_state().unwrap(),
            map.project_view(),
        );
        project.source_path = Some(original_gpx.clone());
        project.name = "Saved & resumed".into();
        project.description = "Keep every edit".into();
        project.trainer_mode = true;
        project.show_original = false;
        project.studio_open = true;
        project.chart = crate::ui::Chart::Profile;
        project.selection = [100.0, 200.0];
        let path = directory.path().join("work.gpxproj");
        project.save(&path, Some(&video)).unwrap();
        std::fs::remove_file(original_gpx).unwrap();
        let restored = crate::project::Project::load(&path).unwrap();
        assert_eq!(*restored.track, track);
        assert_eq!(*restored.source, source);
        assert_eq!(restored.studio.sync_points(), points);
        assert_eq!(restored.studio.cuts(), cuts);
        assert_eq!(restored.studio.position, 35.04);
        assert!(!restored.studio.playing);
        assert!(restored.studio.settings.source_timing.is_some());
        assert_eq!(restored.studio.settings.country, "SE");
        assert_eq!(restored.studio.settings.folder, "my-ride");
        assert_eq!(restored.studio.sync_draft, studio.sync_draft);
        assert_eq!(restored.document.name, "Saved & resumed");
        assert_eq!(restored.document.selection, [100.0, 200.0]);
        assert!(restored.document.trainer_mode);
        assert!(restored.document.studio_open);
        assert!(!restored.document.show_original);
        assert!(restored.document.chart == crate::ui::Chart::Profile);
        let expected = mapping.export(&source, "", "").unwrap();
        let output = restored
            .studio
            .mapping()
            .unwrap()
            .export(&restored.source, "", "")
            .unwrap();
        assert_eq!(output.gpx, expected.gpx);
        let mut reset = (*restored.track).clone();
        reset.reset();
        let mut original_reset = track.clone();
        original_reset.reset();
        assert_eq!(
            reset.export("", "").unwrap(),
            original_reset.export("", "").unwrap()
        );
        let rebuilt = timeline::rebuild_cuts(&restored.source, restored.studio.cuts()).unwrap();
        assert!(rebuilt
            .points
            .iter()
            .any(|point| point.source_index == Some(15)));
        assert_eq!(std::fs::read(&video).unwrap(), b"unchanged video");
        std::fs::remove_file(&video).unwrap();
        let missing_video = crate::project::Project::load(&path).unwrap();
        assert_eq!(missing_video.studio.sync_points(), points);
        assert!(missing_video
            .studio
            .error
            .as_ref()
            .unwrap()
            .contains("not found"));
    }

    #[test]
    fn sync_points_link_sliders_skip_original_route_and_round_trip_through_history() {
        let source = timeline_track();
        let mut studio = prepared_studio(&source, 40.04);
        let before = studio.settings();
        let points = vec![
            SyncPoint {
                video: 0.0,
                gpx: 0.0,
                kind: Kind::Normal,
            },
            SyncPoint {
                video: 10.0,
                gpx: 10.0,
                kind: Kind::BeforeCut,
            },
            SyncPoint {
                video: 10.04,
                gpx: 30.0,
                kind: Kind::AfterCut,
            },
            SyncPoint {
                video: 40.04,
                gpx: 60.0,
                kind: Kind::Normal,
            },
        ];
        let mapping = studio.plan_for_points(&points).unwrap().unwrap();
        let cut = timeline::cut_ranges(&source, mapping.cuts()).unwrap();
        studio.commit_sync(&cut, points.clone()).unwrap();
        let after = studio.settings();
        let mut history = rlv_forge::history::History::default();
        history.record(
            (before.clone(), source.clone()),
            &(after.clone(), cut.clone()),
            None,
        );
        studio.seek_video(10.02, 40.0);
        assert_eq!(studio.gpx_time(), 10.0);
        assert_eq!(
            studio
                .mapping()
                .unwrap()
                .original_at(studio.position)
                .unwrap(),
            10.0
        );
        studio.seek_gpx(10.0, 40.0).unwrap();
        assert_eq!(studio.position, 10.04);
        assert_eq!(
            studio
                .mapping()
                .unwrap()
                .original_at(studio.position)
                .unwrap(),
            30.0
        );
        studio.seek_gpx(studio.gpx_time() + 10.0, 40.0).unwrap();
        assert!((studio.position - 20.04).abs() < 1e-9);
        assert!(studio
            .cuts_with_interval(Clip {
                start: 5.0,
                end: 15.0
            })
            .is_err());
        let manual = studio
            .cuts_with_interval(Clip {
                start: 15.0,
                end: 20.0,
            })
            .unwrap();
        assert_eq!(
            manual,
            [CutRange::Time(Clip {
                start: 35.0,
                end: 40.0
            })]
        );
        let (settings, track) = history.undo((after.clone(), cut.clone())).unwrap();
        studio.restore(settings, Some(&track));
        assert!(studio.mapping().is_none());
        assert!(studio.sync_points().is_empty());
        let (settings, track) = history.redo((before, source.clone())).unwrap();
        studio.restore(settings, Some(&track));
        assert_eq!(studio.sync_points(), points);
        assert!(studio.mapping().is_some());
        studio.commit_sync(&source, Vec::new()).unwrap();
        assert!(studio.mapping().is_none());
        assert_eq!(studio.timing.as_ref().unwrap().duration, 60.0);
        assert!(studio.settings.source_timing.is_some());
    }

    #[test]
    fn ambiguous_map_picks_require_confirmation_and_invalid_sync_changes_are_atomic() {
        let source = timeline_track();
        let mut studio = prepared_studio(&source, 40.0);
        studio.pick_kind = Some(Kind::Normal);
        studio.position = 5.0;
        studio.receive_picks(vec![
            crate::map::RoutePick {
                left: 0,
                right: 1,
                fraction: 0.25,
            },
            crate::map::RoutePick {
                left: 1,
                right: 2,
                fraction: 0.25,
            },
        ]);
        assert!(studio.sync_draft.is_none());
        assert_eq!(studio.pick_candidates, [7.5, 37.5]);
        studio.append_sync_point(37.5);
        assert_eq!(studio.sync_draft.as_ref().unwrap()[0].gpx, 37.5);
        assert!(studio.sync_points().is_empty());
        let before = studio.settings();
        let invalid = vec![SyncPoint {
            video: 5.0,
            gpx: 70.0,
            kind: Kind::BeforeCut,
        }];
        assert!(studio.commit_sync(&source, invalid).is_err());
        assert!(studio.settings() == before);
        assert!(studio.mapping().is_none());
        studio.seek_video(10.0, 40.0);
        assert!(studio.pick_kind.is_none());
        assert!(studio.pick_candidates.is_empty());
    }

    #[test]
    fn sync_controls_apply_and_cancel_drafts_without_mutating_committed_points() {
        let mut studio = prepared_studio(&timeline_track(), 40.0);
        let point = SyncPoint {
            video: 5.0,
            gpx: 10.0,
            kind: Kind::Normal,
        };
        let ctx = egui::Context::default();
        let draw = |studio: &mut Studio, events| {
            let mut request = None;
            let output = crate::test_ui(
                &ctx,
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        request = studio.show_sync_points(ui);
                    });
                },
            );
            (output, request)
        };
        studio.sync_draft = Some(vec![point]);
        let (output, _) = draw(&mut studio, vec![]);
        let pos = button(&output, "Apply sync points");
        draw(&mut studio, click(pos, true));
        let (_, request) = draw(&mut studio, click(pos, false));
        assert_eq!(request, Some(vec![point]));
        assert!(studio.sync_points().is_empty());
        let (output, _) = draw(&mut studio, vec![]);
        let pos = button(&output, "Cancel sync changes");
        draw(&mut studio, click(pos, true));
        draw(&mut studio, click(pos, false));
        assert!(studio.sync_draft.is_none());
        assert!(studio.sync_points().is_empty());
    }

    fn click(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    fn button(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
        output
            .shapes
            .iter()
            .find_map(|shape| {
                if let egui::epaint::Shape::Text(text) = &shape.shape {
                    if text.galley.job.text == label {
                        return Some(text.pos + text.galley.size() * 0.5);
                    }
                }
                None
            })
            .unwrap()
    }

    #[test]
    fn editable_cut_table_applies_original_clock_rows_and_cancels_drafts() {
        let source = timeline_track();
        let mut studio = prepared_studio(&source, 40.0);
        let original = vec![
            CutRange::Time(Clip {
                start: 3.0,
                end: 7.0,
            }),
            CutRange::Time(Clip {
                start: 20.0,
                end: 25.0,
            }),
        ];
        studio
            .commit_cuts(
                &timeline::rebuild_cuts(&source, &original).unwrap(),
                original.clone(),
            )
            .unwrap();
        let before = studio.settings();
        let revised = vec![
            CutRange::Time(Clip {
                start: 5.0,
                end: 8.0,
            }),
            original[1].clone(),
        ];
        studio.cut_draft = Some(revised.clone());
        let ctx = egui::Context::default();
        let draw = |studio: &mut Studio, events| {
            let mut request = None;
            let output = crate::test_ui(
                &ctx,
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        request = studio.show_cuts(ui);
                    });
                },
            );
            (output, request)
        };
        let (output, _) = draw(&mut studio, vec![]);
        let pos = button(&output, "Apply cut changes");
        draw(&mut studio, click(pos, true));
        let (_, request) = draw(&mut studio, click(pos, false));
        assert_eq!(request, Some(revised.clone()));
        assert!(studio.settings() == before);
        let rebuilt = timeline::rebuild_cuts(&source, request.as_ref().unwrap()).unwrap();
        studio.commit_cuts(&rebuilt, request.unwrap()).unwrap();
        assert_eq!(studio.cuts()[1], original[1]);
        let after = studio.settings();
        studio.cut_draft = Some(original);
        let (output, _) = draw(&mut studio, vec![]);
        let pos = button(&output, "Cancel cut changes");
        draw(&mut studio, click(pos, true));
        draw(&mut studio, click(pos, false));
        assert!(studio.cut_draft.is_none());
        assert!(studio.settings() == after);
        assert!(studio
            .commit_cuts(
                &source,
                vec![CutRange::Time(Clip {
                    start: 70.0,
                    end: 80.0
                })]
            )
            .is_err());
        assert!(studio.settings() == after);
    }

    #[test]
    fn replacing_video_preserves_applied_sync_or_rejects_invalid_duration_atomically() {
        let source = timeline_track();
        let mut studio = prepared_studio(&source, 40.0);
        let points = vec![SyncPoint {
            video: 30.0,
            gpx: 30.0,
            kind: Kind::Normal,
        }];
        studio.commit_sync(&source, points.clone()).unwrap();
        let before = studio.settings();
        let info = studio.settings.info.clone().unwrap();
        let ctx = egui::Context::default();
        for (duration, accepted) in [(20.0, false), (50.0, true)] {
            let (sender, receiver) = mpsc::channel();
            sender
                .send(Ok(Job::Loaded(
                    PathBuf::from("replacement.mov"),
                    VideoInfo {
                        duration,
                        ..info.clone()
                    },
                )))
                .unwrap();
            studio.worker = Some(receiver);
            studio.poll(&ctx);
            assert_eq!(
                studio.settings.info.as_ref().unwrap().duration,
                if accepted { 50.0 } else { 40.0 }
            );
            assert_eq!(studio.sync_points(), points);
            assert_eq!(studio.mapping().unwrap().original_at(30.0).unwrap(), 30.0);
            if !accepted {
                assert!(studio.settings() == before);
                assert!(studio.error.is_some());
            }
        }
    }

    fn timeline_track() -> Track {
        Track::parse(
            "<gpx><trk><trkseg>\
            <trkpt lat=\"0\" lon=\"0\"><ele>100</ele><time>2026-01-01T00:00:00Z</time></trkpt>\
            <trkpt lat=\"0\" lon=\"0.03\"><ele>130</ele><time>2026-01-01T00:00:30Z</time></trkpt>\
            <trkpt lat=\"0\" lon=\"0.06\"><ele>160</ele><time>2026-01-01T00:01:00Z</time></trkpt>\
            </trkseg></trk></gpx>"
                .into(),
        )
        .unwrap()
    }

    #[test]
    fn linked_seeking_and_ten_second_steps_use_the_alignment_in_both_directions() {
        let mut studio = Studio::default();
        studio.set_track(&timeline_track());
        for offset in [0.0, 5.0, -5.0] {
            studio.settings.offset = offset;
            studio.playing = true;
            studio.seek_video(20.0, 50.0);
            assert_eq!(studio.gpx_time(), 20.0 + offset);
            assert!(!studio.playing);
            studio.playing = true;
            studio.seek_gpx(30.0 + offset, 50.0).unwrap();
            assert_eq!(studio.position, 30.0);
            assert!(!studio.playing);
            studio.seek_gpx(studio.gpx_time() + 10.0, 50.0).unwrap();
            assert_eq!(studio.position, 40.0);
            studio.seek_gpx(studio.gpx_time() - 10.0, 50.0).unwrap();
            assert_eq!(studio.position, 30.0);
            studio.seek_video(studio.position + 10.0, 50.0);
            assert_eq!(studio.gpx_time(), 40.0 + offset);
            studio.seek_video(studio.position - 10.0, 50.0);
            assert_eq!(studio.gpx_time(), 30.0 + offset);
        }
    }

    #[test]
    fn linked_seeks_respect_shared_coverage_and_updated_cut_duration() {
        let track = timeline_track();
        let mut studio = Studio::default();
        studio.set_track(&track);
        studio.settings.offset = 5.0;
        studio.seek_gpx(0.0, 50.0).unwrap();
        assert_eq!(studio.position, 0.0);
        assert_eq!(studio.gpx_time(), 5.0);
        studio.seek_gpx(60.0, 50.0).unwrap();
        assert_eq!(studio.position, 50.0);
        assert_eq!(studio.gpx_time(), 55.0);
        studio.settings.offset = -5.0;
        studio.seek_gpx(0.0, 50.0).unwrap();
        assert_eq!(studio.position, 5.0);
        studio.seek_gpx(60.0, 50.0).unwrap();
        assert_eq!(studio.position, 50.0);
        assert_eq!(studio.gpx_time(), 45.0);
        studio.settings.offset = 70.0;
        assert!(studio.seek_gpx(20.0, 50.0).is_err());
        assert_eq!(studio.position, 50.0);
        studio.settings.offset = 0.0;
        let before = studio.settings();
        let cut = timeline::cut(
            &track,
            Clip {
                start: 10.0,
                end: 20.0,
            },
        )
        .unwrap();
        studio
            .track_edited(&cut, Some(10.0), [0.0, track.total_distance()])
            .unwrap();
        studio.seek_gpx(25.0, 50.0).unwrap();
        assert_eq!(studio.position, 25.0);
        assert!((studio.route_position(Some(&cut)).unwrap()[0] - 0.035).abs() < 1e-9);
        studio.restore(before, Some(&track));
        assert_eq!(studio.gpx_time(), 25.0);
        assert!((studio.route_position(Some(&track)).unwrap()[0] - 0.025).abs() < 1e-9);
    }

    #[test]
    fn dragging_either_slider_updates_the_other_timeline() {
        let ctx = egui::Context::default();
        let mut studio = Studio::default();
        studio.set_track(&timeline_track());
        studio.settings.offset = 5.0;
        let mut gpx_rect = egui::Rect::NOTHING;
        let mut video_rect = egui::Rect::NOTHING;
        let mut draw = |studio: &mut Studio, events| {
            let _ = crate::test_ui(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        vec2(800.0, 400.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        gpx_rect = studio.gpx_position_control(ui, 50.0).unwrap().rect;
                        video_rect = studio.video_position_control(ui, 50.0).rect;
                    });
                },
            );
            (gpx_rect, video_rect)
        };
        let (gpx, _) = draw(&mut studio, Vec::new());
        let click = |rect: egui::Rect, pressed| {
            vec![
                egui::Event::PointerMoved(rect.center()),
                egui::Event::PointerButton {
                    pos: rect.center(),
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        studio.playing = true;
        draw(&mut studio, click(gpx, true));
        assert!((studio.gpx_time() - 30.0).abs() < 1.0);
        assert!((studio.position - 25.0).abs() < 1.0);
        assert!(!studio.playing);
        let (_, video) = draw(&mut studio, click(gpx, false));
        studio.seek_video(0.0, 50.0);
        studio.playing = true;
        draw(&mut studio, click(video, true));
        assert!((studio.position - 25.0).abs() < 1.0);
        assert!((studio.gpx_time() - 30.0).abs() < 1.0);
        assert!(!studio.playing);
        draw(&mut studio, click(video, false));
    }

    #[test]
    fn map_position_uses_the_shortened_clock_after_cuts_and_history_restores() {
        let points: String = (0..=10).map(|i| format!(
            "<trkpt lat=\"0\" lon=\"{}\"><ele>{}</ele><time>2026-01-01T00:00:{i:02}Z</time></trkpt>",
            i as f64 * 0.01, 100 + i,
        )).collect();
        let original =
            Track::parse(format!("<gpx><trk><trkseg>{points}</trkseg></trk></gpx>")).unwrap();
        let mut studio = Studio::default();
        studio.set_track(&original);
        studio.position = 3.0;
        let before = studio.settings();
        assert!((studio.route_position(Some(&original)).unwrap()[0] - 0.03).abs() < 1e-9);
        let cut = timeline::cut(
            &original,
            Clip {
                start: 3.0,
                end: 7.0,
            },
        )
        .unwrap();
        studio
            .track_edited(&cut, Some(3.0), [0.0, original.total_distance()])
            .unwrap();
        let after = studio.settings();
        for (video_seconds, longitude) in [(2.0, 0.02), (3.0, 0.07), (3.5, 0.075), (5.0, 0.09)] {
            studio.position = video_seconds;
            assert!((studio.route_position(Some(&cut)).unwrap()[0] - longitude).abs() < 1e-9);
        }
        studio.position = 3.0;
        studio.restore(before, Some(&original));
        assert!((studio.route_position(Some(&original)).unwrap()[0] - 0.03).abs() < 1e-9);
        studio.restore(after, Some(&cut));
        assert!((studio.route_position(Some(&cut)).unwrap()[0] - 0.07).abs() < 1e-9);
        let cut_twice = timeline::cut(
            &cut,
            Clip {
                start: 1.0,
                end: 2.0,
            },
        )
        .unwrap();
        studio
            .track_edited(&cut_twice, Some(1.0), [0.0, cut.total_distance()])
            .unwrap();
        assert!((studio.route_position(Some(&cut_twice)).unwrap()[0] - 0.08).abs() < 1e-9);
        studio.settings.offset = 1.0;
        assert!((studio.route_position(Some(&cut_twice)).unwrap()[0] - 0.09).abs() < 1e-9);
    }

    #[test]
    fn video_slider_fills_the_available_width_at_different_window_sizes() {
        for width in [500.0, 1200.0] {
            let ctx = egui::Context::default();
            let mut studio = Studio::default();
            let _ = crate::test_ui(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        vec2(width, 400.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let available = ui.available_width();
                        let response = studio.video_position_control(ui, 100.0);
                        assert!((response.rect.width() - available).abs() < 1.0);
                    });
                },
            );
        }
    }

    #[test]
    fn marking_uses_video_seconds_directly_and_can_be_undone() {
        let mut studio = Studio::default();
        studio.settings.offset = 10.0;
        studio.settings.marked = Clip {
            start: 0.0,
            end: 100.0,
        };
        let before = studio.settings();
        studio.position = 42.123;
        studio.mark_from_video(true);
        assert_eq!(studio.settings.marked.start, 42.123);
        assert_eq!(studio.settings.marked.end, 100.0);
        studio.position = 67.89;
        studio.mark_from_video(false);
        assert_eq!(studio.settings.marked.start, 42.123);
        assert_eq!(studio.settings.marked.end, 67.89);
        let marked = studio.settings();
        studio.restore(before.clone(), None);
        assert!(studio.settings() == before);
        studio.restore(marked.clone(), None);
        assert!(studio.settings() == marked);
    }

    #[test]
    fn cut_rows_accumulate_follow_history_and_clear_with_a_new_track() {
        let points: String = (0..=10).map(|i| format!(
            "<trkpt lat=\"0\" lon=\"{}\"><ele>{}</ele><time>2026-01-01T00:00:{i:02}Z</time></trkpt>",
            i as f64 * 0.0001, 100 + i,
        )).collect();
        let original =
            Track::parse(format!("<gpx><trk><trkseg>{points}</trkseg></trk></gpx>")).unwrap();
        let mut studio = Studio::default();
        studio.set_track(&original);
        let before = studio.settings();
        let first = Clip {
            start: 3.0,
            end: 7.0,
        };
        let cut = timeline::cut(&original, first).unwrap();
        studio
            .track_edited(&cut, Some(first.start), [0.0, original.total_distance()])
            .unwrap();
        let after_first = studio.settings();
        let second = Clip {
            start: 1.0,
            end: 2.0,
        };
        let cut_twice = timeline::cut(&cut, second).unwrap();
        studio
            .track_edited(&cut_twice, Some(second.start), [0.0, cut.total_distance()])
            .unwrap();
        assert!(studio.settings.cuts == vec![CutRange::Time(first), CutRange::Time(second)]);
        let after_second = studio.settings();
        studio.restore(after_first.clone(), Some(&cut));
        assert!(studio.settings() == after_first);
        studio.restore(before, Some(&original));
        assert!(studio.settings.cuts.is_empty());
        studio.restore(after_second.clone(), Some(&cut_twice));
        assert!(studio.settings() == after_second);
        studio.set_track(&original);
        assert!(studio.settings.cuts.is_empty());
        let untimed = Track::parse(
            "<gpx><rte><rtept lat=\"0\" lon=\"0\"/><rtept lat=\"0\" lon=\"0.01\"/></rte></gpx>"
                .into(),
        )
        .unwrap();
        studio.track_edited(&untimed, None, [100.0, 200.0]).unwrap();
        assert!(studio.settings.cuts == vec![CutRange::Distance([100.0, 200.0])]);
    }

    #[test]
    fn restoring_settings_includes_video_alignment_cut_bounds_and_export_options() {
        let mut studio = Studio::default();
        let before = studio.settings();
        studio.settings.path = Some(PathBuf::from("prepared.mov"));
        studio.settings.offset = 10.0;
        studio.settings.anchor = 15.0;
        studio.settings.marked = Clip {
            start: 3.0,
            end: 7.0,
        };
        studio.settings.country = "SE".into();
        studio.settings.folder = "new-bundle".into();
        let after = studio.settings();
        studio.restore(before.clone(), None);
        assert!(studio.settings() == before);
        studio.restore(after.clone(), None);
        assert!(studio.settings() == after);
        studio.adjust_after_cut(Clip {
            start: 2.0,
            end: 5.0,
        });
        assert_eq!(studio.settings.offset, 7.0);
        assert_eq!(studio.settings.anchor, 12.0);
        studio.adjust_after_cut(Clip {
            start: 5.0,
            end: 9.0,
        });
        assert_eq!(studio.settings.offset, 5.0);
        assert_eq!(studio.settings.anchor, 8.0);
    }
}
