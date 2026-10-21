use eframe::egui::{
    self, pos2, vec2, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind,
};
use rlv_forge::{
    gpx::{Operation, Track},
    history::History,
    sync::SyncPoint,
    timeline::{self, Clip, CutRange},
};
use std::{
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, Receiver, TryRecvError},
        Arc,
    },
};

const BLUE: Color32 = Color32::from_rgb(0, 136, 204);
const ORIGINAL: Color32 = Color32::from_rgb(40, 85, 210);
const GREEN: Color32 = Color32::from_rgb(25, 145, 75);
const INK: Color32 = Color32::from_rgb(51, 51, 51);
const BACKGROUND: Color32 = Color32::from_rgb(252, 252, 250);

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Chart {
    Elevation,
    Slope,
    Profile,
}

#[derive(Default)]
pub(crate) struct LinkedProfile {
    track: Option<Arc<Track>>,
    modified_slopes: Vec<f64>,
    seek_distance: Option<f64>,
}

impl LinkedProfile {
    pub fn take_seek(&mut self) -> Option<f64> {
        self.seek_distance.take()
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        track: Option<&Arc<Track>>,
        distance: Option<f64>,
        span: f64,
    ) -> Option<[f64; 2]> {
        let span = span.max(1.0);
        self.seek_distance = None;
        ui.strong("Elevation profile");
        let (frame, painter) =
            ui.allocate_painter(vec2(ui.available_width(), 225.0), Sense::hover());
        let plot = frame.rect.shrink2(vec2(55.0, 24.0));
        let response = ui.interact(
            plot,
            frame.id.with("scrub"),
            if track.is_some() && distance.is_some() {
                Sense::click_and_drag()
            } else {
                Sense::hover()
            },
        );
        let selection = track.zip(distance).map(|(track, distance)| {
            if self
                .track
                .as_ref()
                .is_none_or(|cached| !Arc::ptr_eq(cached, track))
            {
                self.modified_slopes = track.slopes(false);
                self.track = Some(track.clone());
            }
            let selection = [distance - span / 2.0, distance + span / 2.0];
            paint_chart(
                &painter,
                plot,
                track,
                &[],
                &self.modified_slopes,
                selection,
                Chart::Profile,
                false,
                true,
            );
            painter.line_segment(
                [
                    pos2(plot.center().x, plot.top()),
                    pos2(plot.center().x, plot.bottom()),
                ],
                Stroke::new(1.5_f32, Color32::from_rgb(200, 55, 45)),
            );
            let index = track
                .points
                .partition_point(|point| point.distance < distance)
                .min(track.points.len() - 1);
            painter.text(
                pos2(plot.center().x, plot.top() - 5.0),
                Align2::CENTER_BOTTOM,
                format!("{:+.1}%", self.modified_slopes[index]),
                FontId::proportional(14.0),
                INK,
            );
            let target = if response.dragged() {
                let delta = ui.input(|input| input.pointer.delta().x);
                (delta != 0.0).then_some(distance - f64::from(delta / plot.width()) * span)
            } else if response.clicked() {
                response.interact_pointer_pos().map(|pointer| {
                    selection[0] + f64::from((pointer.x - plot.left()) / plot.width()) * span
                })
            } else if response.hovered() {
                let (scroll, shift) =
                    ui.input(|input| (input.smooth_scroll_delta, input.modifiers.shift));
                if scroll.x != 0.0 || (shift && scroll.y != 0.0) {
                    let delta = if scroll.x != 0.0 { scroll.x } else { scroll.y };
                    ui.input_mut(|input| input.smooth_scroll_delta = egui::Vec2::ZERO);
                    Some(distance - f64::from(delta / plot.width()) * span)
                } else {
                    None
                }
            } else {
                None
            };
            self.seek_distance = target.map(|target| target.clamp(0.0, track.total_distance()));
            if response.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            }
            selection
        });
        if selection.is_none() {
            painter.rect_filled(plot, 0.0, Color32::WHITE);
            painter.rect_stroke(
                plot,
                0.0,
                Stroke::new(1.0_f32, Color32::from_gray(180)),
                StrokeKind::Inside,
            );
            painter.text(
                plot.center(),
                Align2::CENTER_CENTER,
                "No GPX position at this video time",
                FontId::proportional(14.0),
                INK,
            );
        }
        ui.horizontal_wrapped(slope_legend);
        let current = distance.map_or_else(
            || "outside GPX coverage".into(),
            |distance| format!("{:.3} km", distance / 1000.0),
        );
        ui.add(
            egui::Label::new(format!(
                "Current: {current} | map span: {:.3} km",
                span / 1000.0
            ))
            .truncate(),
        );
        response.on_hover_cursor(if selection.is_some() {
            egui::CursorIcon::Grab
        } else {
            egui::CursorIcon::Default
        })
            .on_hover_text("Click to scrub; drag horizontally or Shift+scroll to pan and seek. Map zoom controls the visible distance. While picking a landmark, only the GPX moves and the video stays frozen.");
        selection
    }
}

fn slope_legend(ui: &mut egui::Ui) {
    ui.label("Slope colours:");
    for (slope, label) in [
        (-10.0, "downhill"),
        (0.0, "flat"),
        (5.0, "+5%"),
        (12.0, "+12%"),
    ] {
        ui.colored_label(slope_color(slope), label);
    }
}

enum Job {
    Loaded {
        track: Track,
        path: PathBuf,
    },
    Saved(PathBuf),
    ProjectSaved(PathBuf),
    ProjectLoaded {
        project: Box<crate::project::LoadedProject>,
        path: PathBuf,
    },
    Preview(String),
    AutoSmoothed {
        track: Track,
        source: Track,
        changed: usize,
        max_shift: f64,
        trainer: bool,
    },
    Cut {
        track: Track,
        start: Option<f64>,
    },
    RebuiltCuts {
        track: Track,
        cuts: Vec<CutRange>,
    },
    Synced {
        track: Track,
        points: Vec<SyncPoint>,
    },
    Cancelled,
}

#[derive(Clone, PartialEq)]
struct EditState {
    track: Option<Arc<Track>>,
    cut_source: Option<Arc<Track>>,
    filename: String,
    name: String,
    description: String,
    source_path: Option<PathBuf>,
    selection: [f64; 2],
    chart: Chart,
    show_original: bool,
    trainer_mode: bool,
    studio_open: bool,
    studio: crate::studio::Settings,
    map: crate::map::ViewState,
    project_path: Option<PathBuf>,
}

pub struct RlvForge {
    track: Option<Arc<Track>>,
    cut_source: Option<Arc<Track>>,
    worker: Option<Receiver<Result<Job, String>>>,
    filename: String,
    name: String,
    description: String,
    source_path: Option<PathBuf>,
    selection: [f64; 2],
    hover_distance: Option<f64>,
    chart: Chart,
    show_original: bool,
    status: String,
    error: Option<String>,
    preview: Option<String>,
    modified_slopes: Vec<f64>,
    original_slopes: Vec<f64>,
    handle: Option<usize>,
    dirty: bool,
    confirm: Option<Pending>,
    map: crate::map::TrackMap,
    studio: crate::studio::Studio,
    studio_open: bool,
    trainer_mode: bool,
    pending_video: Option<PathBuf>,
    history: History<EditState>,
    project_path: Option<PathBuf>,
    project_dirty: bool,
}

enum Pending {
    Open(Option<PathBuf>),
    OpenProject,
    Reset,
    Close,
}

