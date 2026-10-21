use rlv_forge::{
    gpx::Track,
    timeline::{self, Clip, CutRange},
};

#[test]
fn profile_distance_seeking_interpolates_time_and_keeps_stops_and_cut_boundaries_unambiguous() {
    let track = ride();
    let timing = timeline::timing(&track).unwrap();
    for seconds in [0.0, 0.5, 3.25, 9.9, 10.0] {
        let distance = timeline::distance_at(&timing, &track, seconds).unwrap();
        assert!(
            (timeline::time_at_distance(&timing, &track, distance, 0.0).unwrap() - seconds).abs()
                < 1e-9
        );
    }
    let cut = timeline::cut(
        &track,
        Clip {
            start: 2.0,
            end: 7.0,
        },
    )
    .unwrap();
    let timing = timeline::timing(&cut).unwrap();
    for seconds in [0.0, 0.5, 1.9, 2.0, 2.1, 5.0] {
        let distance = timeline::distance_at(&timing, &cut, seconds).unwrap();
        assert!(
            (timeline::time_at_distance(&timing, &cut, distance, seconds).unwrap() - seconds).abs()
                < 1e-9
        );
    }
    let track = Track::parse(
        "<gpx><trk><trkseg>\
        <trkpt lat=\"0\" lon=\"0\"><ele>100</ele><time>2026-01-01T00:00:00Z</time></trkpt>\
        <trkpt lat=\"0\" lon=\"0\"><ele>100</ele><time>2026-01-01T00:00:10Z</time></trkpt>\
        <trkpt lat=\"0\" lon=\"0.001\"><ele>110</ele><time>2026-01-01T00:00:20Z</time></trkpt>\
        </trkseg><trkseg>\
        <trkpt lat=\"50\" lon=\"100\"><ele>500</ele><time>2026-01-01T00:00:30Z</time></trkpt>\
        </trkseg></trk></gpx>"
            .into(),
    )
    .unwrap();
    let timing = timeline::timing(&track).unwrap();
    assert_eq!(
        timeline::time_at_distance(&timing, &track, 0.0, 5.0),
        Some(5.0)
    );
    assert_eq!(
        timeline::time_at_distance(&timing, &track, 0.0, 15.0),
        Some(10.0)
    );
    assert_eq!(
        timeline::time_at_distance(&timing, &track, track.total_distance(), 25.0),
        Some(20.0)
    );
    assert_eq!(
        timeline::time_at_distance(&timing, &track, track.total_distance(), 28.0),
        Some(30.0)
    );
    assert_eq!(
        timeline::time_at_distance(&timing, &track, track.total_distance() / 2.0, 0.0),
        Some(15.0)
    );
    for distance in [-1.0, f64::NAN, f64::INFINITY, track.total_distance() + 1.0] {
        assert!(timeline::time_at_distance(&timing, &track, distance, 0.0).is_none());
    }
    assert!(timeline::time_at_distance(&timing, &track, 0.0, f64::NAN).is_none());
}

#[test]
fn speed_uses_connected_gpx_intervals_and_handles_stops_boundaries_and_gaps() {
    let track = Track::parse(
        "<gpx><trk><trkseg>\
        <trkpt lat=\"0\" lon=\"0\"><ele>100</ele><time>2026-01-01T00:00:00Z</time></trkpt>\
        <trkpt lat=\"0\" lon=\"0\"><ele>100</ele><time>2026-01-01T00:00:10Z</time></trkpt>\
        <trkpt lat=\"0\" lon=\"0.001\"><ele>110</ele><time>2026-01-01T00:00:20Z</time></trkpt>\
        </trkseg><trkseg>\
        <trkpt lat=\"50\" lon=\"100\"><ele>500</ele><time>2026-01-01T00:00:30Z</time></trkpt>\
        <trkpt lat=\"50\" lon=\"100.001\"><ele>510</ele><time>2026-01-01T00:00:40Z</time></trkpt>\
        </trkseg></trk></gpx>"
            .into(),
    )
    .unwrap();
    let timing = timeline::timing(&track).unwrap();
    for seconds in [0.0, 5.0] {
        assert_eq!(timeline::speed_at(&timing, &track, seconds), Some(0.0));
    }
    for seconds in [10.0, 15.0, 20.0] {
        let speed = timeline::speed_at(&timing, &track, seconds).unwrap();
        assert!((speed - track.points[2].step / 10.0).abs() < 1e-9);
    }
    for seconds in [30.0, 35.0, 40.0] {
        let speed = timeline::speed_at(&timing, &track, seconds).unwrap();
        assert!((speed - track.points[4].step / 10.0).abs() < 1e-9);
    }
    for seconds in [-1.0, 25.0, 41.0, f64::NAN, f64::INFINITY] {
        assert!(timeline::speed_at(&timing, &track, seconds).is_none());
    }
}

