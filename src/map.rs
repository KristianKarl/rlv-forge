use eframe::egui::{self, vec2, Color32, Shape, Stroke, Vec2};
use rlv_forge::gpx::{Point, Track};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use walkers::{
    lon_lat, sources::OpenStreetMap, HttpOptions, HttpTiles, Map, MapMemory, Plugin, Position,
    Projector, TileId, TilePiece, Tiles,
};

const ROUTE: Color32 = Color32::from_rgb(140, 150, 160);
const SELECTED: Color32 = Color32::from_rgb(0, 110, 220);
const RED_MARKER: Color32 = Color32::from_rgb(200, 55, 45);
const MERCATOR_LIMIT: f64 = 85.051_128_78;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ProjectView {
    center: Option<[f64; 2]>,
    zoom: f64,
    online: bool,
}

#[derive(Clone, Debug)]
pub struct RoutePick {
    pub left: usize,
    pub right: usize,
    pub fraction: f64,
}

pub struct SyncMarker {
    pub position: [f64; 2],
    pub label: String,
}

#[derive(Default)]
pub struct SyncOverlay {
    pub viewport: Option<egui::Vec2>,
    pub markers: Vec<SyncMarker>,
    pub picking: bool,
    pub hint: Option<usize>,
}

enum Cursor {
    Distance(f64),
    Position([f64; 2]),
}

struct SyncInteraction {
    overlay: SyncOverlay,
    picked: Rc<RefCell<Option<Vec<RoutePick>>>>,
}

pub struct TrackMap {
    reference_track: Option<Arc<Track>>,
    memory: MapMemory,
    tiles: Option<HttpTiles>,
    online: bool,
    auto_fit: bool,
    fit_selection: bool,
    last_selection: Option<[f64; 2]>,
    last_size: Vec2,
    waiting_since: Option<Instant>,
    error: Option<String>,
    revision: u64,
    last_cursor: Option<Position>,
}

#[derive(Clone)]
pub struct ViewState {
    reference_track: Option<Arc<Track>>,
    memory: MapMemory,
    online: bool,
    auto_fit: bool,
    fit_selection: bool,
    last_selection: Option<[f64; 2]>,
    last_size: Vec2,
    revision: u64,
}

impl PartialEq for ViewState {
    fn eq(&self, other: &Self) -> bool {
        self.revision == other.revision
            && self.online == other.online
            && self.reference_track == other.reference_track
    }
}

impl Default for TrackMap {
    fn default() -> Self {
        Self {
            reference_track: None,
            memory: MapMemory::default(),
            tiles: None,
            online: true,
            auto_fit: true,
            fit_selection: true,
            last_selection: None,
            last_size: Vec2::ZERO,
            waiting_since: None,
            error: None,
            revision: 0,
            last_cursor: None,
        }
    }
}