impl RlvForge {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        path: Option<PathBuf>,
        video: Option<PathBuf>,
    ) -> Self {
        let mut style = (*cc.egui_ctx.global_style()).clone();
        style.visuals = egui::Visuals::light();
        style.visuals.panel_fill = BACKGROUND;
        style.visuals.window_fill = BACKGROUND;
        style.visuals.override_text_color = Some(INK);
        style.visuals.selection.bg_fill = BLUE;
        style.spacing.item_spacing = vec2(9.0, 9.0);
        style.spacing.button_padding = vec2(11.0, 6.0);
        style
            .text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(16.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(16.0));
        cc.egui_ctx.set_global_style(style);
        let mut app = Self::empty(video);
        if let Some(path) = path {
            app.open(Some(path), &cc.egui_ctx);
        }
        app
    }

    fn empty(video: Option<PathBuf>) -> Self {
        Self {
            track: None,
            cut_source: None,
            worker: None,
            filename: "ride.gpx".into(),
            name: String::new(),
            description: String::new(),
            source_path: None,
            selection: [0.0, 0.0],
            hover_distance: None,
            chart: Chart::Elevation,
            show_original: true,
            status: "Open a GPX file or drop one anywhere in this window.".into(),
            error: None,
            preview: None,
            modified_slopes: Vec::new(),
            original_slopes: Vec::new(),
            handle: None,
            dirty: false,
            confirm: None,
            map: crate::map::TrackMap::default(),
            studio: crate::studio::Studio::default(),
            studio_open: false,
            trainer_mode: false,
            pending_video: video,
            history: History::default(),
            project_path: None,
            project_dirty: false,
        }
    }

    fn spawn(
        &mut self,
        ctx: &egui::Context,
        work: impl FnOnce() -> Result<Job, String> + Send + 'static,
    ) {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        self.worker = Some(receiver);
        self.error = None;
        std::thread::spawn(move || {
            let _ = sender.send(work());
            ctx.request_repaint();
        });
    }

    fn open(&mut self, path: Option<PathBuf>, ctx: &egui::Context) {
        self.open_kind(path, ctx, false);
    }

    fn open_kind(&mut self, path: Option<PathBuf>, ctx: &egui::Context, project_only: bool) {
        self.status = "Opening ride...".into();
        self.studio.pause();
        self.spawn(ctx, move || {
            let path = path.or_else(|| {
                let dialog = rfd::FileDialog::new().add_filter("RLV Forge project", &["gpxproj"]);
                if project_only {
                    dialog.pick_file()
                } else {
                    dialog.add_filter("GPX tracks", &["gpx"]).pick_file()
                }
            });
            match path {
                Some(path)
                    if path
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("gpxproj")) =>
                {
                    let project = crate::project::Project::load(&path)?;
                    let path = std::path::absolute(path).map_err(|error| error.to_string())?;
                    Ok(Job::ProjectLoaded {
                        project: Box::new(project),
                        path,
                    })
                }
                Some(path) => Ok(Job::Loaded {
                    track: Track::read(&path)?,
                    path,
                }),
                None => Ok(Job::Cancelled),
            }
        });
    }

    fn request(&mut self, pending: Pending, ctx: &egui::Context) {
        if self.dirty
            || ((self.studio.dirty || self.project_dirty)
                && matches!(
                    pending,
                    Pending::Open(_) | Pending::OpenProject | Pending::Close
                ))
        {
            self.confirm = Some(pending);
        } else {
            self.perform(pending, ctx);
        }
    }

    fn perform(&mut self, pending: Pending, ctx: &egui::Context) {
        match pending {
            Pending::Open(path) => self.open(path, ctx),
            Pending::OpenProject => self.open_kind(None, ctx, true),
            Pending::Reset => {
                if let Some(track) = &mut self.track {
                    Arc::make_mut(track).reset();
                    if let Some(source) = &mut self.cut_source {
                        Arc::make_mut(source).reset();
                    }
                    self.selection = [0.0, track.total_distance()];
                    self.modified_slopes = track.slopes(false);
                    self.preview = None;
                    self.dirty = true;
                    self.status = "Original elevations restored; the full ride is selected.".into();
                }
            }
            Pending::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    fn poll(&mut self, ctx: &egui::Context) -> bool {
        let result = match self.worker.as_ref().map(Receiver::try_recv) {
            Some(Ok(result)) => result,
            Some(Err(TryRecvError::Disconnected)) => {
                Err("The background task stopped unexpectedly.".into())
            }
            _ => return false,
        };
        self.worker = None;
        let mut project_clean = false;
        match result {
            Ok(Job::Loaded { track, path }) => {
                self.hover_distance = None;
                self.selection = [0.0, track.total_distance()];
                self.filename = format!(
                    "{}-smoothed.gpx",
                    path.file_stem().unwrap_or_default().to_string_lossy()
                );
                self.name = track.name.clone();
                self.description = track.description.clone();
                self.original_slopes = track.slopes(true);
                self.modified_slopes = self.original_slopes.clone();
                self.status = format!(
                    "Loaded {} points from {}.",
                    track.points.len(),
                    path.display()
                );
                self.source_path = Some(path);
                self.studio.set_track(&track);
                let track = Arc::new(track);
                self.map.set_reference_track(track.clone());
                self.cut_source = Some(track.clone());
                self.track = Some(track);
                self.preview = None;
                self.dirty = false;
                self.project_path = None;
            }
            Ok(Job::ProjectLoaded { project, path }) => {
                let project = *project;
                let document = project.document;
                self.track = Some(project.track);
                self.cut_source = Some(project.source);
                self.studio = project.studio;
                self.map = project.map;
                self.filename = document.filename;
                self.name = document.name;
                self.description = document.description;
                self.source_path = document.source_path;
                self.selection = document.selection;
                self.chart = document.chart;
                self.show_original = document.show_original;
                self.trainer_mode = document.trainer_mode;
                self.studio_open = document.studio_open;
                self.original_slopes = self.track.as_ref().unwrap().slopes(true);
                self.modified_slopes = self.track.as_ref().unwrap().slopes(false);
                self.preview = None;
                self.hover_distance = None;
                self.handle = None;
                self.pending_video = None;
                self.project_path = Some(path.clone());
                self.project_dirty = false;
                self.dirty = false;
                project_clean = true;
                self.status = format!(
                    "Restored project {}. Video references remain external; playback is paused.",
                    path.display()
                );
            }
            Ok(Job::ProjectSaved(path)) => {
                self.project_path = Some(path.clone());
                self.project_dirty = false;
                self.dirty = false;
                self.studio.dirty = false;
                project_clean = true;
                self.status = format!("Saved project {}.", path.display());
                if let Some(pending) = self.confirm.take() {
                    self.perform(pending, ctx);
                }
            }
            Ok(Job::Saved(path)) => {
                self.status = format!("Saved {}.", path.display());
                self.dirty = false;
            }
            Ok(Job::Cut { track, start }) => self.apply_cut(Ok(track), start),
            Ok(Job::RebuiltCuts { track, cuts }) => match self.studio.commit_cuts(&track, cuts) {
                Ok(()) => self.install_cut_track(track),
                Err(error) => self.error = Some(error),
            },
            Ok(Job::Synced { track, points }) => match self.studio.commit_sync(&track, points) {
                Ok(()) => {
                    self.install_cut_track(track);
                    self.status = "Sync points applied. Map and timelines follow the mapping; GPX jumps are levelled and video is unchanged.".into();
                }
                Err(error) => self.error = Some(error),
            },
            Ok(Job::Preview(xml)) => {
                self.preview = Some(xml);
                self.status = "GPX preview ready. Export GPX saves the full file without browser size limits.".into();
            }
            Ok(Job::AutoSmoothed {
                track,
                source,
                changed,
                max_shift,
                trainer,
            }) => {
                if changed > 0 {
                    self.modified_slopes = track.slopes(false);
                    self.track = Some(Arc::new(track));
                    self.cut_source = Some(Arc::new(source));
                    self.preview = None;
                    self.dirty = true;
                    let mode = if trainer {
                        "Incyclist smoothing"
                    } else {
                        "Auto Smooth"
                    };
                    self.status = format!("{mode} updated {changed} points (largest change: {max_shift:.2} m). Endpoints preserved; no slope limits applied.");
                } else {
                    self.status = "Auto Smooth made no changes: the selected data is already smooth or too short/sparse for reliable smoothing.".into();
                }
            }
            Ok(Job::Cancelled) => self.status = "File dialog cancelled.".into(),
            Err(error) => {
                self.status =
                    "The operation failed; the current ride has not been replaced.".into();
                self.error = Some(error);
            }
        }
        project_clean
    }

    fn project_document(&self) -> Result<crate::project::Project, String> {
        let track = self
            .track
            .as_deref()
            .ok_or("Open a GPX before saving a project.")?;
        let source = self.cut_source.as_deref().unwrap_or(track);
        let mut document = crate::project::Project::new(
            track,
            source,
            self.studio.project_state()?,
            self.map.project_view(),
        );
        document.filename = self.filename.clone();
        document.name = self.name.clone();
        document.description = self.description.clone();
        document.source_path = self
            .source_path
            .as_deref()
            .map(std::path::absolute)
            .transpose()
            .map_err(|error| format!("Cannot resolve source GPX path: {error}"))?;
        document.selection = self.selection;
        document.chart = self.chart;
        document.show_original = self.show_original;
        document.trainer_mode = self.trainer_mode;
        document.studio_open = self.studio_open;
        Ok(document)
    }

    fn save_project(&mut self, ctx: &egui::Context, save_as: bool) {
        let document = match self.project_document() {
            Ok(document) => document,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        self.studio.pause();
        let path = if save_as {
            None
        } else {
            self.project_path.clone()
        };
        let suggested = self
            .project_path
            .as_ref()
            .and_then(|path| path.file_name())
            .map_or_else(
                || "ride.gpxproj".into(),
                |name| name.to_string_lossy().into_owned(),
            );
        let video = self.studio.video_path();
        self.status = "Saving editable project...".into();
        self.spawn(ctx, move || {
            let path = path.or_else(|| {
                rfd::FileDialog::new()
                    .add_filter("RLV Forge project", &["gpxproj"])
                    .set_file_name(suggested)
                    .save_file()
            });
            let Some(mut path) = path else {
                return Ok(Job::Cancelled);
            };
            if path.extension().is_none() {
                path.set_extension("gpxproj");
            }
            document.save(&path, video.as_deref())?;
            Ok(Job::ProjectSaved(
                std::path::absolute(path).map_err(|error| error.to_string())?,
            ))
        });
    }

    fn export(&mut self, ctx: &egui::Context, preview: bool) {
        let Some(track) = &self.track else { return };
        let result = if let Some(mapping) = self.studio.mapping() {
            self.cut_source
                .as_deref()
                .ok_or_else(|| "The uncut GPX is unavailable for synchronized export.".to_owned())
                .and_then(|source| {
                    mapping
                        .export(source, &self.name, &self.description)
                        .map(|edited| edited.gpx)
                })
        } else {
            track.export(&self.name, &self.description)
        };
        let xml = match result {
            Ok(xml) => xml,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        let filename = self.filename.clone();
        let source = self.source_path.clone();
        let video = self.studio.video_path();
        self.status = if preview {
            "Preparing GPX preview..."
        } else {
            "Saving GPX..."
        }
        .into();
        self.spawn(ctx, move || {
            if preview {
                return Ok(Job::Preview(xml));
            }
            let mut dialog = rfd::FileDialog::new()
                .add_filter("GPX tracks", &["gpx"])
                .set_file_name(&filename);
            if let Some(parent) = source.as_ref().and_then(|p| p.parent()) {
                dialog = dialog.set_directory(parent);
            }
            let Some(path) = dialog.save_file() else {
                return Ok(Job::Cancelled);
            };
            // The save dialog confirms overwrite; write only after the user chooses a destination.
            save_gpx(&path, &xml, video.as_deref())?;
            Ok(Job::Saved(path))
        });
    }

    fn auto_smooth(&mut self, ctx: &egui::Context) {
        let Some(track) = &self.track else {
            return;
        };
        let source = self.cut_source.clone().unwrap_or_else(|| track.clone());
        let mut track = (**track).clone();
        let selection = self.selection;
        let trainer = self.trainer_mode;
        self.status = "Automatically removing elevation spikes and short-distance noise...".into();
        self.spawn(ctx, move || {
            let before_track = track.clone();
            let before: Vec<_> = track.points.iter().map(|point| point.elevation).collect();
            track.apply(
                if trainer {
                    Operation::IncyclistSmooth
                } else {
                    Operation::AutoSmooth
                },
                selection,
            )?;
            let mut changed = 0;
            let mut max_shift: f64 = 0.0;
            for (point, before) in track.points.iter().zip(before) {
                let shift = (point.elevation - before).abs();
                if shift > 0.0 {
                    changed += 1;
                }
                max_shift = max_shift.max(shift);
            }
            let mut source = (*source).clone();
            source.retain_elevation_changes(&before_track, &track)?;
            Ok(Job::AutoSmoothed {
                track,
                source,
                changed,
                max_shift,
                trainer,
            })
        });
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.label("2. Smooth the data:");
        ui.horizontal_wrapped(|ui| {
            if ui.add(egui::Button::new(egui::RichText::new("Auto Smooth").color(Color32::WHITE)).fill(BLUE))
                .on_hover_text("Remove isolated spikes and short-distance elevation noise in the selected range. Uses distance-based filtering, preserves endpoints and sustained climbs, and applies no slope limits.")
                .clicked() {
                self.auto_smooth(ui.ctx());
            }
            ui.label("Remove spikes and noise; preserve the route.");
        });
        ui.checkbox(&mut self.trainer_mode, "Incyclist: soften grade transitions")
            .on_hover_text("Adds distance-weighted grade smoothing over a 25 m radius, retaining endpoint elevations and sustained climbs. No grade caps. Coordinates, timestamps and point count are unchanged.");
    }

    fn import_controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.label("1. Select a GPX file:");
        ui.horizontal_wrapped(|ui| {
            if ui.button("Open GPX...").clicked() {
                self.request(Pending::Open(None), ctx);
            }
            if let Some(path) = &self.source_path {
                ui.label(path.display().to_string());
            } else {
                ui.label("Or drop a .gpx file into this window.");
            }
        });
        if let Some(track) = &self.track {
            if track.missing_elevations > 0 {
                ui.colored_label(Color32::from_rgb(160, 90, 0), format!(
                    "{} missing elevations filled from the preceding point, or 100 m at a segment start.",
                    track.missing_elevations));
            }
        }
    }

    fn top_section(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let width = ui.available_width();
        let map_width = (width * 0.36).clamp(260.0, 420.0);
        let left_width = width - map_width - ui.spacing().item_spacing.x;
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                vec2(left_width, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(left_width);
                    self.import_controls(ui, ctx);
                    ui.add_space(8.0);
                    ui.add_enabled_ui(self.track.is_some(), |ui| self.controls(ui));
                },
            );
            ui.allocate_ui_with_layout(
                vec2(map_width, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(map_width);
                    self.map.show(
                        ui,
                        self.track.as_deref(),
                        self.selection,
                        self.hover_distance,
                    );
                },
            );
        });
    }

    fn charts(&mut self, ui: &mut egui::Ui) {
        self.hover_distance = None;
        ui.label("3. Check the resulting elevation and slope. Hover over the chart for details:");
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.chart, Chart::Elevation, "Elevation Chart");
            ui.selectable_value(&mut self.chart, Chart::Slope, "Slope Chart");
            ui.selectable_value(&mut self.chart, Chart::Profile, "Elevation Profile");
            ui.checkbox(&mut self.show_original, "Show original");
        });
        let (response, painter) =
            ui.allocate_painter(vec2(ui.available_width(), 285.0), Sense::click_and_drag());
        let plot = response.rect.shrink2(vec2(55.0, 24.0));
        if let Some(track) = &self.track {
            paint_chart(
                &painter,
                plot,
                track,
                &self.original_slopes,
                &self.modified_slopes,
                self.selection,
                self.chart,
                self.show_original,
                true,
            );
            if response.double_clicked() {
                self.selection = [0.0, track.total_distance()];
            } else if response.dragged() {
                let shift = -f64::from(ui.input(|i| i.pointer.delta().x) / plot.width())
                    * (self.selection[1] - self.selection[0]);
                self.selection = moved_range(self.selection, shift, track.total_distance());
            }
            if response.hovered() {
                let (scroll, shift) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers.shift));
                if (scroll.y != 0.0 || (shift && scroll.x != 0.0)) && track.total_distance() > 0.0 {
                    ui.input_mut(|input| {
                        input.smooth_scroll_delta = egui::Vec2::ZERO;
                    });
                    let fraction = response.hover_pos().map_or(0.5, |p| {
                        ((p.x - plot.left()) / plot.width()).clamp(0.0, 1.0)
                    });
                    self.selection = chart_wheel_range(
                        self.selection,
                        track.total_distance(),
                        scroll,
                        shift,
                        fraction,
                        plot.width(),
                    );
                }
                if let Some(pointer) = response.hover_pos().filter(|p| plot.contains(*p)) {
                    let distance = self.selection[0]
                        + f64::from((pointer.x - plot.left()) / plot.width())
                            * (self.selection[1] - self.selection[0]);
                    self.hover_distance = Some(distance);
                    let index = track
                        .points
                        .partition_point(|p| p.distance < distance)
                        .min(track.points.len() - 1);
                    let point = &track.points[index];
                    let elapsed = self
                        .studio
                        .elapsed_at(index)
                        .map_or(String::new(), |seconds| {
                            format!("\nCurrent GPX elapsed: {seconds:.3} s")
                        });
                    painter.line_segment(
                        [pos2(pointer.x, plot.top()), pos2(pointer.x, plot.bottom())],
                        Stroke::new(1.0_f32, Color32::GRAY),
                    );
                    response.clone().on_hover_text(format!(
                        "Cursor: {:.3} km\nSample: {:.3} km  |  point {}{elapsed}\nOriginal: {:.2} m  ({:.2} %)\nNew: {:.2} m  ({:.2} %)\nLat {:.6}, lon {:.6}",
                        distance / 1000.0, point.distance / 1000.0, index + 1, point.original, self.original_slopes[index],
                        point.elevation, self.modified_slopes[index], point.latitude, point.longitude,
                    ));
                }
            }
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(ORIGINAL, "Original");
                ui.colored_label(GREEN, "New");
                if self.chart == Chart::Profile {
                    slope_legend(ui);
                }
                let start = track
                    .points
                    .partition_point(|p| p.distance < self.selection[0]);
                let end = track
                    .points
                    .partition_point(|p| p.distance <= self.selection[1]);
                let count = (start..end)
                    .filter(|&i| track.points[i].step > 0.01)
                    .count();
                let average = if count == 0 {
                    0.0
                } else {
                    (start..end)
                        .filter(|&i| track.points[i].step > 0.01)
                        .map(|i| self.modified_slopes[i])
                        .sum::<f64>()
                        / count as f64
                };
                ui.label(format!("Average slope: {average:.2}%"));
            });
        } else {
            painter.rect_filled(plot, 2.0, Color32::from_rgb(242, 246, 248));
            painter.text(
                plot.center(),
                Align2::CENTER_CENTER,
                "Open a GPX file to see its elevation profile",
                FontId::proportional(18.0),
                BLUE,
            );
        }
        ui.add_space(6.0);
        ui.label("4. Drag the triangles below to select the portion to smooth. Scroll the main chart to zoom; Shift+scroll or drag to pan.");
        self.overview(ui);
    }

    fn overview(&mut self, ui: &mut egui::Ui) {
        let (response, painter) =
            ui.allocate_painter(vec2(ui.available_width(), 110.0), Sense::click_and_drag());
        let plot = response.rect.shrink2(vec2(55.0, 16.0));
        let Some(track) = &self.track else { return };
        let total = track.total_distance();
        paint_chart(
            &painter,
            plot,
            track,
            &self.original_slopes,
            &self.modified_slopes,
            [0.0, total],
            Chart::Elevation,
            self.show_original,
            false,
        );
        let x = |distance: f64| plot.left() + (distance / total.max(0.001)) as f32 * plot.width();
        let handles = [x(self.selection[0]), x(self.selection[1])];
        painter.rect_filled(
            Rect::from_min_max(
                pos2(handles[0], plot.top()),
                pos2(handles[1], plot.bottom()),
            ),
            0.0,
            BLUE.linear_multiply(0.12),
        );
        for handle in handles {
            painter.line_segment(
                [pos2(handle, plot.top()), pos2(handle, plot.bottom())],
                Stroke::new(2.0_f32, BLUE),
            );
            painter.add(Shape::convex_polygon(
                vec![
                    pos2(handle, plot.bottom() - 9.0),
                    pos2(handle - 8.0, plot.bottom() + 5.0),
                    pos2(handle + 8.0, plot.bottom() + 5.0),
                ],
                BLUE,
                Stroke::NONE,
            ));
        }
        if (response.drag_started() || response.clicked()) && total > 0.0 {
            if let Some(pos) = response.interact_pointer_pos() {
                self.handle = Some(
                    if (pos.x - handles[0]).abs() <= (pos.x - handles[1]).abs() {
                        0
                    } else {
                        1
                    },
                );
            }
        }
        if response.dragged() || response.clicked() {
            if let (Some(handle), Some(pos)) = (self.handle, response.interact_pointer_pos()) {
                let value =
                    f64::from(((pos.x - plot.left()) / plot.width()).clamp(0.0, 1.0)) * total;
                self.selection[handle] = if handle == 0 {
                    value.min(self.selection[1])
                } else {
                    value.max(self.selection[0])
                };
            }
        }
        if response.drag_stopped() {
            self.handle = None;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Selection:");
            let mut start = self.selection[0] / 1000.0;
            let mut end = self.selection[1] / 1000.0;
            let start_changed = ui
                .add(
                    egui::DragValue::new(&mut start)
                        .speed(0.01)
                        .range(0.0..=end)
                        .suffix(" km"),
                )
                .changed();
            ui.label("to");
            let end_changed = ui
                .add(
                    egui::DragValue::new(&mut end)
                        .speed(0.01)
                        .range(start..=total / 1000.0)
                        .suffix(" km"),
                )
                .changed();
            if start_changed || end_changed {
                self.selection = [
                    (start * 1000.0).clamp(0.0, total),
                    (end * 1000.0).clamp(0.0, total),
                ];
            }
            if ui.button("Full ride").clicked() {
                self.selection = [0.0, total];
            }
            ui.label(format!(
                "{} points | {:.2} km total",
                track.points.len(),
                total / 1000.0
            ));
        });
        if ui
            .add_enabled(
                !self.studio.has_cut_draft(),
                egui::Button::new("Cut selected GPX section & level elevation"),
            )
            .clicked()
        {
            if let Some(track) = self.track.clone() {
                let selection = self.selection;
                let start = track
                    .points
                    .partition_point(|point| point.distance < selection[0]);
                let start = self.studio.elapsed_at(start);
                if self.studio.mapping().is_some() {
                    let end = track
                        .points
                        .partition_point(|point| point.distance <= selection[1])
                        .min(track.points.len() - 1);
                    if let (Some(start), Some(end)) = (start, self.studio.elapsed_at(end)) {
                        self.cut_time(Clip { start, end }, ui.ctx());
                    } else {
                        self.error = Some("The selected range has no valid GPX timestamps.".into());
                    }
                    return;
                }
                self.status =
                    "Removing the selected GPX section and levelling the elevation join...".into();
                self.spawn(ui.ctx(), move || {
                    let track = track.cut_distance(selection)?;
                    Ok(Job::Cut { track, start })
                });
            }
        }
        ui.small("Cut removes the selected recorded samples, closes distance/time gaps, and levels the following elevations without changing their gradients.");
        if let Some(cuts) = self.studio.show_cuts(ui) {
            self.replace_cuts(cuts, ui.ctx());
        }
    }

    fn cut_time(&mut self, cut: Clip, ctx: &egui::Context) {
        if self.studio.mapping().is_some() {
            match self.studio.cuts_with_interval(cut) {
                Ok(cuts) => self.replace_cuts(cuts, ctx),
                Err(error) => self.error = Some(error),
            }
            return;
        }
        if let Some(track) = self.track.clone() {
            self.status =
                "Removing the marked GPX time interval and levelling the elevation join...".into();
            self.spawn(ctx, move || {
                let track = timeline::cut(&track, cut)?;
                Ok(Job::Cut {
                    track,
                    start: Some(cut.start),
                })
            });
        }
    }

    fn apply_cut(&mut self, result: Result<Track, String>, start: Option<f64>) {
        match result {
            Ok(track) => {
                if let Err(error) = self.studio.track_edited(&track, start, self.selection) {
                    self.error = Some(error);
                    return;
                }
                if self.cut_source.is_none() {
                    self.cut_source = self.track.clone();
                }
                self.install_cut_track(track);
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn replace_cuts(&mut self, cuts: Vec<CutRange>, ctx: &egui::Context) {
        let Some(source) = self.cut_source.clone() else {
            self.error = Some(
                "The uncut GPX is unavailable; reopen the source track before editing cuts.".into(),
            );
            return;
        };
        let mapping = match self.studio.plan_for_cuts(&cuts) {
            Ok(mapping) => mapping,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        self.status =
            "Rebuilding the GPX from original-clock cuts and levelling the joins...".into();
        self.spawn(ctx, move || {
            let track = match mapping {
                Some(mapping) => {
                    mapping.validate_source(&source)?;
                    timeline::cut_ranges(&source, mapping.cuts())?
                }
                None => timeline::rebuild_cuts(&source, &cuts)?,
            };
            Ok(Job::RebuiltCuts { track, cuts })
        });
    }

    fn apply_sync(&mut self, points: Vec<SyncPoint>, ctx: &egui::Context) {
        let Some(source) = self.cut_source.clone() else {
            self.error = Some("Load an original GPX before applying sync points.".into());
            return;
        };
        let mapping = match self.studio.plan_for_points(&points) {
            Ok(mapping) => mapping,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        let cuts = self.studio.cuts().to_vec();
        self.status = "Applying sync points and rebuilding levelled GPX jumps...".into();
        self.spawn(ctx, move || {
            let track = match mapping {
                Some(mapping) => {
                    mapping.validate_source(&source)?;
                    timeline::cut_ranges(&source, mapping.cuts())?
                }
                None => timeline::rebuild_cuts(&source, &cuts)?,
            };
            Ok(Job::Synced { track, points })
        });
    }

    fn install_cut_track(&mut self, track: Track) {
        self.selection = [0.0, track.total_distance()];
        self.original_slopes = track.slopes(true);
        self.modified_slopes = track.slopes(false);
        self.track = Some(Arc::new(track));
        self.preview = None;
        self.hover_distance = None;
        self.map.reset_view();
        self.dirty = true;
        self.error = None;
        self.status = "GPX cuts applied and elevation joins levelled. Following gradients preserved; video unchanged. Undo restores the previous cuts.".into();
    }

    fn edit_state(&self) -> EditState {
        EditState {
            track: self.track.clone(),
            cut_source: self.cut_source.clone(),
            filename: self.filename.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            source_path: self.source_path.clone(),
            selection: self.selection,
            chart: self.chart,
            show_original: self.show_original,
            trainer_mode: self.trainer_mode,
            studio_open: self.studio_open,
            studio: self.studio.settings(),
            map: self.map.view_state(),
            project_path: self.project_path.clone(),
        }
    }

    fn restore_state(&mut self, state: EditState) {
        let content_changed = self.track != state.track
            || self.name != state.name
            || self.description != state.description;
        self.track = state.track;
        self.cut_source = state.cut_source;
        self.filename = state.filename;
        self.name = state.name;
        self.description = state.description;
        self.source_path = state.source_path;
        self.project_path = state.project_path;
        self.selection = state.selection;
        self.chart = state.chart;
        self.show_original = state.show_original;
        self.trainer_mode = state.trainer_mode;
        self.studio_open = state.studio_open;
        self.studio.restore(state.studio, self.track.as_deref());
        self.map.restore_view(state.map);
        self.original_slopes = self
            .track
            .as_ref()
            .map_or_else(Vec::new, |track| track.slopes(true));
        self.modified_slopes = self
            .track
            .as_ref()
            .map_or_else(Vec::new, |track| track.slopes(false));
        self.preview = None;
        self.hover_distance = None;
        self.handle = None;
        self.error = None;
        self.dirty = self.track.is_some() && (self.dirty || content_changed);
    }

    fn undo_redo(&mut self, redo: bool) {
        let current = self.edit_state();
        let state = if redo {
            self.history.redo(current)
        } else {
            self.history.undo(current)
        };
        if let Some(state) = state {
            self.restore_state(state);
            self.status = if redo {
                "Action redone."
            } else {
                "Action undone."
            }
            .into();
        }
    }

    fn export_controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label("5. Try different smoothing:");
            if ui.button("Reset The Data").clicked() {
                self.request(Pending::Reset, ctx);
            }
        });
        ui.horizontal(|ui| {
            ui.label("6.");
            if ui
                .add(
                    egui::Button::new(egui::RichText::new("Export GPX").color(Color32::WHITE))
                        .fill(BLUE),
                )
                .clicked()
            {
                self.export(ctx, false);
            }
            ui.label(if self.dirty {
                "Unsaved changes"
            } else {
                "All processing stays on your computer."
            });
        });
        egui::Grid::new("export").num_columns(2).show(ui, |ui| {
            ui.label("File name:");
            ui.text_edit_singleline(&mut self.filename);
            ui.end_row();
            ui.label("GPX name:");
            let name_changed = ui
                .add(egui::TextEdit::singleline(&mut self.name).desired_width(550.0))
                .changed();
            ui.end_row();
            ui.label("Description:");
            let description_changed = ui
                .add(egui::TextEdit::singleline(&mut self.description).desired_width(550.0))
                .changed();
            ui.end_row();
            if name_changed || description_changed {
                self.dirty = true;
                self.preview = None;
            }
        });
        ui.add_space(5.0);
        ui.collapsing("7. Resulting GPX (optional text preview)", |ui| {
            if ui.button("Generate GPX preview").clicked() { self.export(ctx, true); }
            if let Some(xml) = &self.preview {
                ui.label(format!("{} bytes. Export GPX always writes the complete document.", xml.len()));
                if ui.button("Copy complete GPX").clicked() { ctx.copy_text(xml.clone()); }
                // Keep the editor responsive for multi-megabyte documents; copying/export is complete.
                let end = xml.char_indices().nth(20_000).map_or(xml.len(), |(i, _)| i);
                let mut text = &xml[..end];
                ui.add(egui::TextEdit::multiline(&mut text).font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY).desired_rows(12));
                if end < xml.len() { ui.label("Preview truncated to 20,000 characters; Copy complete GPX includes everything."); }
            }
        });
    }
}

