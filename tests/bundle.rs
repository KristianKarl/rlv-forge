use rlv_forge::{
    bundle::{self, Export},
    gpx::{Operation, Track},
    sync::{Kind, SyncMap, SyncPoint},
    timeline::{self, Clip},
    video,
};
use std::{
    cell::RefCell,
    io::Read,
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
};

#[test]
fn exports_a_complete_bundle_with_a_byte_identical_prepared_video() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mp4");
    let result = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=12:duration=3",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=3",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(&source)
        .output()
        .expect("Run in nix develop for FFmpeg");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let original = std::fs::read(&source).unwrap();
    let points: String = (0..=16).map(|i| format!(
        "<trkpt lat=\"0\" lon=\"{}\"><ele>{}</ele><time>2026-01-01T00:00:{:06.3}Z</time><extensions><power>200</power></extensions></trkpt>",
        i as f64 * 0.0001, 100.0 + i as f64 * 0.2, i as f64 / 4.0,
    )).collect();
    let mut track =
        Track::parse(format!("<gpx><trk><trkseg>{points}</trkseg></trk></gpx>")).unwrap();
    track
        .apply(Operation::IncyclistSmooth, [0.0, track.total_distance()])
        .unwrap();
    let track = timeline::cut(
        &track,
        Clip {
            start: 1.0,
            end: 2.0,
        },
    )
    .unwrap();
    let destination = directory.path().join("bundle");
    let cancel = AtomicBool::new(false);
    let options = || Export {
        track: &track,
        video: &source,
        offset: 0.0,
        name: "Test & ride",
        description: "",
        country: "SE",
        sync: None,
    };
    let output = bundle::export(options(), &destination, &cancel).unwrap();
    assert!(output.archive.is_none());
    assert_eq!(output.frames, 36);
    assert_eq!(output.duration, 3.0);
    assert_eq!(std::fs::read(&source).unwrap(), original);
    assert_eq!(
        std::fs::read(destination.join("ride.mp4")).unwrap(),
        original
    );
    let rendered = video::probe(&destination.join("ride.mp4")).unwrap();
    assert_eq!(rendered.frames, 36);
    assert!(rendered.has_audio);
    let edited = Track::read(&destination.join("ride.gpx")).unwrap();
    assert_eq!(timeline::timing(&edited).unwrap().duration, 3.0);
    assert_eq!(edited.points.len(), output.points);
    let xml = std::fs::read_to_string(destination.join("ride.xml")).unwrap();
    let doc = roxmltree::Document::parse(&xml).unwrap();
    assert_eq!(doc.root_element().tag_name().name(), "gpx-import");
    assert!(xml.contains("<name>Test &amp; ride</name>"));
    assert!(xml.contains("<end-frame>36</end-frame>"));
    assert!(xml.contains("<cut time=\"0:00:01.000\"/>"));
    assert!(bundle::export(options(), &destination, &cancel).is_err());
    let cancelled = directory.path().join("cancelled");
    assert!(bundle::export(options(), &cancelled, &AtomicBool::new(true)).is_err());
    assert!(!cancelled.exists());
    let mov = directory.path().join("prepared.mov");
    std::fs::write(&mov, &original).unwrap();
    let mov_destination = directory.path().join("mov-bundle");
    bundle::export(
        Export {
            video: &mov,
            ..options()
        },
        &mov_destination,
        &cancel,
    )
    .unwrap();
    assert_eq!(
        std::fs::read(mov_destination.join("ride.mov")).unwrap(),
        original
    );
    let xml = std::fs::read_to_string(mov_destination.join("ride.xml")).unwrap();
    assert!(xml.contains("<video-file-path>ride.mov</video-file-path>"));
    let source_track =
        Track::parse(format!("<gpx><trk><trkseg>{points}</trkseg></trk></gpx>")).unwrap();
    let sync = SyncMap::new(
        &[
            SyncPoint {
                video: 0.0,
                gpx: 0.0,
                kind: Kind::Normal,
            },
            SyncPoint {
                video: 0.9,
                gpx: 1.0,
                kind: Kind::BeforeCut,
            },
            SyncPoint {
                video: 1.0 + 1.0 / 30.0,
                gpx: 2.0,
                kind: Kind::AfterCut,
            },
            SyncPoint {
                video: 3.0,
                gpx: 4.0,
                kind: Kind::Normal,
            },
        ],
        &[],
        4.0,
        3.0,
    )
    .unwrap();
    let synchronized = directory.path().join("synchronized");
    bundle::export(
        Export {
            track: &source_track,
            sync: Some(&sync),
            ..options()
        },
        &synchronized,
        &cancel,
    )
    .unwrap();
    assert_eq!(
        std::fs::read(synchronized.join("ride.mp4")).unwrap(),
        original
    );
    let synchronized_gpx = std::fs::read_to_string(synchronized.join("ride.gpx")).unwrap();
    assert_eq!(
        synchronized_gpx,
        sync.export(&source_track, "Test & ride", "").unwrap().gpx
    );
    let synchronized_xml = std::fs::read_to_string(synchronized.join("ride.xml")).unwrap();
    assert!(synchronized_xml.contains("<cut time=\"0:00:01.033\"/>"));

    let zipped = directory.path().join("zipped.bundle");
    let progress = RefCell::new(Vec::new());
    let output = bundle::export_with_progress(options(), &zipped, &cancel, true, |value| {
        progress.borrow_mut().push(value);
    })
    .unwrap();
    assert_eq!(
        output.archive.as_deref(),
        Some(directory.path().join("zipped.bundle.zip").as_path())
    );
    let values = progress.into_inner();
    assert_eq!(values.first(), Some(&0.0));
    assert_eq!(values.last(), Some(&1.0));
    assert!(values.windows(2).all(|pair| pair[0] <= pair[1]));
    let mut archive =
        zip::ZipArchive::new(std::fs::File::open(output.archive.unwrap()).unwrap()).unwrap();
    assert_eq!(archive.len(), 3);
    for filename in ["ride.mp4", "ride.gpx", "ride.xml"] {
        let mut bytes = Vec::new();
        archive
            .by_name(filename)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, std::fs::read(zipped.join(filename)).unwrap());
    }

    let cancelled_zip = directory.path().join("cancelled-zip");
    let cancel_zip = AtomicBool::new(false);
    assert!(
        bundle::export_with_progress(options(), &cancelled_zip, &cancel_zip, true, |value| {
            if value >= 0.6 {
                cancel_zip.store(true, Ordering::Relaxed);
            }
        })
        .unwrap_err()
        .contains("cancelled")
    );
    assert!(!cancelled_zip.exists());
    assert!(!directory.path().join("cancelled-zip.zip").exists());

    let collision = directory.path().join("collision");
    let existing_zip = directory.path().join("collision.zip");
    std::fs::write(&existing_zip, b"existing archive").unwrap();
    assert!(bundle::export_with_progress(options(), &collision, &cancel, true, |_| {}).is_err());
    assert!(!collision.exists());
    assert_eq!(std::fs::read(&existing_zip).unwrap(), b"existing archive");

    let raced = directory.path().join("raced");
    let raced_zip = directory.path().join("raced.zip");
    assert!(
        bundle::export_with_progress(options(), &raced, &cancel, true, |value| {
            if value == 0.98 {
                std::fs::write(&raced_zip, b"concurrent archive").unwrap();
            }
        })
        .is_err()
    );
    assert!(!raced.exists());
    assert_eq!(std::fs::read(&raced_zip).unwrap(), b"concurrent archive");
    assert_eq!(std::fs::read(&source).unwrap(), original);
}
