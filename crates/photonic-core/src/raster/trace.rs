//! Deterministic bitmap-to-vector tracing for the interactive Area Trace tool.
//!
//! Pixels are reduced to a small palette, then each palette mask is converted
//! to closed boundary loops. The result is one compound editable path per
//! color, ordered from broad/background colors to smaller foreground colors.

use crate::PathData;
use geo::{Coord, LineString, Simplify};
use kurbo::{BezPath, PathEl, Point};
use std::collections::HashMap;

/// User-facing trace controls after the GUI has sampled the requested area.
#[derive(Debug, Clone, Copy)]
pub struct TraceOptions {
    /// Maximum palette size.
    pub colors: usize,
    /// Ignore sampled pixels below this alpha.
    pub alpha_threshold: u8,
    /// Drop closed contours smaller than this many sampled cells.
    pub min_area: u32,
    /// Curve cleanup in document units. Zero preserves crisp polygon edges.
    pub smoothing: f64,
    /// Treat near-white palette entries as background.
    pub ignore_white: bool,
}

impl Default for TraceOptions {
    fn default() -> Self {
        Self {
            colors: 8,
            alpha_threshold: 16,
            min_area: 4,
            smoothing: 1.0,
            ignore_white: true,
        }
    }
}

/// One palette color and all of its traced contours.
#[derive(Debug, Clone)]
pub struct TracedShape {
    pub rgba: [u8; 4],
    pub path: PathData,
    pub sampled_cells: usize,
}

#[derive(Clone, Copy)]
struct Edge {
    start: (u32, u32),
    end: (u32, u32),
}

/// Trace an RGBA sample grid into editable vector paths spanning `bounds`
/// (`[x0, y0, x1, y1]`) in document coordinates.
pub fn trace_bitmap(
    pixels: &[[u8; 4]],
    width: u32,
    height: u32,
    bounds: [f64; 4],
    options: TraceOptions,
) -> Vec<TracedShape> {
    if width == 0
        || height == 0
        || pixels.len() < width as usize * height as usize
        || !bounds.iter().all(|v| v.is_finite())
        || bounds[2] <= bounds[0]
        || bounds[3] <= bounds[1]
    {
        return Vec::new();
    }

    let Some(palette) = build_palette(pixels, options) else {
        return Vec::new();
    };
    let mut assignments = vec![usize::MAX; width as usize * height as usize];
    let mut counts = vec![0usize; palette.len()];
    for (i, &px) in pixels.iter().take(assignments.len()).enumerate() {
        if px[3] < options.alpha_threshold {
            continue;
        }
        if options.ignore_white && is_near_white(px) {
            continue;
        }
        let cluster = nearest_color(px, &palette);
        if options.ignore_white && is_near_white(palette[cluster]) {
            continue;
        }
        assignments[i] = cluster;
        counts[cluster] += 1;
    }

    let cell_w = (bounds[2] - bounds[0]) / width as f64;
    let cell_h = (bounds[3] - bounds[1]) / height as f64;
    let mut order: Vec<usize> = (0..palette.len()).collect();
    order.sort_by_key(|&cluster| std::cmp::Reverse(counts[cluster]));

    let mut traced = Vec::new();
    for cluster in order {
        if counts[cluster] == 0 {
            continue;
        }
        let loops = mask_loops(&assignments, width, height, cluster, options.min_area);
        if loops.is_empty() {
            continue;
        }
        let mut bez = BezPath::new();
        for ring in loops {
            let points = remove_collinear(&ring);
            if points.len() < 3 {
                continue;
            }
            let points: Vec<Point> = points
                .into_iter()
                .map(|(x, y)| {
                    Point::new(bounds[0] + x as f64 * cell_w, bounds[1] + y as f64 * cell_h)
                })
                .collect();
            append_smoothed_ring(&mut bez, &points, options.smoothing);
        }
        if !bez
            .elements()
            .iter()
            .any(|el| matches!(el, PathEl::ClosePath))
        {
            continue;
        }

        traced.push(TracedShape {
            rgba: palette[cluster],
            path: PathData::from_bez_path(&bez),
            sampled_cells: counts[cluster],
        });
    }
    traced
}

