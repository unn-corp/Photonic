//! Proportional editing — Blender-style falloff for path anchor points.
//!
//! Moving one (or a set of) "primary" anchor drags every other anchor of the
//! same path by a fraction of the same delta, where the fraction is a falloff of
//! the anchor's distance to the nearest primary. Two scales drive it: **spread**
//! (the radius `R` beyond which influence is 0) and **curve** (the exponent `k`
//! shaping the ramp from 1 at the primary to 0 at `R`).
//!
//! This is the single source of truth shared by the GUI's interactive
//! Proportional Move tool and the `proportional_move_anchor` MCP tool — pure
//! kurbo geometry, no UI or document deps.
use std::collections::HashMap;

use kurbo::{BezPath, PathEl, Point};

use crate::path::PathData;

/// How the distance used by the falloff is measured. Only [`Euclidean`] is
/// implemented; [`Connected`] (arc-length along the path, Blender's "Connected"
/// mode) is a reserved seam that currently falls back to Euclidean.
///
/// [`Euclidean`]: DistanceMetric::Euclidean
/// [`Connected`]: DistanceMetric::Connected
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DistanceMetric {
    /// Straight-line distance in path space from the nearest primary anchor.
    #[default]
    Euclidean,
    /// Distance along the path (not yet implemented — falls back to
    /// [`Euclidean`](DistanceMetric::Euclidean)).
    Connected,
}

/// Bounds for the spread radius (path units).
pub const MIN_SPREAD: f64 = 1.0;
pub const MAX_SPREAD: f64 = 100_000.0;
/// Bounds for the falloff curve exponent.
pub const MIN_CURVE: f64 = 0.1;
pub const MAX_CURVE: f64 = 8.0;
/// Sensible starting values for a fresh session.
pub const DEFAULT_SPREAD: f64 = 120.0;
pub const DEFAULT_CURVE: f64 = 2.0;

/// Falloff weight in `[0, 1]` for a normalized distance `t = d / R`.
///
/// `f(0) = 1` (primary moves fully), `f(t>=1) = 0` (outside the radius nothing
/// moves), monotonically non-increasing between. The curve is `(1 - t)^k`:
/// `k = 1` linear, `k > 1` concentrates influence near the primary (sharp),
/// `k < 1` broadens it into a plateau (soft). `k` is clamped to a sane positive
/// range so it can never invert.
pub fn falloff(t: f64, k: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let k = k.clamp(MIN_CURVE, MAX_CURVE);
    (1.0 - t).powf(k)
}

/// `(element_index, point)` for every element of `bez` that has an on-curve
/// endpoint (`ClosePath` excluded — it owns no anchor). Element indices are the
/// stable per-anchor keys used by [`compute_weights`] and
/// [`bez_move_anchors_weighted`].
pub fn anchor_points(bez: &BezPath) -> Vec<(usize, Point)> {
    bez.elements()
        .iter()
        .enumerate()
        .filter_map(|(i, el)| match el {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => Some((i, *p)),
            PathEl::CurveTo(_, _, p) => Some((i, *p)),
            PathEl::QuadTo(_, p) => Some((i, *p)),
            PathEl::ClosePath => None,
        })
        .collect()
}

/// Per-anchor falloff weights for a proportional move.
///
/// `primary` holds the element indices dragged directly (each gets weight
/// `1.0`). Every other anchor gets `falloff(d / R, k)` where `d` is its distance
/// (per `metric`) to the *nearest* primary anchor; anchors at or beyond `R` are
/// omitted (weight 0 → untouched). Keyed by element index.
pub fn compute_weights(
    bez: &BezPath,
    primary: &[usize],
    radius: f64,
    curve: f64,
    metric: DistanceMetric,
) -> HashMap<usize, f64> {
    // Connected mode not yet implemented — behave as Euclidean.
    let _ = metric;
    let anchors = anchor_points(bez);
    let primary_pts: Vec<Point> = anchors
        .iter()
        .filter(|(i, _)| primary.contains(i))
        .map(|(_, p)| *p)
        .collect();

    let mut weights = HashMap::new();
    let radius = radius.max(f64::MIN_POSITIVE);
    for (idx, p) in &anchors {
        if primary.contains(idx) {
            weights.insert(*idx, 1.0);
            continue;
        }
        let Some(d) = primary_pts
            .iter()
            .map(|q| ((p.x - q.x).powi(2) + (p.y - q.y).powi(2)).sqrt())
            .fold(None, |acc: Option<f64>, d| {
                Some(acc.map_or(d, |a| a.min(d)))
            })
        else {
            continue; // no primaries → nothing to weight against
        };
        let t = d / radius;
        if t >= 1.0 {
            continue;
        }
        let w = falloff(t, curve);
        if w > 0.0 {
            weights.insert(*idx, w);
        }
    }
    weights
}

