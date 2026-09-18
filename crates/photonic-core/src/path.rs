use kurbo::{BezPath, Shape};
use serde::{Deserialize, Serialize};

/// Wrapper around `kurbo::BezPath` — the canonical path representation.
/// Paths use SVG coordinate convention (Y-axis down).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PathData {
    /// SVG path data string, kept as the serialized form.
    /// The kurbo BezPath is derived from this on demand.
    svg: String,
}

impl PathData {
    pub fn new() -> Self {
        Self { svg: String::new() }
    }

    /// Construct from an SVG path data string (M, L, C, Q, A, Z commands).
    pub fn from_svg(svg: &str) -> Result<Self, String> {
        // Validate by parsing. kurbo intentionally accepts some strings that
        // contain no SVG commands (for example "_") as an empty path, so an
        // `Ok` result alone is not sufficient for a geometry constructor.
        let parsed = BezPath::from_svg(svg).map_err(|e| e.to_string())?;
        if parsed.elements().is_empty() {
            return Err("SVG path data contains no geometry".to_string());
        }
        Ok(Self {
            svg: svg.to_string(),
        })
    }

    /// Construct from a kurbo `BezPath`.
    pub fn from_bez_path(path: &BezPath) -> Self {
        Self { svg: path.to_svg() }
    }

    /// Construct from a rectangle.
    pub fn rect(x: f64, y: f64, width: f64, height: f64) -> Self {
        let path = kurbo::Rect::new(x, y, x + width, y + height).to_path(0.1);
        Self::from_bez_path(&path)
    }

    /// Construct a rectangle with rounded corners.
    /// `radius` is the corner radius (clamped to half the shortest side).
    pub fn rounded_rect(x: f64, y: f64, width: f64, height: f64, radius: f64) -> Self {
        let max_r = (width.min(height) / 2.0).max(0.0);
        let r = radius.min(max_r).max(0.0);
        let rect = kurbo::Rect::new(x, y, x + width, y + height);
        let path = kurbo::RoundedRect::from_rect(rect, r).to_path(0.1);
        Self::from_bez_path(&path)
    }

    /// Construct from an ellipse.
    pub fn ellipse(cx: f64, cy: f64, rx: f64, ry: f64) -> Self {
        let path = kurbo::Ellipse::new((cx, cy), (rx, ry), 0.0).to_path(0.1);
        Self::from_bez_path(&path)
    }

    /// Construct a regular polygon.
    pub fn regular_polygon(cx: f64, cy: f64, radius: f64, sides: usize) -> Self {
        assert!(sides >= 3);
        let mut bez = BezPath::new();
        for i in 0..sides {
            let angle =
                (i as f64 / sides as f64) * std::f64::consts::TAU - std::f64::consts::FRAC_PI_2;
            let x = cx + radius * angle.cos();
            let y = cy + radius * angle.sin();
            if i == 0 {
                bez.move_to((x, y));
            } else {
                bez.line_to((x, y));
            }
        }
        bez.close_path();
        Self::from_bez_path(&bez)
    }

    /// Construct a star shape.
    pub fn star(cx: f64, cy: f64, outer_r: f64, inner_r: f64, points: usize) -> Self {
        assert!(points >= 3);
        let mut bez = BezPath::new();
        let count = points * 2;
        for i in 0..count {
            let angle =
                (i as f64 / count as f64) * std::f64::consts::TAU - std::f64::consts::FRAC_PI_2;
            let r = if i % 2 == 0 { outer_r } else { inner_r };
            let x = cx + r * angle.cos();
            let y = cy + r * angle.sin();
            if i == 0 {
                bez.move_to((x, y));
            } else {
                bez.line_to((x, y));
            }
        }
        bez.close_path();
        Self::from_bez_path(&bez)
    }

    /// Returns a polar (radial) grid centered at `(cx, cy)`.
    ///
    /// Produces `rings + 1` concentric ellipses (from `inner_r` to `outer_r`, inclusive of both)
    /// and `sectors` radial spokes from the inner radius to the outer radius.
    /// All subpaths are open or closed ellipse approximations; spokes are open line segments.
    ///
    /// `inner_r` may be 0 for a full-disk grid. `rings` ≥ 1. `sectors` ≥ 1.
    pub fn polar_grid(
        cx: f64,
        cy: f64,
        outer_r: f64,
        inner_r: f64,
        rings: u32,
        sectors: u32,
    ) -> Self {
        use std::f64::consts::TAU;
        let outer_r = outer_r.abs().max(1.0);
        let inner_r = inner_r.abs().min(outer_r);
        let rings = rings.max(1);
        let sectors = sectors.max(1);

        let mut path = BezPath::new();

        // Concentric circles (drawn as cubic Bézier approximations)
        for i in 0..=rings {
            let t = i as f64 / rings as f64;
            let r = inner_r + (outer_r - inner_r) * t;
            if r < 1e-9 {
                continue;
            }
            // Approximate circle with 4 cubic segments (k = 4/3 * tan(π/8) ... actually simpler:
            // use 4 arcs at 90° each)
            let k = 4.0 / 3.0 * (TAU / 16.0).tan(); // = 4/3 * tan(π/4) for 4-segment circle
            path.move_to((cx + r, cy));
            path.curve_to((cx + r, cy + r * k), (cx + r * k, cy + r), (cx, cy + r));
            path.curve_to((cx - r * k, cy + r), (cx - r, cy + r * k), (cx - r, cy));
            path.curve_to((cx - r, cy - r * k), (cx - r * k, cy - r), (cx, cy - r));
            path.curve_to((cx + r * k, cy - r), (cx + r, cy - r * k), (cx + r, cy));
            path.close_path();
        }

        // Radial spokes
        for s in 0..sectors {
            let angle = TAU * (s as f64 / sectors as f64);
            let (cos_a, sin_a) = (angle.cos(), angle.sin());
            path.move_to((cx + inner_r * cos_a, cy + inner_r * sin_a));
            path.line_to((cx + outer_r * cos_a, cy + outer_r * sin_a));
        }

        Self::from_bez_path(&path)
    }