/// Append a closed polygon with bounded quadratic rounding at each corner.
///
/// Area Trace previously sent dense, closed pixel contours through kurbo's
/// optimal cubic fitter. Certain high-tolerance contours make that fitter's
/// root solver abort the process, which cannot be recovered with
/// `catch_unwind`. Keeping smoothing local to each corner is deterministic and
/// guarantees that even a very large requested radius cannot cross either
/// adjacent segment.
fn append_smoothed_ring(path: &mut BezPath, points: &[Point], smoothing: f64) {
    if points.len() < 3 {
        return;
    }

    let smoothing = if smoothing.is_finite() {
        smoothing.max(0.0)
    } else {
        0.0
    };
    if smoothing <= f64::EPSILON {
        path.move_to(points[0]);
        for &point in &points[1..] {
            path.line_to(point);
        }
        path.close_path();
        return;
    }

    // Preserve the useful point-count reduction of the old smoothing stage,
    // but run RDP directly on a closed polyline. If a large tolerance would
    // collapse the ring, retain the original geometry.
    let points = simplify_closed_ring(points, smoothing * 0.45);
    let mut corners = Vec::with_capacity(points.len());
    for index in 0..points.len() {
        let previous = points[(index + points.len() - 1) % points.len()];
        let corner = points[index];
        let next = points[(index + 1) % points.len()];
        let incoming = corner.distance(previous);
        let outgoing = corner.distance(next);
        if incoming <= f64::EPSILON || outgoing <= f64::EPSILON {
            corners.push((corner, corner, corner));
            continue;
        }

        // Never consume more than 45% of either neighboring edge. Adjacent
        // rounded corners therefore cannot overlap, even at the slider maximum.
        let inset = smoothing.min(incoming * 0.45).min(outgoing * 0.45);
        let entry = point_toward(corner, previous, inset / incoming);
        let exit = point_toward(corner, next, inset / outgoing);
        corners.push((entry, corner, exit));
    }

    path.move_to(corners[0].0);
    path.quad_to(corners[0].1, corners[0].2);
    for &(entry, corner, exit) in &corners[1..] {
        path.line_to(entry);
        path.quad_to(corner, exit);
    }
    path.close_path();
}

fn point_toward(origin: Point, target: Point, fraction: f64) -> Point {
    Point::new(
        origin.x + (target.x - origin.x) * fraction,
        origin.y + (target.y - origin.y) * fraction,
    )
}

fn simplify_closed_ring(points: &[Point], tolerance: f64) -> Vec<Point> {
    if points.len() <= 3 || !tolerance.is_finite() || tolerance <= f64::EPSILON {
        return points.to_vec();
    }
    let mut coordinates: Vec<Coord<f64>> = points
        .iter()
        .map(|point| Coord {
            x: point.x,
            y: point.y,
        })
        .collect();
    coordinates.push(coordinates[0]);
    let simplified = LineString::new(coordinates).simplify(&tolerance);
    let mut simplified: Vec<Point> = simplified
        .coords()
        .map(|coordinate| Point::new(coordinate.x, coordinate.y))
        .collect();
    if simplified.len() > 1 && simplified[0].distance(*simplified.last().unwrap()) <= f64::EPSILON {
        simplified.pop();
    }
    if simplified.len() >= 3 {
        simplified
    } else {
        points.to_vec()
    }
}