/// Move a `BezPath`'s anchors by `w * (dx, dy)`, where `w` is each anchor's
/// weight from [`compute_weights`]. Handle-ownership rules keep the path smooth:
///
/// - endpoint `p` and incoming handle `c2` belong to anchor `j` → shifted by
///   `w[j]`;
/// - outgoing handle `c1` belongs to the previous anchor `j-1` → shifted by
///   `w[j-1]` (skipped when `j-1` is a `ClosePath`);
/// - a `QuadTo`'s single control is shared, so it takes the larger of the two
///   incident weights.
///
/// Anchors absent from `weights` have weight 0 and are left untouched. All-ones
/// weights reproduce a rigid translation, so this is a strict superset of a
/// plain anchor move.
pub fn bez_move_anchors_weighted(
    bez: &BezPath,
    weights: &HashMap<usize, f64>,
    dx: f64,
    dy: f64,
) -> BezPath {
    let els: Vec<PathEl> = bez.elements().to_vec();
    let w_of = |j: usize| weights.get(&j).copied().unwrap_or(0.0);
    let shift = |p: Point, w: f64| Point::new(p.x + w * dx, p.y + w * dy);

    let mut result = BezPath::new();
    for (j, el) in els.iter().enumerate() {
        let w_anchor = w_of(j);
        let w_prev = if j > 0 && !matches!(els[j - 1], PathEl::ClosePath) {
            w_of(j - 1)
        } else {
            0.0
        };
        let new_el = match *el {
            PathEl::MoveTo(p) => PathEl::MoveTo(shift(p, w_anchor)),
            PathEl::LineTo(p) => PathEl::LineTo(shift(p, w_anchor)),
            PathEl::CurveTo(c1, c2, p) => {
                PathEl::CurveTo(shift(c1, w_prev), shift(c2, w_anchor), shift(p, w_anchor))
            }
            PathEl::QuadTo(c, p) => {
                PathEl::QuadTo(shift(c, w_anchor.max(w_prev)), shift(p, w_anchor))
            }
            PathEl::ClosePath => PathEl::ClosePath,
        };
        result.push(new_el);
    }
    result
}

