use crate::handlers::shared::random::xorshift64;
use photonic_core::path::PathData;

/// Apply a kurbo Affine transform to every point in a PathData, baking
/// the transform into the path coordinates. Used before boolean operations.
pub(crate) fn apply_affine_to_path(path: &PathData, affine: kurbo::Affine) -> PathData {
    use kurbo::{BezPath, PathEl};
    let mut result = BezPath::new();
    for el in path.to_bez_path().elements() {
        let transformed = match *el {
            PathEl::MoveTo(p) => PathEl::MoveTo(affine * p),
            PathEl::LineTo(p) => PathEl::LineTo(affine * p),
            PathEl::CurveTo(c1, c2, p) => PathEl::CurveTo(affine * c1, affine * c2, affine * p),
            PathEl::QuadTo(c, p) => PathEl::QuadTo(affine * c, affine * p),
            PathEl::ClosePath => PathEl::ClosePath,
        };
        result.push(transformed);
    }
    PathData::from_bez_path(&result)
}

/// Apply zig-zag distortion to every segment of a BezPath.
pub(crate) fn apply_zig_zag(
    bez: &kurbo::BezPath,
    size: f64,
    ridges: usize,
    smooth: bool,
) -> kurbo::BezPath {
    use kurbo::{PathEl, Point};

    let mut result = kurbo::BezPath::new();
    let mut current = Point::ZERO;
    let mut subpath_start = Point::ZERO;

    for el in bez.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                result.move_to(p);
                current = p;
                subpath_start = p;
            }
            PathEl::ClosePath => {
                // Zig-zag the closing segment from current to subpath_start.
                if current != subpath_start {
                    zig_zag_segment(&mut result, current, subpath_start, size, ridges, smooth);
                }
                result.close_path();
                current = subpath_start;
            }
            _ => {
                // Flatten curves to a line for simplicity, then zig-zag.
                let seg = match *el {
                    PathEl::LineTo(p) => {
                        current = p;
                        p
                    }
                    PathEl::CurveTo(_, _, p) => {
                        current = p;
                        p
                    }
                    PathEl::QuadTo(_, p) => {
                        current = p;
                        p
                    }
                    _ => unreachable!(),
                };
                let start = match result.elements().last() {
                    Some(PathEl::MoveTo(p)) => *p,
                    _ => {
                        // Walk backward to find the last endpoint.
                        let els = result.elements();
                        let mut pt = Point::ZERO;
                        for e in els.iter().rev() {
                            match e {
                                PathEl::MoveTo(p)
                                | PathEl::LineTo(p)
                                | PathEl::CurveTo(_, _, p)
                                | PathEl::QuadTo(_, p) => {
                                    pt = *p;
                                    break;
                                }
                                PathEl::ClosePath => {}
                            }
                        }
                        pt
                    }
                };
                zig_zag_segment(&mut result, start, seg, size, ridges, smooth);
            }
        }
    }
    result
}

/// Emit zig-zag points between `from` and `to`, appending to `path`.
/// Does NOT emit a MoveTo — assumes the pen is already at `from`.
pub(crate) fn zig_zag_segment(
    path: &mut kurbo::BezPath,
    from: kurbo::Point,
    to: kurbo::Point,
    size: f64,
    ridges: usize,
    smooth: bool,
) {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-9 {
        path.line_to(to);
        return;
    }

    // Unit tangent and normal.
    let tx = dx / len;
    let ty = dy / len;
    let nx = -ty;
    let ny = tx;

    // Total subdivisions = ridges * 2 (each ridge has a peak and a valley).
    let steps = ridges * 2;
    let step_len = len / steps as f64;

    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        let px = from.x + dx * t;
        let py = from.y + dy * t;

        // Alternate displacement: odd = +size/2, even = -size/2.
        // Last point (i == steps) has zero displacement to land on `to`.
        let disp = if i == steps {
            0.0
        } else if i % 2 == 1 {
            size / 2.0
        } else {
            -size / 2.0
        };

        let pt = kurbo::Point::new(px + nx * disp, py + ny * disp);

        if smooth && i < steps {
            // Smooth: use cubic bezier with handles along the tangent direction.
            let handle_len = step_len * 0.3;
            // Previous point displacement.
            let prev_disp = if i == 1 {
                0.0 // from point has no displacement
            } else if (i - 1) % 2 == 1 {
                size / 2.0
            } else {
                -size / 2.0
            };
            let prev_t = (i - 1) as f64 / steps as f64;
            let prev_x = from.x + dx * prev_t + nx * prev_disp;
            let prev_y = from.y + dy * prev_t + ny * prev_disp;

            let cp1 = kurbo::Point::new(prev_x + tx * handle_len, prev_y + ty * handle_len);
            let cp2 = kurbo::Point::new(pt.x - tx * handle_len, pt.y - ty * handle_len);
            path.curve_to(cp1, cp2, pt);
        } else {
            path.line_to(pt);
        }
    }
}