impl TrackMap {
    pub fn ground_span(&self, width: f32, latitude: f64) -> f64 {
        let center = self
            .memory
            .detached()
            .unwrap_or_else(|| map_position(0.0, latitude));
        let projector = Projector::new(
            egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(width, 1.0)),
            &self.memory,
            center,
        );
        f64::from(width / projector.scale_pixel_per_meter(center))
    }

    pub fn project_view(&self) -> ProjectView {
        ProjectView {
            center: self.memory.detached().map(|position| {
                [
                    (position.x() + 180.0).rem_euclid(360.0) - 180.0,
                    position.y().clamp(-MERCATOR_LIMIT, MERCATOR_LIMIT),
                ]
            }),
            zoom: self.memory.zoom(),
            online: self.online,
        }
    }

    pub fn from_project(
        view: &ProjectView,
        source: Arc<Track>,
        selection: [f64; 2],
    ) -> Result<Self, String> {
        if !view.zoom.is_finite() {
            return Err("Invalid project map zoom.".into());
        }
        let mut map = Self {
            online: view.online,
            auto_fit: false,
            last_selection: Some(selection),
            ..Default::default()
        };
        map.memory
            .set_zoom(view.zoom)
            .map_err(|error| format!("Invalid project map zoom: {error}"))?;
        if let Some([lon, lat]) = view.center {
            if !lon.is_finite()
                || !lat.is_finite()
                || lon.abs() > 180.0
                || lat.abs() > MERCATOR_LIMIT
            {
                return Err("Invalid project map center.".into());
            }
            map.memory.center_at(map_position(lon, lat));
        }
        map.reference_track = Some(source);
        Ok(map)
    }

    #[cfg(test)]
    pub fn offline() -> Self {
        Self {
            online: false,
            ..Default::default()
        }
    }

    pub fn view_state(&self) -> ViewState {
        ViewState {
            reference_track: self.reference_track.clone(),
            memory: self.memory.clone(),
            online: self.online,
            auto_fit: self.auto_fit,
            fit_selection: self.fit_selection,
            last_selection: self.last_selection,
            last_size: self.last_size,
            revision: self.revision,
        }
    }

    pub fn restore_view(&mut self, state: ViewState) {
        if self.online != state.online {
            self.tiles = None;
        }
        self.memory = state.memory;
        self.reference_track = state.reference_track;
        self.online = state.online;
        self.auto_fit = state.auto_fit;
        self.fit_selection = state.fit_selection;
        self.last_selection = state.last_selection;
        self.last_size = state.last_size;
        self.revision = state.revision;
        self.waiting_since = None;
        self.error = None;
        self.last_cursor = None;
    }

    pub fn reset_view(&mut self) {
        self.auto_fit = true;
        self.last_size = Vec2::ZERO;
    }

    pub fn set_reference_track(&mut self, track: Arc<Track>) {
        self.reference_track = Some(track);
        self.reset_view();
    }

    fn connect(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let dirs = directories::ProjectDirs::from("", "", "rlv-forge")
            .ok_or("Cannot locate the map tile cache directory.")?;
        let cache = dirs.cache_dir().join("osm-tiles");
        std::fs::create_dir_all(&cache)
            .map_err(|error| format!("Cannot create map cache {}: {error}", cache.display()))?;
        self.tiles = Some(HttpTiles::with_options(
            OpenStreetMap,
            HttpOptions {
                cache: Some(cache),
                user_agent: Some(walkers::HeaderValue::from_static(concat!(
                    "RLV-Forge/",
                    env!("CARGO_PKG_VERSION"),
                    " (native desktop indoor cycling route editor)"
                ))),
                ..Default::default()
            },
            ctx.clone(),
        ));
        self.error = None;
        self.waiting_since = None;
        Ok(())
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        track: Option<&Track>,
        selection: [f64; 2],
        hover_distance: Option<f64>,
    ) {
        let _ = self.show_with_marker(
            ui,
            track,
            selection,
            hover_distance.map(Cursor::Distance),
            SyncOverlay::default(),
        );
    }

    pub fn show_at_position(
        &mut self,
        ui: &mut egui::Ui,
        track: Option<&Track>,
        selection: [f64; 2],
        position: Option<[f64; 2]>,
        sync: SyncOverlay,
    ) -> Option<Vec<RoutePick>> {
        self.show_with_marker(ui, track, selection, position.map(Cursor::Position), sync)
    }

    fn show_with_marker(
        &mut self,
        ui: &mut egui::Ui,
        track: Option<&Track>,
        selection: [f64; 2],
        cursor: Option<Cursor>,
        sync: SyncOverlay,
    ) -> Option<Vec<RoutePick>> {
        let (hover_distance, marker_position) = match cursor {
            Some(Cursor::Distance(distance)) => (Some(distance), None),
            Some(Cursor::Position([lon, lat])) => (None, Some(map_position(lon, lat))),
            None => (None, None),
        };
        let picked = Rc::new(RefCell::new(None));
        if self.last_selection != Some(selection) {
            self.last_selection = Some(selection);
            self.fit_selection = true;
            self.reset_view();
        }
        ui.horizontal(|ui| {
            ui.strong("Track map");
            if ui
                .add_enabled(track.is_some(), egui::Button::new("Fit track"))
                .clicked()
            {
                self.reset_view();
                self.fit_selection = false;
                self.revision += 1;
            }
            if ui.checkbox(&mut self.online, "Online tiles").changed() {
                self.tiles = None;
                self.waiting_since = None;
                self.error = None;
            }
        });
        let size = sync
            .viewport
            .unwrap_or_else(|| vec2(ui.available_width(), 210.0));
        let Some(track) = track else {
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 3.0, Color32::from_rgb(235, 241, 244));
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Open a GPX or FIT file to see its route",
                egui::FontId::proportional(15.0),
                ROUTE,
            );
            attribution(ui);
            return None;
        };
        if self.online && self.tiles.is_none() && self.error.is_none() {
            if let Err(error) = self.connect(ui.ctx()) {
                self.error = Some(error);
            }
        }
        let fitting = self.auto_fit && size != self.last_size;
        if fitting {
            let (fit_track, range) =
                if self.fit_selection && selection != [0.0, track.total_distance()] {
                    (track, selection)
                } else {
                    let reference = self.reference_track.as_deref().unwrap_or(track);
                    (reference, [0.0, reference.total_distance()])
                };
            let (center, zoom) = fitted_view(fit_track, size, range);
            self.memory.center_at(center);
            if let Err(error) = self.memory.set_zoom(zoom) {
                self.error = Some(format!("Cannot fit the track map: {error}"));
            }
            self.last_size = size;
        }
        let follow = marker_position
            .or_else(|| hover_distance.map(|distance| position_at_distance(track, distance)));
        if let Some(position) = follow {
            if fitting || self.last_cursor != Some(position) {
                self.memory.center_at(position);
            }
        }
        self.last_cursor = follow;
        let center = map_position(track.points[0].longitude, track.points[0].latitude);
        let mut observed = self
            .tiles
            .as_mut()
            .map(|tiles| ObservedTiles { tiles, missing: 0 });
        let response = ui
            .allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0, Color32::from_rgb(235, 241, 244));
                let tiles = observed.as_mut().map(|tiles| tiles as &mut dyn Tiles);
                ui.add(
                    Map::new(tiles, &mut self.memory, center)
                        .zoom_with_ctrl(false)
                        .with_plugin(RouteOverlay {
                            track,
                            reference_track: self.reference_track.as_deref(),
                            selection,
                            hover_distance,
                            marker_position,
                            sync: Some(SyncInteraction {
                                overlay: sync,
                                picked: picked.clone(),
                            }),
                        }),
                )
            })
            .inner;
        if response.changed() {
            self.auto_fit = false;
            self.revision += 1;
        }
        if response.hovered() {
            // Consume map zoom gestures instead of scrolling the surrounding editor.
            ui.input_mut(|input| {
                input.smooth_scroll_delta = Vec2::ZERO;
            });
        }
        attribution(ui);
        let missing = observed.as_ref().map_or(0, |tiles| tiles.missing);
        ui.allocate_ui_with_layout(vec2(size.x, 72.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            if let Some(error) = &self.error {
                ui.add(egui::Label::new(egui::RichText::new(error).color(Color32::from_rgb(180, 35, 35))).truncate())
                    .on_hover_text(error);
                if ui.button("Retry map connection").clicked() {
                    self.error = None;
                    self.tiles = None;
                }
            } else if missing > 0 {
                let since = self.waiting_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_secs(15) {
                    ui.add(egui::Label::new(egui::RichText::new("Map tiles unavailable or slow. Check your connection.").color(Color32::from_rgb(170, 85, 0))).truncate());
                    if ui.button("Retry map tiles").clicked() {
                        self.tiles = None;
                        self.error = None;
                        self.waiting_since = None;
                    }
                } else {
                    ui.add(egui::Label::new("Loading map tiles...").truncate());
                }
                ui.ctx().request_repaint_after(Duration::from_millis(250));
            } else {
                self.waiting_since = None;
                let hint = if self.online {
                    "Seeking centers the red cursor. Drag to explore; scroll to zoom. Grey: full route; blue: kept range."
                } else {
                    "Online tiles off. Route shown without a background map."
                };
                ui.add(egui::Label::new(egui::RichText::new(hint).small()).truncate()).on_hover_text(hint);
            }
        });
        let result = picked.borrow_mut().take();
        result
    }
}

fn attribution(ui: &mut egui::Ui) {
    ui.hyperlink_to(
        "© OpenStreetMap contributors",
        "https://www.openstreetmap.org/copyright",
    );
}

// Walkers logs HTTP errors; also make missing tiles and retry visible in the app.
struct ObservedTiles<'a> {
    tiles: &'a mut HttpTiles,
    missing: usize,
}