impl eframe::App for RlvForge {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &root.ctx().clone();
        let mut before = self.edit_state();
        let mut history_action = false;
        if ctx.input(|input| input.pointer.any_pressed()) {
            self.history.end_group();
        }
        let was_busy = self.worker.is_some() || self.studio.busy();
        let previous_selection = self.selection;
        let previous_hover = self.hover_distance;
        let previous_video_position = self.studio.position();
        let project_clean = self.poll(ctx);
        self.studio.poll(ctx);
        if self.worker.is_none() && !self.studio.busy() {
            if let Some(video) = self.pending_video.take() {
                self.studio_open = true;
                self.studio.open_video(ctx, Some(video));
            }
        }
        if !self.studio_open {
            self.studio.pause();
        }
        if ctx.input(|i| i.viewport().close_requested())
            && (self.dirty
                || self.project_dirty
                || self.studio.dirty
                || self.worker.is_some()
                || self.studio.busy())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.worker.is_none() && !self.studio.busy() {
                self.confirm = Some(Pending::Close);
            }
        }
        if self.worker.is_none() && !self.studio.busy() && self.confirm.is_none() {
            let save = ctx.input_mut(|input| {
                let save_as = input.consume_key(
                    egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                    egui::Key::S,
                );
                let save = save_as || input.consume_key(egui::Modifiers::COMMAND, egui::Key::S);
                save.then_some(save_as)
            });
            if let Some(save_as) = save {
                self.save_project(ctx, save_as);
            }
        }
        if self.worker.is_none() && !self.studio.busy() && self.confirm.is_none() {
            let dropped = ctx.input(|i| {
                i.raw
                    .dropped_files
                    .first()
                    .map(|file| file.path().to_owned())
            });
            if let Some(path) = dropped {
                self.request(Pending::Open(Some(path)), ctx);
            }
        }
        if self.worker.is_none() && !self.studio.busy() && self.confirm.is_none() {
            let (undo, redo) = ctx.input_mut(|input| {
                let redo = input.consume_key(
                    egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                    egui::Key::Z,
                ) || input.consume_key(egui::Modifiers::COMMAND, egui::Key::Y);
                let undo = !redo && input.consume_key(egui::Modifiers::COMMAND, egui::Key::Z);
                (undo, redo)
            });
            if undo || redo {
                self.history.record(before, &self.edit_state(), None);
                self.undo_redo(redo);
                before = self.edit_state();
                history_action = true;
                ctx.request_repaint();
            }
        }
        egui::Panel::top("header").show(root, |ui| {
            ui.add_space(8.0);
            ui.heading(egui::RichText::new("RLV Forge").size(28.0).color(BLUE));
            ui.add_space(6.0);
            ui.add_enabled_ui(
                self.worker.is_none() && !self.studio.busy() && self.confirm.is_none(),
                |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Open project...").clicked() {
                            self.request(Pending::OpenProject, ctx);
                        }
                        if ui
                            .add_enabled(self.track.is_some(), egui::Button::new("Save project"))
                            .clicked()
                        {
                            self.save_project(ctx, false);
                        }
                        if ui
                            .add_enabled(
                                self.track.is_some(),
                                egui::Button::new("Save project as..."),
                            )
                            .clicked()
                        {
                            self.save_project(ctx, true);
                        }
                        if ui
                            .add_enabled(self.history.can_undo(), egui::Button::new("Undo"))
                            .clicked()
                        {
                            self.undo_redo(false);
                            history_action = true;
                        }
                        if ui
                            .add_enabled(self.history.can_redo(), egui::Button::new("Redo"))
                            .clicked()
                        {
                            self.undo_redo(true);
                            history_action = true;
                        }
                        ui.selectable_value(&mut self.studio_open, false, "GPX smoothing");
                        ui.selectable_value(&mut self.studio_open, true, "Video & Incyclist");
                    });
                    if self.track.is_some() {
                        let name = self
                            .project_path
                            .as_ref()
                            .and_then(|path| path.file_name())
                            .map_or_else(|| "Not saved yet".into(), |name| name.to_string_lossy());
                        let unsaved = if self.project_dirty || self.dirty || self.studio.dirty {
                            " (unsaved changes)"
                        } else {
                            ""
                        };
                        let label = format!("Project: {name}{unsaved}");
                        ui.add(egui::Label::new(egui::RichText::new(&label).small()).truncate())
                            .on_hover_text(label);
                    }
                },
            );
        });
        egui::Panel::bottom("status").show(root, |ui| {
            ui.horizontal_wrapped(|ui| {
                if self.worker.is_some() || self.studio.busy() {
                    ui.spinner();
                }
                ui.label(&self.status);
            });
            if let Some(error) = &self.error {
                ui.colored_label(Color32::from_rgb(180, 35, 35), error);
            }
        });
        egui::CentralPanel::default().show(root, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("page")
                .scroll_bar_visibility(if self.studio_open {
                    egui::scroll_area::ScrollBarVisibility::AlwaysVisible
                } else {
                    egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded
                })
                .show(ui, |ui| {
                    ui.add_enabled_ui(self.worker.is_none() && self.confirm.is_none(), |ui| {
                        if self.studio_open {
                            ui.add_enabled_ui(!self.studio.busy(), |ui| {
                                self.import_controls(ui, ctx)
                            });
                            let cut = self.studio.show(
                                ui,
                                crate::studio::Tracks {
                                    current: self.track.as_ref(),
                                    source: self.cut_source.as_ref(),
                                },
                                &self.name,
                                &self.description,
                                &mut self.map,
                            );
                            if let Some(edit) = cut {
                                match edit {
                                    crate::studio::Edit::Cut(cut) => self.cut_time(cut, ctx),
                                    crate::studio::Edit::ReplaceCuts(cuts) => {
                                        self.replace_cuts(cuts, ctx)
                                    }
                                    crate::studio::Edit::Sync(points) => {
                                        self.apply_sync(points, ctx)
                                    }
                                }
                            }
                        } else {
                            self.top_section(ui, ctx);
                            ui.add_space(10.0);
                            self.charts(ui);
                            ui.add_space(10.0);
                            ui.add_enabled_ui(self.track.is_some(), |ui| {
                                self.export_controls(ui, ctx)
                            });
                        }
                    });
                });
        });
        if self.confirm.is_some() {
            egui::Window::new("Discard unsaved changes?")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label("Save a project to resume later with GPX edits, cuts, sync points and video alignment. GPX/bundle export alone does not save an editable project. Reset affects elevations only and can be undone.");
                    ui.add_enabled_ui(self.worker.is_none() && !self.studio.busy(), |ui| { ui.horizontal(|ui| {
                        if ui.add_enabled(self.track.is_some(), egui::Button::new("Save project and continue")).clicked() {
                            self.save_project(ctx, false);
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm = None;
                        }
                        if ui.button("Discard changes").clicked() {
                            if let Some(pending) = self.confirm.take() {
                                // Clear the dirty flag before allowing the native window to close.
                                if matches!(pending, Pending::Close) {
                                    self.dirty = false;
                                    self.studio.dirty = false;
                                    self.project_dirty = false;
                                }
                                self.perform(pending, ctx);
                            }
                        }
                    }); });
                });
        }
        if previous_selection != self.selection
            || previous_hover != self.hover_distance
            || before.track != self.track
        {
            // The map is drawn above the charts, so redraw it with the updated range/cursor.
            ctx.request_repaint();
        }
        let project_changed =
            self.edit_state() != before || self.studio.position() != previous_video_position;
        if !history_action {
            let after = self.edit_state();
            let focused = ctx.memory(|memory| memory.focused());
            let group = if before.track != after.track || was_busy {
                None
            } else if ctx.input(|input| input.pointer.any_down()) {
                Some(focused.map_or(0, |id| id.value()))
            } else if ctx.egui_wants_keyboard_input() {
                focused.map(|id| id.value())
            } else {
                None
            };
            self.history.record(before, &after, group);
        }
        if self.track.is_some()
            && (self.dirty || self.studio.dirty || (!project_clean && project_changed))
        {
            self.project_dirty = true;
        }
    }
}