#[test]
fn original_clock_cut_edits_do_not_reinterpret_later_rows() {
    let track = ride();
    let cuts = [
        CutRange::Time(Clip {
            start: 1.0,
            end: 3.0,
        }),
        CutRange::Time(Clip {
            start: 6.0,
            end: 8.0,
        }),
    ];
    let before = timeline::rebuild_cuts(&track, &cuts).unwrap();
    assert_eq!(timeline::timing(&before).unwrap().duration, 6.0);
    let revised = [
        CutRange::Time(Clip {
            start: 1.0,
            end: 2.0,
        }),
        cuts[1].clone(),
    ];
    let output = timeline::rebuild_cuts(&track, &revised).unwrap();
    assert_eq!(timeline::timing(&output).unwrap().duration, 7.0);
    for index in [6, 7] {
        assert!(!output
            .points
            .iter()
            .any(|point| point.source_index == Some(index)));
    }
    assert!(output
        .points
        .iter()
        .any(|point| point.source_index == Some(2)));
    let timing = timeline::timing(&output).unwrap();
    assert!(
        (timeline::position_at(&timing, &output, 5.0).unwrap()[0] - track.points[8].longitude)
            .abs()
            < 1e-9
    );
    for pair in output
        .points
        .windows(2)
        .filter(|pair| pair[0].segment != pair[1].segment)
    {
        assert!((pair[0].elevation - pair[1].elevation).abs() < 1e-6);
    }
}

#[test]
fn overlapping_cuts_are_a_union_and_original_time_mapping_distinguishes_seam_edges() {
    let cuts = [
        Clip {
            start: 3.0,
            end: 7.0,
        },
        Clip {
            start: 5.0,
            end: 8.0,
        },
    ];
    assert_eq!(
        timeline::kept_intervals(10.0, &cuts).unwrap(),
        [
            Clip {
                start: 0.0,
                end: 3.0
            },
            Clip {
                start: 8.0,
                end: 10.0
            },
        ]
    );
    assert_eq!(
        timeline::original_time(3.0, 10.0, &cuts, false).unwrap(),
        8.0
    );
    assert_eq!(
        timeline::original_time(3.0, 10.0, &cuts, true).unwrap(),
        3.0
    );
    assert_eq!(
        timeline::original_time(4.0, 10.0, &cuts, false).unwrap(),
        9.0
    );
    assert_eq!(timeline::edited_time(9.0, 10.0, &cuts).unwrap(), 4.0);
    assert_eq!(timeline::edited_time(5.0, 10.0, &cuts).unwrap(), 3.0);
    for cuts in [
        vec![Clip {
            start: 0.0,
            end: 10.0,
        }],
        vec![Clip {
            start: 4.0,
            end: 3.0,
        }],
        vec![Clip {
            start: 0.0,
            end: 11.0,
        }],
        vec![Clip {
            start: f64::NAN,
            end: 2.0,
        }],
    ] {
        assert!(timeline::kept_intervals(10.0, &cuts).is_err());
    }
}

#[test]
fn revising_cuts_preserves_smoothing_and_reset_baselines_by_source_sample() {
    let mut source = ride();
    source.points[4].elevation += 0.25;
    let cut = timeline::cut(
        &source,
        Clip {
            start: 1.0,
            end: 3.0,
        },
    )
    .unwrap();
    let mut smoothed = cut.clone();
    let point = smoothed
        .points
        .iter_mut()
        .find(|point| point.source_index == Some(8))
        .unwrap();
    point.elevation += 0.5;
    source.retain_elevation_changes(&cut, &smoothed).unwrap();
    assert_eq!(source.points[4].elevation, 104.25);
    assert_eq!(source.points[8].elevation, 108.5);
    let mut rebuilt = timeline::cut_ranges(
        &source,
        &[Clip {
            start: 1.0,
            end: 2.0,
        }],
    )
    .unwrap();
    let original = rebuilt
        .points
        .iter()
        .find(|point| point.source_index == Some(8))
        .unwrap();
    let previous = rebuilt
        .points
        .iter()
        .find(|point| point.source_index == Some(7))
        .unwrap();
    assert!((original.elevation - previous.elevation - 1.5).abs() < 1e-6);
    rebuilt.reset();
    let written = Track::parse(rebuilt.export("", "").unwrap()).unwrap();
    for (point, output) in rebuilt.points.iter().zip(&written.points) {
        assert!((point.elevation - output.elevation).abs() < 1e-6);
    }
    let before = source.clone();
    let mut invalid = smoothed.clone();
    let boundary = invalid
        .points
        .iter_mut()
        .find(|point| point.source_index.is_none())
        .unwrap();
    boundary.elevation += 1.0;
    assert!(source.retain_elevation_changes(&cut, &invalid).is_err());
    assert_eq!(source, before);
}