impl Tiles for ObservedTiles<'_> {
    fn at(&mut self, tile_id: TileId) -> Option<TilePiece> {
        let tile = self.tiles.at(tile_id);
        if tile.is_none() {
            self.missing += 1;
        }
        tile
    }

    fn attribution(&self) -> walkers::sources::Attribution {
        self.tiles.attribution()
    }

    fn tile_size(&self) -> u32 {
        self.tiles.tile_size()
    }
}

fn map_position(longitude: f64, latitude: f64) -> Position {
    lon_lat(longitude, latitude.clamp(-MERCATOR_LIMIT, MERCATOR_LIMIT))
}

fn mercator_y(latitude: f64) -> f64 {
    let latitude = latitude.clamp(-MERCATOR_LIMIT, MERCATOR_LIMIT).to_radians();
    (1.0 - latitude.tan().asinh() / std::f64::consts::PI) / 2.0
}

fn fitted_view(track: &Track, size: Vec2, selection: [f64; 2]) -> (Position, f64) {
    let mut min = [f64::INFINITY; 2];
    let mut max = [f64::NEG_INFINITY; 2];
    for position in route_sections(track, selection).into_iter().flatten() {
        let coordinates = [(position.x() + 180.0) / 360.0, mercator_y(position.y())];
        for axis in 0..2 {
            min[axis] = min[axis].min(coordinates[axis]);
            max[axis] = max[axis].max(coordinates[axis]);
        }
    }
    if !min[0].is_finite() {
        let index = track
            .points
            .partition_point(|point| point.distance < selection[0])
            .min(track.points.len() - 1);
        let point = &track.points[index];
        return (map_position(point.longitude, point.latitude), 17.0);
    }
    let center_x = (min[0] + max[0]) / 2.0;
    let center_y = (min[1] + max[1]) / 2.0;
    let longitude = center_x * 360.0 - 180.0;
    let latitude = (std::f64::consts::PI * (1.0 - 2.0 * center_y))
        .sinh()
        .atan()
        .to_degrees();
    let zoom_x =
        (f64::from((size.x - 36.0).max(1.0)) / (256.0 * (max[0] - min[0]).max(1e-12))).log2();
    let zoom_y =
        (f64::from((size.y - 36.0).max(1.0)) / (256.0 * (max[1] - min[1]).max(1e-12))).log2();
    (
        lon_lat(longitude, latitude),
        zoom_x.min(zoom_y).clamp(0.0, 17.0),
    )
}

fn connected(a: &Point, b: &Point) -> bool {
    a.segment == b.segment && (a.longitude - b.longitude).abs() <= 180.0
}

fn interpolated_position(a: &Point, b: &Point, distance: f64) -> Position {
    let t = ((distance - a.distance) / (b.distance - a.distance)).clamp(0.0, 1.0);
    map_position(
        a.longitude + t * (b.longitude - a.longitude),
        a.latitude + t * (b.latitude - a.latitude),
    )
}

fn position_at_distance(track: &Track, distance: f64) -> Position {
    let right = track
        .points
        .partition_point(|point| point.distance < distance)
        .min(track.points.len() - 1);
    let point = &track.points[right];
    if right > 0 {
        let previous = &track.points[right - 1];
        if connected(previous, point) && point.distance > previous.distance {
            return interpolated_position(previous, point, distance);
        }
    }
    map_position(point.longitude, point.latitude)
}

fn route_sections(track: &Track, selection: [f64; 2]) -> Vec<Vec<Position>> {
    let mut sections = Vec::new();
    let mut line = Vec::new();
    for (index, point) in track.points.iter().enumerate() {
        let previous = index.checked_sub(1).map(|index| &track.points[index]);
        if let Some(previous) = previous.filter(|previous| connected(previous, point)) {
            // Include the exact chart boundaries even when they fall between GPX samples.
            for boundary in selection {
                if previous.distance < boundary && point.distance > boundary {
                    line.push(interpolated_position(previous, point, boundary));
                }
            }
        } else if !line.is_empty() {
            sections.push(std::mem::take(&mut line));
        }
        if point.distance >= selection[0] && point.distance <= selection[1] {
            line.push(map_position(point.longitude, point.latitude));
        }
    }
    if !line.is_empty() {
        sections.push(line);
    }
    sections
}

struct RouteOverlay<'a> {
    track: &'a Track,
    reference_track: Option<&'a Track>,
    selection: [f64; 2],
    hover_distance: Option<f64>,
    marker_position: Option<Position>,
    sync: Option<SyncInteraction>,
}