    /// Returns a rectangular grid spanning `(x, y, x+w, y+h)` with `cols` columns and `rows` rows.
    ///
    /// The result is a single path with multiple open subpaths: `(cols+1)` vertical lines
    /// and `(rows+1)` horizontal lines that form the grid cell borders.
    pub fn grid(x: f64, y: f64, w: f64, h: f64, cols: u32, rows: u32) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let mut path = BezPath::new();
        // Vertical lines
        for i in 0..=cols {
            let lx = x + w * (i as f64 / cols as f64);
            path.move_to((lx, y));
            path.line_to((lx, y + h));
        }
        // Horizontal lines
        for j in 0..=rows {
            let ly = y + h * (j as f64 / rows as f64);
            path.move_to((x, ly));
            path.line_to((x + w, ly));
        }
        Self::from_bez_path(&path)
    }

    /// Returns an elliptical arc centered at `(cx, cy)` with radii `(rx, ry)`.
    ///
    /// Angles are in degrees (0° = 3 o'clock, 90° = 6 o'clock).
    /// The arc sweeps from `start_deg` to `end_deg` in the direction of increasing angle.
    /// If `closed` is true, a chord is drawn back to the start point (pie / sector shape).
    /// Uses cubic Bézier approximation; segments are split at every 90° for accuracy.
    pub fn arc(
        cx: f64,
        cy: f64,
        rx: f64,
        ry: f64,
        start_deg: f64,
        end_deg: f64,
        closed: bool,
    ) -> Self {
        use std::f64::consts::{PI, TAU};

        let rx = rx.abs().max(0.1);
        let ry = ry.abs().max(0.1);
        let start_rad = start_deg.to_radians();
        let mut sweep = (end_deg - start_deg).to_radians();
        // Clamp to at most one full revolution
        sweep = sweep.clamp(-TAU, TAU);
        if sweep.abs() < 1e-9 {
            sweep = 1e-9;
        }

        let n_segs = ((sweep.abs() / (PI / 2.0)).ceil() as usize).max(1);
        let seg = sweep / n_segs as f64;

        let mut path = BezPath::new();
        path.move_to((cx + rx * start_rad.cos(), cy + ry * start_rad.sin()));

        for i in 0..n_segs {
            let a1 = start_rad + i as f64 * seg;
            let a2 = a1 + seg;
            // Magic constant: 4/3 * tan(θ/4)
            let k = 4.0 / 3.0 * (seg / 4.0).tan();
            let (c1, s1) = (a1.cos(), a1.sin());
            let (c2, s2) = (a2.cos(), a2.sin());
            path.curve_to(
                (cx + rx * (c1 - k * s1), cy + ry * (s1 + k * c1)),
                (cx + rx * (c2 + k * s2), cy + ry * (s2 - k * c2)),
                (cx + rx * c2, cy + ry * s2),
            );
        }

        if closed {
            path.close_path();
        }
        Self::from_bez_path(&path)
    }

    /// Returns a line segment from (x0,y0) to (x1,y1).
    pub fn line(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        let mut bez = BezPath::new();
        bez.move_to((x0, y0));
        bez.line_to((x1, y1));
        Self::from_bez_path(&bez)
    }

    /// Returns an Archimedean spiral centered at `(cx, cy)`.
    ///
    /// The radius grows linearly from `inner_r` to `outer_r` over `turns` full rotations.
    /// Each turn is approximated with `segs_per_turn` cubic Bézier segments (minimum 4).
    pub fn spiral(
        cx: f64,
        cy: f64,
        outer_r: f64,
        inner_r: f64,
        turns: f64,
        segs_per_turn: usize,
    ) -> Self {
        use std::f64::consts::PI;

        let turns = turns.max(0.01);
        let segs_per_turn = segs_per_turn.max(4);
        let outer_r = outer_r.abs().max(1.0);
        let inner_r = inner_r.abs().min(outer_r);

        let total_angle = turns * 2.0 * PI;
        let total_segs = ((turns * segs_per_turn as f64).round() as usize).max(1);
        let step = total_angle / total_segs as f64;

        // Archimedean spiral: r(θ) grows linearly from inner_r to outer_r.
        let b = (outer_r - inner_r) / total_angle;

        let pos_at = |theta: f64| -> (f64, f64) {
            let r = inner_r + b * theta;
            (cx + r * theta.cos(), cy + r * theta.sin())
        };
        // Tangent vector d/dθ (r·cos θ, r·sin θ) = (b·cos θ − r·sin θ, b·sin θ + r·cos θ).
        let tan_at = |theta: f64| -> (f64, f64) {
            let r = inner_r + b * theta;
            (
                b * theta.cos() - r * theta.sin(),
                b * theta.sin() + r * theta.cos(),
            )
        };

        let mut path = BezPath::new();
        let p0 = pos_at(0.0);
        path.move_to(p0);

        for i in 0..total_segs {
            let t0 = i as f64 * step;
            let t1 = (i + 1) as f64 * step;

            let p0 = pos_at(t0);
            let p1 = pos_at(t1);
            let d0 = tan_at(t0);
            let d1 = tan_at(t1);

            let dx = p1.0 - p0.0;
            let dy = p1.1 - p0.1;
            let chord = (dx * dx + dy * dy).sqrt();
            let alpha = chord / 3.0;

            let len0 = (d0.0 * d0.0 + d0.1 * d0.1).sqrt().max(1e-12);
            let len1 = (d1.0 * d1.0 + d1.1 * d1.1).sqrt().max(1e-12);

            let cp1 = (p0.0 + d0.0 / len0 * alpha, p0.1 + d0.1 / len0 * alpha);
            let cp2 = (p1.0 - d1.0 / len1 * alpha, p1.1 - d1.1 / len1 * alpha);

            path.curve_to(cp1, cp2, p1);
        }

        Self::from_bez_path(&path)
    }

    /// Get the underlying kurbo `BezPath`.
    pub fn to_bez_path(&self) -> BezPath {
        self.try_to_bez_path().unwrap_or_default()
    }

    /// Parse the stored SVG data without converting malformed geometry into an
    /// empty path. Mutation code should use this form so a parse failure cannot
    /// silently erase a node's contours.
    pub fn try_to_bez_path(&self) -> Result<BezPath, String> {
        if self.svg.is_empty() {
            Ok(BezPath::new())
        } else {
            BezPath::from_svg(&self.svg).map_err(|e| e.to_string())
        }
    }

    /// Whether this path contains at least one drawable segment. A lone
    /// `MoveTo` is syntactically parseable but has no renderable geometry.
    pub fn has_drawable_geometry(&self) -> bool {
        self.try_to_bez_path()
            .is_ok_and(|path| path.segments().next().is_some())
    }

    /// Get the SVG path data string.
    pub fn as_svg(&self) -> &str {
        &self.svg
    }

    /// Total arc length of the path (sum over all segments).
    pub fn arc_length(&self) -> f64 {
        use kurbo::ParamCurveArclen;
        const ACCURACY: f64 = 0.05;
        self.to_bez_path()
            .segments()
            .map(|seg| seg.arclen(ACCURACY))
            .sum()
    }

    /// Sample the point and tangent angle at arc-length `s` along the path.
    ///
    /// Returns `(x, y, angle_radians)` where `angle` is the direction of travel
    /// along the path (`atan2(dy, dx)`), used to orient glyphs in text-on-path.
    /// `s` is clamped to `[0, total_length]`. Returns `None` for an empty path.
    pub fn sample_at_arc_length(&self, s: f64) -> Option<(f64, f64, f64)> {
        use kurbo::{ParamCurve, ParamCurveArclen, PathSeg};
        const ACCURACY: f64 = 0.05;

        // Tangent direction at parameter `t` via central finite difference —
        // works uniformly for line/quad/cubic segments without per-type matching.
        fn tangent_angle(seg: &PathSeg, t: f64) -> f64 {
            const EPS: f64 = 1e-4;
            let p0 = seg.eval((t - EPS).max(0.0));
            let p1 = seg.eval((t + EPS).min(1.0));
            (p1.y - p0.y).atan2(p1.x - p0.x)
        }

        let bez = self.to_bez_path();
        let mut segs = bez.segments().peekable();
        segs.peek()?; // empty path → None

        let s = s.max(0.0);
        let mut acc = 0.0;
        let mut last_seg = None;
        for seg in bez.segments() {
            let len = seg.arclen(ACCURACY);
            last_seg = Some(seg);
            if s <= acc + len || len == 0.0 {
                let local = (s - acc).clamp(0.0, len);
                let t = seg.inv_arclen(local, ACCURACY);
                let p = seg.eval(t);
                return Some((p.x, p.y, tangent_angle(&seg, t)));
            }
            acc += len;
        }

        // `s` is past the end — clamp to the final segment's endpoint.
        let seg = last_seg?;
        let p = seg.eval(1.0);
        Some((p.x, p.y, tangent_angle(&seg, 1.0)))
    }

    pub fn is_empty(&self) -> bool {
        self.svg.is_empty()
    }

    /// Compute the axis-aligned bounding box.
    pub fn bounding_box(&self) -> Option<kurbo::Rect> {
        let path = self.to_bez_path();
        if path.elements().is_empty() {
            None
        } else {
            Some(path.bounding_box())
        }
    }

    /// Returns the number of path elements.
    pub fn element_count(&self) -> usize {
        self.to_bez_path().elements().len()
    }

    /// Insert a new anchor point at the midpoint of every path segment.
    ///
    /// Each pass doubles the number of on-curve points. `passes` is capped at 8.
    /// Uses de Casteljau subdivision at t = 0.5.
    pub fn subdivide(&self, passes: u32) -> PathData {
        use kurbo::PathEl;

        let passes = passes.clamp(1, 8);
        let mut path = self.to_bez_path();

        for _ in 0..passes {
            let elements: Vec<PathEl> = path.elements().to_vec();
            let mut out = BezPath::new();

            let mut current = kurbo::Point::ZERO;
            let mut start_pt = kurbo::Point::ZERO;

            for el in &elements {
                match *el {
                    PathEl::MoveTo(p) => {
                        out.move_to(p);
                        current = p;
                        start_pt = p;
                    }
                    PathEl::LineTo(p) => {
                        let mid =
                            kurbo::Point::new((current.x + p.x) * 0.5, (current.y + p.y) * 0.5);
                        out.line_to(mid);
                        out.line_to(p);
                        current = p;
                    }
                    PathEl::CurveTo(c1, c2, p) => {
                        // De Casteljau at t=0.5
                        let q0 = lerp(current, c1, 0.5);
                        let q1 = lerp(c1, c2, 0.5);
                        let q2 = lerp(c2, p, 0.5);
                        let r0 = lerp(q0, q1, 0.5);
                        let r1 = lerp(q1, q2, 0.5);
                        let s = lerp(r0, r1, 0.5);
                        out.curve_to(q0, r0, s);
                        out.curve_to(r1, q2, p);
                        current = p;
                    }
                    PathEl::QuadTo(c, p) => {
                        let q0 = lerp(current, c, 0.5);
                        let q1 = lerp(c, p, 0.5);
                        let s = lerp(q0, q1, 0.5);
                        out.quad_to(q0, s);
                        out.quad_to(q1, p);
                        current = p;
                    }
                    PathEl::ClosePath => {
                        // Insert a midpoint on the implicit closing segment if needed.
                        if (current.x - start_pt.x).abs() > 1e-9
                            || (current.y - start_pt.y).abs() > 1e-9
                        {
                            let mid = kurbo::Point::new(
                                (current.x + start_pt.x) * 0.5,
                                (current.y + start_pt.y) * 0.5,
                            );
                            out.line_to(mid);
                        }
                        out.close_path();
                        current = start_pt;
                    }
                }
            }

            path = out;
        }

        PathData::from_bez_path(&path)
    }

    /// Convert all interior anchor points to **smooth** joins.
    ///
    /// At each junction between two cubic segments the outgoing control handle
    /// is replaced by the reflection of the incoming handle through the anchor,
    /// preserving the outgoing handle's original length (so curve magnitude is
    /// unchanged; only direction is made collinear).  LineTo segments and the
    /// path endpoints are left unchanged.  All sub-paths are processed.
    pub fn convert_to_smooth(&self) -> PathData {
        use kurbo::{BezPath, PathEl, Point};

        let bez = self.to_bez_path();
        let els: Vec<PathEl> = bez.elements().to_vec();
        let mut out = BezPath::new();

        // Rebuild with smoothed junctions using running state.
        let mut prev_end_ctrl: Option<Point> = None; // c2 of the last CurveTo
        let mut prev_anchor: Option<Point> = None;

        for &el in &els {
            match el {
                PathEl::MoveTo(p) => {
                    out.move_to(p);
                    prev_end_ctrl = None;
                    prev_anchor = Some(p);
                }
                PathEl::CurveTo(c1, c2, p) => {
                    // If the previous segment also ended with a CurveTo, smooth the junction.
                    let new_c1 = if let (Some(end_ctrl), Some(anchor)) =
                        (prev_end_ctrl, prev_anchor)
                    {
                        // Reflect end_ctrl through anchor.
                        let reflected =
                            Point::new(2.0 * anchor.x - end_ctrl.x, 2.0 * anchor.y - end_ctrl.y);
                        // Preserve original c1 magnitude but use reflected direction.
                        let ref_dir = reflected - anchor;
                        let ref_len = ref_dir.hypot();
                        let orig_len = (c1 - anchor).hypot();
                        if ref_len > 1e-9 && orig_len > 1e-9 {
                            Point::new(
                                anchor.x + ref_dir.x / ref_len * orig_len,
                                anchor.y + ref_dir.y / ref_len * orig_len,
                            )
                        } else {
                            c1
                        }
                    } else {
                        c1
                    };
                    out.curve_to(new_c1, c2, p);
                    prev_end_ctrl = Some(c2);
                    prev_anchor = Some(p);
                }
                PathEl::LineTo(p) => {
                    out.line_to(p);
                    prev_end_ctrl = None;
                    prev_anchor = Some(p);
                }
                PathEl::QuadTo(c, p) => {
                    out.quad_to(c, p);
                    prev_end_ctrl = None;
                    prev_anchor = Some(p);
                }
                PathEl::ClosePath => {
                    out.close_path();
                    prev_end_ctrl = None;
                    prev_anchor = None;
                }
            }
        }
        PathData::from_bez_path(&out)
    }

    /// Convert all cubic anchor points to **corner** (cusp) joins.
    ///
    /// Both control handles of each `CurveTo` segment are retracted to the
    /// segment's start and end anchor points respectively, replacing the cubic
    /// with a straight line while keeping the endpoint structure intact.
    /// LineTo and QuadTo segments are unchanged.
    pub fn convert_to_corner(&self) -> PathData {
        use kurbo::{BezPath, PathEl};

        let bez = self.to_bez_path();
        let mut out = BezPath::new();
        let mut cur = kurbo::Point::ZERO;

        for &el in bez.elements() {
            match el {
                PathEl::MoveTo(p) => {
                    out.move_to(p);
                    cur = p;
                }
                PathEl::CurveTo(_, _, p) => {
                    // Retract both handles to their respective anchors → straight line.
                    out.line_to(p);
                    cur = p;
                }
                PathEl::LineTo(p) => {
                    out.line_to(p);
                    cur = p;
                }
                PathEl::QuadTo(c, p) => {
                    out.quad_to(c, p);
                    cur = p;
                }
                PathEl::ClosePath => {
                    out.close_path();
                }
            }
        }
        let _ = cur; // suppress unused warning
        PathData::from_bez_path(&out)
    }

    /// Split the first subpath at the point on it nearest to `(px, py)`.
    ///
    /// Returns `Some((before, after))` where:
    ///   - `before` starts at the subpath's original `MoveTo` and ends at the cut point (open)
    ///   - `after` starts at the cut point and continues to the end of the subpath (open)
    ///
    /// Returns `None` if the path is empty, has no renderable segments, or if both halves
    /// would be degenerate (zero-length).
    ///
    /// **Only the first subpath is split**; any additional subpaths are discarded.
    pub fn split_at_point(&self, px: f64, py: f64) -> Option<(PathData, PathData)> {
        use kurbo::{CubicBez, ParamCurve, ParamCurveNearest, PathEl, Point, QuadBez};

        let bez = self.to_bez_path();
        let target = Point::new(px, py);

        // Collect the first subpath's segments as (start_pt, PathEl).
        let mut segments: Vec<(Point, PathEl)> = Vec::new();
        let mut current = Point::ZERO;
        let mut start_pt = Point::ZERO;
        let mut got_move = false;

        for &el in bez.elements() {
            match el {
                PathEl::MoveTo(p) => {
                    if got_move {
                        break; // stop at the second subpath
                    }
                    got_move = true;
                    current = p;
                    start_pt = p;
                }
                PathEl::LineTo(p) => {
                    if !got_move {
                        continue;
                    }
                    segments.push((current, el));
                    current = p;
                }
                PathEl::CurveTo(_, _, p) => {
                    if !got_move {
                        continue;
                    }
                    segments.push((current, el));
                    current = p;
                }
                PathEl::QuadTo(_, p) => {
                    if !got_move {
                        continue;
                    }
                    segments.push((current, el));
                    current = p;
                }
                PathEl::ClosePath => {
                    // Treat close as a line segment back to start.
                    if !got_move {
                        continue;
                    }
                    if (current.x - start_pt.x).abs() > 1e-9
                        || (current.y - start_pt.y).abs() > 1e-9
                    {
                        segments.push((current, PathEl::LineTo(start_pt)));
                    }
                    break;
                }
            }
        }

        if segments.is_empty() {
            return None;
        }

        // Find the segment and t with the minimum distance to (px, py).
        let mut best_seg_idx = 0usize;
        let mut best_t = 0.5f64;
        let mut best_dist = f64::MAX;

        for (i, &(seg_start, el)) in segments.iter().enumerate() {
            let (dist, t) = match el {
                PathEl::LineTo(p) => {
                    // Project target onto line segment.
                    let dx = p.x - seg_start.x;
                    let dy = p.y - seg_start.y;
                    let len2 = dx * dx + dy * dy;
                    let t = if len2 < 1e-12 {
                        0.0
                    } else {
                        ((target.x - seg_start.x) * dx + (target.y - seg_start.y) * dy) / len2
                    };
                    let t = t.clamp(0.0, 1.0);
                    let qx = seg_start.x + dx * t;
                    let qy = seg_start.y + dy * t;
                    let d = ((qx - target.x).powi(2) + (qy - target.y).powi(2)).sqrt();
                    (d, t)
                }
                PathEl::CurveTo(c1, c2, p) => {
                    let seg = CubicBez::new(seg_start, c1, c2, p);
                    let nearest = seg.nearest(target, 1e-3);
                    let pt = seg.eval(nearest.t);
                    let d = ((pt.x - target.x).powi(2) + (pt.y - target.y).powi(2)).sqrt();
                    (d, nearest.t)
                }
                PathEl::QuadTo(c, p) => {
                    let seg = QuadBez::new(seg_start, c, p);
                    let nearest = seg.nearest(target, 1e-3);
                    let pt = seg.eval(nearest.t);
                    let d = ((pt.x - target.x).powi(2) + (pt.y - target.y).powi(2)).sqrt();
                    (d, nearest.t)
                }
                _ => continue,
            };
            if dist < best_dist {
                best_dist = dist;
                best_seg_idx = i;
                best_t = t;
            }
        }

        // Clamp t away from endpoints to avoid degenerate zero-length halves.
        let t = best_t.clamp(1e-6, 1.0 - 1e-6);
        let (seg_start, seg_el) = segments[best_seg_idx];

        // Compute the split point and the two half-segments.
        let (half_a, half_b, split_pt) = match seg_el {
            PathEl::LineTo(p) => {
                let mid = Point::new(
                    seg_start.x + (p.x - seg_start.x) * t,
                    seg_start.y + (p.y - seg_start.y) * t,
                );
                (PathEl::LineTo(mid), PathEl::LineTo(p), mid)
            }
            PathEl::CurveTo(c1, c2, p) => {
                let q0 = lerp(seg_start, c1, t);
                let q1 = lerp(c1, c2, t);
                let q2 = lerp(c2, p, t);
                let r0 = lerp(q0, q1, t);
                let r1 = lerp(q1, q2, t);
                let s = lerp(r0, r1, t);
                (PathEl::CurveTo(q0, r0, s), PathEl::CurveTo(r1, q2, p), s)
            }
            PathEl::QuadTo(c, p) => {
                let q0 = lerp(seg_start, c, t);
                let q1 = lerp(c, p, t);
                let m = lerp(q0, q1, t);
                (PathEl::QuadTo(q0, m), PathEl::QuadTo(q1, p), m)
            }
            _ => return None,
        };

        // Build the "before" path: MoveTo(start_pt) + segments[0..best_seg_idx] + half_a
        let mut before = BezPath::new();
        before.move_to(start_pt);
        for &(_, el) in &segments[..best_seg_idx] {
            before.push(el);
        }
        before.push(half_a);

        // Build the "after" path: MoveTo(split_pt) + half_b + segments[best_seg_idx+1..]
        let mut after = BezPath::new();
        after.move_to(split_pt);
        after.push(half_b);
        for &(_, el) in &segments[best_seg_idx + 1..] {
            after.push(el);
        }

        // Require both halves to have at least one non-move element.
        let has_seg = |p: &BezPath| p.elements().iter().any(|e| !matches!(e, PathEl::MoveTo(_)));
        if !has_seg(&before) || !has_seg(&after) {
            return None;
        }

        Some((
            PathData::from_bez_path(&before),
            PathData::from_bez_path(&after),
        ))
    }

    /// Smooth the path using Chaikin's corner-cutting algorithm.
    ///
    /// Each pass replaces every pair of adjacent on-curve points with two new
    /// points at ¼ and ¾ along each edge. The resulting polyline is then
    /// converted to a cubic Bézier approximation.
    ///
    /// * `factor` — smoothing strength in [0, 1]; 0.25 is the classic Chaikin value.
    ///   Values closer to 0.5 produce rounder curves.
    /// * `iterations` — number of passes; capped at 8.
    pub fn smooth(&self, factor: f64, iterations: u32) -> PathData {
        use kurbo::PathEl;

        let factor = factor.clamp(0.0, 0.5);
        let passes = iterations.min(8);

        let bez = self.to_bez_path();
        let elements: Vec<PathEl> = bez.elements().to_vec();

        // Split into sub-paths. Each sub-path is a Vec of (x, y) on-curve points plus a closed flag.
        let mut sub_paths: Vec<(Vec<(f64, f64)>, bool)> = Vec::new();
        let mut current: Vec<(f64, f64)> = Vec::new();
        let mut closed = false;

        let on_curve = |el: &PathEl| -> Option<(f64, f64)> {
            match *el {
                PathEl::MoveTo(p) => Some((p.x, p.y)),
                PathEl::LineTo(p) => Some((p.x, p.y)),
                PathEl::QuadTo(_, p) => Some((p.x, p.y)),
                PathEl::CurveTo(_, _, p) => Some((p.x, p.y)),
                PathEl::ClosePath => None,
            }
        };

        for el in &elements {
            match *el {
                PathEl::MoveTo(p) => {
                    if !current.is_empty() {
                        sub_paths.push((std::mem::take(&mut current), closed));
                    }
                    closed = false;
                    current.push((p.x, p.y));
                }
                PathEl::ClosePath => {
                    closed = true;
                }
                _ => {
                    if let Some(pt) = on_curve(el) {
                        current.push(pt);
                    }
                }
            }
        }
        if !current.is_empty() {
            sub_paths.push((current, closed));
        }

        // Run Chaikin on each sub-path.
        let chaikin_pass = |pts: &[(f64, f64)], closed: bool, f: f64| -> Vec<(f64, f64)> {
            let n = pts.len();
            if n < 2 {
                return pts.to_vec();
            }
            let mut out = Vec::with_capacity(n * 2);
            let pairs = if closed { n } else { n - 1 };
            for i in 0..pairs {
                let a = pts[i];
                let b = pts[(i + 1) % n];
                let q = (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f);
                let r = (a.0 + (b.0 - a.0) * (1.0 - f), a.1 + (b.1 - a.1) * (1.0 - f));
                out.push(q);
                out.push(r);
            }
            if !closed && n >= 2 {
                // Keep original endpoints to avoid shrinkage.
                out.insert(0, pts[0]);
                out.push(*pts.last().unwrap());
            }
            out
        };

        let mut out_path = BezPath::new();
        for (mut pts, closed) in sub_paths {
            for _ in 0..passes {
                pts = chaikin_pass(&pts, closed, factor);
            }
            if pts.is_empty() {
                continue;
            }
            // Convert polyline to cubic Bézier path.
            out_path.move_to(kurbo::Point::new(pts[0].0, pts[0].1));
            let n = pts.len();
            let mut i = 1;
            while i < n {
                if i + 1 < n {
                    // Fit a cubic through pts[i-1], pts[i], pts[i+1] using catmull-rom tangents.
                    let p0 = pts[i - 1];
                    let p1 = pts[i];
                    let p2 = pts.get(i + 1).copied().unwrap_or(p1);
                    // Control points: c1 = p1 + (p1 - p0) / 6, c2 = p2 - (p2 - p1) / 6 (approx)
                    let c1x = p1.0 + (p1.0 - p0.0) / 6.0;
                    let c1y = p1.1 + (p1.1 - p0.1) / 6.0;
                    let c2x = p2.0 - (p2.0 - p1.0) / 6.0;
                    let c2y = p2.1 - (p2.1 - p1.1) / 6.0;
                    out_path.curve_to(
                        kurbo::Point::new(c1x, c1y),
                        kurbo::Point::new(c2x, c2y),
                        kurbo::Point::new(p2.0, p2.1),
                    );
                    i += 2;
                } else {
                    out_path.line_to(kurbo::Point::new(pts[i].0, pts[i].1));
                    i += 1;
                }
            }
            if closed {
                out_path.close_path();
            }
        }

        PathData::from_bez_path(&out_path)
    }

    /// Reverse the winding direction of the path.
    ///
    /// Each sub-path's start becomes its old endpoint. For cubic segments the
    /// two control points are swapped; for quadratic segments the control point
    /// remains at the same absolute position (only the destination changes).
    /// Closed paths remain closed.
    pub fn reverse(&self) -> PathData {
        use kurbo::PathEl;

        let bez = self.to_bez_path();
        let elements: Vec<PathEl> = bez.elements().to_vec();

        // ── 1. Split into sub-paths ───────────────────────────────────────────
        let mut sub_paths: Vec<(Vec<PathEl>, bool)> = Vec::new(); // (els, closed)
        let mut current_sub: Vec<PathEl> = Vec::new();
        let mut current_closed = false;

        for el in &elements {
            match *el {
                PathEl::MoveTo(_) => {
                    if !current_sub.is_empty() {
                        sub_paths.push((std::mem::take(&mut current_sub), current_closed));
                        current_closed = false;
                    }
                    current_sub.push(*el);
                }
                PathEl::ClosePath => {
                    current_closed = true;
                }
                _ => {
                    current_sub.push(*el);
                }
            }
        }
        if !current_sub.is_empty() {
            sub_paths.push((current_sub, current_closed));
        }

        // ── 2. Rebuild each sub-path in reverse ──────────────────────────────
        let mut out = BezPath::new();
        for (els, closed) in sub_paths {
            let PathEl::MoveTo(start) = els[0] else {
                continue;
            };
            let segs = &els[1..];

            // Track the absolute start position of each segment
            let mut seg_starts: Vec<kurbo::Point> = Vec::with_capacity(segs.len());
            let mut pos = start;
            for seg in segs {
                seg_starts.push(pos);
                pos = match *seg {
                    PathEl::LineTo(p) => p,
                    PathEl::CurveTo(_, _, p) => p,
                    PathEl::QuadTo(_, p) => p,
                    _ => pos,
                };
            }

            // New path starts at the last endpoint
            out.move_to(pos);

            // Walk segments in reverse, flipping each one's direction
            for (i, seg) in segs.iter().enumerate().rev() {
                let dest = seg_starts[i];
                match *seg {
                    PathEl::LineTo(_) => out.line_to(dest),
                    PathEl::CurveTo(c1, c2, _) => out.curve_to(c2, c1, dest),
                    PathEl::QuadTo(c, _) => out.quad_to(c, dest),
                    _ => {}
                }
            }

            if closed {
                out.close_path();
            }
        }

        PathData::from_bez_path(&out)
    }

    /// Move all anchor points (on-curve points) to their average position on
    /// the requested axes.  Bézier control handles are shifted by the same
    /// delta as their owning anchor so local curve shape is preserved.
    ///
    /// * `avg_x` — equalise all anchor X-coordinates to their mean
    /// * `avg_y` — equalise all anchor Y-coordinates to their mean
    ///
    /// Returns the path unchanged if it has fewer than 2 anchor points, or if
    /// neither axis is selected.
    pub fn average_anchor_points(&self, avg_x: bool, avg_y: bool) -> PathData {
        use kurbo::PathEl;

        if !avg_x && !avg_y {
            return self.clone();
        }

        let bez = self.to_bez_path();
        let elements: Vec<PathEl> = bez.elements().to_vec();

        // ── Pass 1: collect on-curve anchor positions ────────────────────────
        let mut anchors: Vec<kurbo::Point> = Vec::new();
        for el in &elements {
            match *el {
                PathEl::MoveTo(p)
                | PathEl::LineTo(p)
                | PathEl::CurveTo(_, _, p)
                | PathEl::QuadTo(_, p) => anchors.push(p),
                PathEl::ClosePath => {}
            }
        }

        if anchors.len() < 2 {
            return self.clone();
        }

        let n = anchors.len() as f64;
        let mean_x = anchors.iter().map(|p| p.x).sum::<f64>() / n;
        let mean_y = anchors.iter().map(|p| p.y).sum::<f64>() / n;

        // Per-anchor delta (only on active axes).
        let deltas: Vec<kurbo::Point> = anchors
            .iter()
            .map(|p| {
                kurbo::Point::new(
                    if avg_x { mean_x - p.x } else { 0.0 },
                    if avg_y { mean_y - p.y } else { 0.0 },
                )
            })
            .collect();

        // ── Pass 2: rebuild path, shifting anchors and their handles ─────────
        let mut out = BezPath::new();
        let mut anchor_idx: usize = 0;
        let mut prev_delta = kurbo::Point::new(0.0, 0.0);

        for el in &elements {
            match *el {
                PathEl::MoveTo(p) => {
                    let d = deltas[anchor_idx];
                    out.move_to(shift_pt(p, d));
                    prev_delta = d;
                    anchor_idx += 1;
                }
                PathEl::LineTo(p) => {
                    let d = deltas[anchor_idx];
                    out.line_to(shift_pt(p, d));
                    prev_delta = d;
                    anchor_idx += 1;
                }
                PathEl::CurveTo(c1, c2, p) => {
                    let d = deltas[anchor_idx];
                    // c1 is the outgoing handle of the *previous* anchor.
                    // c2 is the incoming handle of the *current* anchor.
                    out.curve_to(shift_pt(c1, prev_delta), shift_pt(c2, d), shift_pt(p, d));
                    prev_delta = d;
                    anchor_idx += 1;
                }
                PathEl::QuadTo(c, p) => {
                    let d = deltas[anchor_idx];
                    // Quadratic control point lies between both anchors;
                    // shift it by the average of the surrounding deltas.
                    let c_delta =
                        kurbo::Point::new((prev_delta.x + d.x) * 0.5, (prev_delta.y + d.y) * 0.5);
                    out.quad_to(shift_pt(c, c_delta), shift_pt(p, d));
                    prev_delta = d;
                    anchor_idx += 1;
                }
                PathEl::ClosePath => out.close_path(),
            }
        }

        PathData::from_bez_path(&out)
    }

    /// Sample `count` equally arc-length-spaced positions along the first subpath of this path.
    ///
    /// Returns a `Vec` of `(x, y, angle_deg)` where `angle_deg` is the tangent direction at
    /// that point (0° = rightward, 90° = downward, consistent with SVG/screen coordinates).
    /// Returns an empty vec if the path has no segments or `count` is 0.
    pub fn sample_positions(&self, count: usize) -> Vec<(f64, f64, f64)> {
        use kurbo::{CubicBez, PathEl, QuadBez};
        if count == 0 {
            return Vec::new();
        }

        // Flatten to polyline (first subpath only).
        let bez = self.to_bez_path();
        let mut pts: Vec<(f64, f64)> = Vec::new();
        let mut last = (0.0f64, 0.0f64);

        for el in bez.elements() {
            match *el {
                PathEl::MoveTo(p) => {
                    if !pts.is_empty() {
                        break;
                    } // only first subpath
                    pts.push((p.x, p.y));
                    last = (p.x, p.y);
                }
                PathEl::LineTo(p) => {
                    pts.push((p.x, p.y));
                    last = (p.x, p.y);
                }
                PathEl::CurveTo(c1, c2, p) => {
                    let seg = CubicBez::new(kurbo::Point::new(last.0, last.1), c1, c2, p);
                    use kurbo::ParamCurve;
                    for i in 1..=16 {
                        let pt = seg.eval(i as f64 / 16.0);
                        pts.push((pt.x, pt.y));
                    }
                    last = (p.x, p.y);
                }
                PathEl::QuadTo(c, p) => {
                    let seg = QuadBez::new(kurbo::Point::new(last.0, last.1), c, p);
                    use kurbo::ParamCurve;
                    for i in 1..=8 {
                        let pt = seg.eval(i as f64 / 8.0);
                        pts.push((pt.x, pt.y));
                    }
                    last = (p.x, p.y);
                }
                PathEl::ClosePath => break,
            }
        }

        if pts.len() < 2 {
            return Vec::new();
        }

        // Build cumulative arc lengths.
        let mut cum: Vec<f64> = Vec::with_capacity(pts.len());
        cum.push(0.0);
        for i in 1..pts.len() {
            let dx = pts[i].0 - pts[i - 1].0;
            let dy = pts[i].1 - pts[i - 1].1;
            cum.push(cum[i - 1] + (dx * dx + dy * dy).sqrt());
        }
        let total = *cum.last().unwrap();
        if total < 1e-9 {
            return Vec::new();
        }

        // Sample at equal arc-length intervals.
        let mut result = Vec::with_capacity(count);
        for k in 0..count {
            let target = total * (k as f64 / (count.max(2) - 1) as f64);
            // Binary search for the segment containing `target`.
            let idx = cum.partition_point(|&c| c < target).min(cum.len() - 1);
            let (x, y) = if idx == 0 {
                pts[0]
            } else {
                let seg_len = cum[idx] - cum[idx - 1];
                let (x, y) = if seg_len < 1e-12 {
                    pts[idx]
                } else {
                    let t = (target - cum[idx - 1]) / seg_len;
                    let (ax, ay) = pts[idx - 1];
                    let (bx, by) = pts[idx];
                    (ax + (bx - ax) * t, ay + (by - ay) * t)
                };
                (x, y)
            };
            // Compute tangent angle from the surrounding segment direction.
            let angle_deg = if idx == 0 || idx >= pts.len() - 1 {
                let (ax, ay) = pts[idx.min(pts.len() - 2)];
                let (bx, by) = pts[(idx + 1).min(pts.len() - 1)];
                (by - ay).atan2(bx - ax).to_degrees()
            } else {
                let (ax, ay) = pts[idx - 1];
                let (bx, by) = pts[idx];
                (by - ay).atan2(bx - ax).to_degrees()
            };
            result.push((x, y, angle_deg));
        }

        result
    }
}

