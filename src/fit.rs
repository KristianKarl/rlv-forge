use crate::gpx::Track;
use fitparser::{profile::MesgNum, FitDataRecord, Value};
use std::{fmt::Write, path::Path};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

pub(crate) fn read(path: &Path) -> Result<Track, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    let records =
        fitparser::from_bytes(&bytes).map_err(|error| format!("Invalid FIT file: {error}"))?;
    let name = path.file_stem().unwrap_or_default().to_string_lossy();
    let track = convert(&records)?;
    // Use the GPX exporter to validate and escape the imported track's name.
    Track::parse(track.export(&name, "")?)
}

fn field<'a>(record: &'a FitDataRecord, name: &str) -> Option<&'a Value> {
    record
        .fields()
        .iter()
        .find(|field| field.name() == name && !matches!(field.value(), Value::Invalid))
        .map(|field| field.value())
}

fn number(record: &FitDataRecord, name: &str) -> Result<Option<f64>, String> {
    let Some(value) = field(record, name) else {
        return Ok(None);
    };
    let value: f64 = value
        .clone()
        .try_into()
        .map_err(|error| format!("Invalid FIT {name}: {error}"))?;
    if !value.is_finite() {
        return Err(format!("FIT {name} must be finite."));
    }
    Ok(Some(value))
}