fn build_palette(pixels: &[[u8; 4]], options: TraceOptions) -> Option<Vec<[u8; 4]>> {
    // A 5-bit/channel histogram keeps initialization fast on large sampled
    // regions while retaining enough chroma separation for stable k-means.
    let mut bins: HashMap<u16, (u64, [u64; 4])> = HashMap::new();
    for &px in pixels {
        if px[3] < options.alpha_threshold {
            continue;
        }
        if options.ignore_white && is_near_white(px) {
            continue;
        }
        let key = ((px[0] as u16 >> 3) << 10) | ((px[1] as u16 >> 3) << 5) | (px[2] as u16 >> 3);
        let entry = bins.entry(key).or_insert((0, [0; 4]));
        entry.0 += 1;
        for channel in 0..4 {
            entry.1[channel] += px[channel] as u64;
        }
    }
    if bins.is_empty() {
        return None;
    }
    let mut candidates: Vec<([u8; 4], u64, u16)> = bins
        .into_iter()
        .map(|(key, (count, sum))| {
            (
                [
                    (sum[0] / count) as u8,
                    (sum[1] / count) as u8,
                    (sum[2] / count) as u8,
                    (sum[3] / count) as u8,
                ],
                count,
                key,
            )
        })
        .collect();
    candidates.sort_by_key(|(_, count, key)| (std::cmp::Reverse(*count), *key));

    let target = options.colors.clamp(1, 32).min(candidates.len());
    let mut palette = vec![candidates[0].0];
    while palette.len() < target {
        let next = candidates
            .iter()
            .filter(|(color, _, _)| !palette.contains(color))
            .max_by_key(|(color, count, _)| {
                let min_dist = palette
                    .iter()
                    .map(|p| color_distance_sq(*color, *p) as u64)
                    .min()
                    .unwrap_or(0);
                min_dist.saturating_mul(count.isqrt().max(1))
            })
            .map(|(color, _, _)| *color);
        let Some(next) = next else { break };
        palette.push(next);
    }

    // A handful of deterministic Lloyd iterations is sufficient after the
    // histogram/farthest-point initialization above.
    for _ in 0..8 {
        let mut sums = vec![[0u64; 4]; palette.len()];
        let mut counts = vec![0u64; palette.len()];
        for &px in pixels {
            if px[3] < options.alpha_threshold {
                continue;
            }
            if options.ignore_white && is_near_white(px) {
                continue;
            }
            let cluster = nearest_color(px, &palette);
            counts[cluster] += 1;
            for channel in 0..4 {
                sums[cluster][channel] += px[channel] as u64;
            }
        }
        for cluster in 0..palette.len() {
            if counts[cluster] == 0 {
                continue;
            }
            for channel in 0..4 {
                palette[cluster][channel] = (sums[cluster][channel] / counts[cluster]) as u8;
            }
        }
    }
    Some(palette)
}