/// Displace every point in a BezPath radially from `center`.
/// Positive strength = bloat (outward), negative = pucker (inward).
pub(crate) fn apply_pucker_bloat(
    bez: &kurbo::BezPath,
    strength: f64,
    center: kurbo::Point,
) -> kurbo::BezPath {
    let displace = |p: kurbo::Point| -> kurbo::Point {
        let dx = p.x - center.x;
        let dy = p.y - center.y;
        let dist = (dx * dx + dy * dy).sqrt();
        if dist < 1e-9 {
            return p;
        }
        // Displacement proportional to distance from center.
        let factor = 1.0 + strength;
        kurbo::Point::new(center.x + dx * factor, center.y + dy * factor)
    };

    let mut result = kurbo::BezPath::new();
    for el in bez.elements() {
        match *el {
            kurbo::PathEl::MoveTo(p) => result.move_to(displace(p)),
            kurbo::PathEl::LineTo(p) => result.line_to(displace(p)),
            kurbo::PathEl::CurveTo(c1, c2, p) => {
                result.curve_to(displace(c1), displace(c2), displace(p))
            }
            kurbo::PathEl::QuadTo(c, p) => result.quad_to(displace(c), displace(p)),
            kurbo::PathEl::ClosePath => result.close_path(),
        }
    }
    result
}

/// Displace every point in a BezPath by a random amount up to `size`.
pub(crate) fn apply_roughen(bez: &kurbo::BezPath, size: f64, seed: u64) -> kurbo::BezPath {
    let mut rng = seed.max(1); // avoid zero state

    let displace = |p: kurbo::Point, rng: &mut u64| -> kurbo::Point {
        let dx = xorshift64(rng) * size;
        let dy = xorshift64(rng) * size;
        kurbo::Point::new(p.x + dx, p.y + dy)
    };

    let mut result = kurbo::BezPath::new();
    for el in bez.elements() {
        match *el {
            kurbo::PathEl::MoveTo(p) => result.move_to(displace(p, &mut rng)),
            kurbo::PathEl::LineTo(p) => result.line_to(displace(p, &mut rng)),
            kurbo::PathEl::CurveTo(c1, c2, p) => {
                result.curve_to(
                    displace(c1, &mut rng),
                    displace(c2, &mut rng),
                    displace(p, &mut rng),
                );
            }
            kurbo::PathEl::QuadTo(c, p) => {
                result.quad_to(displace(c, &mut rng), displace(p, &mut rng));
            }
            kurbo::PathEl::ClosePath => result.close_path(),
        }
    }
    result
}

/// Twirl: rotate each point around `center` by an angle that decreases
/// with distance from center (points near center rotate more → spiral).
pub(crate) fn apply_twirl(
    bez: &kurbo::BezPath,
    angle_rad: f64,
    center: kurbo::Point,
) -> kurbo::BezPath {
    // Find max distance from center to determine falloff.
    let mut max_dist = 0.0f64;
    for el in bez.elements() {
        let pts: Vec<kurbo::Point> = match *el {
            kurbo::PathEl::MoveTo(p) | kurbo::PathEl::LineTo(p) => vec![p],
            kurbo::PathEl::CurveTo(c1, c2, p) => vec![c1, c2, p],
            kurbo::PathEl::QuadTo(c, p) => vec![c, p],
            kurbo::PathEl::ClosePath => vec![],
        };
        for p in pts {
            let d = ((p.x - center.x).powi(2) + (p.y - center.y).powi(2)).sqrt();
            if d > max_dist {
                max_dist = d;
            }
        }
    }

    if max_dist < 1e-9 {
        return bez.clone();
    }

    let twirl_point = |p: kurbo::Point| -> kurbo::Point {
        let dx = p.x - center.x;
        let dy = p.y - center.y;
        let dist = (dx * dx + dy * dy).sqrt();
        // Rotation angle falls off linearly: full angle at center, 0 at max_dist.
        let t = 1.0 - (dist / max_dist).min(1.0);
        let a = angle_rad * t;
        let cos_a = a.cos();
        let sin_a = a.sin();
        kurbo::Point::new(
            center.x + dx * cos_a - dy * sin_a,
            center.y + dx * sin_a + dy * cos_a,
        )
    };

    let mut result = kurbo::BezPath::new();
    for el in bez.elements() {
        match *el {
            kurbo::PathEl::MoveTo(p) => result.move_to(twirl_point(p)),
            kurbo::PathEl::LineTo(p) => result.line_to(twirl_point(p)),
            kurbo::PathEl::CurveTo(c1, c2, p) => {
                result.curve_to(twirl_point(c1), twirl_point(c2), twirl_point(p))
            }
            kurbo::PathEl::QuadTo(c, p) => result.quad_to(twirl_point(c), twirl_point(p)),
            kurbo::PathEl::ClosePath => result.close_path(),
        }
    }
    result
}

