use rlv_forge::{
    gpx::Track,
    sync::{Kind, SyncMap, SyncPoint},
    timeline::{self, Clip},
};

fn ride() -> Track {
    let points: String = (0..=60).map(|i| format!(
        "<trkpt lat=\"0\" lon=\"{}\"><ele>{}</ele><time>2026-01-01T00:{:02}:{:02}Z</time><extensions><power>{}</power></extensions></trkpt>",
        i as f64 * 0.0001, 100 + i, i / 60, i % 60, 200 + i,
    )).collect();
    Track::parse(format!("<gpx><trk><trkseg>{points}</trkseg></trk></gpx>")).unwrap()
}

fn point(video: f64, gpx: f64, kind: Kind) -> SyncPoint {
    SyncPoint { video, gpx, kind }
}

#[test]
fn landmarks_support_offset_drift_inverse_seeking_and_exact_export_landmarks() {
    let single = SyncMap::new(&[point(5.0, 10.0, Kind::Normal)], &[], 60.0, 40.0).unwrap();
    assert_eq!(single.edited_at(15.0), 20.0);
    assert_eq!(single.video_at(20.0), 15.0);
    let map = SyncMap::new(
        &[
            point(0.0, 2.0, Kind::Normal),
            point(10.0, 12.5, Kind::Normal),
            point(30.0, 36.0, Kind::Normal),
        ],
        &[],
        60.0,
        30.0,
    )
    .unwrap();
    assert!((map.edited_at(20.0) - 24.25).abs() < 1e-9);
    assert!((map.video_at(24.25) - 20.0).abs() < 1e-9);
    let source = ride();
    let original = source.clone();
    let exported = map.export(&source, "Sync", "").unwrap();
    let track = Track::parse(exported.gpx).unwrap();
    let timing = timeline::timing(&track).unwrap();
    assert_eq!(timing.duration, 30.0);
    let position = timeline::position_at(&timing, &track, 10.0).unwrap();
    assert!((position[0] - 0.00125).abs() < 1e-9);
    assert_eq!(source, original);
}

#[test]
fn paired_jump_boundaries_hold_then_skip_and_level_the_export() {
    let map = SyncMap::new(
        &[
            point(0.0, 0.0, Kind::Normal),
            point(10.0, 10.0, Kind::BeforeCut),
            point(10.04, 30.0, Kind::AfterCut),
            point(40.04, 60.0, Kind::Normal),
        ],
        &[],
        60.0,
        40.04,
    )
    .unwrap();
    assert_eq!(map.original_at(10.02).unwrap(), 10.0);
    assert_eq!(map.original_at(10.04).unwrap(), 30.0);
    assert!((map.original_at(20.04).unwrap() - 40.0).abs() < 1e-9);
    assert_eq!(map.video_at(10.0), 10.04);
    let output = map.export(&ride(), "Jump", "").unwrap();
    assert_eq!(output.cuts, [10.04]);
    let track = Track::parse(output.gpx.clone()).unwrap();
    let timing = timeline::timing(&track).unwrap();
    assert_eq!(timing.duration, 40.04);
    let before = timing
        .seconds
        .iter()
        .position(|time| *time == 10.0)
        .unwrap();
    let after = timing
        .seconds
        .iter()
        .position(|time| *time == 10.04)
        .unwrap();
    assert_eq!(
        track.points[before].elevation,
        track.points[after].elevation
    );
    assert_ne!(track.points[before].segment, track.points[after].segment);
    for pair in track.points.windows(2) {
        if pair[0].segment == pair[1].segment {
            assert!(
                (pair[1].elevation
                    - pair[0].elevation
                    - (pair[1].longitude - pair[0].longitude) * 10_000.0)
                    .abs()
                    < 1e-6
            );
        }
    }
    assert!(!output.gpx.contains("<power>220</power>"));
    assert!(output.gpx.contains("<power>230</power>"));
    timeline::validate_incyclist(&output).unwrap();
}

#[test]
fn fractional_frame_cut_markers_match_serialized_gpx_samples() {
    for (before, after) in [
        (10.0, 10.0 + 1.0 / 30.0),
        (10.2217391304348, 10.1426858513186),
    ] {
        let map = SyncMap::new(
            &[
                point(0.0, 0.0, Kind::Normal),
                point(10.0, before, Kind::BeforeCut),
                point(after, 30.5, Kind::AfterCut),
                point(after + 30.0, 60.0, Kind::Normal),
            ],
            &[],
            60.0,
            after + 30.0,
        )
        .unwrap();
        let output = map.export(&ride(), "Fractional cut", "").unwrap();
        let track = Track::parse(output.gpx.clone()).unwrap();
        let timing = timeline::timing(&track).unwrap();
        let cut = (after * 1000.0).round() / 1000.0;
        assert!((output.cuts[0] - after).abs() < 1e-9);
        assert!(timing.seconds.contains(&cut));
        timeline::validate_incyclist(&output).unwrap();
        let mut invalid = output;
        invalid.cuts[0] = cut + 0.001;
        assert!(timeline::validate_incyclist(&invalid)
            .unwrap_err()
            .contains("no matching moving sample"));
    }
}