fn ride() -> Track {
    let points: String = (0..=10).map(|i| format!(
        "<trkpt lat=\"0\" lon=\"{}\"><ele>{}</ele><time>2026-01-01T00:00:{i:02}Z</time><extensions><power>{}</power></extensions></trkpt>",
        i as f64 / 11_119.508_023_353_29, 100 + i, 200 + i,
    )).collect();
    Track::parse(format!("<gpx xmlns=\"http://www.topografix.com/GPX/1/1\"><metadata><name>Keep metadata</name></metadata><trk><name>Ride</name><trkseg>{points}</trkseg></trk></gpx>")).unwrap()
}

#[test]
fn cuts_video_time_and_gpx_time_together_without_changing_retained_samples() {
    let track = ride();
    let edited = timeline::edit(
        &track,
        "Edited",
        "Description",
        &[
            Clip {
                start: 0.0,
                end: 3.0,
            },
            Clip {
                start: 7.0,
                end: 10.0,
            },
        ],
        10.0,
        0.0,
        10.0,
    )
    .unwrap();
    let output = Track::parse(edited.gpx.clone()).unwrap();
    let timing = timeline::timing(&output).unwrap();
    assert_eq!(edited.duration, 6.0);
    assert_eq!(timing.duration, 6.0);
    assert_eq!(edited.cuts, vec![3.0]);
    assert_eq!(timing.seconds, vec![0.0, 1.0, 2.0, 2.9, 3.0, 4.0, 5.0, 6.0]);
    assert_eq!(output.points.len(), edited.points);
    assert_ne!(output.points[3].segment, output.points[4].segment);
    assert!(edited
        .gpx
        .contains("<metadata><name>Keep metadata</name></metadata>"));
    for (source, target) in [(0, 0), (1, 1), (2, 2), (7, 4), (8, 5), (9, 6), (10, 7)] {
        assert_eq!(
            track.points[source].latitude,
            output.points[target].latitude
        );
        assert_eq!(
            track.points[source].longitude,
            output.points[target].longitude
        );
        assert_eq!(
            track.points[source].elevation,
            output.points[target].elevation
        );
        assert!(edited.gpx.contains(&format!(
            "<extensions><power>{}</power></extensions>",
            source + 200
        )));
    }
    assert!(!edited.gpx.contains("<power>204</power>"));
    // Emulate Incyclist's cut correction: the omitted 40 m must not become riding distance.
    let before = &output.points[3];
    let after = &output.points[4];
    let next = &output.points[5];
    let speed = (next.distance - after.distance) / (timing.seconds[5] - timing.seconds[4]);
    let distance_at_cut = before.distance + speed * (timing.seconds[4] - timing.seconds[3]);
    assert!((distance_at_cut - 30.0).abs() < 0.01);
}

#[test]
fn supports_offsets_fractional_boundaries_and_frame_alignment() {
    let track = ride();
    let edited = timeline::edit(
        &track,
        "Ride",
        "",
        &[Clip {
            start: 0.24,
            end: 4.26,
        }],
        5.0,
        2.0,
        10.0,
    )
    .unwrap();
    let output = Track::parse(edited.gpx).unwrap();
    let timing = timeline::timing(&output).unwrap();
    assert!((timing.duration - 4.1).abs() < 1e-6);
    assert!((output.points[0].elevation - 102.2).abs() < 1e-6);
    assert!((output.points.last().unwrap().elevation - 106.3).abs() < 1e-6);
    assert_eq!(timing.seconds[0], 0.0);
    let negative = timeline::edit(
        &track,
        "Ride",
        "",
        &[Clip {
            start: 2.0,
            end: 5.0,
        }],
        6.0,
        -2.0,
        10.0,
    )
    .unwrap();
    assert_eq!(
        timeline::timing(&Track::parse(negative.gpx).unwrap())
            .unwrap()
            .duration,
        3.0
    );
}