/// One-shot proportional move on a [`PathData`]: drag the `primary` anchors by
/// `(dx, dy)` and pull the rest along the falloff. Convenience wrapper over
/// [`compute_weights`] + [`bez_move_anchors_weighted`] for non-interactive
/// callers (the MCP tool). Coordinates and `radius` are in the path's own
/// (local) space.
pub fn proportional_move(
    path: &PathData,
    primary: &[usize],
    dx: f64,
    dy: f64,
    radius: f64,
    curve: f64,
    metric: DistanceMetric,
) -> PathData {
    let bez = path.to_bez_path();
    let weights = compute_weights(&bez, primary, radius, curve, metric);
    let moved = bez_move_anchors_weighted(&bez, &weights, dx, dy);
    PathData::from_bez_path(&moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn falloff_endpoints_and_monotonic() {
        for &k in &[0.1, 0.5, 1.0, 2.0, 8.0] {
            assert!(approx(falloff(0.0, k), 1.0), "f(0) must be 1 (k={k})");
            assert!(approx(falloff(1.0, k), 0.0), "f(1) must be 0 (k={k})");
            assert!(approx(falloff(1.5, k), 0.0), "beyond radius clamps to 0");
        }
        let mut prev = f64::INFINITY;
        for i in 0..=20 {
            let w = falloff(i as f64 / 20.0, 2.0);
            assert!(w <= prev + 1e-12, "falloff must not increase");
            prev = w;
        }
    }

    #[test]
    fn curve_exponent_sharpens_and_broadens() {
        let sharp = falloff(0.5, 3.0);
        let linear = falloff(0.5, 1.0);
        let soft = falloff(0.5, 0.3);
        assert!(approx(linear, 0.5), "linear at half radius is 0.5");
        assert!(
            sharp < linear,
            "higher k concentrates influence near primary"
        );
        assert!(soft > linear, "lower k broadens influence");
    }

    /// `M (0,0) L (10,0) L (20,0)` — three colinear anchors 10 units apart.
    fn three_point_line() -> BezPath {
        let mut b = BezPath::new();
        b.move_to((0.0, 0.0));
        b.line_to((10.0, 0.0));
        b.line_to((20.0, 0.0));
        b
    }

    #[test]
    fn weight_one_is_full_move_weight_zero_untouched() {
        let bez = three_point_line();
        let w = compute_weights(&bez, &[0], 5.0, 2.0, DistanceMetric::Euclidean);
        assert!(approx(w[&0], 1.0), "primary weight is 1");
        assert!(!w.contains_key(&1), "anchor beyond radius is omitted");
        let moved = bez_move_anchors_weighted(&bez, &w, 3.0, 7.0);
        let pts = anchor_points(&moved);
        assert!(
            approx(pts[0].1.x, 3.0) && approx(pts[0].1.y, 7.0),
            "primary moved fully"
        );
        assert!(
            approx(pts[1].1.x, 10.0) && approx(pts[1].1.y, 0.0),
            "far anchor untouched"
        );
    }

    #[test]
    fn neighbor_moves_by_its_weight() {
        let bez = three_point_line();
        let w = compute_weights(&bez, &[0], 20.0, 1.0, DistanceMetric::Euclidean);
        assert!(approx(w[&0], 1.0));
        assert!(approx(w[&1], 0.5), "linear falloff at half radius");
        assert!(!w.contains_key(&2), "anchor exactly at radius is excluded");
        let moved = bez_move_anchors_weighted(&bez, &w, 4.0, 0.0);
        let pts = anchor_points(&moved);
        assert!(approx(pts[0].1.x, 4.0), "primary +4");
        assert!(approx(pts[1].1.x, 12.0), "neighbor +2 (0.5 * 4)");
        assert!(approx(pts[2].1.x, 20.0), "outside anchor unchanged");
    }

    #[test]
    fn all_ones_weights_is_rigid_translation() {
        let bez = three_point_line();
        let mut w = HashMap::new();
        for (i, _) in anchor_points(&bez) {
            w.insert(i, 1.0);
        }
        let moved = bez_move_anchors_weighted(&bez, &w, 5.0, -3.0);
        for (before, after) in anchor_points(&bez).iter().zip(anchor_points(&moved).iter()) {
            assert!(
                approx(after.1.x, before.1.x + 5.0),
                "every anchor shifts +5 in x"
            );
            assert!(
                approx(after.1.y, before.1.y - 3.0),
                "every anchor shifts -3 in y"
            );
        }
    }

    #[test]
    fn curve_handle_scales_with_its_owning_anchor() {
        // OUT handle c1 belongs to anchor 0; IN handle c2 + endpoint to anchor 1.
        let mut bez = BezPath::new();
        bez.move_to((0.0, 0.0));
        bez.curve_to((2.0, 2.0), (8.0, 2.0), (10.0, 0.0));
        let mut w = HashMap::new();
        w.insert(0usize, 1.0);
        w.insert(1usize, 0.5);
        let moved = bez_move_anchors_weighted(&bez, &w, 10.0, 0.0);
        match moved.elements()[1] {
            PathEl::CurveTo(c1, c2, p) => {
                assert!(approx(c1.x, 12.0), "c1 (anchor 0) moves +10");
                assert!(approx(c2.x, 13.0), "c2 (anchor 1) moves +5");
                assert!(approx(p.x, 15.0), "endpoint (anchor 1) moves +5");
            }
            _ => panic!("expected CurveTo"),
        }
    }

    #[test]
    fn proportional_move_on_pathdata_roundtrips() {
        let path = PathData::from_bez_path(&three_point_line());
        let out = proportional_move(&path, &[0], 4.0, 0.0, 20.0, 1.0, DistanceMetric::Euclidean);
        let pts = anchor_points(&out.to_bez_path());
        assert!(approx(pts[0].1.x, 4.0), "primary +4");
        assert!(approx(pts[1].1.x, 12.0), "neighbor +2 via 0.5 weight");
    }
}
