use crate::{
    map::{ProjectView, TrackMap},
    studio::{ProjectState, Studio},
    ui::Chart,
};
use rlv_forge::gpx::Track;
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

const FORMAT: &str = "rlv-forge-project";
const LEGACY_FORMAT: &str = "gpx-smoother-project";
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
pub struct SavedTrack {
    xml: String,
    elevations: Vec<f64>,
    source_indices: Vec<Option<usize>>,
}

impl SavedTrack {
    pub fn capture(track: &Track) -> Self {
        Self {
            xml: track.source_xml().to_owned(),
            elevations: track.points.iter().map(|point| point.elevation).collect(),
            source_indices: track
                .points
                .iter()
                .map(|point| point.source_index)
                .collect(),
        }
    }

    fn restore(&self) -> Result<Track, String> {
        let mut track = Track::parse(self.xml.clone())?;
        if track.points.len() != self.elevations.len()
            || track.points.len() != self.source_indices.len()
        {
            return Err("Project GPX sample counts do not match.".into());
        }
        for ((point, elevation), index) in track
            .points
            .iter_mut()
            .zip(&self.elevations)
            .zip(&self.source_indices)
        {
            if !elevation.is_finite() || elevation.abs() > 1_000_000.0 {
                return Err("A project elevation is out of range.".into());
            }
            point.elevation = *elevation;
            point.source_index = *index;
        }
        Ok(track)
    }
}

#[derive(Serialize, Deserialize)]
pub struct Project {
    format: String,
    version: u32,
    pub track: SavedTrack,
    pub source: SavedTrack,
    pub filename: String,
    pub name: String,
    pub description: String,
    pub source_path: Option<PathBuf>,
    pub selection: [f64; 2],
    pub chart: Chart,
    pub show_original: bool,
    pub trainer_mode: bool,
    pub studio_open: bool,
    pub studio: ProjectState,
    pub map: ProjectView,
}

pub struct LoadedProject {
    pub document: Project,
    pub track: Arc<Track>,
    pub source: Arc<Track>,
    pub studio: Studio,
    pub map: TrackMap,
}

impl Project {
    pub fn new(track: &Track, source: &Track, studio: ProjectState, map: ProjectView) -> Self {
        Self {
            format: FORMAT.into(),
            version: VERSION,
            track: SavedTrack::capture(track),
            source: SavedTrack::capture(source),
            filename: "smoothed.gpx".into(),
            name: track.name.clone(),
            description: track.description.clone(),
            source_path: None,
            selection: [0.0, track.total_distance()],
            chart: Chart::Elevation,
            show_original: true,
            trainer_mode: false,
            studio_open: false,
            studio,
            map,
        }
    }