#[test]
fn shared_coverage_snaps_inward_at_fractional_offsets() {
    let track = ride();
    for offset in [-2.012, 2.012] {
        let clip = timeline::shared_interval(10.0, 10.0, offset, 30.0).unwrap();
        assert!(clip.start + offset >= 0.0);
        assert!(clip.end + offset <= 10.0);
        timeline::edit(&track, "", "", &[clip], 10.0, offset, 30.0).unwrap();
    }
    assert!(timeline::shared_interval(10.0, 10.0, 20.0, 30.0).is_err());
}

#[test]
fn rejects_invalid_ranges_missing_times_and_disconnected_boundaries() {
    let track = ride();
    for (clips, offset) in [
        (vec![], 0.0),
        (
            vec![Clip {
                start: 4.0,
                end: 2.0,
            }],
            0.0,
        ),
        (
            vec![
                Clip {
                    start: 0.0,
                    end: 8.0,
                },
                Clip {
                    start: 7.0,
                    end: 10.0,
                },
            ],
            0.0,
        ),
        (
            vec![Clip {
                start: 0.0,
                end: 10.0,
            }],
            1.0,
        ),
        (
            vec![Clip {
                start: 0.0,
                end: 10.0,
            }],
            f64::NAN,
        ),
    ] {
        assert!(timeline::edit(&track, "", "", &clips, 10.0, offset, 30.0).is_err());
    }
    let missing = Track::parse("<gpx><trk><trkseg><trkpt lat=\"0\" lon=\"0\"/><trkpt lat=\"0\" lon=\"1\"/></trkseg></trk></gpx>".into()).unwrap();
    assert!(timeline::timing(&missing)
        .unwrap_err()
        .contains("no timestamp"));
    let duplicate = track
        .export("", "")
        .unwrap()
        .replace("00:00:01Z", "00:00:00Z");
    assert!(timeline::timing(&Track::parse(duplicate).unwrap()).is_err());
    let xml = track.export("", "").unwrap();
    let doc = roxmltree::Document::parse(&xml).unwrap();
    let index = doc
        .descendants()
        .filter(|n| n.tag_name().name() == "trkpt")
        .nth(5)
        .unwrap()
        .range()
        .start;
    let gap = format!("{}</trkseg><trkseg>{}", &xml[..index], &xml[index..]);
    let gap = Track::parse(gap).unwrap();
    assert!(timeline::edit(
        &gap,
        "",
        "",
        &[Clip {
            start: 4.5,
            end: 8.0
        }],
        10.0,
        0.0,
        10.0
    )
    .unwrap_err()
    .contains("segment gap"));
}

#[test]
fn writes_fractional_incyclist_cut_markers_and_interpolates_cursor_distance() {
    let xml = timeline::add_cut_markers("<gpx-import></gpx-import>", &[3.033, 3661.5]).unwrap();
    assert!(xml.contains("time=\"0:00:03.033\""));
    assert!(xml.contains("time=\"0:00:3661.500\""));
    roxmltree::Document::parse(&xml).unwrap();
    let track = ride();
    let timing = timeline::timing(&track).unwrap();
    assert!((timeline::distance_at(&timing, &track, 2.5).unwrap() - 25.0).abs() < 0.01);
    assert!(timeline::distance_at(&timing, &track, -1.0).is_none());
}

#[test]
fn incyclist_cut_times_exactly_match_its_gpx_millisecond_clock() {
    // Incyclist uses strict equality after Number(S) + Number(M)*60 + Number(H)*3600.
    let parse_time = |value: &str| {
        let parts: Vec<f64> = value.split(':').map(|part| part.parse().unwrap()).collect();
        parts[2] + parts[1] * 60.0 + parts[0] * 3600.0
    };
    let gpx_time = 502_143.0 / 1000.0;
    assert_ne!(parse_time("0:08:22.143"), gpx_time);

    let cuts: Vec<f64> = (0..=14_400)
        .flat_map(|seconds| {
            [1, 33, 143, 500, 999].map(|millis| (seconds * 1000 + millis) as f64 / 1000.0)
        })
        .collect();
    let xml = timeline::add_cut_markers("<gpx-import></gpx-import>", &cuts).unwrap();
    let doc = roxmltree::Document::parse(&xml).unwrap();
    let markers: Vec<_> = doc
        .descendants()
        .filter(|node| node.has_tag_name("cut"))
        .collect();
    assert_eq!(markers.len(), cuts.len());
    for (marker, expected) in markers.iter().zip(cuts) {
        assert_eq!(parse_time(marker.attribute("time").unwrap()), expected);
    }
}