fn nearest_color(pixel: [u8; 4], palette: &[[u8; 4]]) -> usize {
    palette
        .iter()
        .enumerate()
        .min_by_key(|(_, color)| color_distance_sq(pixel, **color))
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn color_distance_sq(a: [u8; 4], b: [u8; 4]) -> u32 {
    // Slight green emphasis approximates perceived sRGB distance without a
    // costly color-space conversion in the interactive path.
    let dr = a[0] as i32 - b[0] as i32;
    let dg = a[1] as i32 - b[1] as i32;
    let db = a[2] as i32 - b[2] as i32;
    (2 * dr * dr + 3 * dg * dg + 2 * db * db) as u32
}

fn is_near_white(color: [u8; 4]) -> bool {
    color[0] >= 245 && color[1] >= 245 && color[2] >= 245
}

fn mask_loops(
    assignments: &[usize],
    width: u32,
    height: u32,
    cluster: usize,
    min_area: u32,
) -> Vec<Vec<(u32, u32)>> {
    let filled = |x: i32, y: i32| -> bool {
        x >= 0
            && y >= 0
            && x < width as i32
            && y < height as i32
            && assignments[y as usize * width as usize + x as usize] == cluster
    };
    let mut edges = Vec::new();
    for y in 0..height {
        for x in 0..width {
            if !filled(x as i32, y as i32) {
                continue;
            }
            if !filled(x as i32, y as i32 - 1) {
                edges.push(Edge {
                    start: (x, y),
                    end: (x + 1, y),
                });
            }
            if !filled(x as i32 + 1, y as i32) {
                edges.push(Edge {
                    start: (x + 1, y),
                    end: (x + 1, y + 1),
                });
            }
            if !filled(x as i32, y as i32 + 1) {
                edges.push(Edge {
                    start: (x + 1, y + 1),
                    end: (x, y + 1),
                });
            }
            if !filled(x as i32 - 1, y as i32) {
                edges.push(Edge {
                    start: (x, y + 1),
                    end: (x, y),
                });
            }
        }
    }

    let mut outgoing: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (index, edge) in edges.iter().enumerate() {
        outgoing.entry(edge.start).or_default().push(index);
    }
    let mut used = vec![false; edges.len()];
    let mut loops = Vec::new();
    for first in 0..edges.len() {
        if used[first] {
            continue;
        }
        let start = edges[first].start;
        let mut ring = vec![start];
        let mut current = first;
        let mut closed = false;
        for _ in 0..=edges.len() {
            if used[current] {
                break;
            }
            used[current] = true;
            let edge = edges[current];
            ring.push(edge.end);
            if edge.end == start {
                closed = true;
                break;
            }
            let direction = edge_direction(edge);
            let Some(candidates) = outgoing.get(&edge.end) else {
                break;
            };
            let priorities = [
                (direction + 1) % 4,
                direction,
                (direction + 3) % 4,
                (direction + 2) % 4,
            ];
            let next = priorities.iter().find_map(|wanted| {
                candidates
                    .iter()
                    .copied()
                    .find(|&i| !used[i] && edge_direction(edges[i]) == *wanted)
            });
            let Some(next) = next else { break };
            current = next;
        }
        if closed {
            ring.pop(); // duplicate closing vertex
            if polygon_area_twice(&ring).unsigned_abs() >= min_area.max(1) as u64 * 2 {
                loops.push(ring);
            }
        }
    }
    loops
}

fn edge_direction(edge: Edge) -> u8 {
    match (
        edge.end.0 as i64 - edge.start.0 as i64,
        edge.end.1 as i64 - edge.start.1 as i64,
    ) {
        (1, 0) => 0,
        (0, 1) => 1,
        (-1, 0) => 2,
        _ => 3,
    }
}

fn polygon_area_twice(points: &[(u32, u32)]) -> i64 {
    points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(a, b)| a.0 as i64 * b.1 as i64 - b.0 as i64 * a.1 as i64)
        .sum()
}