fn save_gpx(path: &Path, xml: &str, video: Option<&Path>) -> Result<(), String> {
    use std::io::Write;
    if let Some(video) = video {
        let video = video
            .canonicalize()
            .map_err(|error| format!("Cannot verify the prepared video's location: {error}"))?;
        if path.canonicalize().is_ok_and(|path| path == video) {
            return Err(
                "Cannot overwrite the prepared video with GPX. Choose a different destination."
                    .into(),
            );
        }
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut staged = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("Cannot stage GPX in {}: {error}", parent.display()))?;
    staged
        .write_all(xml.as_bytes())
        .and_then(|()| staged.as_file().sync_all())
        .map_err(|error| format!("Cannot save {}: {error}", path.display()))?;
    // Replace the destination rather than modifying an inode that may alias a video.
    staged
        .persist(path)
        .map_err(|error| format!("Cannot publish {}: {error}", path.display()))?;
    Ok(())
}

fn moved_range(range: [f64; 2], shift: f64, total: f64) -> [f64; 2] {
    let width = (range[1] - range[0]).clamp(0.0, total);
    let start = (range[0] + shift).clamp(0.0, total - width);
    [start, start + width]
}

fn chart_wheel_range(
    range: [f64; 2],
    total: f64,
    scroll: egui::Vec2,
    shift: bool,
    fraction: f32,
    plot_width: f32,
) -> [f64; 2] {
    if total == 0.0 {
        return range;
    }
    if shift {
        // egui/platforms may translate Shift+vertical wheel into a horizontal delta.
        let delta = if scroll.x.abs() > scroll.y.abs() {
            scroll.x
        } else {
            scroll.y
        };
        return moved_range(
            range,
            -f64::from(delta) / f64::from(plot_width) * (range[1] - range[0]),
            total,
        );
    }
    let width = (range[1] - range[0]).max(0.001);
    let new_width = (width * (-f64::from(scroll.y) * 0.003).exp()).clamp(total.min(1.0), total);
    let left = range[0] + (width - new_width) * f64::from(fraction);
    moved_range([left, left + new_width], 0.0, total)
}