    pub fn save(&self, path: &Path, video: Option<&Path>) -> Result<(), String> {
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gpxproj"))
        {
            return Err("Choose a .gpxproj filename for the saved project.".into());
        }
        for protected in [video, self.source_path.as_deref()].into_iter().flatten() {
            if path.canonicalize().is_ok_and(|target| {
                protected
                    .canonicalize()
                    .is_ok_and(|source| source == target)
            }) {
                return Err(
                    "A project cannot overwrite its referenced GPX or prepared video.".into(),
                );
            }
        }
        let bytes =
            serde_json::to_vec(self).map_err(|error| format!("Cannot encode project: {error}"))?;
        serde_json::from_slice::<Self>(&bytes)
            .map_err(|error| format!("Project contains an unsaveable value: {error}"))?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut file = tempfile::NamedTempFile::new_in(parent)
            .map_err(|error| format!("Cannot stage project: {error}"))?;
        file.write_all(&bytes)
            .and_then(|()| file.as_file().sync_all())
            .map_err(|error| format!("Cannot write project: {error}"))?;
        file.persist(path)
            .map_err(|error| format!("Cannot publish project {}: {error}", path.display()))?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<LoadedProject, String> {
        let path = std::path::absolute(path)
            .map_err(|error| format!("Cannot resolve project path: {error}"))?;
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("Cannot read project {}: {error}", path.display()))?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Invalid project JSON: {error}"))?;
        if !matches!(value["format"].as_str(), Some(FORMAT | LEGACY_FORMAT)) {
            return Err("This file is not an RLV Forge project.".into());
        }
        if value["version"].as_u64() != Some(u64::from(VERSION)) {
            return Err(
                "Unsupported project version. Open it with a compatible version of RLV Forge."
                    .into(),
            );
        }
        let mut document: Self = serde_json::from_value(value)
            .map_err(|error| format!("Invalid project data: {error}"))?;
        document.format = FORMAT.into();
        let directory = path
            .parent()
            .ok_or("The project has no parent directory.")?;
        document.studio.resolve_video(directory);
        if let Some(source) = &mut document.source_path {
            if source.is_relative() {
                *source = directory.join(&*source);
            }
        }
        let track = document.track.restore()?;
        let source = document.source.restore()?;
        if source
            .points
            .iter()
            .enumerate()
            .any(|(index, point)| point.source_index != Some(index))
        {
            return Err("The project's uncut GPX source has invalid sample provenance.".into());
        }
        for point in &track.points {
            if let Some(index) = point.source_index {
                let original = source
                    .points
                    .get(index)
                    .ok_or("Invalid source sample index in project.")?;
                if (point.latitude - original.latitude).abs() > 1e-7
                    || (point.longitude - original.longitude).abs() > 1e-7
                {
                    return Err("Project GPX samples do not match the uncut route.".into());
                }
            }
        }
        if document.selection.iter().any(|value| !value.is_finite())
            || document.selection[0] < 0.0
            || document.selection[1] < document.selection[0]
            || document.selection[1] > track.total_distance() + 1e-6
        {
            return Err("Invalid project chart selection.".into());
        }
        let studio = Studio::from_project(document.studio.clone(), &source, &track)?;
        let source = Arc::new(source);
        let map = TrackMap::from_project(&document.map, source.clone(), document.selection)?;
        Ok(LoadedProject {
            document,
            track: Arc::new(track),
            source,
            studio,
            map,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> Project {
        let track = Track::parse("<gpx><rte><rtept lat=\"0\" lon=\"0\"><ele>100</ele></rtept><rtept lat=\"0\" lon=\"0.01\"><ele>110</ele></rtept></rte></gpx>".into()).unwrap();
        let mut studio = Studio::default();
        studio.set_track(&track);
        Project::new(
            &track,
            &track,
            studio.project_state().unwrap(),
            TrackMap::offline().project_view(),
        )
    }

    #[test]
    fn legacy_projects_load_and_save_with_the_rlv_forge_identity() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("work.gpxproj");
        let mut value = serde_json::to_value(document()).unwrap();
        assert_eq!(value["format"], FORMAT);
        value["format"] = serde_json::json!(LEGACY_FORMAT);
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

        let loaded = Project::load(&path).unwrap();
        assert_eq!(loaded.document.format, FORMAT);
        loaded.document.save(&path, None).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["format"] = serde_json::json!(FORMAT);
        assert_eq!(saved, value);
        assert!(Project::load(&path).is_ok());
    }

    #[test]
    fn projects_validate_version_geometry_and_sample_provenance_before_loading() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("work.gpxproj");
        let original = serde_json::to_value(document()).unwrap();
        for (field, replacement) in [
            ("version", serde_json::json!(999)),
            ("format", serde_json::json!("some-other-file")),
            ("selection", serde_json::json!([2000.0, 3000.0])),
        ] {
            let mut value = original.clone();
            value[field] = replacement;
            std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(Project::load(&path).is_err());
        }
        for invalid in [
            {
                let mut value = original.clone();
                value["track"]["source_indices"][0] = serde_json::json!(500);
                value
            },
            {
                let mut value = original.clone();
                value["source"]["source_indices"][0] = serde_json::Value::Null;
                value
            },
            {
                let mut value = original.clone();
                value["track"]["elevations"] = serde_json::json!([100.0]);
                value
            },
            {
                let mut value = original.clone();
                value["track"]["xml"] = serde_json::json!("<bad/>");
                value
            },
            {
                let mut value = original.clone();
                value["map"]["zoom"] = serde_json::json!(1e100);
                value
            },
        ] {
            std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
            assert!(Project::load(&path).is_err());
        }
        std::fs::write(&path, b"{invalid json").unwrap();
        assert!(Project::load(&path).is_err());
    }

    #[test]
    fn project_saves_are_atomic_and_never_overwrite_referenced_source_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("work.gpxproj");
        let mut project = document();
        project.save(&path, None).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        project.track.elevations[0] = f64::NAN;
        assert!(project.save(&path, None).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let mut project = document();
        let media = directory.path().join("protected.gpxproj");
        std::fs::write(&media, b"unchanged source").unwrap();
        assert!(project.save(&media, Some(&media)).is_err());
        project.source_path = Some(media.clone());
        assert!(project.save(&media, None).is_err());
        assert_eq!(std::fs::read(&media).unwrap(), b"unchanged source");
        let gpx = directory.path().join("source.gpx");
        std::fs::write(&gpx, b"original GPX").unwrap();
        assert!(project.save(&gpx, None).is_err());
        assert_eq!(std::fs::read(&gpx).unwrap(), b"original GPX");
        assert!(Project::load(&path).is_ok());
    }
}