fn remove_collinear(points: &[(u32, u32)]) -> Vec<(u32, u32)> {
    if points.len() <= 3 {
        return points.to_vec();
    }
    (0..points.len())
        .filter_map(|i| {
            let prev = points[(i + points.len() - 1) % points.len()];
            let cur = points[i];
            let next = points[(i + 1) % points.len()];
            let a = (cur.0 as i64 - prev.0 as i64, cur.1 as i64 - prev.1 as i64);
            let b = (next.0 as i64 - cur.0 as i64, next.1 as i64 - cur.1 as i64);
            (a.0 * b.1 != a.1 * b.0).then_some(cur)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> TraceOptions {
        TraceOptions {
            smoothing: 0.0,
            ignore_white: false,
            min_area: 1,
            ..TraceOptions::default()
        }
    }

    #[test]
    fn traces_two_color_split_to_two_editable_shapes() {
        let mut pixels = vec![[255, 0, 0, 255]; 8 * 4];
        for y in 0..4 {
            for x in 4..8 {
                pixels[y * 8 + x] = [0, 0, 255, 255];
            }
        }
        let out = trace_bitmap(
            &pixels,
            8,
            4,
            [10.0, 20.0, 90.0, 60.0],
            TraceOptions {
                colors: 2,
                ..defaults()
            },
        );
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|shape| shape.sampled_cells == 16));
        assert!(out.iter().all(|shape| shape.path.bounding_box().is_some()));
    }

    #[test]
    fn transparent_and_white_background_can_be_ignored() {
        let pixels = vec![[255, 255, 255, 255], [0, 0, 0, 0], [20, 30, 40, 255]];
        let out = trace_bitmap(
            &pixels,
            3,
            1,
            [0.0, 0.0, 3.0, 1.0],
            TraceOptions {
                colors: 3,
                ignore_white: true,
                ..defaults()
            },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rgba[..3], [20, 30, 40]);
    }

    #[test]
    fn one_color_mode_does_not_spend_its_palette_slot_on_ignored_white() {
        let pixels = vec![[255, 255, 255, 255], [20, 30, 40, 255]];
        let out = trace_bitmap(
            &pixels,
            2,
            1,
            [0.0, 0.0, 2.0, 1.0],
            TraceOptions {
                colors: 1,
                ignore_white: true,
                ..defaults()
            },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rgba[..3], [20, 30, 40]);
    }

    #[test]
    fn minimum_area_drops_isolated_specks() {
        let mut pixels = vec![[0, 0, 0, 0]; 5 * 5];
        pixels[0] = [255, 0, 0, 255];
        for y in 2..5 {
            for x in 2..5 {
                pixels[y * 5 + x] = [255, 0, 0, 255];
            }
        }
        let out = trace_bitmap(
            &pixels,
            5,
            5,
            [0.0, 0.0, 5.0, 5.0],
            TraceOptions {
                colors: 1,
                min_area: 4,
                ..defaults()
            },
        );
        assert_eq!(out.len(), 1);
        let moves = out[0]
            .path
            .to_bez_path()
            .elements()
            .iter()
            .filter(|el| matches!(el, PathEl::MoveTo(_)))
            .count();
        assert_eq!(moves, 1, "the one-cell speck should be omitted");
    }

    #[test]
    fn enclosed_transparency_becomes_a_hole_with_opposite_winding() {
        let mut assignments = vec![0usize; 3 * 3];
        assignments[4] = usize::MAX;
        let loops = mask_loops(&assignments, 3, 3, 0, 1);
        assert_eq!(loops.len(), 2);
        let areas: Vec<i64> = loops.iter().map(|ring| polygon_area_twice(ring)).collect();
        assert!(areas.iter().any(|area| *area > 0));
        assert!(areas.iter().any(|area| *area < 0));
    }

    #[test]
    fn high_smoothing_on_a_dense_closed_contour_is_finite_and_curved() {
        let width = 64;
        let height = 64;
        let mut pixels = vec![[0, 0, 0, 0]; width * height];
        for y in 0..height {
            // A connected sawtooth silhouette exercises the dense closed-ring
            // case that previously reached kurbo's aborting cubic root solver.
            let edge = if (y / 2) % 2 == 0 { 48 } else { 32 };
            for x in 0..edge {
                pixels[y * width + x] = [30, 120, 220, 255];
            }
        }

        let out = trace_bitmap(
            &pixels,
            width as u32,
            height as u32,
            [0.0, 0.0, 64.0, 64.0],
            TraceOptions {
                smoothing: 8.0,
                ..defaults()
            },
        );
        assert_eq!(out.len(), 1);
        let bez = out[0].path.to_bez_path();
        assert!(bez
            .elements()
            .iter()
            .any(|element| matches!(element, PathEl::QuadTo(_, _))));
        assert!(bez.elements().iter().all(path_element_is_finite));
    }

    #[test]
    fn non_finite_smoothing_falls_back_to_crisp_geometry() {
        let pixels = vec![[10, 20, 30, 255]; 4];
        let out = trace_bitmap(
            &pixels,
            2,
            2,
            [0.0, 0.0, 2.0, 2.0],
            TraceOptions {
                smoothing: f64::INFINITY,
                ..defaults()
            },
        );
        assert_eq!(out.len(), 1);
        let bez = out[0].path.to_bez_path();
        assert!(!bez
            .elements()
            .iter()
            .any(|element| matches!(element, PathEl::QuadTo(_, _))));
        assert!(bez.elements().iter().all(path_element_is_finite));
    }

    fn path_element_is_finite(element: &PathEl) -> bool {
        let point_is_finite = |point: Point| point.x.is_finite() && point.y.is_finite();
        match *element {
            PathEl::MoveTo(point) | PathEl::LineTo(point) => point_is_finite(point),
            PathEl::QuadTo(control, point) => point_is_finite(control) && point_is_finite(point),
            PathEl::CurveTo(control1, control2, point) => {
                point_is_finite(control1) && point_is_finite(control2) && point_is_finite(point)
            }
            PathEl::ClosePath => true,
        }
    }
}
