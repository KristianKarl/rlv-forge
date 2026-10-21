use crate::gpx::Point;

const GRID_STEP: f64 = 5.0;
const MAX_GAP: f64 = 50.0;
const REGRESSION_RADIUS: f64 = 25.0;
const MAX_GRID_INTERVALS: usize = 200_000;

pub(crate) fn smooth(
    points: &[Point],
    elevations: &mut [f64],
    soften_grades: bool,
) -> Result<(), String> {
    let mut start = 0;
    while start < points.len() {
        let mut end = start + 1;
        while end < points.len() && points[end].distance - points[end - 1].distance <= MAX_GAP {
            end += 1;
        }
        smooth_block(
            &points[start..end],
            &mut elevations[start..end],
            soften_grades,
        )?;
        start = end;
    }
    Ok(())
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_unstable_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

fn smooth_block(
    points: &[Point],
    elevations: &mut [f64],
    soften_grades: bool,
) -> Result<(), String> {
    let mut distances = Vec::new();
    let mut heights = Vec::new();
    let mut start = 0;
    while start < points.len() {
        let mut end = start + 1;
        while end < points.len() && points[end].distance == points[start].distance {
            end += 1;
        }
        distances.push(points[start].distance);
        heights.push(median(&mut elevations[start..end].to_vec()));
        start = end;
    }
    if distances.len() < 5 {
        return Ok(());
    }
    let length = distances[distances.len() - 1] - distances[0];
    if length < 10.0 {
        return Ok(());
    }
    heights[0] = elevations[0];
    *heights.last_mut().unwrap() = *elevations.last().unwrap();
    let intervals = (length / GRID_STEP - 1e-9).ceil() as usize;
    if intervals > MAX_GRID_INTERVALS {
        return Err("The automatic smoother supports continuous spans up to 1,000 km. Select a shorter distance range.".into());
    }
    let step = length / intervals as f64;
    let mut grid = Vec::with_capacity(intervals + 1);
    let mut right = 1;
    for i in 0..=intervals {
        let distance = distances[0] + i as f64 * step;
        while right + 1 < distances.len() && distances[right] < distance {
            right += 1;
        }
        let t = ((distance - distances[right - 1]) / (distances[right] - distances[right - 1]))
            .clamp(0.0, 1.0);
        grid.push(heights[right - 1] + t * (heights[right] - heights[right - 1]));
    }

    // A Hampel filter rejects isolated spikes, not a steep but consistent climb.
    let mut despiked = grid.clone();
    for i in 1..intervals {
        let left = i.saturating_sub(3);
        let right = (i + 4).min(grid.len());
        let mut neighbors = grid[left..right].to_vec();
        let center = median(&mut neighbors);
        for value in &mut neighbors {
            *value = (*value - center).abs();
        }
        let threshold = (3.0 * 1.4826 * median(&mut neighbors)).max(1.0);
        if (grid[i] - center).abs() > threshold {
            despiked[i] = center;
        }
    }

    let mut filtered = despiked.clone();
    let radius = (REGRESSION_RADIUS / step).ceil() as usize;
    for (i, value) in filtered.iter_mut().enumerate().take(intervals).skip(1) {
        let left = i.saturating_sub(radius);
        let right = i.saturating_add(radius + 1).min(grid.len());
        let (mut sw, mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (j, elevation) in despiked.iter().enumerate().take(right).skip(left) {
            let x = (j as f64 - i as f64) * step;
            let fraction = (x.abs() / REGRESSION_RADIUS).min(1.0);
            let weight = (1.0 - fraction.powi(3)).powi(3);
            sw += weight;
            sx += weight * x;
            sy += weight * elevation;
            sxx += weight * x * x;
            sxy += weight * x * elevation;
        }
        // A local straight-line fit preserves sustained grades and avoids edge bias.
        let denominator = sw * sxx - sx * sx;
        if denominator > 1e-12 {
            *value = (sy * sxx - sx * sxy) / denominator;
        }
    }
    filtered[0] = elevations[0];
    filtered[intervals] = *elevations.last().unwrap();
    if soften_grades {
        soften_grade_transitions(&mut filtered, step);
    }
    let offset = distances[0];
    for (point, elevation) in points
        .iter()
        .zip(elevations.iter_mut())
        .skip(1)
        .take(points.len() - 2)
    {
        let position = ((point.distance - offset) / step).clamp(0.0, intervals as f64);
        let left = (position.floor() as usize).min(intervals - 1);
        let t = position - left as f64;
        let value = filtered[left] + t * (filtered[left + 1] - filtered[left]);
        if (value - *elevation).abs() > 1e-6 {
            *elevation = value;
        }
    }
    Ok(())
}

fn soften_grade_transitions(heights: &mut [f64], step: f64) {
    let grades: Vec<_> = heights
        .windows(2)
        .map(|pair| (pair[1] - pair[0]) / step)
        .collect();
    let radius = (REGRESSION_RADIUS / step).ceil() as usize;
    let first = heights[0];
    let last = heights[heights.len() - 1];
    let mut elevation = first;
    for i in 0..grades.len() {
        let mut weighted = 0.0;
        let mut weights = 0.0;
        for (j, grade) in grades
            .iter()
            .enumerate()
            .take((i + radius + 1).min(grades.len()))
            .skip(i.saturating_sub(radius))
        {
            let fraction = ((j as f64 - i as f64).abs() * step / REGRESSION_RADIUS).min(1.0);
            let weight = (1.0 - fraction.powi(3)).powi(3);
            weighted += weight * grade;
            weights += weight;
        }
        elevation += step * weighted / weights;
        heights[i + 1] = elevation;
    }
    // Distribute the integration residual as a constant grade to retain both anchors.
    let residual = last - elevation;
    let intervals = grades.len() as f64;
    for (i, height) in heights.iter_mut().enumerate().skip(1) {
        *height += residual * i as f64 / intervals;
    }
    *heights.last_mut().unwrap() = last;
}

#[cfg(test)]
mod tests {
    use crate::gpx::{Operation, Track};

    fn track(samples: &[(f64, f64)]) -> Track {
        let points: String = samples
            .iter()
            .map(|(distance, elevation)| {
                format!(
                    "<trkpt lat=\"0\" lon=\"{}\"><ele>{elevation}</ele></trkpt>",
                    distance / 111_195.080_233_532_9
                )
            })
            .collect();
        Track::parse(format!("<gpx><trk><trkseg>{points}</trkseg></trk></gpx>")).unwrap()
    }

    fn automatic(track: &mut Track) {
        track
            .apply(Operation::AutoSmooth, [0.0, track.total_distance()])
            .unwrap();
    }

    #[test]
    fn removes_isolated_spikes_and_keeps_endpoints() {
        let samples: Vec<_> = (0..=100)
            .map(|i| (i as f64 * 5.0, if i == 50 { 150.0 } else { 50.0 }))
            .collect();
        let mut ride = track(&samples);
        automatic(&mut ride);
        assert!(ride
            .points
            .iter()
            .all(|point| (point.elevation - 50.0).abs() < 1e-6));
        assert_eq!(ride.points[0].elevation, 50.0);
        assert_eq!(ride.points[100].elevation, 50.0);
    }

    #[test]
    fn reduces_short_distance_noise_without_limiting_a_steep_climb() {
        let samples: Vec<_> = (0..=100)
            .map(|i| {
                let distance = i as f64 * 5.0;
                let noise = if i == 0 || i == 100 {
                    0.0
                } else if i % 2 == 0 {
                    0.5
                } else {
                    -0.5
                };
                (distance, 50.0 + 0.4 * distance + noise)
            })
            .collect();
        let mut ride = track(&samples);
        let before: f64 = samples
            .iter()
            .map(|(d, h)| (h - (50.0 + 0.4 * d)).powi(2))
            .sum();
        automatic(&mut ride);
        let after: f64 = ride
            .points
            .iter()
            .zip(&samples)
            .map(|(point, (d, _))| (point.elevation - (50.0 + 0.4 * d)).powi(2))
            .sum();
        assert!(after < before * 0.1, "Noise power: {before} -> {after}");
        assert!(ride.slopes(false)[20..80].iter().all(|slope| *slope > 35.0));
    }

    #[test]
    fn already_smooth_profiles_and_broad_hills_are_preserved() {
        for grade in [-0.3, 0.0, 0.4] {
            let samples: Vec<_> = (0..=100)
                .map(|i| (i as f64 * 5.0, 200.0 + grade * i as f64 * 5.0))
                .collect();
            let mut ride = track(&samples);
            automatic(&mut ride);
            assert!(ride
                .points
                .iter()
                .all(|point| point.elevation == point.original));
        }
        let samples: Vec<_> = (0..=100)
            .map(|i| {
                let distance = i as f64 * 5.0;
                (
                    distance,
                    50.0 + 30.0 * (std::f64::consts::PI * distance / 500.0).sin(),
                )
            })
            .collect();
        let mut ride = track(&samples);
        automatic(&mut ride);
        assert!(ride.points[50].elevation > 79.5);
        assert!(ride
            .points
            .iter()
            .zip(samples)
            .all(|(point, (_, h))| (point.elevation - h).abs() < 0.5));
    }

    #[test]
    fn extra_samples_do_not_change_the_smoothing_strength() {
        let coarse: Vec<_> = (0..=100)
            .map(|i| {
                let distance = i as f64 * 5.0;
                (
                    distance,
                    50.0 + 10.0 * (distance / 100.0).sin() + if i % 2 == 0 { 0.5 } else { -0.5 },
                )
            })
            .collect();
        let mut dense = Vec::new();
        for pair in coarse.windows(2) {
            for step in 0..5 {
                let t = step as f64 / 5.0;
                dense.push((pair[0].0 + t * 5.0, pair[0].1 + t * (pair[1].1 - pair[0].1)));
            }
        }
        dense.push(*coarse.last().unwrap());
        let mut coarse_ride = track(&coarse);
        let mut dense_ride = track(&dense);
        automatic(&mut coarse_ride);
        automatic(&mut dense_ride);
        for (index, point) in coarse_ride.points.iter().enumerate() {
            assert!((point.elevation - dense_ride.points[index * 5].elevation).abs() < 1e-5);
        }
    }

    #[test]
    fn incyclist_mode_softens_grade_changes_without_capping_climbs() {
        let samples: Vec<_> = (0..=200)
            .map(|i| {
                let distance = i as f64 * 5.0;
                (
                    distance,
                    100.0
                        + if distance < 500.0 {
                            0.4 * distance
                        } else {
                            200.0 - 0.2 * (distance - 500.0)
                        },
                )
            })
            .collect();
        let mut normal = track(&samples);
        let mut trainer = normal.clone();
        automatic(&mut normal);
        trainer
            .apply(Operation::IncyclistSmooth, [0.0, trainer.total_distance()])
            .unwrap();
        let largest_change = |ride: &Track| {
            ride.slopes(false)[2..]
                .windows(2)
                .map(|pair| (pair[1] - pair[0]).abs())
                .fold(0.0, f64::max)
        };
        assert!(largest_change(&trainer) < largest_change(&normal));
        assert!((trainer.slopes(false)[50] - 40.0).abs() < 0.1);
        assert!((trainer.slopes(false)[150] + 20.0).abs() < 0.1);
        assert_eq!(trainer.points[0].elevation, trainer.points[0].original);
        assert_eq!(trainer.points[200].elevation, trainer.points[200].original);
        for (before, after) in normal.points.iter().zip(&trainer.points) {
            assert_eq!(before.latitude, after.latitude);
            assert_eq!(before.longitude, after.longitude);
            assert_eq!(before.distance, after.distance);
        }
    }

    #[test]
    fn incyclist_mode_is_distance_based_and_respects_selection_and_gaps() {
        let coarse: Vec<_> = (0..=100)
            .map(|i| {
                let distance = i as f64 * 5.0;
                (distance, 100.0 + 10.0 * (distance / 50.0).sin())
            })
            .collect();
        let mut dense = Vec::new();
        for pair in coarse.windows(2) {
            for i in 0..5 {
                let t = i as f64 / 5.0;
                dense.push((pair[0].0 + t * 5.0, pair[0].1 + t * (pair[1].1 - pair[0].1)));
            }
        }
        dense.push(*coarse.last().unwrap());
        let mut a = track(&coarse);
        let mut b = track(&dense);
        for ride in [&mut a, &mut b] {
            ride.apply(Operation::IncyclistSmooth, [0.0, ride.total_distance()])
                .unwrap();
        }
        for (i, point) in a.points.iter().enumerate() {
            assert!((point.elevation - b.points[i * 5].elevation).abs() < 1e-5);
        }
        let mut selected = track(&coarse);
        selected
            .apply(
                Operation::IncyclistSmooth,
                [selected.points[20].distance, selected.points[80].distance],
            )
            .unwrap();
        for (i, point) in selected.points.iter().enumerate() {
            if i <= 20 || i >= 80 {
                assert_eq!(point.elevation, point.original);
            }
        }
        let gaps: Vec<_> = (0..10)
            .map(|i| (i as f64 * 5.0, 100.0))
            .chain((0..10).map(|i| (200.0 + i as f64 * 5.0, 200.0)))
            .collect();
        let mut ride = track(&gaps);
        ride.apply(Operation::IncyclistSmooth, [0.0, ride.total_distance()])
            .unwrap();
        assert!(ride
            .points
            .iter()
            .all(|point| point.elevation == point.original));
    }

    #[test]
    fn selected_range_and_gap_boundaries_are_anchored() {
        let samples: Vec<_> = (0..=100)
            .map(|i| (i as f64 * 5.0, 50.0 + if i % 2 == 0 { 0.5 } else { -0.5 }))
            .collect();
        let mut ride = track(&samples);
        let selection = [ride.points[20].distance, ride.points[80].distance];
        ride.apply(Operation::AutoSmooth, selection).unwrap();
        for (index, point) in ride.points.iter().enumerate() {
            if index <= 20 || index >= 80 {
                assert_eq!(point.elevation, point.original);
            }
        }
        assert!(ride.points[21..80]
            .iter()
            .any(|point| point.elevation != point.original));

        let samples: Vec<_> = (0..10)
            .map(|i| (i as f64 * 5.0, 10.0))
            .chain((0..10).map(|i| (200.0 + i as f64 * 5.0, 100.0)))
            .collect();
        let mut ride = track(&samples);
        automatic(&mut ride);
        assert!(ride
            .points
            .iter()
            .all(|point| point.elevation == point.original));
    }

    #[test]
    fn duplicate_locations_and_short_tracks_are_safe() {
        for samples in [
            vec![(0.0, 10.0)],
            vec![(0.0, 10.0), (0.0, 20.0), (0.0, 10.0)],
            vec![(0.0, 10.0), (5.0, 12.0), (10.0, 11.0)],
        ] {
            let mut ride = track(&samples);
            automatic(&mut ride);
            assert!(ride
                .points
                .iter()
                .all(|point| point.elevation == point.original));
        }
        let samples: Vec<_> = (0..=20)
            .flat_map(|i| [(i as f64 * 5.0, 50.0), (i as f64 * 5.0, 50.0)])
            .collect();
        let mut ride = track(&samples);
        automatic(&mut ride);
        assert!(ride.slopes(false).iter().all(|slope| slope.is_finite()));
        assert!(ride.points.iter().all(|point| point.elevation == 50.0));
    }
}