/// Round corners of a BezPath by replacing sharp corners with quadratic bezier arcs.
pub(crate) fn apply_round_corners(bez: &kurbo::BezPath, radius: f64) -> kurbo::BezPath {
    // Collect subpaths as sequences of endpoints.
    let elements = bez.elements();
    if elements.is_empty() || radius <= 0.0 {
        return bez.clone();
    }

    // For each subpath, collect the line endpoints and process corners.
    let mut result = kurbo::BezPath::new();
    let mut subpath: Vec<kurbo::Point> = Vec::new();
    let mut is_closed = false;

    let flush = |result: &mut kurbo::BezPath, pts: &[kurbo::Point], closed: bool, radius: f64| {
        if pts.len() < 2 {
            if let Some(&p) = pts.first() {
                result.move_to(p);
            }
            return;
        }

        let n = pts.len();
        let effective_n = n;

        for i in 0..effective_n {
            let prev = if i == 0 {
                if closed {
                    pts[n - 1]
                } else {
                    pts[0]
                }
            } else {
                pts[i - 1]
            };
            let curr = pts[i];
            let next = if i == n - 1 {
                if closed {
                    pts[0]
                } else {
                    pts[n - 1]
                }
            } else {
                pts[i + 1]
            };

            let is_endpoint = (!closed) && (i == 0 || i == n - 1);

            if is_endpoint {
                if i == 0 {
                    result.move_to(curr);
                } else {
                    result.line_to(curr);
                }
            } else {
                // Compute the fillet at this corner.
                let dx_in = curr.x - prev.x;
                let dy_in = curr.y - prev.y;
                let len_in = (dx_in * dx_in + dy_in * dy_in).sqrt();

                let dx_out = next.x - curr.x;
                let dy_out = next.y - curr.y;
                let len_out = (dx_out * dx_out + dy_out * dy_out).sqrt();

                if len_in < 1e-9 || len_out < 1e-9 {
                    if i == 0 {
                        result.move_to(curr);
                    } else {
                        result.line_to(curr);
                    }
                    continue;
                }

                // Clamp radius so adjacent fillets never overlap. Corners here are
                // interior vertices (endpoints handled by the is_endpoint branch),
                // so a neighbour is rounded (and shares the edge 50/50) unless it is
                // an open-run endpoint. For a closed subpath every neighbour is
                // rounded, keeping the L/2 split; on an open run, a corner adjacent
                // to an endpoint retreats (almost) the full edge instead.
                let eps = 1e-3;
                let prev_rounded = closed || i >= 2; // prev (i-1) is not endpoint 0
                let next_rounded = closed || i < n - 2; // next (i+1) is not endpoint n-1
                let max_in = if prev_rounded {
                    len_in / 2.0
                } else {
                    len_in * (1.0 - eps)
                };
                let max_out = if next_rounded {
                    len_out / 2.0
                } else {
                    len_out * (1.0 - eps)
                };
                let r = radius.min(max_in).min(max_out);

                // Points on incoming and outgoing segments at distance r from corner.
                let fillet_start =
                    kurbo::Point::new(curr.x - (dx_in / len_in) * r, curr.y - (dy_in / len_in) * r);
                let fillet_end = kurbo::Point::new(
                    curr.x + (dx_out / len_out) * r,
                    curr.y + (dy_out / len_out) * r,
                );

                if i == 0 {
                    result.move_to(fillet_start);
                } else {
                    result.line_to(fillet_start);
                }

                // Quadratic bezier with control point at the original corner
                // produces a smooth fillet arc.
                result.quad_to(curr, fillet_end);
            }
        }

        if closed {
            result.close_path();
        }
    };

    for el in elements {
        match *el {
            kurbo::PathEl::MoveTo(p) => {
                if !subpath.is_empty() {
                    flush(&mut result, &subpath, is_closed, radius);
                }
                subpath.clear();
                subpath.push(p);
                is_closed = false;
            }
            kurbo::PathEl::LineTo(p) => {
                subpath.push(p);
            }
            kurbo::PathEl::CurveTo(_, _, p) | kurbo::PathEl::QuadTo(_, p) => {
                // For curves, just keep the endpoint (fillet only applies to line corners).
                subpath.push(p);
            }
            kurbo::PathEl::ClosePath => {
                is_closed = true;
            }
        }
    }

    if !subpath.is_empty() {
        flush(&mut result, &subpath, is_closed, radius);
    }

    result
}