#[test]
fn manual_cuts_remain_a_fallback_and_bad_sync_points_are_explicit_errors() {
    let points = [
        point(0.0, 0.0, Kind::Normal),
        point(58.0, 60.0, Kind::Normal),
    ];
    let map = SyncMap::new(
        &points,
        &[Clip {
            start: 2.0,
            end: 4.0,
        }],
        60.0,
        58.0,
    )
    .unwrap();
    assert_eq!(map.original_at(2.0).unwrap(), 4.0);
    assert_eq!(map.export(&ride(), "", "").unwrap().duration, 58.0);
    for invalid in [
        vec![point(1.0, 5.0, Kind::Normal), point(2.0, 4.0, Kind::Normal)],
        vec![point(1.0, 5.0, Kind::Normal), point(1.0, 6.0, Kind::Normal)],
        vec![point(f64::NAN, 5.0, Kind::Normal)],
        vec![point(1.0, 61.0, Kind::Normal)],
    ] {
        assert!(SyncMap::new(&invalid, &[], 60.0, 40.0).is_err());
    }

    assert!(SyncMap::new(
        &[point(1.0, 3.0, Kind::Normal)],
        &[Clip {
            start: 2.0,
            end: 4.0
        }],
        60.0,
        40.0,
    )
    .is_err());
    let uncovered = SyncMap::new(&[point(10.0, 0.0, Kind::Normal)], &[], 60.0, 40.0).unwrap();
    assert!(uncovered.export(&ride(), "", "").is_err());
}

#[test]
fn unpaired_cut_boundaries_align_as_landmarks_until_the_pair_is_complete() {
    let before = point(10.0, 10.0, Kind::BeforeCut);
    let after = point(10.04, 30.0, Kind::AfterCut);
    for boundary in [before, after] {
        let map = SyncMap::new(&[boundary], &[], 60.0, 40.0).unwrap();
        let landmark = SyncMap::new(
            &[point(boundary.video, boundary.gpx, Kind::Normal)],
            &[],
            60.0,
            40.0,
        )
        .unwrap();
        assert!(map.has_unpaired_boundaries());
        assert!(map.cuts().is_empty());
        assert_eq!(map.original_at(boundary.video).unwrap(), boundary.gpx);
        for video in [0.0, 5.0, 10.0, 30.0, 40.0] {
            assert_eq!(map.edited_at(video), landmark.edited_at(video));
        }
        assert_eq!(
            map.export(&ride(), "", "").unwrap().gpx,
            landmark.export(&ride(), "", "").unwrap().gpx
        );
    }
    let map = SyncMap::new(&[after, before], &[], 60.0, 40.0).unwrap();
    assert!(!map.has_unpaired_boundaries());
    assert_eq!(
        map.cuts(),
        &[Clip {
            start: 10.0,
            end: 30.0
        }]
    );
    assert_eq!(map.original_at(10.02).unwrap(), 10.0);
    assert_eq!(map.original_at(10.04).unwrap(), 30.0);
    assert_eq!(map.export(&ride(), "", "").unwrap().cuts, [10.04]);
}

#[test]
fn export_rejects_colliding_timestamps_and_uncovered_jump_endpoints() {
    let xml = ride().export("", "").unwrap();
    assert!(timeline::retime_gpx(&xml, &[0.0]).is_err());
    let mut times: Vec<_> = (0..=60).map(f64::from).collect();
    times[1] = 0.0001;
    assert!(timeline::retime_gpx(&xml, &times).is_err());
    for bad in [f64::NAN, f64::INFINITY, -1.0] {
        times[1] = bad;
        assert!(timeline::retime_gpx(&xml, &times).is_err());
    }
    for points in [
        vec![
            point(0.0, 10.0, Kind::BeforeCut),
            point(0.04, 30.0, Kind::AfterCut),
        ],
        vec![
            point(9.96, 10.0, Kind::BeforeCut),
            point(10.0, 30.0, Kind::AfterCut),
        ],
    ] {
        let map = SyncMap::new(&points, &[], 60.0, 10.0).unwrap();
        assert!(map.export(&ride(), "", "").is_err());
    }
    assert!(SyncMap::new(
        &[
            point(10.0, 10.0, Kind::BeforeCut),
            point(10.04, 30.0, Kind::AfterCut),
        ],
        &[Clip {
            start: 5.0,
            end: 10.0
        }],
        60.0,
        40.0
    )
    .is_err());
}

#[test]
fn sync_locations_cannot_interpolate_across_disconnected_recordings() {
    let source = ride().export("", "").unwrap().replace(
        "<trkpt lat=\"0\" lon=\"0.003\">",
        "</trkseg><trkseg><trkpt lat=\"0\" lon=\"0.003\">",
    );
    let track = Track::parse(source).unwrap();
    let map = SyncMap::new(&[point(29.5, 29.5, Kind::Normal)], &[], 60.0, 60.0).unwrap();
    assert!(map.validate_source(&track).is_err());
    assert!(map.export(&track, "", "").is_err());
}