#[test]
fn preserves_namespace_scopes_segment_extensions_and_stationary_points() {
    let source = ride().export("", "").unwrap();
    let source = source
        .replace(
            "<trkseg>",
            "<g:trkseg xmlns:g=\"http://www.topografix.com/GPX/1/1\">",
        )
        .replace(
            "</trkseg>",
            "<extensions><label>segment data</label></extensions></g:trkseg>",
        );
    let track = Track::parse(source).unwrap();
    let edited = timeline::edit(
        &track,
        "",
        "",
        &[Clip {
            start: 0.5,
            end: 9.5,
        }],
        10.0,
        0.0,
        10.0,
    )
    .unwrap();
    roxmltree::Document::parse(&edited.gpx).unwrap();
    assert!(edited.gpx.contains("<g:trkpt"));
    assert!(edited.gpx.contains("<label>segment data</label>"));
    let source = ride().export("", "").unwrap();
    let doc = roxmltree::Document::parse(&source).unwrap();
    let points: Vec<_> = doc
        .descendants()
        .filter(|n| n.tag_name().name() == "trkpt")
        .collect();
    let stationary = source.replacen(
        &source[points[3].range()],
        &source[points[3].range()].replace(
            points[3].attribute("lon").unwrap(),
            points[2].attribute("lon").unwrap(),
        ),
        1,
    );
    let track = Track::parse(stationary).unwrap();
    let edited = timeline::edit(
        &track,
        "",
        "",
        &[Clip {
            start: 0.0,
            end: 10.0,
        }],
        10.0,
        0.0,
        10.0,
    )
    .unwrap();
    let output = Track::parse(edited.gpx).unwrap();
    assert_eq!(output.points.len(), track.points.len());
    assert_eq!(output.points[3].longitude, output.points[2].longitude);
    assert_eq!(timeline::timing(&output).unwrap().seconds[3], 3.0);
}

#[test]
fn supplied_tracks_have_video_ready_timestamps() {
    for filename in ["2026-09-22_120200.gpx", "2026-07-14_113900.gpx"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join(filename);
        let track = Track::read(&path).unwrap();
        let timing = timeline::timing(&track).unwrap();
        assert!(timing.duration > 1000.0);
        assert_eq!(timing.seconds.len(), track.points.len());
    }
}

#[test]
fn gpx_only_cuts_close_time_distance_and_elevation_gaps_without_changing_gradients() {
    let mut track = ride();
    track.points[8].elevation += 0.25;
    let before = track.clone();
    let cut = timeline::cut(
        &track,
        Clip {
            start: 3.0,
            end: 7.0,
        },
    )
    .unwrap();
    assert_eq!(track, before);
    let timing = timeline::timing(&cut).unwrap();
    assert_eq!(timing.duration, 6.0);
    let seam = cut
        .points
        .windows(2)
        .position(|pair| pair[0].segment != pair[1].segment)
        .unwrap()
        + 1;
    assert_eq!(cut.points[seam - 1].elevation, cut.points[seam].elevation);
    assert!((cut.total_distance() - 59.99).abs() < 0.01);
    for (source, target) in [(8, seam + 1), (9, seam + 2), (10, seam + 3)] {
        assert!((track.slopes(false)[source] - cut.slopes(false)[target]).abs() < 1e-5);
    }
    let xml = cut.export("", "").unwrap();
    assert!(!xml.contains("<power>204</power>"));
    assert!(xml.contains("<power>208</power>"));
    assert!(xml.contains("Keep metadata"));
    let mut reset = cut.clone();
    reset.reset();
    assert_eq!(
        reset.points[seam - 1].elevation,
        reset.points[seam].elevation
    );
    assert_ne!(
        reset.points[seam + 1].elevation,
        cut.points[seam + 1].elevation
    );
    let exported_reset = Track::parse(reset.export("", "").unwrap()).unwrap();
    for (point, exported) in reset.points.iter().zip(exported_reset.points) {
        assert!((point.elevation - exported.elevation).abs() < 1e-6);
    }
    let again = timeline::cut(
        &cut,
        Clip {
            start: 1.0,
            end: 2.0,
        },
    )
    .unwrap();
    assert_eq!(timeline::timing(&again).unwrap().duration, 5.0);
    for pair in again
        .points
        .windows(2)
        .filter(|pair| pair[0].segment != pair[1].segment)
    {
        assert!((pair[0].elevation - pair[1].elevation).abs() < 1e-6);
    }
}