fn convert(records: &[FitDataRecord]) -> Result<Track, String> {
    let mut xml = String::from(
        "<gpx version=\"1.1\" creator=\"RLV Forge\" xmlns=\"http://www.topografix.com/GPX/1/1\" \
         xmlns:gpxtpx=\"http://www.garmin.com/xmlschemas/TrackPointExtension/v1\">\
         <trk><name></name><trkseg>",
    );
    let mut connected = false;
    let mut has_points = false;
    for record in records
        .iter()
        .filter(|record| record.kind() == MesgNum::Record)
    {
        let (Some(latitude), Some(longitude)) = (
            number(record, "position_lat")?,
            number(record, "position_long")?,
        ) else {
            connected = false;
            continue;
        };
        // FIT positions use semicircles; elevations are already scaled to metres.
        let latitude = latitude * 180.0 / 2_147_483_648.0;
        let longitude = longitude * 180.0 / 2_147_483_648.0;
        if has_points && !connected {
            xml.push_str("</trkseg><trkseg>");
        }
        write!(xml, "<trkpt lat=\"{latitude}\" lon=\"{longitude}\">").unwrap();
        if let Some(elevation) =
            number(record, "enhanced_altitude")?.or(number(record, "altitude")?)
        {
            write!(xml, "<ele>{elevation}</ele>").unwrap();
        }
        if let Some(value) = field(record, "timestamp") {
            let Value::Timestamp(timestamp) = value else {
                return Err("Invalid FIT record timestamp.".into());
            };
            let timestamp = OffsetDateTime::from_unix_timestamp(timestamp.timestamp())
                .map_err(|error| format!("Invalid FIT timestamp: {error}"))?
                .format(&Rfc3339)
                .map_err(|error| format!("Cannot format FIT timestamp: {error}"))?;
            write!(xml, "<time>{timestamp}</time>").unwrap();
        }
        let power = number(record, "power")?;
        let temperature = number(record, "temperature")?;
        let heart_rate = number(record, "heart_rate")?;
        let cadence = number(record, "cadence")?;
        if power.is_some() || temperature.is_some() || heart_rate.is_some() || cadence.is_some() {
            xml.push_str("<extensions>");
            if let Some(power) = power {
                write!(xml, "<power>{power}</power>").unwrap();
            }
            if temperature.is_some() || heart_rate.is_some() || cadence.is_some() {
                xml.push_str("<gpxtpx:TrackPointExtension>");
                for (tag, value) in [("atemp", temperature), ("hr", heart_rate), ("cad", cadence)] {
                    if let Some(value) = value {
                        write!(xml, "<gpxtpx:{tag}>{value}</gpxtpx:{tag}>").unwrap();
                    }
                }
                xml.push_str("</gpxtpx:TrackPointExtension>");
            }
            xml.push_str("</extensions>");
        }
        xml.push_str("</trkpt>");
        has_points = true;
        connected = true;
    }
    if !has_points {
        return Err("This FIT contains no GPS track points. Indoor activities without GPS are not supported.".into());
    }
    xml.push_str("</trkseg></trk></gpx>");
    Track::parse(xml)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fitparser::FitDataField;

    fn record(fields: &[(&str, Value)]) -> FitDataRecord {
        let mut record = FitDataRecord::new(MesgNum::Record);
        for (name, value) in fields {
            record.push(FitDataField::new(
                (*name).into(),
                0,
                None,
                value.clone(),
                String::new(),
            ));
        }
        record
    }

    fn position() -> FitDataRecord {
        record(&[
            ("position_lat", Value::SInt32(536_870_912)),
            ("position_long", Value::SInt32(-1_073_741_824)),
        ])
    }

    #[test]
    fn converts_units_and_preserves_time_and_sensor_samples() {
        let mut point = position();
        point.extend(
            record(&[
                ("altitude", Value::Float64(10.0)),
                ("enhanced_altitude", Value::Float64(12.4)),
                (
                    "timestamp",
                    Value::Timestamp("2026-10-24T12:57:53+02:00".parse().unwrap()),
                ),
                ("power", Value::UInt16(0)),
                ("temperature", Value::SInt8(-3)),
                ("heart_rate", Value::UInt8(87)),
                ("cadence", Value::UInt8(26)),
            ])
            .into_vec(),
        );
        let track = convert(&[point]).unwrap();
        assert_eq!(track.points[0].latitude, 45.0);
        assert_eq!(track.points[0].longitude, -90.0);
        assert_eq!(track.points[0].elevation, 12.4);
        assert_eq!(track.missing_elevations, 0);
        for element in [
            "<time>2026-10-24T10:57:53Z</time>",
            "<power>0</power>",
            "<gpxtpx:atemp>-3</gpxtpx:atemp>",
            "<gpxtpx:hr>87</gpxtpx:hr>",
            "<gpxtpx:cad>26</gpxtpx:cad>",
        ] {
            assert!(track.source_xml().contains(element), "{element}");
        }
    }

    #[test]
    fn handles_missing_values_without_joining_disconnected_gps_samples() {
        let mut first = position();
        first.extend(
            record(&[
                ("enhanced_altitude", Value::Invalid),
                ("altitude", Value::Float64(20.0)),
                ("heart_rate", Value::Invalid),
            ])
            .into_vec(),
        );
        let gap = record(&[
            ("position_lat", Value::Invalid),
            ("position_long", Value::SInt32(0)),
        ]);
        let last = record(&[
            ("position_lat", Value::SInt32(0)),
            ("position_long", Value::SInt32(0)),
        ]);
        let track = convert(&[first, gap, last]).unwrap();
        assert_eq!(track.points.len(), 2);
        assert_eq!(track.points[0].elevation, 20.0);
        assert_eq!(track.missing_elevations, 1);
        assert_ne!(track.points[0].segment, track.points[1].segment);
        assert_eq!(track.total_distance(), 0.0);
        assert!(!track.source_xml().contains("<extensions>"));
        assert!(!track.source_xml().contains("<time>"));
    }

    #[test]
    fn rejects_activities_without_gps_and_invalid_coordinates_or_fields() {
        assert!(convert(&[]).unwrap_err().contains("no GPS"));
        assert!(convert(&[FitDataRecord::new(MesgNum::Session)])
            .unwrap_err()
            .contains("no GPS"));
        for (name, value) in [
            ("position_lat", Value::SInt32(1_500_000_000)),
            ("enhanced_altitude", Value::Float64(f64::NAN)),
            ("power", Value::String("invalid".into())),
            ("timestamp", Value::UInt32(42)),
        ] {
            let mut point = if name == "position_lat" {
                record(&[("position_long", Value::SInt32(0))])
            } else {
                position()
            };
            point.extend(record(&[(name, value)]).into_vec());
            assert!(convert(&[point]).is_err(), "{name}");
        }
    }
}
