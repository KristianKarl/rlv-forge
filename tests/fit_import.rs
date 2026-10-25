use rlv_forge::{
    gpx::{Operation, Track},
    timeline::{self, Clip},
};
use std::{collections::BTreeMap, path::Path};

fn sample(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join(name)
}

struct RideSample {
    latitude: f64,
    longitude: f64,
    elevation: f64,
    sensors: BTreeMap<String, f64>,
}

fn samples(xml: &str) -> BTreeMap<String, RideSample> {
    let doc = roxmltree::Document::parse(xml).unwrap();
    doc.descendants()
        .filter(|node| node.has_tag_name(("http://www.topografix.com/GPX/1/1", "trkpt")))
        .map(|point| {
            let text = |name| {
                point
                    .children()
                    .find(|node| node.tag_name().name() == name)
                    .unwrap()
                    .text()
                    .unwrap()
            };
            (
                text("time").to_owned(),
                RideSample {
                    latitude: point.attribute("lat").unwrap().parse().unwrap(),
                    longitude: point.attribute("lon").unwrap().parse().unwrap(),
                    elevation: text("ele").parse().unwrap(),
                    sensors: point
                        .descendants()
                        .filter(|node| {
                            node.is_element()
                                && matches!(
                                    node.tag_name().name(),
                                    "power" | "hr" | "cad" | "atemp"
                                )
                        })
                        .map(|node| {
                            (
                                node.tag_name().name().to_owned(),
                                node.text().unwrap().parse().unwrap(),
                            )
                        })
                        .collect(),
                },
            )
        })
        .collect()
}

#[test]
fn paired_fit_and_gpx_samples_describe_the_same_ride() {
    let fit = Track::read(&sample("2026-10-24.fit")).unwrap();
    let gpx = Track::read(&sample("2026-10-24.gpx")).unwrap();
    assert!(fit.points.len() > 1000);
    assert_eq!(fit.name, "2026-10-24");
    assert_eq!(fit.missing_elevations, 0);
    let fit_samples = samples(fit.source_xml());
    let gpx_samples = samples(gpx.source_xml());
    assert_eq!(fit_samples.len(), 4522);
    assert_eq!(fit_samples.len(), gpx_samples.len());
    for (timestamp, fit) in &fit_samples {
        let gpx = &gpx_samples[timestamp];
        // The Strava GPX rounds positions to six decimal places.
        assert!(
            (fit.latitude - gpx.latitude).abs() < 0.000_000_51,
            "{timestamp}: latitude"
        );
        assert!(
            (fit.longitude - gpx.longitude).abs() < 0.000_000_51,
            "{timestamp}: longitude"
        );
        assert!(
            (fit.elevation - gpx.elevation).abs() < 1e-9,
            "{timestamp}: elevation"
        );
        // Strava fills some missing FIT sensor samples; compare recorded values only.
        for (sensor, value) in &fit.sensors {
            assert_eq!(
                Some(value),
                gpx.sensors.get(sensor),
                "{timestamp}: {sensor}"
            );
        }
    }
    assert!((fit.total_distance() - gpx.total_distance()).abs() < 10.0);
    let fit_timing = timeline::timing(&fit).unwrap();
    let gpx_timing = timeline::timing(&gpx).unwrap();
    assert_eq!(fit_timing, gpx_timing);
}

#[test]
fn imported_fit_supports_editing_cuts_and_gpx_export_without_losing_sensors() {
    let mut track = Track::read(&sample("2026-10-24.fit")).unwrap();
    let elements = |xml: &str, tag: &str| {
        let doc = roxmltree::Document::parse(xml).unwrap();
        doc.descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == tag)
            .map(|node| xml[node.range()].to_owned())
            .collect::<Vec<_>>()
    };
    let source = track.source_xml().to_owned();
    for tag in ["time", "power", "hr", "cad", "atemp"] {
        assert!(!elements(&source, tag).is_empty(), "{tag}");
    }
    track
        .apply(Operation::AutoSmooth, [0.0, track.total_distance()])
        .unwrap();
    let output = track.export("FIT & GPX <ride>", "").unwrap();
    let reloaded = Track::parse(output.clone()).unwrap();
    assert_eq!(reloaded.name, "FIT & GPX <ride>");
    assert_eq!(reloaded.points.len(), track.points.len());
    assert_eq!(reloaded.total_distance(), track.total_distance());
    for tag in ["time", "extensions"] {
        assert_eq!(elements(&source, tag), elements(&output, tag));
    }
    let timing = timeline::timing(&reloaded).unwrap();
    let edited = timeline::edit_gpx(
        &reloaded,
        &reloaded.name,
        "",
        &[
            Clip {
                start: 0.0,
                end: timing.duration / 3.0,
            },
            Clip {
                start: timing.duration * 2.0 / 3.0,
                end: timing.duration,
            },
        ],
        0.0,
    )
    .unwrap();
    let cut = Track::parse(edited.gpx).unwrap();
    assert!(cut.points.len() < reloaded.points.len());
    assert!(timeline::timing(&cut).unwrap().duration < timing.duration);
}

#[test]
fn fit_import_accepts_uppercase_extensions_and_rejects_corrupt_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("Ride & hills.FIT");
    let mut bytes = std::fs::read(sample("2026-10-24.fit")).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let track = Track::read(&path).unwrap();
    assert_eq!(track.name, "Ride & hills");
    assert!(track.source_xml().contains("<name>Ride &amp; hills</name>"));
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(&path, &bytes).unwrap();
    assert!(Track::read(&path).unwrap_err().contains("Invalid FIT"));
    for invalid in [&bytes[..30], b"not a FIT file", b""] {
        std::fs::write(&path, invalid).unwrap();
        assert!(Track::read(&path).is_err());
    }
}