fn route_picks(
    track: &Track,
    projector: &Projector,
    pointer: egui::Pos2,
    hint: Option<usize>,
) -> Vec<RoutePick> {
    let mut groups: Vec<(RoutePick, f32, usize)> = Vec::new();
    for (left, point) in track.points.iter().enumerate() {
        let right = if left + 1 < track.points.len() && connected(point, &track.points[left + 1]) {
            left + 1
        } else {
            left
        };
        let a = projector
            .project(map_position(point.longitude, point.latitude))
            .to_pos2();
        let next = &track.points[right];
        let b = projector
            .project(map_position(next.longitude, next.latitude))
            .to_pos2();
        let delta = b - a;
        let fraction = if delta.length_sq() > 0.0 {
            ((pointer - a).dot(delta) / delta.length_sq()).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let distance = pointer.distance(a + fraction * delta);
        if distance > 12.0 {
            continue;
        }
        if right != left && next.step <= 0.01 {
            for index in [left, right] {
                groups.push((
                    RoutePick {
                        left: index,
                        right: index,
                        fraction: 0.0,
                    },
                    distance,
                    index,
                ));
            }
            continue;
        }
        let pick = RoutePick {
            left,
            right,
            fraction: f64::from(fraction),
        };
        if let Some((best, best_distance, last)) = groups.last_mut().filter(|(best, _, last)| {
            best.left != best.right
                && *last + 1 == left
                && track.points[*last].segment == point.segment
        }) {
            let closer_hint =
                hint.is_some_and(|hint| left.abs_diff(hint) < best.left.abs_diff(hint));
            if distance < *best_distance - 0.1
                || ((distance - *best_distance).abs() <= 0.1 && closer_hint)
            {
                *best = pick;
                *best_distance = distance;
            }
            *last = left;
        } else {
            groups.push((pick, distance, left));
        }
    }
    groups.into_iter().map(|(pick, _, _)| pick).collect()
}

impl Plugin for RouteOverlay<'_> {
    fn run(
        self: Box<Self>,
        ui: &mut egui::Ui,
        response: &egui::Response,
        projector: &Projector,
        memory: &MapMemory,
    ) {
        let painter = ui.painter().with_clip_rect(response.rect);
        // Walkers centers its tiles in the clipped viewport, not the full scrolled widget.
        let projector = Projector::new(
            painter.clip_rect(),
            memory,
            projector.unproject(response.rect.center().to_vec2()),
        );
        for selected in [false, true] {
            let track = if selected {
                self.track
            } else {
                self.reference_track.unwrap_or(self.track)
            };
            let range = if selected {
                self.selection
            } else {
                [0.0, track.total_distance()]
            };
            for section in route_sections(track, range) {
                let mut line = Vec::new();
                for (index, position) in section.iter().enumerate() {
                    let pos = projector.project(*position).to_pos2();
                    // Keep detail at any zoom while avoiding thousands of subpixel segments.
                    if line
                        .last()
                        .is_none_or(|last: &egui::Pos2| last.distance(pos) >= 1.0)
                        || index + 1 == section.len()
                    {
                        line.push(pos);
                    }
                }
                draw_route(&painter, line, selected);
            }
        }
        let start = &self.track.points[0];
        let end = &self.track.points[self.track.points.len() - 1];
        let red_position = self.marker_position.unwrap_or_else(|| {
            self.hover_distance.map_or_else(
                || map_position(end.longitude, end.latitude),
                |distance| position_at_distance(self.track, distance),
            )
        });
        for (position, color) in [
            (
                map_position(start.longitude, start.latitude),
                Color32::from_rgb(20, 160, 70),
            ),
            (red_position, RED_MARKER),
        ] {
            let pos = projector.project(position).to_pos2();
            painter.circle_filled(pos, 4.5, color);
            painter.circle_stroke(pos, 4.5, Stroke::new(1.5_f32, Color32::WHITE));
        }
        if let Some(sync) = self.sync {
            for marker in sync.overlay.markers {
                let pos = projector
                    .project(map_position(marker.position[0], marker.position[1]))
                    .to_pos2();
                painter.circle_stroke(
                    pos,
                    7.0,
                    Stroke::new(2.0_f32, Color32::from_rgb(235, 140, 20)),
                );
                painter.text(
                    pos + vec2(7.0, -7.0),
                    egui::Align2::LEFT_BOTTOM,
                    marker.label,
                    egui::FontId::proportional(13.0),
                    Color32::BLACK,
                );
            }
            if sync.overlay.picking && response.clicked() {
                if let Some(pointer) = response.interact_pointer_pos() {
                    *sync.picked.borrow_mut() = Some(route_picks(
                        self.reference_track.unwrap_or(self.track),
                        &projector,
                        pointer,
                        sync.overlay.hint,
                    ));
                }
            }
        }
    }
}

