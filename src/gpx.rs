use roxmltree::{Document, Node};
use std::{ops::Range, path::Path, sync::Arc};

const GPX_10: &str = "http://www.topografix.com/GPX/1/0";
const GPX_11: &str = "http://www.topografix.com/GPX/1/1";

#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    pub latitude: f64,
    pub longitude: f64,
    pub distance: f64,
    pub step: f64,
    pub segment: usize,
    pub original: f64,
    pub elevation: f64,
    /// Index in the imported track; interpolated cut boundaries have no source sample.
    pub source_index: Option<usize>,
    elevation_range: Range<usize>,
    elevation_tag: String,
    missing: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum Operation {
    AutoSmooth,
    IncyclistSmooth,
    Smooth(usize),
    SlopeRange { min: f64, max: f64 },
    Flatten(f64),
    Elevate(f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    source: Arc<str>,
    pub points: Vec<Point>,
    pub name: String,
    pub description: String,
    pub missing_elevations: usize,
    metadata: Vec<TextEdit>,
}

#[derive(Clone, Debug, PartialEq)]
struct TextEdit {
    range: Range<usize>,
    tag: String,
    description: bool,
}

fn is_gpx(node: Node<'_, '_>, name: &str, namespace: Option<&str>) -> bool {
    node.is_element() && node.tag_name().name() == name && node.tag_name().namespace() == namespace
}

fn qualified_tag(source: &str, node: Node<'_, '_>, name: &str) -> String {
    let start = &source[node.range().start + 1..];
    let tag = start
        .split([' ', '\t', '\r', '\n', '/', '>'])
        .next()
        .unwrap();
    match tag.split_once(':') {
        Some((prefix, _)) => format!("{prefix}:{name}"),
        None => name.to_owned(),
    }
}

fn opening_end(source: &str, node: Node<'_, '_>) -> usize {
    let mut quote = None;
    for (offset, byte) in source.as_bytes()[node.range()].iter().copied().enumerate() {
        match (quote, byte) {
            (None, b'\'' | b'"') => quote = Some(byte),
            (Some(delimiter), _) if byte == delimiter => quote = None,
            (None, b'>') => return node.range().start + offset + 1,
            _ => {}
        }
    }
    unreachable!("Parsed XML elements always have a closing angle bracket")
}

fn coordinate(node: Node<'_, '_>, name: &str, limit: f64) -> Result<f64, String> {
    let value = node
        .attribute(name)
        .ok_or_else(|| format!("A GPX point is missing its {name} coordinate."))?
        .parse::<f64>()
        .map_err(|_| format!("Invalid {name} coordinate."))?;
    if !value.is_finite() || value.abs() > limit {
        return Err(format!(
            "{name} must be finite and between -{limit} and {limit}."
        ));
    }
    Ok(value)
}

fn distance(a: &Point, latitude: f64, longitude: f64) -> f64 {
    let lat1 = a.latitude.to_radians();
    let lat2 = latitude.to_radians();
    let dlat = lat2 - lat1;
    let dlon = (longitude - a.longitude).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    6_371_008.8 * 2.0 * h.clamp(0.0, 1.0).sqrt().asin()
}

impl Track {
    pub fn source_xml(&self) -> &str {
        &self.source
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("fit"))
        {
            return crate::fit::read(path);
        }
        let source = std::fs::read_to_string(path)
            .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
        Self::parse(source)
    }

    pub fn parse(source: String) -> Result<Self, String> {
        let doc = Document::parse(&source).map_err(|error| format!("Invalid GPX XML: {error}"))?;
        let root = doc.root_element();
        let ns = root.tag_name().namespace();
        if root.tag_name().name() != "gpx" || !matches!(ns, None | Some(GPX_10) | Some(GPX_11)) {
            return Err("Expected a GPX 1.0 or 1.1 document.".into());
        }
        let mut points: Vec<Point> = Vec::new();
        let mut missing_elevations = 0;
        let mut metadata = Vec::new();
        let mut name = String::new();
        let mut description = String::new();
        let mut segment = 0;
        for container in root
            .children()
            .filter(|n| is_gpx(*n, "trk", ns) || is_gpx(*n, "rte", ns))
        {
            for (field, is_description) in [("name", false), ("desc", true)] {
                let tag = qualified_tag(&source, container, field);
                let child = container.children().find(|n| is_gpx(*n, field, ns));
                if let Some(child) = child {
                    let text: String = child.children().filter_map(|n| n.text()).collect();
                    let target = if is_description {
                        &mut description
                    } else {
                        &mut name
                    };
                    if target.is_empty() {
                        *target = text;
                    }
                    metadata.push(TextEdit {
                        range: child.range(),
                        tag,
                        description: is_description,
                    });
                } else {
                    let position = opening_end(&source, container);
                    // A self-closing empty track has no insertion point and no ride data.
                    if !source[..position].ends_with("/>") {
                        let position = if is_description {
                            container
                                .children()
                                .find(|n| is_gpx(*n, "name", ns))
                                .map_or(position, |n| n.range().end)
                        } else {
                            position
                        };
                        metadata.push(TextEdit {
                            range: position..position,
                            tag,
                            description: is_description,
                        });
                    }
                }
            }
            let groups: Vec<_> = if is_gpx(container, "rte", ns) {
                vec![container]
            } else {
                container
                    .children()
                    .filter(|n| is_gpx(*n, "trkseg", ns))
                    .collect()
            };
            for group in groups {
                segment += 1;
                let mut previous_elevation = 100.0;
                for node in group
                    .children()
                    .filter(|n| is_gpx(*n, "trkpt", ns) || is_gpx(*n, "rtept", ns))
                {
                    let latitude = coordinate(node, "lat", 90.0)?;
                    let longitude = coordinate(node, "lon", 180.0)?;
                    let elevations: Vec<_> =
                        node.children().filter(|n| is_gpx(*n, "ele", ns)).collect();
                    if elevations.len() > 1 {
                        return Err("A GPX point contains more than one elevation.".into());
                    }
                    let elevation_node = elevations.first().copied();
                    let elevation = if let Some(ele) = elevation_node {
                        let value = ele
                            .text()
                            .unwrap_or("")
                            .trim()
                            .parse::<f64>()
                            .map_err(|_| "A GPX elevation is not a number.".to_owned())?;
                        if !value.is_finite() || value.abs() > 1_000_000.0 {
                            return Err("Elevations must be finite and within 1,000,000 metres of sea level.".into());
                        }
                        value
                    } else {
                        missing_elevations += 1;
                        previous_elevation
                    };
                    let step = points
                        .last()
                        .filter(|p| p.segment == segment)
                        .map_or(0.0, |p| distance(p, latitude, longitude));
                    let cumulative = points.last().map_or(0.0, |p| p.distance) + step;
                    let elevation_range = elevation_node.map_or_else(
                        || {
                            let position = opening_end(&source, node) - 1;
                            if source.as_bytes()[position - 1] == b'/' {
                                // Replace the closing "/>" to expand a self-closing point.
                                position - 1..position + 1
                            } else {
                                position + 1..position + 1
                            }
                        },
                        |n| n.range(),
                    );
                    let elevation_tag = qualified_tag(&source, node, "ele");
                    points.push(Point {
                        source_index: Some(points.len()),
                        latitude,
                        longitude,
                        distance: cumulative,
                        step,
                        segment,
                        original: elevation,
                        elevation,
                        elevation_range,
                        elevation_tag,
                        missing: elevation_node.is_none(),
                    });
                    previous_elevation = elevation;
                }
            }
        }
        if points.is_empty() {
            return Err("This GPX contains no track or route points.".into());
        }
        drop(doc);
        Ok(Self {
            source: source.into(),
            points,
            name,
            description,
            missing_elevations,
            metadata,
        })
    }

    pub fn total_distance(&self) -> f64 {
        self.points.last().map_or(0.0, |p| p.distance)
    }

    pub fn slopes(&self, original: bool) -> Vec<f64> {
        let mut result = vec![0.0; self.points.len()];
        for (index, pair) in self.points.windows(2).enumerate() {
            if pair[0].segment == pair[1].segment {
                result[index + 1] = if pair[1].step > 0.01 {
                    let elevation = |p: &Point| if original { p.original } else { p.elevation };
                    100.0 * (elevation(&pair[1]) - elevation(&pair[0])) / pair[1].step
                } else {
                    // Stationary samples have no grade; zero would create false slope spikes.
                    result[index]
                };
            }
        }
        result
    }

    pub fn reset(&mut self) {
        for point in &mut self.points {
            point.elevation = point.original;
        }
    }

    pub fn retain_elevation_changes(
        &mut self,
        before: &Track,
        after: &Track,
    ) -> Result<(), String> {
        if before.points.len() != after.points.len() {
            return Err("Cannot preserve elevation changes after the point count changed.".into());
        }
        let mut changes = Vec::new();
        for (before, after) in before.points.iter().zip(&after.points) {
            if before.source_index != after.source_index
                || before.latitude != after.latitude
                || before.longitude != after.longitude
            {
                return Err("Cannot match edited elevations to their original GPX samples.".into());
            }
            let delta = after.elevation - before.elevation;
            if delta == 0.0 {
                continue;
            }
            let index = before
                .source_index
                .ok_or("Cannot preserve an elevation edit on an interpolated cut boundary.")?;
            let source = self
                .points
                .get(index)
                .ok_or("An edited GPX sample has no matching original point.")?;
            let elevation = source.elevation + delta;
            if !elevation.is_finite() || elevation.abs() > 1_000_000.0 {
                return Err(
                    "Preserving smoothing would produce an invalid original elevation.".into(),
                );
            }
            changes.push((index, elevation));
        }
        for (index, elevation) in changes {
            self.points[index].elevation = elevation;
        }
        Ok(())
    }

    /// Cut recorded samples selected by distance, including untimed routes.
    pub fn cut_distance(&self, selection: [f64; 2]) -> Result<Self, String> {
        if !selection.iter().all(|value| value.is_finite())
            || selection[0] < 0.0
            || selection[1] <= selection[0]
            || selection[1] > self.total_distance()
        {
            return Err("Select a non-empty distance range within the GPX.".into());
        }
        let start = self
            .points
            .partition_point(|point| point.distance < selection[0]);
        let end = self
            .points
            .partition_point(|point| point.distance <= selection[1]);
        if start == end || self.points.len() - (end - start) < 2 {
            return Err(
                "The cut must remove recorded points and leave at least two GPX points.".into(),
            );
        }
        let xml = self.export(&self.name, &self.description)?;
        let doc = Document::parse(&xml).map_err(|error| error.to_string())?;
        let ns = doc.root_element().tag_name().namespace();
        if doc.descendants().any(|node| {
            is_gpx(node, "time", ns)
                && node.parent().is_some_and(|parent| {
                    is_gpx(parent, "trkpt", ns) || is_gpx(parent, "rtept", ns)
                })
        }) {
            let timing = crate::timeline::timing(self)
                .map_err(|error| format!("Cannot close the GPX time gap: {error}"))?;
            return crate::timeline::cut(
                self,
                crate::timeline::Clip {
                    start: if start == 0 {
                        0.0
                    } else {
                        timing.seconds[start]
                    },
                    end: if end == self.points.len() {
                        timing.duration
                    } else {
                        timing.seconds[end]
                    },
                },
            );
        }
        let nodes: Vec<_> = doc
            .descendants()
            .filter(|node| is_gpx(*node, "trkpt", ns) || is_gpx(*node, "rtept", ns))
            .filter(|node| {
                node.parent()
                    .is_some_and(|parent| is_gpx(parent, "trkseg", ns) || is_gpx(parent, "rte", ns))
            })
            .collect();
        if nodes.len() != self.points.len() {
            return Err("Cannot match GPX points to their XML containers.".into());
        }
        let mut changes: Vec<(Range<usize>, String)> = nodes[start..end]
            .iter()
            .map(|node| (node.range(), String::new()))
            .collect();
        if start > 0
            && end < nodes.len()
            && self.points[start - 1].segment == self.points[end].segment
        {
            let group = nodes[end].parent().unwrap();
            let tag = qualified_tag(&xml, group, group.tag_name().name());
            let opening = &xml[group.range().start..opening_end(&xml, group)];
            let mut boundary = String::new();
            // Preserve the route metadata and segment namespace scope on both sides.
            for child in group.children().filter(|child| {
                child.is_element() && !is_gpx(*child, "trkpt", ns) && !is_gpx(*child, "rtept", ns)
            }) {
                if is_gpx(group, "trkseg", ns) {
                    boundary.push_str(&xml[child.range()]);
                }
            }
            boundary.push_str(&format!("</{tag}>{opening}"));
            if is_gpx(group, "rte", ns) {
                for child in group.children().filter(|child| {
                    child.is_element()
                        && !is_gpx(*child, "rtept", ns)
                        && !is_gpx(*child, "extensions", ns)
                }) {
                    boundary.push_str(&xml[child.range()]);
                }
            }
            changes.push((nodes[end].range().start..nodes[end].range().start, boundary));
        }
        let mut result_xml = xml.clone();
        changes.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        for (range, replacement) in changes {
            result_xml.replace_range(range, &replacement);
        }
        let mut result = Self::parse(result_xml)?;
        let retained = self.points[..start].iter().chain(&self.points[end..]);
        let shift = if start > 0 && end < self.points.len() {
            self.points[start - 1].elevation - self.points[end].elevation
        } else {
            0.0
        };
        let original_shift = if start > 0 && end < self.points.len() {
            self.points[start - 1].original - self.points[end].original
        } else {
            0.0
        };
        let mut elevations = Vec::with_capacity(result.points.len());
        for (index, (point, source)) in result.points.iter_mut().zip(retained).enumerate() {
            elevations.push((
                source.elevation + if index >= start { shift } else { 0.0 },
                source.source_index,
            ));
            point.elevation = source.original + if index >= start { original_shift } else { 0.0 };
        }
        let mut result = Self::parse(result.export(&self.name, &self.description)?)?;
        for (point, (elevation, source_index)) in result.points.iter_mut().zip(elevations) {
            point.elevation = elevation;
            point.source_index = source_index;
        }
        Ok(result)
    }

    pub fn apply(&mut self, operation: Operation, selection: [f64; 2]) -> Result<(), String> {
        if !selection.iter().all(|v| v.is_finite())
            || selection[0] < 0.0
            || selection[1] < selection[0]
            || selection[1] > self.total_distance()
        {
            return Err("Select a valid distance range within the ride.".into());
        }
        match operation {
            Operation::Smooth(n) if n < 3 || n % 2 == 0 => {
                return Err("Smoothing requires an odd number of points, at least 3.".into())
            }
            Operation::SlopeRange { min, max }
                if !min.is_finite() || !max.is_finite() || min > max =>
            {
                return Err(
                    "Minimum slope must not exceed maximum slope; both must be finite.".into(),
                )
            }
            Operation::Flatten(delta) if !delta.is_finite() || delta < 0.0 => {
                return Err("Maximum slope change must be finite and non-negative.".into())
            }
            Operation::Elevate(offset) if !offset.is_finite() => {
                return Err("Elevation shift must be finite.".into())
            }
            _ => {}
        }
        let start = self.points.partition_point(|p| p.distance < selection[0]);
        let end = self.points.partition_point(|p| p.distance <= selection[1]);
        if start == end {
            return Err("The selected distance range contains no points.".into());
        }
        let old: Vec<_> = self.points.iter().map(|p| p.elevation).collect();
        let old_slopes = self.slopes(false);
        let mut elevations = old.clone();
        let mut group_start = start;
        while group_start < end {
            let segment = self.points[group_start].segment;
            let mut group_end = group_start + 1;
            while group_end < end && self.points[group_end].segment == segment {
                group_end += 1;
            }
            if matches!(
                operation,
                Operation::AutoSmooth | Operation::IncyclistSmooth
            ) {
                crate::automatic::smooth(
                    &self.points[group_start..group_end],
                    &mut elevations[group_start..group_end],
                    matches!(operation, Operation::IncyclistSmooth),
                )?;
                group_start = group_end;
                continue;
            }
            let prefix = if matches!(operation, Operation::Smooth(_)) {
                let mut sums = Vec::with_capacity(group_end - group_start + 1);
                sums.push(0.0);
                for value in &old[group_start..group_end] {
                    sums.push(sums.last().unwrap() + value);
                }
                sums
            } else {
                Vec::new()
            };
            for i in group_start..group_end {
                elevations[i] = match operation {
                    Operation::Smooth(n) => {
                        let left = i.saturating_sub(n / 2).max(group_start);
                        let right = i.saturating_add(n / 2).saturating_add(1).min(group_end);
                        (prefix[right - group_start] - prefix[left - group_start])
                            / (right - left) as f64
                    }
                    Operation::Elevate(offset) => old[i] + offset,
                    Operation::SlopeRange { min, max }
                        if i > 0 && self.points[i - 1].segment == segment =>
                    {
                        elevations[i - 1]
                            + old_slopes[i].clamp(min, max) * self.points[i].step / 100.0
                    }
                    Operation::Flatten(delta) if i > 0 && self.points[i - 1].segment == segment => {
                        let previous_slope = if i > 1
                            && self.points[i - 2].segment == segment
                            && self.points[i - 1].step > 0.01
                        {
                            100.0 * (elevations[i - 1] - elevations[i - 2])
                                / self.points[i - 1].step
                        } else {
                            0.0
                        };
                        let slope =
                            old_slopes[i].clamp(previous_slope - delta, previous_slope + delta);
                        elevations[i - 1] + slope * self.points[i].step / 100.0
                    }
                    _ => old[i],
                };
            }
            group_start = group_end;
        }
        if elevations
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 1_000_000.0)
        {
            return Err(
                "This operation would produce an invalid elevation (limit: +/-1,000,000 m).".into(),
            );
        }
        for (point, elevation) in self.points.iter_mut().zip(elevations) {
            point.elevation = elevation;
        }
        Ok(())
    }

    /// Patch the source rather than reserialize: sensor extensions and unrelated XML stay intact.
    pub fn export(&self, name: &str, description: &str) -> Result<String, String> {
        if [name, description].iter().any(|s| {
            s.chars().any(|c| {
                !matches!(c, '\t' | '\n' | '\r')
                    && (c < '\u{20}' || matches!(c, '\u{fffe}' | '\u{ffff}'))
            })
        }) {
            return Err("GPX name and description contain characters forbidden in XML.".into());
        }
        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        for point in &self.points {
            if !point.missing && point.elevation == point.original {
                continue;
            }
            let ele = format!(
                "<{}>{:.6}</{}>",
                point.elevation_tag, point.elevation, point.elevation_tag
            );
            let replacement = if &self.source[point.elevation_range.clone()] == "/>" {
                let before = &self.source[..point.elevation_range.start];
                let opening = before.rfind('<').unwrap();
                let tag = self.source[opening + 1..]
                    .split_whitespace()
                    .next()
                    .unwrap();
                format!(">{ele}</{tag}>")
            } else {
                ele
            };
            edits.push((point.elevation_range.clone(), replacement));
        }
        for edit in &self.metadata {
            let value = if edit.description { description } else { name };
            let escaped = value
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            edits.push((
                edit.range.clone(),
                format!("<{}>{escaped}</{}>", edit.tag, edit.tag),
            ));
        }
        edits.sort_by_key(|(range, _)| range.start);
        let mut output = String::with_capacity(self.source.len() + edits.len() * 20);
        let mut cursor = 0;
        for (range, value) in edits {
            output.push_str(&self.source[cursor..range.start]);
            output.push_str(&value);
            cursor = range.end;
        }
        output.push_str(&self.source[cursor..]);
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ride(elevations: &[f64]) -> Track {
        let points: String = elevations.iter().enumerate().map(|(i, ele)| {
            format!("<trkpt lat=\"0\" lon=\"{}\"><ele>{ele}</ele><time>2026-01-01T00:00:00Z</time><extensions><power>123</power></extensions></trkpt>", i as f64 * 0.0001)
        }).collect();
        Track::parse(format!(
            "<gpx xmlns=\"{GPX_11}\"><trk><name>Ride</name><trkseg>{points}</trkseg></trk></gpx>"
        ))
        .unwrap()
    }

    #[test]
    fn smoothing_uses_symmetric_truncated_windows_and_accumulates() {
        let mut track = ride(&[0.0, 0.0, 90.0, 0.0, 0.0]);
        track
            .apply(Operation::Smooth(3), [0.0, track.total_distance()])
            .unwrap();
        assert_eq!(
            track.points.iter().map(|p| p.elevation).collect::<Vec<_>>(),
            [0.0, 30.0, 30.0, 30.0, 0.0]
        );
        track
            .apply(Operation::Smooth(5), [0.0, track.total_distance()])
            .unwrap();
        assert_eq!(track.points[0].elevation, 20.0);
        track.reset();
        assert_eq!(track.points[2].elevation, 90.0);
    }

    #[test]
    fn selection_does_not_change_unselected_points() {
        for operation in [
            Operation::Smooth(5),
            Operation::Elevate(10.0),
            Operation::SlopeRange {
                min: -2.0,
                max: 3.0,
            },
            Operation::Flatten(1.0),
        ] {
            let mut track = ride(&[0.0, 50.0, 0.0, 80.0, 0.0]);
            let selection = [track.points[1].distance, track.points[3].distance];
            track.apply(operation, selection).unwrap();
            assert_eq!(track.points[0].elevation, 0.0);
            assert_eq!(track.points[4].elevation, 0.0);
        }
    }

    #[test]
    fn slope_range_and_flatten_bound_resulting_slopes() {
        let mut track = ride(&[10.0, 100.0, -20.0, 80.0, 5.0]);
        track
            .apply(
                Operation::SlopeRange {
                    min: -3.0,
                    max: 8.0,
                },
                [0.0, track.total_distance()],
            )
            .unwrap();
        assert!(track
            .slopes(false)
            .iter()
            .all(|s| *s >= -3.000001 && *s <= 8.000001));
        track.reset();
        track
            .apply(Operation::Flatten(1.0), [0.0, track.total_distance()])
            .unwrap();
        assert!(track
            .slopes(false)
            .windows(2)
            .all(|s| (s[1] - s[0]).abs() <= 1.000001));
    }

    #[test]
    fn invalid_operations_are_atomic() {
        let mut track = ride(&[0.0, 20.0]);
        for operation in [
            Operation::Smooth(4),
            Operation::Flatten(-1.0),
            Operation::Elevate(f64::NAN),
            Operation::Elevate(f64::MAX),
            Operation::SlopeRange {
                min: 10.0,
                max: 2.0,
            },
        ] {
            assert!(track
                .apply(operation, [0.0, track.total_distance()])
                .is_err());
            assert_eq!(track.points[1].elevation, 20.0);
        }
        assert!(track.apply(Operation::Smooth(3), [1.0, 0.0]).is_err());
    }

    #[test]
    fn export_preserves_extensions_coordinates_and_times() {
        let mut track = ride(&[12.0, 15.0]);
        track
            .apply(Operation::Elevate(7.5), [0.0, track.total_distance()])
            .unwrap();
        let output = track.export("A & B <ride>", "Test & description").unwrap();
        assert!(output.contains("<extensions><power>123</power></extensions>"));
        assert!(output.contains("<time>2026-01-01T00:00:00Z</time>"));
        assert!(output.contains("lat=\"0\" lon=\"0.0001\""));
        let reloaded = Track::parse(output).unwrap();
        assert_eq!(reloaded.name, "A & B <ride>");
        assert_eq!(reloaded.description, "Test & description");
        assert_eq!(reloaded.points[0].elevation, 19.5);
        assert!(track.export("Bad\u{1}", "").is_err());
    }

    #[test]
    fn missing_self_closing_and_prefixed_elevations_are_handled() {
        let xml = format!("<g:gpx xmlns:g=\"{GPX_11}\"><g:rte><g:rtept lat=\"0\" lon=\"0\"/><g:rtept lat=\"0\" lon=\"0.1\"><g:ele>25</g:ele></g:rtept><g:rtept lat=\"0\" lon=\"0.2\"/></g:rte></g:gpx>");
        let track = Track::parse(xml).unwrap();
        assert_eq!(track.missing_elevations, 2);
        assert_eq!(track.points[0].elevation, 100.0);
        assert_eq!(track.points[2].elevation, 25.0);
        let reloaded = Track::parse(track.export("Route", "").unwrap()).unwrap();
        assert_eq!(reloaded.missing_elevations, 0);
        assert_eq!(reloaded.points.len(), 3);
    }

    #[test]
    fn separate_segments_and_duplicate_coordinates_stay_finite() {
        let mut track = Track::parse("<gpx><trk><trkseg><trkpt lat=\"0\" lon=\"0\"><ele>0</ele></trkpt><trkpt lat=\"0\" lon=\"0\"><ele>2</ele></trkpt></trkseg><trkseg><trkpt lat=\"20\" lon=\"20\"><ele>1000</ele></trkpt></trkseg></trk></gpx>".into()).unwrap();
        assert_eq!(track.total_distance(), 0.0);
        track.apply(Operation::Smooth(5), [0.0, 0.0]).unwrap();
        assert_eq!(track.points[2].elevation, 1000.0);
        assert!(track.slopes(false).iter().all(|s| s.is_finite()));
    }

    #[test]
    fn stationary_samples_retain_the_previous_grade_within_their_segment() {
        let segments = [
            vec![
                (0.0, 100.0),
                (0.0001, 99.0),
                (0.0001, 98.0),
                (0.00010001, 97.0),
                (0.0002, 96.0),
            ],
            vec![(1.0, 200.0), (1.0, 199.0), (1.0001, 198.0), (1.0001, 197.0)],
            vec![(2.0, 10.0), (2.0, 11.0)],
        ];
        let xml: String = segments
            .iter()
            .map(|samples| {
                let points: String = samples
                    .iter()
                    .map(|(longitude, elevation)| {
                        format!(
                            "<trkpt lat=\"0\" lon=\"{longitude}\"><ele>{elevation}</ele></trkpt>"
                        )
                    })
                    .collect();
                format!("<trkseg>{points}</trkseg>")
            })
            .collect();
        let mut track = Track::parse(format!("<gpx><trk>{xml}</trk></gpx>")).unwrap();
        assert!(track.points[3].step > 0.0 && track.points[3].step < 0.01);
        track.points[1].elevation = 98.0;
        for original in [true, false] {
            let slopes = track.slopes(original);
            assert_eq!(slopes[0], 0.0);
            assert!(slopes[1] < 0.0);
            assert_eq!(slopes[2], slopes[1]);
            assert_eq!(slopes[3], slopes[1]);
            assert_eq!(slopes[5], 0.0);
            assert_eq!(slopes[6], 0.0);
            assert!(slopes[7] < 0.0);
            assert_eq!(slopes[8], slopes[7]);
            assert_eq!(slopes[9], 0.0);
            assert_eq!(slopes[10], 0.0);
            assert!(slopes.iter().all(|slope| slope.is_finite()));
        }
        assert_ne!(track.slopes(true)[2], track.slopes(false)[2]);
    }

    #[test]
    fn rejects_invalid_xml_coordinates_and_elevations() {
        for xml in [
            "<gpx>",
            "<wrong/>",
            "<gpx><trk><trkseg><trkpt lat=\"NaN\" lon=\"0\"/></trkseg></trk></gpx>",
            "<gpx><rte><rtept lat=\"0\" lon=\"181\"/></rte></gpx>",
            "<gpx><rte><rtept lat=\"0\" lon=\"0\"><ele>NaN</ele></rtept></rte></gpx>",
        ] {
            assert!(Track::parse(xml.into()).is_err());
        }
    }

    #[test]
    fn metadata_insertion_preserves_order_and_handles_quoted_angle_brackets() {
        for fields in ["", "<name>Old</name>", "<desc>Old</desc>"] {
            let xml = format!("<gpx xmlns=\"{GPX_10}\"><trk label=\"a>b\">{fields}<trkseg><trkpt lat=\"0\" lon=\"0\" label=\"a>b\"></trkpt></trkseg></trk></gpx>");
            let track = Track::parse(xml).unwrap();
            let xml = track.export("Name", "Description").unwrap();
            let doc = Document::parse(&xml).unwrap();
            let trk = doc
                .descendants()
                .find(|n| n.has_tag_name((GPX_10, "trk")))
                .unwrap();
            let tags: Vec<_> = trk
                .children()
                .filter(|n| n.is_element())
                .map(|n| n.tag_name().name())
                .collect();
            assert_eq!(tags, ["name", "desc", "trkseg"]);
            let reloaded = Track::parse(xml).unwrap();
            assert_eq!(reloaded.points[0].elevation, 100.0);
            assert_eq!(reloaded.name, "Name");
        }
    }
}