#[cfg(test)]
mod round_corners_tests {
    use super::*;

    fn end_vertices(bez: &kurbo::BezPath) -> Vec<kurbo::Point> {
        bez.elements()
            .iter()
            .filter_map(|el| match el {
                kurbo::PathEl::MoveTo(p) => Some(*p),
                kurbo::PathEl::LineTo(p) => Some(*p),
                kurbo::PathEl::QuadTo(_, p) => Some(*p),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn open_run_corner_rounds_past_half_edge() {
        // Open 3-vertex polyline: (0,0) → (10,0) → (10,10). The interior corner at
        // (10,0) borders two endpoints, so it retreats almost the full edge rather
        // than being capped at L/2 = 5.
        let mut bez = kurbo::BezPath::new();
        bez.move_to((0.0, 0.0));
        bez.line_to((10.0, 0.0));
        bez.line_to((10.0, 10.0));
        let out = apply_round_corners(&bez, 8.0);

        // fillet_start is the LineTo preceding the corner's QuadTo (control = corner).
        let els: Vec<kurbo::PathEl> = out.elements().to_vec();
        let mut fillet_start = None;
        for (i, el) in els.iter().enumerate() {
            if let kurbo::PathEl::QuadTo(ctrl, _) = el {
                assert!(
                    (ctrl.x - 10.0).abs() < 1e-6 && ctrl.y.abs() < 1e-6,
                    "quad control should be the corner (10,0), got {ctrl:?}"
                );
                if let kurbo::PathEl::LineTo(p) = els[i - 1] {
                    fillet_start = Some(p);
                }
            }
        }
        let p = fillet_start.expect("expected a rounded corner preceded by a LineTo");
        // From (10,0) toward (0,0) by r=8 ⇒ x=2, past the midpoint x=5. The old
        // unconditional L/2 clamp would have stopped at x=5.
        assert!(
            p.x < 5.0 - 1e-6,
            "fillet_start x {} should be past the half-edge (5.0)",
            p.x
        );
        assert!(
            (p.x - 2.0).abs() < 1e-6,
            "fillet_start x {} expected 2.0",
            p.x
        );
    }

    #[test]
    fn closed_square_splits_edges_fifty_fifty() {
        // Closed 10×10 square with an oversized radius. Every neighbour is rounded,
        // so each corner stays clamped to L/2 = 5 and its fillet points land on the
        // edge midpoints — adjacent fillets meet but never overlap.
        let mut bez = kurbo::BezPath::new();
        bez.move_to((0.0, 0.0));
        bez.line_to((10.0, 0.0));
        bez.line_to((10.0, 10.0));
        bez.line_to((0.0, 10.0));
        bez.close_path();
        let out = apply_round_corners(&bez, 100.0);

        for p in end_vertices(&out) {
            if p.y.abs() < 1e-6 || (p.y - 10.0).abs() < 1e-6 {
                assert!(
                    (p.x - 5.0).abs() < 1e-6,
                    "fillet point {p:?} on a horizontal edge crosses the midpoint x=5"
                );
            }
            if p.x.abs() < 1e-6 || (p.x - 10.0).abs() < 1e-6 {
                assert!(
                    (p.y - 5.0).abs() < 1e-6,
                    "fillet point {p:?} on a vertical edge crosses the midpoint y=5"
                );
            }
        }
    }
}