fn slope_color(slope: f64) -> Color32 {
    if slope < -3.0 {
        Color32::from_rgb(40, 110, 200)
    } else if slope < 3.0 {
        GREEN
    } else if slope < 8.0 {
        Color32::from_rgb(215, 150, 20)
    } else {
        Color32::from_rgb(200, 55, 45)
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_chart(
    painter: &egui::Painter,
    plot: Rect,
    track: &Track,
    original_slopes: &[f64],
    modified_slopes: &[f64],
    selection: [f64; 2],
    chart: Chart,
    show_original: bool,
    axes: bool,
) {
    painter.rect_filled(plot, 0.0, Color32::WHITE);
    // Include neighbouring samples so narrow, moving windows still interpolate the profile.
    let start = track
        .points
        .partition_point(|p| p.distance < selection[0])
        .saturating_sub(1);
    let end =
        (track.points.partition_point(|p| p.distance <= selection[1]) + 1).min(track.points.len());
    let value = |i: usize, original: bool| -> f64 {
        if chart == Chart::Slope {
            if original {
                original_slopes[i]
            } else {
                modified_slopes[i]
            }
        } else if original {
            track.points[i].original
        } else {
            track.points[i].elevation
        }
    };
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for i in start..end {
        for original in [false, true] {
            if original && !show_original {
                continue;
            }
            let y = value(i, original);
            min = min.min(y);
            max = max.max(y);
        }
    }
    if start == end {
        min = 0.0;
        max = 1.0;
    }
    let pad = ((max - min) * 0.08).max(1.0);
    min -= pad;
    max += pad;
    let position = |i: usize, original: bool| -> Pos2 {
        let p = &track.points[i];
        let x = ((p.distance - selection[0]) / (selection[1] - selection[0]).max(0.001)) as f32;
        let y = ((value(i, original) - min) / (max - min)) as f32;
        pos2(
            plot.left() + x * plot.width(),
            plot.bottom() - y * plot.height(),
        )
    };
    if axes {
        for tick in 0..=4 {
            let t = tick as f32 / 4.0;
            let y = plot.bottom() - t * plot.height();
            painter.line_segment(
                [pos2(plot.left(), y), pos2(plot.right(), y)],
                Stroke::new(1.0_f32, Color32::from_gray(230)),
            );
            painter.text(
                pos2(plot.left() - 7.0, y),
                Align2::RIGHT_CENTER,
                format!("{:.0}", min + f64::from(t) * (max - min)),
                FontId::proportional(12.0),
                INK,
            );
            let x = plot.left() + t * plot.width();
            painter.text(
                pos2(x, plot.bottom() + 8.0),
                Align2::CENTER_TOP,
                format!(
                    "{:.1}",
                    (selection[0] + f64::from(t) * (selection[1] - selection[0])) / 1000.0
                ),
                FontId::proportional(12.0),
                INK,
            );
        }
        painter.text(
            pos2(plot.left(), plot.top() - 5.0),
            Align2::LEFT_BOTTOM,
            if chart == Chart::Slope {
                "Slope (%)"
            } else {
                "Elevation (m)"
            },
            FontId::proportional(12.0),
            INK,
        );
        painter.text(
            pos2(plot.right(), plot.top() - 5.0),
            Align2::RIGHT_BOTTOM,
            "Distance (km)",
            FontId::proportional(12.0),
            INK,
        );
    }
    painter.rect_stroke(
        plot,
        0.0,
        Stroke::new(1.0_f32, Color32::from_gray(180)),
        StrokeKind::Inside,
    );
    let clipped = painter.with_clip_rect(plot);
    // Bound drawing work to the viewport; calculations and exports always use every point.
    let stride = ((end - start) / (plot.width().max(1.0) as usize * 2).max(1)).max(1);
    for original in [true, false] {
        if original && !show_original {
            continue;
        }
        let mut line = Vec::new();
        let mut previous: Option<usize> = None;
        for (i, slope) in modified_slopes.iter().enumerate().take(end).skip(start) {
            let segment_start =
                i == start || track.points[i - 1].segment != track.points[i].segment;
            let segment_end =
                i + 1 == end || track.points[i + 1].segment != track.points[i].segment;
            if !segment_start && !segment_end && (i - start) % stride != 0 {
                continue;
            }
            if segment_start {
                if line.len() == 1 {
                    clipped.circle_filled(line[0], 2.5, if original { ORIGINAL } else { GREEN });
                    line.clear();
                } else if !line.is_empty() {
                    clipped.add(Shape::line(
                        std::mem::take(&mut line),
                        Stroke::new(1.5_f32, if original { ORIGINAL } else { GREEN }),
                    ));
                }
                previous = None;
            }
            let pos = position(i, original);
            if chart == Chart::Profile && !original {
                if let Some(prev) = previous {
                    let prev_pos = position(prev, false);
                    let color = slope_color(*slope);
                    clipped.add(Shape::convex_polygon(
                        vec![
                            pos2(prev_pos.x, plot.bottom()),
                            prev_pos,
                            pos,
                            pos2(pos.x, plot.bottom()),
                        ],
                        color.linear_multiply(0.18),
                        Stroke::NONE,
                    ));
                    clipped.line_segment([prev_pos, pos], Stroke::new(2.0_f32, color));
                } else {
                    clipped.circle_filled(pos, 2.5, slope_color(*slope));
                }
            } else {
                line.push(pos);
            }
            previous = Some(i);
        }
        if line.len() == 1 {
            clipped.circle_filled(line[0], 2.5, if original { ORIGINAL } else { GREEN });
        }
        if line.len() >= 2 {
            clipped.add(Shape::line(
                line,
                Stroke::new(1.5_f32, if original { ORIGINAL } else { GREEN }),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_clicks_scrub_and_drags_pan_without_accumulating_idle_motion() {
        let track = Arc::new(Track::parse("<gpx><rte><rtept lat=\"0\" lon=\"0\"><ele>100</ele></rtept><rtept lat=\"0\" lon=\"0.01\"><ele>150</ele></rtept></rte></gpx>".into()).unwrap());
        let ctx = egui::Context::default();
        let mut profile = LinkedProfile::default();
        let mut draw = |center, events| {
            let output = crate::test_ui(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 400.0))),
                    events,
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        let _ = profile.show(ui, Some(&track), Some(center), 1000.0);
                    });
                },
            );
            let plot = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    Shape::Rect(rect) if rect.fill == Color32::WHITE => Some(rect.rect),
                    _ => None,
                })
                .unwrap();
            (plot, profile.take_seek())
        };
        let click = |pos, pressed| {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    pressed,
                    button: egui::PointerButton::Primary,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        let (plot, seek) = draw(500.0, vec![]);
        assert!(seek.is_none());
        let point = pos2(plot.left() + plot.width() * 0.75, plot.center().y);
        assert!(draw(500.0, click(point, true)).1.is_none());
        let center = draw(500.0, click(point, false)).1.unwrap();
        assert!((center - 750.0).abs() < 1e-6);
        let origin = plot.center();
        draw(center, click(origin, true));
        let moved = origin - vec2(80.0, 0.0);
        let next = draw(center, vec![egui::Event::PointerMoved(moved)])
            .1
            .unwrap();
        assert!((next - center - 80.0 / f64::from(plot.width()) * 1000.0).abs() < 1e-4);
        assert!(draw(next, vec![]).1.is_none());
        let point = moved + vec2(40.0, 0.0);
        let previous = draw(next, vec![egui::Event::PointerMoved(point)])
            .1
            .unwrap();
        assert!((previous - next + 40.0 / f64::from(plot.width()) * 1000.0).abs() < 1e-4);
        draw(previous, click(point, false));
        draw(500.0, click(origin, true));
        let end = draw(
            500.0,
            vec![egui::Event::PointerMoved(origin - vec2(10_000.0, 0.0))],
        )
        .1
        .unwrap();
        assert_eq!(end, track.total_distance());
    }

    #[test]
    fn profile_wheel_navigation_preserves_page_scrolling_and_respects_disabled_states() {
        let track = Arc::new(Track::parse("<gpx><rte><rtept lat=\"0\" lon=\"0\"><ele>100</ele></rtept><rtept lat=\"0\" lon=\"0.01\"><ele>150</ele></rtept></rte></gpx>".into()).unwrap());
        for (shift, wheel, enabled, distance, should_seek) in [
            (false, vec2(40.0, 0.0), true, Some(500.0), true),
            (true, vec2(0.0, 40.0), true, Some(500.0), true),
            (false, vec2(0.0, 40.0), true, Some(500.0), false),
            (false, vec2(40.0, 0.0), false, Some(500.0), false),
            (false, vec2(40.0, 0.0), true, None, false),
        ] {
            let ctx = egui::Context::default();
            let mut profile = LinkedProfile::default();
            let modifiers = egui::Modifiers {
                shift,
                ..Default::default()
            };
            let mut remaining = egui::Vec2::ZERO;
            let mut draw = |events| {
                crate::test_ui(
                    &ctx,
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 400.0))),
                        events,
                        ..Default::default()
                    },
                    |root| {
                        egui::CentralPanel::default().show(root, |ui| {
                            ui.add_enabled_ui(enabled, |ui| {
                                let _ = profile.show(ui, Some(&track), distance, 1000.0);
                            });
                            remaining = ui.input(|input| input.smooth_scroll_delta);
                        });
                    },
                )
            };
            draw(vec![egui::Event::PointerMoved(pos2(400.0, 100.0))]);
            draw(vec![
                egui::Event::ModifiersChanged(modifiers),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: wheel,
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Unknown,
                    modifiers,
                },
            ]);
            let seek = profile.take_seek();
            assert_eq!(seek.is_some(), should_seek);
            if let Some(seek) = seek {
                assert!(seek < 500.0);
                assert_eq!(remaining, egui::Vec2::ZERO);
            } else {
                assert_ne!(remaining, egui::Vec2::ZERO);
            }
        }
    }

    #[test]
    fn linked_profile_uses_shared_colours_interpolates_small_windows_and_keeps_the_cursor_centered()
    {
        fn draw(
            profile: &mut LinkedProfile,
            ctx: &egui::Context,
            track: Option<&Arc<Track>>,
            distance: Option<f64>,
            span: f64,
        ) -> (egui::FullOutput, Option<[f64; 2]>) {
            let mut range = None;
            let output = crate::test_ui(
                ctx,
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 400.0))),
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        range = profile.show(ui, track, distance, span);
                    });
                },
            );
            (output, range)
        }
        let track = Arc::new(Track::parse("<gpx><rte><rtept lat=\"0\" lon=\"0\"><ele>100</ele></rtept><rtept lat=\"0\" lon=\"0.01\"><ele>150</ele></rtept></rte></gpx>".into()).unwrap());
        let ctx = egui::Context::default();
        let mut profile = LinkedProfile::default();
        for distance in [0.0, 500.0, track.total_distance()] {
            let (output, range) = draw(&mut profile, &ctx, Some(&track), Some(distance), 1.0);
            let range = range.unwrap();
            assert_eq!((range[0] + range[1]) / 2.0, distance);
            assert_eq!(range[1] - range[0], 1.0);
            let plot = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    Shape::Rect(rect) if rect.fill == Color32::WHITE => Some(rect.rect),
                    _ => None,
                })
                .unwrap();
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                Shape::LineSegment { points, stroke } if stroke.color == Color32::from_rgb(200, 55, 45)
                    && points.iter().all(|point| (point.x - plot.center().x).abs() < 0.001))));
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                Shape::Path(path) if path.fill == slope_color(track.slopes(false)[1]).linear_multiply(0.18))));
            assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape,
                Shape::Path(path) if path.stroke.color == egui::epaint::ColorMode::Solid(ORIGINAL))));
            assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape,
                Shape::Text(text) if text.galley.job.text == "Original")));
            let index = track
                .points
                .partition_point(|point| point.distance < distance)
                .min(track.points.len() - 1);
            let expected = format!("{:+.1}%", track.slopes(false)[index]);
            let label = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    Shape::Text(text) if text.galley.job.text == expected => Some(text),
                    _ => None,
                })
                .unwrap();
            assert!((label.pos.x + label.galley.size().x / 2.0 - plot.center().x).abs() < 0.01);
            assert!(label.pos.y + label.galley.size().y < plot.top());
        }
        let cached = profile.modified_slopes.as_ptr();
        draw(&mut profile, &ctx, Some(&track), Some(200.0), 100.0);
        assert_eq!(profile.modified_slopes.as_ptr(), cached);
        let mut changed = (*track).clone();
        changed.points[1].elevation += 20.0;
        let changed = Arc::new(changed);
        draw(&mut profile, &ctx, Some(&changed), Some(200.0), 100.0);
        assert!(Arc::ptr_eq(profile.track.as_ref().unwrap(), &changed));
        assert_eq!(profile.modified_slopes, changed.slopes(false));
        for elevation in [100.0, 50.0] {
            let mut changed = (*track).clone();
            changed.points[1].elevation = elevation;
            let changed = Arc::new(changed);
            let (output, _) = draw(&mut profile, &ctx, Some(&changed), Some(500.0), 100.0);
            let expected = format!("{:+.1}%", changed.slopes(false)[1]);
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                Shape::Text(text) if text.galley.job.text == expected)));
        }
        let (_, range) = draw(&mut profile, &ctx, Some(&track), None, 100.0);
        assert!(range.is_none());
    }

    #[test]
    fn opening_and_saving_a_project_restores_work_and_failed_loads_preserve_it() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.gpx");
        std::fs::write(&source, "<gpx><rte><rtept lat=\"0\" lon=\"0\"><ele>100</ele></rtept><rtept lat=\"0\" lon=\"0.01\"><ele>110</ele></rtept></rte></gpx>").unwrap();
        let ctx = egui::Context::default();
        let deliver = |app: &mut RlvForge, result| {
            let (sender, receiver) = mpsc::channel();
            sender.send(result).unwrap();
            app.worker = Some(receiver);
            app.poll(&ctx);
        };
        let mut first = RlvForge::empty(None);
        deliver(
            &mut first,
            Ok(Job::Loaded {
                track: Track::read(&source).unwrap(),
                path: source.clone(),
            }),
        );
        first.name = "Saved ride".into();
        first.description = "My edits".into();
        first.trainer_mode = true;
        first.studio_open = true;
        let path = directory.path().join("work.gpxproj");
        first.project_document().unwrap().save(&path, None).unwrap();
        let mut restarted = RlvForge::empty(None);
        deliver(
            &mut restarted,
            Ok(Job::ProjectLoaded {
                project: Box::new(crate::project::Project::load(&path).unwrap()),
                path: path.clone(),
            }),
        );
        assert_eq!(restarted.name, first.name);
        assert_eq!(restarted.description, first.description);
        assert_eq!(restarted.track, first.track);
        assert_eq!(restarted.cut_source, first.cut_source);
        assert_eq!(restarted.project_path, Some(path.clone()));
        assert!(restarted.trainer_mode && restarted.studio_open);
        assert!(!restarted.dirty && !restarted.project_dirty && !restarted.studio.dirty);
        let before = restarted.edit_state();
        deliver(&mut restarted, Err("Invalid project".into()));
        assert!(restarted.edit_state() == before);
        assert!(restarted
            .error
            .as_ref()
            .unwrap()
            .contains("Invalid project"));
        restarted.name = "Another edit".into();
        restarted.dirty = true;
        restarted.project_dirty = true;
        restarted.confirm = Some(Pending::Close);
        restarted.save_project(&ctx, false);
        let result = restarted
            .worker
            .take()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        deliver(&mut restarted, result);
        assert!(restarted.confirm.is_none());
        assert!(!restarted.dirty && !restarted.project_dirty && !restarted.studio.dirty);
        assert_eq!(
            crate::project::Project::load(&path).unwrap().document.name,
            "Another edit"
        );
    }

    #[test]
    fn gpx_export_cannot_overwrite_the_prepared_video_even_through_aliases() {
        let directory = tempfile::tempdir().unwrap();
        let video = directory.path().join("prepared.mp4");
        std::fs::write(&video, b"prepared video bytes").unwrap();
        assert!(save_gpx(&video, "<gpx/>", Some(&video)).is_err());
        let alias = directory.path().join("alias.gpx");
        std::fs::hard_link(&video, &alias).unwrap();
        save_gpx(&alias, "<gpx/>", Some(&video)).unwrap();
        assert_eq!(std::fs::read(&video).unwrap(), b"prepared video bytes");
        assert_eq!(std::fs::read(&alias).unwrap(), b"<gpx/>");
    }

    #[test]
    fn undo_redo_restores_cut_smoothing_reset_and_file_replacement() {
        let points: String = (0..=10).map(|i| format!(
            "<trkpt lat=\"0\" lon=\"{}\"><ele>{}</ele><time>2026-01-01T00:00:{i:02}Z</time></trkpt>",
            i as f64 * 0.0001, 100 + i,
        )).collect();
        let track =
            Track::parse(format!("<gpx><trk><trkseg>{points}</trkseg></trk></gpx>")).unwrap();
        let mut app = RlvForge::empty(None);
        let empty = app.edit_state();
        app.studio.set_track(&track);
        app.selection = [0.0, track.total_distance()];
        app.track = Some(Arc::new(track));
        app.history.record(empty.clone(), &app.edit_state(), None);
        let loaded = app.edit_state();
        let mut smoothed = (**app.track.as_ref().unwrap()).clone();
        smoothed.points[8].elevation += 0.5;
        app.track = Some(Arc::new(smoothed));
        app.history.record(loaded.clone(), &app.edit_state(), None);
        let smoothed = app.edit_state();
        let cut = Clip {
            start: 3.0,
            end: 7.0,
        };
        app.apply_cut(
            timeline::cut(app.track.as_ref().unwrap(), cut),
            Some(cut.start),
        );
        app.history
            .record(smoothed.clone(), &app.edit_state(), None);
        let cut_state = app.edit_state();
        app.perform(Pending::Reset, &egui::Context::default());
        app.history
            .record(cut_state.clone(), &app.edit_state(), None);
        let reset = app.edit_state();
        app.undo_redo(false);
        assert!(app.edit_state() == cut_state);
        app.undo_redo(false);
        assert!(app.edit_state() == smoothed);
        app.undo_redo(false);
        assert!(app.edit_state() == loaded);
        app.undo_redo(false);
        assert!(app.edit_state() == empty);
        for expected in [loaded, smoothed, cut_state, reset] {
            app.undo_redo(true);
            assert!(app.edit_state() == expected);
            let track = app.track.as_ref().unwrap();
            assert_eq!(app.modified_slopes, track.slopes(false));
            assert_eq!(app.original_slopes, track.slopes(true));
        }
    }

    #[test]
    fn metadata_and_view_options_share_history_without_marking_view_changes_dirty() {
        let mut app = RlvForge::empty(None);
        let before = app.edit_state();
        app.name = "Edited title".into();
        app.filename = "new-name.gpx".into();
        app.description = "Description".into();
        app.selection = [10.0, 20.0];
        app.chart = Chart::Slope;
        app.show_original = false;
        app.trainer_mode = true;
        app.studio_open = true;
        app.history.record(before.clone(), &app.edit_state(), None);
        let after = app.edit_state();
        app.undo_redo(false);
        assert!(app.edit_state() == before);
        app.undo_redo(true);
        assert!(app.edit_state() == after);
        assert!(!app.dirty);
    }

    #[test]
    fn panning_preserves_width_and_clamps_to_ride() {
        assert_eq!(moved_range([20.0, 40.0], -100.0, 100.0), [0.0, 20.0]);
        assert_eq!(moved_range([20.0, 40.0], 100.0, 100.0), [80.0, 100.0]);
        assert_eq!(moved_range([0.0, 0.0], 10.0, 0.0), [0.0, 0.0]);
    }

    #[test]
    fn shift_wheel_pans_in_both_directions_without_changing_zoom() {
        for delta in [vec2(0.0, 50.0), vec2(50.0, 0.0)] {
            let left = chart_wheel_range([200.0, 400.0], 1000.0, delta, true, 0.5, 1000.0);
            let right = chart_wheel_range([200.0, 400.0], 1000.0, -delta, true, 0.5, 1000.0);
            assert_eq!(left, [190.0, 390.0]);
            assert_eq!(right, [210.0, 410.0]);
            for range in [left, right] {
                assert_eq!(range[1] - range[0], 200.0);
            }
        }
        assert_eq!(
            chart_wheel_range([0.0, 200.0], 1000.0, vec2(0.0, 500.0), true, 0.5, 1000.0),
            [0.0, 200.0]
        );
        assert_eq!(
            chart_wheel_range(
                [800.0, 1000.0],
                1000.0,
                vec2(0.0, -500.0),
                true,
                0.5,
                1000.0
            ),
            [800.0, 1000.0]
        );
        assert_eq!(
            chart_wheel_range([0.0, 1000.0], 1000.0, vec2(50.0, 0.0), true, 0.5, 1000.0),
            [0.0, 1000.0]
        );
    }

    #[test]
    fn ordinary_wheel_keeps_zooming_and_stationary_tracks_are_safe() {
        let zoomed = chart_wheel_range([200.0, 400.0], 1000.0, vec2(0.0, 50.0), false, 0.5, 1000.0);
        assert!(zoomed[1] - zoomed[0] < 200.0);
        assert!(((zoomed[0] + zoomed[1]) / 2.0 - 300.0).abs() < 1e-9);
        assert_eq!(
            chart_wheel_range([0.0, 0.0], 0.0, vec2(0.0, 50.0), true, 0.5, 1000.0),
            [0.0, 0.0]
        );
        assert_eq!(
            chart_wheel_range([0.0, 0.0], 0.0, vec2(0.0, 50.0), false, 0.5, 1000.0),
            [0.0, 0.0]
        );
    }

    #[test]
    fn normalized_egui_shift_wheel_input_preserves_the_chart_width() {
        let ctx = egui::Context::default();
        let modifiers = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let mut range = [200.0, 400.0];
        let _ = crate::test_ui(
            &ctx,
            egui::RawInput {
                events: vec![
                    egui::Event::ModifiersChanged(modifiers),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: vec2(0.0, 50.0),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Unknown,
                        modifiers,
                    },
                ],
                ..Default::default()
            },
            |ctx| {
                let (scroll, shift) =
                    ctx.input(|input| (input.smooth_scroll_delta, input.modifiers.shift));
                assert!(shift);
                assert!(scroll != egui::Vec2::ZERO);
                range = chart_wheel_range(range, 1000.0, scroll, shift, 0.5, 1000.0);
            },
        );
        assert!(range[0] < 200.0);
        assert!((range[1] - range[0] - 200.0).abs() < 1e-9);
    }
}
