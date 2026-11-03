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
fn video_start_trims_the_prefix_and_exports_from_video_zero_without_changing_source() {
    let source = ride();
    let original = source.clone();
    for start in [0.0, 10.25] {
        let mapping = SyncMap::new(
            &[point(0.0, start, Kind::VideoStart)],
            &[],
            60.0,
            60.0 - start,
        )
        .unwrap();
        assert_eq!(mapping.edited_at(0.0), 0.0);
        assert_eq!(mapping.original_at(0.0).unwrap(), start);
        assert_eq!(mapping.video_at(0.0), 0.0);
        let track = timeline::cut_ranges(&source, mapping.cuts()).unwrap();
        assert!((track.points[0].longitude - start * 0.0001).abs() < 1e-9);
        assert_eq!(timeline::timing(&track).unwrap().duration, 60.0 - start);
        let exported = mapping.export(&source, "Start", "").unwrap();
        timeline::validate_incyclist(&exported).unwrap();
        let exported = Track::parse(exported.gpx).unwrap();
        assert!((exported.points[0].longitude - start * 0.0001).abs() < 1e-9);
        assert_eq!(
            timeline::timing(&exported).unwrap().start,
            "2000-01-01T00:00:00Z"
        );
        assert_eq!(source, original);
    }
    for (video, gpx) in [(1.0, 10.0), (0.0, 60.0)] {
        assert!(SyncMap::new(&[point(video, gpx, Kind::VideoStart)], &[], 60.0, 40.0).is_err());
    }
}

