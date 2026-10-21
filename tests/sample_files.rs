use rlv_forge::gpx::{Operation, Track};
use std::path::Path;

#[test]
fn automatic_smoothing_removes_slope_spikes_near_28_4_km() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/2026-09-22_120200.gpx");
    let mut track = Track::read(&path).unwrap();
    track
        .apply(Operation::AutoSmooth, [0.0, track.total_distance()])
        .unwrap();
    let reloaded = Track::parse(track.export(&track.name, &track.description).unwrap()).unwrap();
    for ride in [&track, &reloaded] {
        let slopes = ride.slopes(false);
        let mut checked = 0;
        for (index, point) in ride.points.iter().enumerate() {
            if (28_390.0..=28_401.0).contains(&point.distance) {
                checked += 1;
                assert!(
                    (slopes[index] - slopes[index - 1]).abs() < 1.0,
                    "Slope spike at {:.6} m: {:.6}% -> {:.6}%",
                    point.distance,
                    slopes[index - 1],
                    slopes[index],
                );
            }
        }
        assert!(checked > 0);
    }
}

#[test]
fn both_sample_files_can_be_edited_and_round_tripped_without_losing_sensor_data() {
    for filename in ["2026-09-22_120200.gpx", "2026-07-14_113900.gpx"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join(filename);
        let source = std::fs::read_to_string(&path).unwrap();
        let mut track = Track::read(&path).unwrap();
        assert!(track.points.len() > 1000);
        assert!(track.total_distance() > 1000.0);
        let count = track.points.len();
        let total = track.total_distance();
        let first = (track.points[0].latitude, track.points[0].longitude);
        for operation in [
            Operation::AutoSmooth,
            Operation::IncyclistSmooth,
            Operation::Smooth(5),
            Operation::SlopeRange {
                min: -8.0,
                max: 8.0,
            },
            Operation::Flatten(1.0),
            Operation::Elevate(10.0),
        ] {
            track.apply(operation, [0.0, total]).unwrap();
        }
        let output = track.export(&track.name, "Smoothed ride").unwrap();
        let parsed = Track::parse(output.clone()).unwrap();
        assert_eq!(parsed.points.len(), count);
        assert_eq!(parsed.total_distance(), total);
        assert_eq!(
            (parsed.points[0].latitude, parsed.points[0].longitude),
            first
        );
        assert!(parsed.points.iter().all(|p| p.elevation.is_finite()));
        let elements = |xml: &str, name: &str| {
            let doc = roxmltree::Document::parse(xml).unwrap();
            doc.descendants()
                .filter(|node| node.is_element() && node.tag_name().name() == name)
                .map(|node| xml[node.range()].to_owned())
                .collect::<Vec<_>>()
        };
        for element in ["extensions", "time"] {
            assert_eq!(elements(&source, element), elements(&output, element));
        }
        track.reset();
        assert!(track.points.iter().all(|p| p.elevation == p.original));
    }
}
