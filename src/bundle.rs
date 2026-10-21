use crate::{
    gpx::Track,
    sync::SyncMap,
    timeline::{self, Clip},
    video,
};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

pub struct Export<'a> {
    pub track: &'a Track,
    pub video: &'a Path,
    pub offset: f64,
    pub name: &'a str,
    pub description: &'a str,
    pub country: &'a str,
    pub sync: Option<&'a SyncMap>,
}

pub struct ExportedBundle {
    pub directory: PathBuf,
    pub duration: f64,
    pub frames: u64,
    pub points: usize,
}

pub fn export(
    options: Export<'_>,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<ExportedBundle, String> {
    if destination.symlink_metadata().is_ok() {
        return Err("The destination already exists. Choose a new bundle folder; existing files are never overwritten.".into());
    }
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let info = video::probe(options.video)?;
    let edited = if let Some(sync) = options.sync {
        sync.export(options.track, options.name, options.description)?
    } else {
        timeline::edit_gpx(
            options.track,
            options.name,
            options.description,
            &[Clip {
                start: 0.0,
                end: info.duration,
            }],
            options.offset,
        )?
    };
    if (edited.duration - info.duration).abs() > 0.001 {
        return Err("The synchronized GPX duration does not match the prepared video.".into());
    }
    timeline::validate_incyclist(&edited)?;
    if cancel.load(Ordering::Relaxed) {
        return Err("Incyclist export cancelled.".into());
    }
    let staging = tempfile::Builder::new()
        .prefix(".incyclist-")
        .tempdir_in(parent)
        .map_err(|error| format!("Cannot create bundle staging directory: {error}"))?;
    let video_filename = format!(
        "ride.{}",
        options
            .video
            .extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| !extension.is_empty())
            .ok_or("The prepared video needs a filename extension.")?
    );
    copy_video(options.video, &staging.path().join(&video_filename), cancel)?;
    let pairing = video::pairing_xml(
        options.name,
        options.country,
        &video_filename,
        "ride.gpx",
        &info,
    )?;
    let pairing = timeline::add_cut_markers(&pairing, &edited.cuts)?;
    for (filename, content) in [("ride.gpx", edited.gpx), ("ride.xml", pairing)] {
        std::fs::write(staging.path().join(filename), content)
            .map_err(|error| format!("Cannot write staged {filename}: {error}"))?;
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("Incyclist export cancelled.".into());
    }
    std::fs::create_dir(destination).map_err(|error| {
        format!(
            "Cannot create bundle folder {}: {error}",
            destination.display()
        )
    })?;
    let mut published = Vec::new();
    for filename in [video_filename.as_str(), "ride.gpx", "ride.xml"] {
        let path = destination.join(filename);
        if let Err(error) = std::fs::hard_link(staging.path().join(filename), &path) {
            let mut cleanup_errors = Vec::new();
            for file in published {
                if let Err(cleanup) = std::fs::remove_file(file) {
                    cleanup_errors.push(cleanup.to_string());
                }
            }
            if let Err(cleanup) = std::fs::remove_dir(destination) {
                cleanup_errors.push(cleanup.to_string());
            }
            return Err(format!(
                "Cannot publish bundle: {error}. Cleanup errors: {cleanup_errors:?}"
            ));
        }
        published.push(path);
    }
    Ok(ExportedBundle {
        directory: destination.to_path_buf(),
        duration: info.duration,
        frames: info.frames,
        points: edited.points,
    })
}

fn copy_video(source: &Path, destination: &Path, cancel: &AtomicBool) -> Result<(), String> {
    use std::io::{Read, Write};
    let mut input = std::fs::File::open(source)
        .map_err(|error| format!("Cannot open prepared video: {error}"))?;
    let mut output = std::fs::File::create(destination)
        .map_err(|error| format!("Cannot create video copy: {error}"))?;
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Incyclist export cancelled.".into());
        }
        let count = input
            .read(&mut buffer)
            .map_err(|error| format!("Cannot read prepared video: {error}"))?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .map_err(|error| format!("Cannot copy prepared video: {error}"))?;
    }
    Ok(())
}