#[test]
fn video_end_trims_the_tail_and_holds_the_last_position_for_the_final_frame() {
    let source = ride();
    let original = source.clone();
    for fps in [25.0, 30_000.0 / 1001.0] {
        let last_frame = 40.0 - 1.0 / fps;
        let mapping = SyncMap::new(
            &[
                point(0.0, 10.0, Kind::VideoStart),
                point(last_frame, 50.0, Kind::VideoEnd),
            ],
            &[],
            60.0,
            40.0,
        )
        .unwrap();
        assert_eq!(
            mapping.cuts(),
            &[
                Clip {
                    start: 0.0,
                    end: 10.0
                },
                Clip {
                    start: 50.0,
                    end: 60.0
                }
            ]
        );
        assert_eq!(mapping.original_at(last_frame).unwrap(), 50.0);
        assert_eq!(mapping.original_at(40.0).unwrap(), 50.0);
        assert_eq!(mapping.edited_at(40.0), 40.0);
        assert_eq!(mapping.edited_rate_at(last_frame), 0.0);
        assert_eq!(mapping.video_at(40.0), last_frame);
        assert!(mapping.coverage_error().is_none());
        let track = timeline::cut_ranges(&source, mapping.cuts()).unwrap();
        assert!((track.points.last().unwrap().longitude - 0.005).abs() < 1e-9);
        let exported = mapping.export(&source, "End", "").unwrap();
        timeline::validate_incyclist(&exported).unwrap();
        assert_eq!(exported.source_times.len(), exported.points);
        assert_eq!(exported.source_indices.len(), exported.points);
        let track = Track::parse(exported.gpx).unwrap();
        let timing = timeline::timing(&track).unwrap();
        assert_eq!(timing.duration, 40.0);
        assert_eq!(
            timing.seconds[timing.seconds.len() - 2],
            (last_frame * 1000.0).round() / 1000.0
        );
        let last = &track.points[track.points.len() - 2..];
        assert_eq!(last[0].longitude, last[1].longitude);
        assert_eq!(last[0].elevation, last[1].elevation);
        assert!((last[1].longitude - 0.005).abs() < 1e-9);
        assert_eq!(source, original);
    }
    for points in [
        vec![point(0.0, 50.0, Kind::VideoEnd)],
        vec![point(39.96, 0.0, Kind::VideoEnd)],
        vec![
            point(39.96, 50.0, Kind::VideoEnd),
            point(40.0, 55.0, Kind::Normal),
        ],
    ] {
        assert!(SyncMap::new(&points, &[], 60.0, 40.0).is_err());
    }
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

#[test]
fn coverage_reports_both_missing_ends_and_suggests_only_boundary_landmarks() {
    let points = [
        point(10.0, 5.0, Kind::Normal),
        point(30.0, 55.0, Kind::Normal),
    ];
    let map = SyncMap::new(&points, &[], 60.0, 40.0).unwrap();
    let error = map.export(&ride(), "", "").unwrap_err();
    assert!(error.contains("Start: video 0.000 to 8.000 s"));
    assert!(error.contains("End: video 32.000 to 40.000 s"));
    assert!(error.contains("20.000 s before retained GPX start"));
    assert!(error.contains("20.000 s beyond retained GPX end"));
    assert!(error.contains("First sync point: video 10.000 s / original GPX 5.000 s"));
    assert!(error.contains("Last sync point: video 30.000 s / original GPX 55.000 s"));
    let additions = map.suggest_coverage_points().unwrap();
    assert_eq!(
        additions,
        [
            point(0.0, 0.0, Kind::Normal),
            point(40.0, 60.0, Kind::Normal)
        ]
    );
    let mut corrected = points.to_vec();
    corrected.extend(additions);
    let corrected = SyncMap::new(&corrected, &[], 60.0, 40.0).unwrap();
    assert!(corrected.coverage_error().is_none());
    assert!(corrected.suggest_coverage_points().unwrap().is_empty());
    assert_eq!(corrected.edited_at(20.0), map.edited_at(20.0));
    timeline::validate_incyclist(&corrected.export(&ride(), "", "").unwrap()).unwrap();
    assert!(map.coverage_error().is_some());
}

#[test]
fn boundary_suggestions_respect_manual_cuts_and_preserve_paired_jumps() {
    let cuts = [
        Clip {
            start: 0.0,
            end: 2.0,
        },
        Clip {
            start: 58.0,
            end: 60.0,
        },
    ];
    let points = [
        point(10.0, 5.0, Kind::Normal),
        point(15.0, 15.0, Kind::BeforeCut),
        point(15.04, 25.0, Kind::AfterCut),
        point(30.0, 55.0, Kind::Normal),
    ];
    let map = SyncMap::new(&points, &cuts, 60.0, 40.0).unwrap();
    let additions = map.suggest_coverage_points().unwrap();
    assert_eq!(
        additions,
        [
            point(0.0, 2.0, Kind::Normal),
            point(40.0, 58.0, Kind::Normal)
        ]
    );
    let mut corrected = points.to_vec();
    corrected.extend(additions);
    let corrected = SyncMap::new(&corrected, &cuts, 60.0, 40.0).unwrap();
    assert_eq!(corrected.cuts(), map.cuts());
    assert_eq!(corrected.original_at(15.02).unwrap(), 15.0);
    assert_eq!(corrected.original_at(15.04).unwrap(), 25.0);
    let exported = corrected.export(&ride(), "", "").unwrap();
    assert_eq!(exported.cuts, [15.04]);
    timeline::validate_incyclist(&exported).unwrap();
}

#[test]
fn boundary_suggestions_reject_conflicts_instead_of_overwriting_landmarks() {
    // The existing landmark already uses the first route location later in the video.
    let map = SyncMap::new(&[point(10.0, 0.0, Kind::Normal)], &[], 60.0, 40.0).unwrap();
    assert!(map
        .suggest_coverage_points()
        .unwrap_err()
        .contains("conflicting"));
    let hold = SyncMap::new(
        &[
            point(0.0, 10.0, Kind::BeforeCut),
            point(10.0, 30.0, Kind::AfterCut),
        ],
        &[],
        60.0,
        10.0,
    )
    .unwrap();
    assert!(hold
        .coverage_error()
        .unwrap()
        .contains("no forward GPX duration"));
    assert!(hold.suggest_coverage_points().is_err());
}

#[test]
fn october_project_needs_only_a_start_landmark_not_an_end_correction() {
    let points = [
        point(16.602659063885962, 15.6, Kind::Normal),
        point(1077.539208790536, 1076.5365497266498, Kind::BeforeCut),
        point(1077.822659063886, 1093.3, Kind::AfterCut),
        point(1347.760037109964, 1363.237378046078, Kind::BeforeCut),
        point(1348.0800371099638, 1378.3, Kind::AfterCut),
        point(1388.8600371099637, 1419.0799999999997, Kind::BeforeCut),
        point(1389.1400371099635, 1443.6, Kind::AfterCut),
        point(1713.7038830896113, 1768.1638459796457, Kind::BeforeCut),
        point(1713.7638830896112, 1775.2, Kind::AfterCut),
        point(2015.6461093372397, 2077.0822262476263, Kind::BeforeCut),
        point(2015.8461093372396, 2097.8, Kind::AfterCut),
        point(4768.360000000014, 4850.313890662754, Kind::BeforeCut),
        point(4768.500000000017, 4868.5, Kind::AfterCut),
    ];
    let map = SyncMap::new(&points, &[], 5003.0, 4902.02).unwrap();
    let error = map.coverage_error().unwrap();
    assert!(error.contains("Start: video 0.000 to 1.003 s"));
    assert!(!error.contains("End: video"));
    let additions = map.suggest_coverage_points().unwrap();
    assert_eq!(additions, [point(0.0, 0.0, Kind::Normal)]);
    let mut corrected = points.to_vec();
    corrected.extend(additions);
    let corrected = SyncMap::new(&corrected, &[], 5003.0, 4902.02).unwrap();
    assert!(corrected.coverage_error().is_none());
    for pair in points.windows(2) {
        let time = (pair[0].video + pair[1].video) / 2.0;
        assert_eq!(corrected.edited_at(time), map.edited_at(time));
    }
}