#[test]
fn gpx_cuts_support_fractional_boundaries_trim_ends_and_remove_stationary_stops() {
    let track = ride();
    for cut in [
        Clip {
            start: 0.0,
            end: 2.25,
        },
        Clip {
            start: 8.25,
            end: 10.0,
        },
        Clip {
            start: 2.25,
            end: 6.75,
        },
    ] {
        let output = timeline::cut(&track, cut).unwrap();
        assert!(
            (timeline::timing(&output).unwrap().duration - (10.0 - cut.duration())).abs() < 1e-6
        );
    }
    let xml = track.export("", "").unwrap();
    let doc = roxmltree::Document::parse(&xml).unwrap();
    let nodes: Vec<_> = doc
        .descendants()
        .filter(|node| node.tag_name().name() == "trkpt")
        .collect();
    let longitude = nodes[2].attribute("lon").unwrap();
    let mut source = xml.clone();
    for node in nodes[3..=6].iter().rev() {
        source.replace_range(
            node.range(),
            &xml[node.range()].replace(node.attribute("lon").unwrap(), longitude),
        );
    }
    let stopped = Track::parse(source).unwrap();
    let output = timeline::cut(
        &stopped,
        Clip {
            start: 2.0,
            end: 6.0,
        },
    )
    .unwrap();
    assert_eq!(timeline::timing(&output).unwrap().duration, 6.0);
    for cut in [
        Clip {
            start: 0.0,
            end: 10.0,
        },
        Clip {
            start: 3.0,
            end: 3.0,
        },
        Clip {
            start: f64::NAN,
            end: 4.0,
        },
        Clip {
            start: -1.0,
            end: 2.0,
        },
    ] {
        assert!(timeline::cut(&track, cut).is_err());
    }
}

#[test]
fn distance_cuts_support_untimed_prefixed_tracks_and_routes() {
    for (container, group, point) in [("g:trk", "g:trkseg", "g:trkpt"), ("g:rte", "", "g:rtept")] {
        let points: String = (0..=10).map(|i| format!(
                "<{point} lat=\"0\" lon=\"{}\"><g:ele>{}</g:ele><g:extensions><power>{}</power></g:extensions></{point}>",
                i as f64 / 11_119.508_023_353_29, 100 + i * 10, 200 + i,
            )).collect();
        let body = if group.is_empty() {
            points
        } else {
            format!("<{group}>{points}</{group}>")
        };
        let mut track = Track::parse(format!(
                "<g:gpx xmlns:g=\"http://www.topografix.com/GPX/1/1\"><{container}><g:name>Ride</g:name>{body}</{container}></g:gpx>"
            )).unwrap();
        track.points[8].elevation += 2.0;
        let cut = track.cut_distance([25.0, 65.0]).unwrap();
        assert_eq!(cut.points.len(), 7);
        assert_eq!(cut.points[2].elevation, cut.points[3].elevation);
        assert_ne!(cut.points[2].segment, cut.points[3].segment);
        assert!((cut.total_distance() - 50.0).abs() < 0.01);
        assert!((cut.slopes(false)[4] - track.slopes(false)[8]).abs() < 1e-8);
        let xml = cut.export("Edited", "").unwrap();
        assert!(xml.contains("<power>207</power>"));
        assert!(!xml.contains("<power>204</power>"));
        Track::parse(xml).unwrap();
        let mut reset = cut.clone();
        reset.reset();
        let exported = Track::parse(reset.export("", "").unwrap()).unwrap();
        for (point, written) in reset.points.iter().zip(exported.points) {
            assert!((point.elevation - written.elevation).abs() < 1e-6);
        }
        assert!(track.cut_distance([0.0, track.total_distance()]).is_err());
    }
}