#[inline]
fn shift_pt(p: kurbo::Point, d: kurbo::Point) -> kurbo::Point {
    kurbo::Point::new(p.x + d.x, p.y + d.y)
}

#[inline]
fn lerp(a: kurbo::Point, b: kurbo::Point, t: f64) -> kurbo::Point {
    kurbo::Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

impl PartialEq for PathData {
    fn eq(&self, other: &Self) -> bool {
        self.svg == other.svg
    }
}

#[cfg(test)]
mod arc_length_tests {
    use super::*;
    use std::f64::consts::PI;

    #[test]
    fn compact_exported_cubic_syntax_parses_as_drawable_geometry() {
        // Real compact command adjacency and decimal/comma syntax emitted by
        // Photonic's MBD Dancer SVG export.
        let path = PathData::from_svg(
            "M714.6176303490295,407.12902770204585C719.0967149614303,407.12902770204585 718.2340759953883,407.60282833073194 725.4573601610513,400.88144525891346C726.144894070665,400.2416838047889 725.8049499944054,394.8349653218793 725.9927741786374,394.8349653218793Z",
        )
        .expect("compact M/C/Z syntax should parse");
        assert!(path.has_drawable_geometry());
        assert_eq!(path.to_bez_path().elements().len(), 4);
    }

    #[test]
    fn non_command_text_is_not_accepted_as_an_empty_path() {
        let error = PathData::from_svg("_").expect_err("placeholder is not path geometry");
        assert!(error.contains("no geometry"), "{error}");
        assert!(PathData::from_svg("").is_err());
    }

    #[test]
    fn empty_path_returns_none() {
        assert!(PathData::new().sample_at_arc_length(0.0).is_none());
    }

    #[test]
    fn horizontal_line_length_and_sampling() {
        // A 100-unit horizontal line from (0,0) to (100,0).
        let line = PathData::line(0.0, 0.0, 100.0, 0.0);
        assert!((line.arc_length() - 100.0).abs() < 0.5);

        let (x, y, angle) = line.sample_at_arc_length(25.0).unwrap();
        assert!((x - 25.0).abs() < 0.5, "x={x}");
        assert!(y.abs() < 0.5, "y={y}");
        assert!(angle.abs() < 1e-3, "angle={angle}"); // pointing +x
    }

    #[test]
    fn vertical_line_tangent_is_quarter_turn() {
        let line = PathData::line(0.0, 0.0, 0.0, 50.0);
        let (_, _, angle) = line.sample_at_arc_length(25.0).unwrap();
        assert!((angle - PI / 2.0).abs() < 1e-3, "angle={angle}");
    }

    #[test]
    fn s_past_end_clamps_to_endpoint() {
        let line = PathData::line(0.0, 0.0, 10.0, 0.0);
        let (x, _, _) = line.sample_at_arc_length(999.0).unwrap();
        assert!((x - 10.0).abs() < 0.5, "x={x}");
    }

    #[test]
    fn negative_s_clamps_to_start() {
        let line = PathData::line(0.0, 0.0, 10.0, 0.0);
        let (x, _, _) = line.sample_at_arc_length(-5.0).unwrap();
        assert!(x.abs() < 0.5, "x={x}");
    }
}
