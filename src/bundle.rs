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

#[derive(Debug)]
pub struct ExportedBundle {
    pub directory: PathBuf,
    pub archive: Option<PathBuf>,
    pub duration: f64,
    pub frames: u64,
    pub points: usize,
}

pub fn export(
    options: Export<'_>,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<ExportedBundle, String> {
    export_with_progress(options, destination, cancel, false, |_| {})
}

pub fn export_with_progress(
    options: Export<'_>,
    destination: &Path,
    cancel: &AtomicBool,
    archive: bool,
    progress: impl Fn(f32),
) -> Result<ExportedBundle, String> {
    progress(0.0);
    if destination.symlink_metadata().is_ok() {
        return Err("The destination already exists. Choose a new bundle folder; existing files are never overwritten.".into());
    }
    let archive_path = if archive {
        let mut name = destination
            .file_name()
            .ok_or("The bundle needs a folder name.")?
            .to_os_string();
        name.push(".zip");
        let path = destination.with_file_name(name);
        if path.symlink_metadata().is_ok() {
            return Err("The ZIP destination already exists. Choose a new bundle folder; existing archives are never overwritten.".into());
        }
        Some(path)
    } else {
        None
    };
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let info = video::probe(options.video)?;
    progress(0.05);
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
    progress(0.1);
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
    let video_size = std::fs::metadata(options.video)
        .map_err(|error| format!("Cannot read prepared video size: {error}"))?
        .len();
    copy_video(
        options.video,
        &staging.path().join(&video_filename),
        cancel,
        &|bytes| {
            let fraction = bytes as f32 / video_size.max(1) as f32;
            progress(0.1 + fraction.min(1.0) * if archive { 0.5 } else { 0.85 });
        },
    )?;
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
    let filenames = [video_filename.as_str(), "ride.gpx", "ride.xml"];
    let staged_archive = if archive {
        let path = staging.path().join("bundle.zip");
        write_archive(&path, staging.path(), &filenames, cancel, &progress)?;
        Some(path)
    } else {
        None
    };
    progress(0.98);
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
    let mut files: Vec<_> = filenames
        .iter()
        .map(|filename| (staging.path().join(filename), destination.join(filename)))
        .collect();
    if let (Some(staged), Some(path)) = (staged_archive, &archive_path) {
        files.push((staged, path.clone()));
    }
    for (staged, path) in files {
        if let Err(error) = std::fs::hard_link(staged, &path) {
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
    progress(1.0);
    Ok(ExportedBundle {
        directory: destination.to_path_buf(),
        archive: archive_path,
        duration: info.duration,
        frames: info.frames,
        points: edited.points,
    })
}

fn copy_video(
    source: &Path,
    destination: &Path,
    cancel: &AtomicBool,
    progress: &impl Fn(u64),
) -> Result<(), String> {
    let mut input = std::fs::File::open(source)
        .map_err(|error| format!("Cannot open prepared video: {error}"))?;
    let mut output = std::fs::File::create(destination)
        .map_err(|error| format!("Cannot create video copy: {error}"))?;
    copy_bytes(&mut input, &mut output, cancel, progress)
}

fn copy_bytes(
    input: &mut impl std::io::Read,
    output: &mut impl std::io::Write,
    cancel: &AtomicBool,
    progress: &impl Fn(u64),
) -> Result<(), String> {
    let mut buffer = vec![0; 1024 * 1024];
    let mut copied = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Incyclist export cancelled.".into());
        }
        let count = input
            .read(&mut buffer)
            .map_err(|error| format!("Cannot read bundle data: {error}"))?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .map_err(|error| format!("Cannot write bundle data: {error}"))?;
        copied += count as u64;
        progress(copied);
    }
    Ok(())
}

fn write_archive(
    destination: &Path,
    directory: &Path,
    filenames: &[&str],
    cancel: &AtomicBool,
    progress: &impl Fn(f32),
) -> Result<(), String> {
    let file = std::fs::File::create(destination)
        .map_err(|error| format!("Cannot create staged ZIP archive: {error}"))?;
    let mut archive = zip::ZipWriter::new(file);
    let sizes = filenames
        .iter()
        .map(|filename| {
            std::fs::metadata(directory.join(filename))
                .map(|metadata| metadata.len())
                .map_err(|error| format!("Cannot read ZIP input size: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let total = sizes.iter().sum::<u64>().max(1);
    let mut completed = 0;
    for (filename, size) in filenames.iter().zip(sizes) {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(size >= u64::from(u32::MAX));
        archive
            .start_file(*filename, options)
            .map_err(|error| format!("Cannot add {filename} to ZIP: {error}"))?;
        let mut input = std::fs::File::open(directory.join(filename))
            .map_err(|error| format!("Cannot open ZIP input {filename}: {error}"))?;
        copy_bytes(&mut input, &mut archive, cancel, &|bytes| {
            progress(0.6 + (completed + bytes) as f32 / total as f32 * 0.35);
        })?;
        completed += size;
    }
    archive
        .finish()
        .map_err(|error| format!("Cannot finish ZIP archive: {error}"))?;
    Ok(())
}