fn draw_route(painter: &egui::Painter, mut line: Vec<egui::Pos2>, selected: bool) {
    line.dedup();
    let color = if selected { SELECTED } else { ROUTE };
    if line.len() == 1 {
        painter.circle_filled(line[0], 2.0, color);
    } else if line.len() > 1 {
        painter.add(Shape::line(line, Stroke::new(2.5_f32, color)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{pos2, Rect};
    use rlv_forge::timeline::{self, Clip};
    use std::path::Path;

    #[test]
    fn profile_ground_span_tracks_map_zoom_width_and_latitude() {
        let mut map = TrackMap::offline();
        map.memory.set_zoom(12.0).unwrap();
        map.memory.center_at(lon_lat(0.0, 0.0));
        let span = map.ground_span(500.0, 0.0);
        assert!(span > 10_000.0 && span < 30_000.0);
        assert!((map.ground_span(1000.0, 0.0) / span - 2.0).abs() < 1e-6);
        map.memory.set_zoom(13.0).unwrap();
        assert!((map.ground_span(500.0, 0.0) / span - 0.5).abs() < 1e-6);
        map.memory.set_zoom(12.0).unwrap();
        map.memory.center_at(lon_lat(80.0, 60.0));
        assert!((map.ground_span(500.0, 0.0) / span - 0.5).abs() < 1e-6);
    }

    #[test]
    fn scrolling_keeps_routes_markers_and_picks_aligned_with_clipped_map_tiles() {
        struct TestTiles(egui::TextureHandle);
        impl Tiles for TestTiles {
            fn at(&mut self, _: TileId) -> Option<TilePiece> {
                Some(TilePiece::new(
                    walkers::Tile::Raster(self.0.clone()),
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                ))
            }
            fn attribution(&self) -> walkers::sources::Attribution {
                use walkers::sources::TileSource;
                OpenStreetMap.attribution()
            }
            fn tile_size(&self) -> u32 {
                256
            }
        }
        let ctx = egui::Context::default();
        let texture = ctx.load_texture(
            "offline-map-tile",
            egui::ColorImage::from_rgb([1, 1], &[255, 255, 255]),
            Default::default(),
        );
        let mut tiles = TestTiles(texture.clone());
        let track = Track::parse(
            "<gpx><rte><rtept lat=\"0\" lon=\"-0.01\"/><rtept lat=\"0\" lon=\"0.01\"/></rte></gpx>"
                .into(),
        )
        .unwrap();
        let center = lon_lat(0.0, 0.0);
        let mut memory = MapMemory::default();
        memory.set_zoom(0.0).unwrap();
        memory.center_at(center);
        let picked = Rc::new(RefCell::new(None));
        let mut draw = |offset, events| {
            crate::test_ui(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(480.0, 360.0))),
                    events,
                    ..Default::default()
                },
                |root| {
                    egui::Panel::top("header").show(root, |ui| {
                        ui.label("Fixed header");
                    });
                    egui::Panel::bottom("footer").show(root, |ui| {
                        ui.label("Fixed footer");
                    });
                    egui::CentralPanel::default().show(root, |ui| {
                        egui::ScrollArea::vertical()
                            .vertical_scroll_offset(offset)
                            .show(ui, |ui| {
                                ui.add_space(80.0);
                                ui.allocate_ui_with_layout(
                                    vec2(350.0, 240.0),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        ui.add(
                                            Map::new(Some(&mut tiles), &mut memory, center)
                                                .with_plugin(RouteOverlay {
                                                    track: &track,
                                                    reference_track: None,
                                                    selection: [0.0, track.total_distance()],
                                                    hover_distance: None,
                                                    marker_position: Some(center),
                                                    sync: Some(SyncInteraction {
                                                        overlay: SyncOverlay {
                                                            picking: true,
                                                            markers: vec![SyncMarker {
                                                                position: [0.0, 0.0],
                                                                label: "1".into(),
                                                            }],
                                                            ..Default::default()
                                                        },
                                                        picked: picked.clone(),
                                                    }),
                                                }),
                                        );
                                    },
                                );
                                ui.add_space(500.0);
                            });
                    });
                },
            )
        };
        let mut pointer = egui::Pos2::ZERO;
        for offset in [0.0, 120.0, 220.0, 120.0] {
            let output = draw(offset, vec![]);
            let tile = output
                .shapes
                .iter()
                .find(|shape| {
                    matches!(&shape.shape,
                Shape::Mesh(mesh) if mesh.texture_id == texture.id())
                })
                .unwrap();
            let Shape::Mesh(mesh) = &tile.shape else {
                unreachable!()
            };
            let tile_center = mesh.calc_bounds().center();
            let marker = output
                .shapes
                .iter()
                .find(|shape| {
                    matches!(&shape.shape,
                Shape::Circle(circle) if circle.fill == RED_MARKER)
                })
                .unwrap();
            let Shape::Circle(circle) = &marker.shape else {
                unreachable!()
            };
            assert!(
                circle.center.distance(tile_center) < 0.001,
                "offset={offset}"
            );
            assert_eq!(marker.clip_rect, tile.clip_rect);
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                Shape::Path(path) if path.stroke.color == egui::epaint::ColorMode::Solid(SELECTED)
                    && path.points.iter().all(|point| (point.y - tile_center.y).abs() < 0.001))));
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                Shape::Circle(pin) if pin.radius == 7.0 && pin.center.distance(tile_center) < 0.001)));
            pointer = circle.center;
        }
        for pressed in [true, false] {
            draw(
                120.0,
                vec![
                    egui::Event::PointerMoved(pointer),
                    egui::Event::PointerButton {
                        pos: pointer,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        assert!(!picked.borrow().as_ref().unwrap().is_empty());
    }

    #[test]
    fn seeking_centers_the_red_marker_without_changing_zoom_even_across_cuts() {
        let (original, cut) = cut_reference_fixture();
        let mut map = TrackMap::offline();
        map.set_reference_track(original);
        let ctx = egui::Context::default();
        let draw = |map: &mut TrackMap, position| {
            crate::test_ui(&ctx, Default::default(), |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    map.show_at_position(
                        ui,
                        Some(&cut),
                        [0.0, cut.total_distance()],
                        Some(position),
                        SyncOverlay::default(),
                    );
                });
            })
        };
        draw(&mut map, [0.0, 0.0]);
        map.memory.set_zoom(12.0).unwrap();
        let revision = map.revision;
        for position in [[0.5, 0.0], [2.0, 0.0], [2.5, 0.0]] {
            let output = draw(&mut map, position);
            assert_eq!(map.memory.zoom(), 12.0);
            assert_eq!(map.revision, revision);
            let center = map.memory.detached().unwrap();
            assert!((center.x() - position[0]).abs() < 1e-9);
            assert!((center.y() - position[1]).abs() < 1e-9);
            let viewport = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    Shape::Rect(rect) if rect.fill == Color32::from_rgb(235, 241, 244) => {
                        Some(rect.rect)
                    }
                    _ => None,
                })
                .unwrap();
            let marker = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    Shape::Circle(circle) if circle.fill == RED_MARKER => Some(circle.center),
                    _ => None,
                })
                .unwrap();
            assert!(marker.distance(viewport.center()) < 0.001);
        }
        map.memory.center_at(lon_lat(20.0, 0.0));
        draw(&mut map, [2.5, 0.0]);
        assert!((map.memory.detached().unwrap().x() - 20.0).abs() < 1e-9);
        draw(&mut map, [2.6, 0.0]);
        assert!((map.memory.detached().unwrap().x() - 2.6).abs() < 1e-9);
    }

    #[test]
    fn saved_map_zoom_and_tile_preferences_survive_the_first_restored_frame() {
        let (source, cut) = cut_reference_fixture();
        let mut map = TrackMap::offline();
        map.memory.set_zoom(14.0).unwrap();
        map.memory.center_at(lon_lat(541.0, 10.0));
        let view = map.project_view();
        let [lon, lat] = view.center.unwrap();
        assert!((lon + 179.0).abs() < 1e-9 && (lat - 10.0).abs() < 1e-9);
        let selection = [0.0, cut.total_distance()];
        let mut restored = TrackMap::from_project(&view, source, selection).unwrap();
        assert!(!restored.online);
        let ctx = egui::Context::default();
        render_offline_map(&mut restored, &ctx, &cut, selection);
        assert_eq!(restored.memory.zoom(), 14.0);
        assert!((restored.memory.detached().unwrap().x() + 179.0).abs() < 1e-9);
    }

    #[test]
    fn map_clicks_snap_to_the_original_route_and_preserve_loop_occurrences() {
        let points: String = [
            (0.0, -0.03),
            (0.0, 0.03),
            (0.03, 0.03),
            (0.03, -0.03),
            (0.0, -0.03),
            (0.0, 0.03),
        ]
        .iter()
        .map(|(lat, lon)| format!("<trkpt lat=\"{lat}\" lon=\"{lon}\"><ele>100</ele></trkpt>"))
        .collect();
        let track = Arc::new(
            Track::parse(format!("<gpx><trk><trkseg>{points}</trkseg></trk></gpx>")).unwrap(),
        );
        let mut map = TrackMap {
            online: false,
            ..Default::default()
        };
        map.set_reference_track(track.clone());
        let ctx = egui::Context::default();
        let mut draw = |events| {
            let mut picked = None;
            let output = crate::test_ui(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1000.0, 800.0))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        picked = map.show_at_position(
                            ui,
                            Some(&track),
                            [0.0, track.total_distance()],
                            None,
                            SyncOverlay {
                                picking: true,
                                ..Default::default()
                            },
                        );
                    });
                },
            );
            (output, picked)
        };
        let (output, _) = draw(vec![]);
        let pointer = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                Shape::Path(path)
                    if path.stroke.color == egui::epaint::ColorMode::Solid(ROUTE)
                        && path.points.len() >= 2 =>
                {
                    Some(path.points[0].lerp(path.points[1], 0.5))
                }
                _ => None,
            })
            .unwrap();
        let click = |pressed| {
            vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        draw(click(true));
        let (_, picked) = draw(click(false));
        let picks = picked.unwrap();
        assert_eq!(picks.len(), 2);
        assert_eq!((picks[0].left, picks[1].left), (0, 4));
        assert!((picks[0].fraction - 0.5).abs() < 0.001);
    }

    #[test]
    fn stationary_route_locations_offer_distinct_original_times() {
        let track = Track::parse(
            "<gpx><trk><trkseg>\
            <trkpt lat=\"0\" lon=\"-0.03\"/><trkpt lat=\"0\" lon=\"0\"/>\
            <trkpt lat=\"0\" lon=\"0\"/><trkpt lat=\"0\" lon=\"0.03\"/>\
            </trkseg></trk></gpx>"
                .into(),
        )
        .unwrap();
        let rect = Rect::from_min_size(egui::Pos2::ZERO, vec2(400.0, 300.0));
        let center = lon_lat(0.0, 0.0);
        let mut memory = MapMemory::default();
        memory.center_at(center);
        memory.set_zoom(14.0).unwrap();
        let projector = Projector::new(rect, &memory, center);
        let picks = route_picks(
            &track,
            &projector,
            projector.project(center).to_pos2(),
            None,
        );
        assert!(picks.iter().any(|pick| pick.left == 1 && pick.right == 1));
        assert!(picks.iter().any(|pick| pick.left == 2 && pick.right == 2));
    }

    fn cut_reference_fixture() -> (Arc<Track>, Track) {
        let points: String = [(0, 0), (0, 1), (50, 1), (0, 2), (0, 3)]
            .iter().enumerate().map(|(i, (latitude, longitude))| format!(
                "<trkpt lat=\"{latitude}\" lon=\"{longitude}\"><ele>100</ele><time>2026-01-01T00:00:{i:02}Z</time></trkpt>"
            )).collect();
        let original = Arc::new(
            Track::parse(format!("<gpx><trk><trkseg>{points}</trkseg></trk></gpx>")).unwrap(),
        );
        let cut = timeline::cut(
            &original,
            Clip {
                start: 1.0,
                end: 3.0,
            },
        )
        .unwrap();
        (original, cut)
    }

    #[test]
    fn original_route_stays_visible_while_the_marker_jumps_over_a_cut() {
        let (original, cut) = cut_reference_fixture();
        let timing = timeline::timing(&cut).unwrap();
        let ctx = egui::Context::default();
        for (seconds, longitude) in [(0.999, 0.999), (1.0, 2.0), (1.5, 2.5)] {
            let [lon, lat] = timeline::position_at(&timing, &cut, seconds).unwrap();
            let mut expected_marker = egui::Pos2::ZERO;
            let mut omitted_peak = egui::Pos2::ZERO;
            let output = crate::test_ui(&ctx, Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(400.0, 210.0), egui::Sense::hover());
                    let (center, zoom) =
                        fitted_view(&original, rect.size(), [0.0, original.total_distance()]);
                    let mut memory = MapMemory::default();
                    memory.center_at(center);
                    memory.set_zoom(zoom).unwrap();
                    let projector = Projector::new(rect, &memory, center);
                    omitted_peak = projector.project(lon_lat(1.0, 50.0)).to_pos2();
                    expected_marker = projector.project(lon_lat(longitude, 0.0)).to_pos2();
                    Box::new(RouteOverlay {
                        track: &cut,
                        reference_track: Some(&original),
                        selection: [0.0, cut.total_distance()],
                        hover_distance: None,
                        marker_position: Some(lon_lat(lon, lat)),
                        sync: None,
                    })
                    .run(ui, &response, &projector, &memory);
                });
            });
            let paths: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    Shape::Path(path) if path.stroke.width == 2.5 => Some(path),
                    _ => None,
                })
                .collect();
            assert!(paths.iter().any(|path| {
                path.stroke.color == egui::epaint::ColorMode::Solid(ROUTE)
                    && path
                        .points
                        .iter()
                        .any(|point| point.distance(omitted_peak) < 0.001)
            }));
            assert!(paths
                .iter()
                .filter(|path| path.stroke.color == egui::epaint::ColorMode::Solid(SELECTED))
                .all(|path| path
                    .points
                    .iter()
                    .all(|point| point.distance(omitted_peak) > 1.0)));
            let marker = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    Shape::Circle(circle) if circle.fill == RED_MARKER => Some(circle.center),
                    _ => None,
                })
                .unwrap();
            assert!(marker.distance(expected_marker) < 0.001);
        }
    }

    #[test]
    fn full_route_fit_and_reference_follow_file_replacement_and_history() {
        let (original, cut) = cut_reference_fixture();
        let ctx = egui::Context::default();
        let mut map = TrackMap {
            online: false,
            ..Default::default()
        };
        map.set_reference_track(original.clone());
        render_offline_map(&mut map, &ctx, &original, [0.0, original.total_distance()]);
        let before = map.view_state();
        let original_center = map.memory.detached().unwrap();
        let original_zoom = map.memory.zoom();
        render_offline_map(&mut map, &ctx, &cut, [0.0, cut.total_distance()]);
        assert_eq!(map.memory.detached().unwrap(), original_center);
        assert_eq!(map.memory.zoom(), original_zoom);
        let after = map.view_state();
        map.set_reference_track(Arc::new(cut.clone()));
        render_offline_map(&mut map, &ctx, &cut, [0.0, cut.total_distance()]);
        assert!(map.memory.zoom() > original_zoom);
        map.restore_view(before);
        assert!(Arc::ptr_eq(
            map.reference_track.as_ref().unwrap(),
            &original
        ));
        assert_eq!(map.memory.zoom(), original_zoom);
        map.restore_view(after);
        render_offline_map(&mut map, &ctx, &cut, [0.0, cut.total_distance()]);
        assert!(Arc::ptr_eq(
            map.reference_track.as_ref().unwrap(),
            &original
        ));
        assert_eq!(map.memory.zoom(), original_zoom);
    }

    #[test]
    fn both_sample_routes_fit_in_the_map_with_padding() {
        let size = vec2(380.0, 210.0);
        for filename in ["2026-07-14_113900.gpx", "2026-09-22_120200.gpx"] {
            let track = Track::read(
                &Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("data")
                    .join(filename),
            )
            .unwrap();
            let (center, zoom) = fitted_view(&track, size, [0.0, track.total_distance()]);
            let mut memory = MapMemory::default();
            memory.center_at(center);
            memory.set_zoom(zoom).unwrap();
            let rect = Rect::from_min_size(pos2(0.0, 0.0), size);
            let projector = Projector::new(rect, &memory, center);
            for point in &track.points {
                let position = projector
                    .project(map_position(point.longitude, point.latitude))
                    .to_pos2();
                assert!(
                    rect.shrink(16.0).contains(position),
                    "{position:?} outside map at zoom {zoom}"
                );
            }
        }
    }

    #[test]
    fn stationary_and_polar_tracks_have_finite_views() {
        for latitude in [-90.0, 0.0, 90.0] {
            let track = Track::parse(format!(
                "<gpx><rte><rtept lat=\"{latitude}\" lon=\"0\"/></rte></gpx>"
            ))
            .unwrap();
            let (center, zoom) =
                fitted_view(&track, vec2(260.0, 210.0), [0.0, track.total_distance()]);
            assert!(center.x().is_finite() && center.y().is_finite());
            assert!(zoom.is_finite() && (0.0..=17.0).contains(&zoom));
        }
    }

    #[test]
    fn route_map_can_render_without_connecting_to_a_tile_server() {
        let track = Track::parse(
            "<gpx><rte><rtept lat=\"0\" lon=\"0\"/><rtept lat=\"0.1\" lon=\"0.1\"/></rte></gpx>"
                .into(),
        )
        .unwrap();
        let mut map = TrackMap {
            online: false,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let output = crate::test_ui(&ctx, Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.set_width(400.0);
                map.show(ui, Some(&track), [0.0, track.total_distance()], None);
            });
        });
        assert!(!output.shapes.is_empty());
        assert!(map.tiles.is_none());
        assert!(map.error.is_none());
        map.auto_fit = false;
        map.reset_view();
        assert!(map.auto_fit);
        assert_eq!(map.last_size, Vec2::ZERO);
    }

    #[test]
    fn overlay_keeps_segments_separate_and_highlights_only_the_selection() {
        let track = Track::parse(
            "<gpx><trk><trkseg><trkpt lat=\"0\" lon=\"0\"/><trkpt lat=\"0.1\" lon=\"0.1\"/></trkseg><trkseg><trkpt lat=\"0.5\" lon=\"0.5\"/><trkpt lat=\"0.6\" lon=\"0.6\"/></trkseg></trk></gpx>".into()
        ).unwrap();
        let ctx = egui::Context::default();
        let output = crate::test_ui(&ctx, Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let (rect, response) =
                    ui.allocate_exact_size(vec2(400.0, 210.0), egui::Sense::hover());
                let (center, zoom) =
                    fitted_view(&track, rect.size(), [0.0, track.total_distance()]);
                let mut memory = MapMemory::default();
                memory.center_at(center);
                memory.set_zoom(zoom).unwrap();
                let projector = Projector::new(rect, &memory, center);
                Box::new(RouteOverlay {
                    track: &track,
                    reference_track: None,
                    selection: [0.0, track.points[1].distance],
                    hover_distance: None,
                    marker_position: None,
                    sync: None,
                })
                .run(ui, &response, &projector, &memory);
            });
        });
        let lines: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                Shape::Path(path) if path.stroke.width == 2.5 => Some(path),
                _ => None,
            })
            .collect();
        // Two muted segments, plus the selected first segment in blue.
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|path| path.points.len() == 2));
        assert_eq!(
            lines
                .iter()
                .filter(|path| path.stroke.color == egui::epaint::ColorMode::Solid(SELECTED))
                .count(),
            1
        );
        assert_eq!(
            lines
                .iter()
                .filter(|path| path.stroke.color == egui::epaint::ColorMode::Solid(ROUTE))
                .count(),
            2
        );
        assert!(SELECTED.b() > SELECTED.r() && SELECTED.b() > SELECTED.g());
    }

    fn render_offline_map(
        map: &mut TrackMap,
        ctx: &egui::Context,
        track: &Track,
        selection: [f64; 2],
    ) {
        let _ = crate::test_ui(
            ctx,
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(430.0, 330.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    map.show(ui, Some(track), selection, None);
                });
            },
        );
    }

    #[test]
    fn chart_range_changes_refit_the_map_after_independent_map_navigation() {
        let track = Track::parse(
            "<gpx><rte><rtept lat=\"0\" lon=\"0\"/><rtept lat=\"0\" lon=\"1\"/><rtept lat=\"0\" lon=\"2\"/></rte></gpx>".into()
        ).unwrap();
        let ctx = egui::Context::default();
        let mut map = TrackMap {
            online: false,
            ..Default::default()
        };
        let full = [0.0, track.total_distance()];
        render_offline_map(&mut map, &ctx, &track, full);
        let full_zoom = map.memory.zoom();
        let zoomed = [full[1] * 0.2, full[1] * 0.3];
        render_offline_map(&mut map, &ctx, &track, zoomed);
        assert!(map.memory.zoom() > full_zoom);
        assert!((map.memory.detached().unwrap().x() - 0.5).abs() < 1e-6);

        map.memory.center_at(lon_lat(20.0, 20.0));
        map.memory.set_zoom(8.5).unwrap();
        map.auto_fit = false;
        render_offline_map(&mut map, &ctx, &track, zoomed);
        let manual_center = map.memory.detached().unwrap();
        assert!((manual_center.x() - 20.0).abs() < 1e-9);
        assert!((manual_center.y() - 20.0).abs() < 1e-9);
        assert_eq!(map.memory.zoom(), 8.5);

        let panned = [full[1] * 0.6, full[1] * 0.7];
        render_offline_map(&mut map, &ctx, &track, panned);
        let (center, zoom) = fitted_view(&track, map.last_size, panned);
        let actual_center = map.memory.detached().unwrap();
        assert!((actual_center.x() - center.x()).abs() < 1e-9);
        assert!((actual_center.y() - center.y()).abs() < 1e-9);
        assert_eq!(map.memory.zoom(), zoom);
        assert!(map.auto_fit);

        // Fit track can show the whole route without changing the chart selection.
        map.reset_view();
        map.fit_selection = false;
        render_offline_map(&mut map, &ctx, &track, panned);
        assert_eq!(map.memory.zoom(), full_zoom);
        render_offline_map(&mut map, &ctx, &track, zoomed);
        assert!(map.memory.zoom() > full_zoom);
        render_offline_map(&mut map, &ctx, &track, full);
        assert_eq!(map.memory.zoom(), full_zoom);
    }

    #[test]
    fn selection_between_samples_is_clipped_to_exact_distance_bounds() {
        let track = Track::parse(
            "<gpx><rte><rtept lat=\"0\" lon=\"0\"/><rtept lat=\"0\" lon=\"1\"/></rte></gpx>".into(),
        )
        .unwrap();
        let total = track.total_distance();
        let sections = route_sections(&track, [total * 0.25, total * 0.75]);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].len(), 2);
        assert!((sections[0][0].x() - 0.25).abs() < 1e-9);
        assert!((sections[0][1].x() - 0.75).abs() < 1e-9);
        let (_, full_zoom) = fitted_view(&track, vec2(400.0, 210.0), [0.0, total]);
        let (_, zoom) = fitted_view(&track, vec2(400.0, 210.0), [total * 0.25, total * 0.75]);
        assert!(zoom > full_zoom);
        let (center, zoom) = fitted_view(&track, vec2(400.0, 210.0), [total * 0.5, total * 0.5]);
        assert!((center.x() - 0.5).abs() < 1e-9);
        assert_eq!(zoom, 17.0);
    }

    #[test]
    fn red_marker_follows_chart_distance_and_restores_the_route_end() {
        let track = Track::parse(
            "<gpx><rte><rtept lat=\"0\" lon=\"0\"/><rtept lat=\"0\" lon=\"1\"/></rte></gpx>".into(),
        )
        .unwrap();
        let ctx = egui::Context::default();
        for fraction in [Some(0.25), Some(0.75), None] {
            let mut expected = egui::Pos2::ZERO;
            let output = crate::test_ui(&ctx, Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(400.0, 210.0), egui::Sense::hover());
                    let (center, zoom) =
                        fitted_view(&track, rect.size(), [0.0, track.total_distance()]);
                    let mut memory = MapMemory::default();
                    memory.center_at(center);
                    memory.set_zoom(zoom).unwrap();
                    let projector = Projector::new(rect, &memory, center);
                    expected = projector
                        .project(lon_lat(fraction.unwrap_or(1.0), 0.0))
                        .to_pos2();
                    Box::new(RouteOverlay {
                        track: &track,
                        reference_track: None,
                        selection: [0.0, track.total_distance()],
                        hover_distance: fraction.map(|fraction| fraction * track.total_distance()),
                        marker_position: None,
                        sync: None,
                    })
                    .run(ui, &response, &projector, &memory);
                });
            });
            let markers: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    Shape::Circle(circle) if circle.fill == RED_MARKER => Some(circle.center),
                    _ => None,
                })
                .collect();
            assert_eq!(markers.len(), 1);
            assert!(markers[0].distance(expected) < 0.001);
        }
    }

    #[test]
    fn video_marker_uses_the_post_cut_position_at_a_shared_distance() {
        let track = Track::parse(
            "<gpx><trk><trkseg><trkpt lat=\"0\" lon=\"0\"/><trkpt lat=\"0\" lon=\"1\"/></trkseg><trkseg><trkpt lat=\"0\" lon=\"2\"/><trkpt lat=\"0\" lon=\"3\"/></trkseg></trk></gpx>".into()
        ).unwrap();
        assert_eq!(track.points[1].distance, track.points[2].distance);
        let ctx = egui::Context::default();
        let mut expected = egui::Pos2::ZERO;
        let output = crate::test_ui(&ctx, Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let (rect, response) =
                    ui.allocate_exact_size(vec2(400.0, 210.0), egui::Sense::hover());
                let (center, zoom) =
                    fitted_view(&track, rect.size(), [0.0, track.total_distance()]);
                let mut memory = MapMemory::default();
                memory.center_at(center);
                memory.set_zoom(zoom).unwrap();
                let projector = Projector::new(rect, &memory, center);
                expected = projector.project(lon_lat(2.0, 0.0)).to_pos2();
                Box::new(RouteOverlay {
                    track: &track,
                    reference_track: None,
                    selection: [0.0, track.total_distance()],
                    hover_distance: None,
                    marker_position: Some(lon_lat(2.0, 0.0)),
                    sync: None,
                })
                .run(ui, &response, &projector, &memory);
            });
        });
        let markers: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                Shape::Circle(circle) if circle.fill == RED_MARKER => Some(circle.center),
                _ => None,
            })
            .collect();
        assert_eq!(markers.len(), 1);
        assert!(markers[0].distance(expected) < 0.001);
    }

    #[test]
    fn cursor_position_handles_segment_boundaries_and_duplicate_distances() {
        let track = Track::parse(
            "<gpx><trk><trkseg><trkpt lat=\"0\" lon=\"0\"/><trkpt lat=\"0\" lon=\"1\"/></trkseg><trkseg><trkpt lat=\"20\" lon=\"20\"/><trkpt lat=\"20\" lon=\"21\"/></trkseg></trk></gpx>".into()
        ).unwrap();
        let boundary = track.points[1].distance;
        let at_boundary = position_at_distance(&track, boundary);
        assert!((at_boundary.x() - 1.0).abs() < 1e-9);
        let after_boundary =
            position_at_distance(&track, boundary + (track.total_distance() - boundary) * 0.5);
        assert!((after_boundary.x() - 20.5).abs() < 1e-9);
        assert!((after_boundary.y() - 20.0).abs() < 1e-9);
        let stationary = Track::parse(
            "<gpx><rte><rtept lat=\"0\" lon=\"0\"/><rtept lat=\"0\" lon=\"0\"/></rte></gpx>".into(),
        )
        .unwrap();
        assert_eq!(position_at_distance(&stationary, 0.0), lon_lat(0.0, 0.0));
    }
}
