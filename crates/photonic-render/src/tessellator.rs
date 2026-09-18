use photonic_core::path::PathData;
use photonic_core::style::{LineCap, LineJoin};

/// Maximum permitted device-space deviation while flattening curves.
pub const TARGET_FLATTENING_ERROR_PX: f64 = 0.25;

const MIN_EFFECTIVE_SCALE: f64 = 0.01;
const MAX_EFFECTIVE_SCALE: f64 = 64.0;

/// Return a path-space flattening tolerance for a view and object transform.
///
/// Lyon and kurbo flatten paths before the object and camera transforms are
/// applied. Scale the tolerance inversely by a conservative upper bound on the
/// linear transform's operator norm so their device-space error remains
/// bounded, while clamping the scale to keep extreme transforms practical.
pub fn adaptive_tolerance(view_scale: f64, matrix: &[f64; 6]) -> f32 {
    let [a, b, c, d, _, _] = *matrix;
    // The largest singular value is the exact operator norm for this 2×2
    // linear transform. The determinant/geometric mean is not a valid bound
    // for anisotropic or sheared transforms: it can report scale 1 while one
    // axis is stretched by 100x.
    let frobenius_sq = a * a + b * b + c * c + d * d;
    let determinant = a * d - b * c;
    let discriminant = (frobenius_sq * frobenius_sq - 4.0 * determinant * determinant).max(0.0);
    let singular_sq = 0.5 * (frobenius_sq + discriminant.sqrt());
    let object_scale = if singular_sq.is_finite() && singular_sq >= 0.0 {
        singular_sq.sqrt()
    } else {
        // Preserve a finite, conservative fallback for very large finite
        // matrices whose squared values overflow f64.
        a.hypot(b).hypot(c.hypot(d))
    };
    let raw_scale = view_scale.abs() * object_scale;
    let effective_scale = if raw_scale.is_nan() {
        1.0
    } else {
        raw_scale.clamp(MIN_EFFECTIVE_SCALE, MAX_EFFECTIVE_SCALE)
    };
    (TARGET_FLATTENING_ERROR_PX / effective_scale) as f32
}

// ── Corner rounding ────────────────────────────────────────────────────────────

/// Returns a new `BezPath` where every sharp LineTo→LineTo corner is replaced
/// by a cubic-bezier arc of the given radius, mirroring CSS `border-radius`.
///
/// Only straight-segment junctions are rounded; bezier curves pass through
/// unchanged.  The radius is clamped so adjacent fillets never overlap: a corner
/// splits a shared edge 50/50 with a rounded neighbour, but may retreat the full
/// edge toward an unrounded neighbour (an open-run endpoint, a concave corner, or
/// a vertex bordering a curve junction).
pub fn round_corners(bez: &kurbo::BezPath, radius: f64) -> kurbo::BezPath {
    if radius <= 0.0 {
        return bez.clone();
    }

    let els: Vec<kurbo::PathEl> = bez.elements().to_vec();
    if els.is_empty() {
        return bez.clone();
    }

    // Split into per-subpath element lists.
    let mut subpaths: Vec<(Vec<kurbo::PathEl>, bool)> = Vec::new();
    let mut cur: Vec<kurbo::PathEl> = Vec::new();
    for &el in &els {
        match el {
            kurbo::PathEl::MoveTo(_) => {
                if !cur.is_empty() {
                    subpaths.push((cur.clone(), false));
                    cur.clear();
                }
                cur.push(el);
            }
            kurbo::PathEl::ClosePath => {
                cur.push(el);
                subpaths.push((cur.clone(), true));
                cur.clear();
            }
            _ => cur.push(el),
        }
    }
    if !cur.is_empty() {
        subpaths.push((cur, false));
    }

    let mut out = kurbo::BezPath::new();
    for (sp, is_closed) in subpaths {
        round_subpath(&sp, is_closed, radius, &mut out);
    }
    out
}

/// Emit a smooth corner arc from the current path position (which must be `p1`)
/// around `corner` to `p2`, using a quadratic bezier.
///
/// A quadratic bezier with the corner as its control point is guaranteed to be
/// convex and non-overshooting for any interior angle, unlike the cubic
/// `(4/3)·tan` approximation which overshoots for angles > ~100°.
fn emit_corner_arc(
    out: &mut kurbo::BezPath,
    _p1: kurbo::Point, // kept for caller symmetry; path is already positioned here
    corner: kurbo::Point,
    p2: kurbo::Point,
) {
    out.quad_to(corner, p2);
}

/// Round a single subpath.  Only LineTo→LineTo junctions are rounded.
fn round_subpath(sp: &[kurbo::PathEl], is_closed: bool, radius: f64, out: &mut kurbo::BezPath) {
    if sp.is_empty() {
        return;
    }
    let move_pt = match sp[0] {
        kurbo::PathEl::MoveTo(p) => p,
        _ => return,
    };

    // Collect the straight-line vertex run.  Non-LineTo elements break the run.
    // We accumulate line vertices; when a curve or ClosePath is seen we flush.

    let mut line_pts: Vec<kurbo::Point> = vec![move_pt];
    let mut move_emitted = false;

    // Helper: emit a straight-only run with rounded corners.
    // `closed` means the first and last pts are connected by the implicit close edge.
    let emit_line_run =
        |pts: &[kurbo::Point], closed: bool, move_emitted: &mut bool, out: &mut kurbo::BezPath| {
            let n = pts.len();
            if n == 0 {
                return;
            }
            if n == 1 {
                if !*move_emitted {
                    out.move_to(pts[0]);
                    *move_emitted = true;
                }
                return;
            }

            // Determine path winding from signed area so we can identify convex corners.
            // Only convex corners are rounded; concave corners are left sharp to avoid
            // inward arcs that produce overlapping stroke artifacts in the glow.
            //
            // Defined above `clamped_r` because the neighbour-aware clamp consults
            // `is_convex` (Rust closures cannot forward-reference later closures).
            let signed_area: f64 = if closed && n >= 3 {
                (0..n)
                    .map(|i| {
                        let a = pts[i];
                        let b = pts[(i + 1) % n];
                        a.x * b.y - b.x * a.y
                    })
                    .sum::<f64>()
            } else {
                1.0
            };
            let winding = if signed_area >= 0.0 {
                1.0_f64
            } else {
                -1.0_f64
            };

            // Returns true if the turn at vertex i is convex (bends outward).
            let is_convex = |i: usize| -> bool {
                let prev = pts[(i + n - 1) % n];
                let cur = pts[i];
                let next = pts[(i + 1) % n];
                let d_in = cur - prev;
                let d_out = next - cur;
                let cross = d_in.x * d_out.y - d_in.y * d_out.x;
                cross * winding > 0.0
            };

            // A neighbour vertex j is "rounded" iff it is a genuine filleted corner
            // of this run and therefore consumes half of the shared edge. For a
            // closed run that is any convex vertex; for an open run only interior
            // convex vertices (endpoints keep their full vertex and take no edge).
            let is_rounded = |j: usize| -> bool {
                if closed {
                    is_convex(j)
                } else {
                    j >= 1 && j < n - 1 && is_convex(j)
                }
            };

            // For each corner i, compute the retreat point (on the incoming segment)
            // and advance point (on the outgoing segment). The radius is clamped so
            // adjacent fillets never overlap: a corner splits an edge 50/50 with a
            // rounded neighbour, but retreats (almost) the full edge toward an
            // unrounded neighbour (an open-run endpoint, a concave/unrounded corner,
            // or a vertex bordering a curve junction).
            let clamped_r = |i: usize| -> f64 {
                let prev = pts[(i + n - 1) % n];
                let cur = pts[i];
                let next = pts[(i + 1) % n];
                let seg_in = (cur - prev).hypot();
                let seg_out = (next - cur).hypot();
                let eps = 1e-3;
                let prev_rounded = is_rounded((i + n - 1) % n);
                let next_rounded = is_rounded((i + 1) % n);
                let max_in = if prev_rounded {
                    seg_in * 0.5
                } else {
                    seg_in * (1.0 - eps)
                };
                let max_out = if next_rounded {
                    seg_out * 0.5
                } else {
                    seg_out * (1.0 - eps)
                };
                radius.min(max_in).min(max_out)
            };

            let retreat = |i: usize| -> kurbo::Point {
                let r = clamped_r(i);
                let prev = pts[(i + n - 1) % n];
                let cur = pts[i];
                let d = prev - cur;
                let len = d.hypot();
                if len > 1e-9 {
                    cur + d * (r / len)
                } else {
                    cur
                }
            };

            let advance = |i: usize| -> kurbo::Point {
                let r = clamped_r(i);
                let cur = pts[i];
                let next = pts[(i + 1) % n];
                let d = next - cur;
                let len = d.hypot();
                if len > 1e-9 {
                    cur + d * (r / len)
                } else {
                    cur
                }
            };

            if closed {
                // Walk all vertices; round only convex corners.
                // For concave corners we emit a plain LineTo to the corner vertex.
                let start = if is_convex(0) { retreat(0) } else { pts[0] };
                out.move_to(start);
                *move_emitted = true;
                let mut pos = start;
                for i in 0..n {
                    if is_convex(i) {
                        let r_i = retreat(i);
                        if (pos - r_i).hypot() > 1e-6 {
                            out.line_to(r_i);
                        }
                        let adv_i = advance(i);
                        emit_corner_arc(out, r_i, pts[i], adv_i);
                        pos = adv_i;
                    } else {
                        out.line_to(pts[i]);
                        pos = pts[i];
                    }
                }
                out.close_path();
            } else {
                // Open run: only internal vertices (1..n-2) are corners.
                if !*move_emitted {
                    out.move_to(pts[0]);
                    *move_emitted = true;
                }
                let mut pos = pts[0];
                for i in 1..n - 1 {
                    if is_convex(i) {
                        let r_i = retreat(i);
                        if (pos - r_i).hypot() > 1e-6 {
                            out.line_to(r_i);
                        }
                        let adv_i = advance(i);
                        emit_corner_arc(out, r_i, pts[i], adv_i);
                        pos = adv_i;
                    } else {
                        out.line_to(pts[i]);
                        pos = pts[i];
                    }
                }
                if (pos - pts[n - 1]).hypot() > 1e-6 {
                    out.line_to(pts[n - 1]);
                }
            }
        };

    for el in &sp[1..] {
        match el {
            kurbo::PathEl::LineTo(p) => {
                line_pts.push(*p);
            }
            kurbo::PathEl::ClosePath => {
                emit_line_run(&line_pts, true, &mut move_emitted, out);
                line_pts.clear();
            }
            kurbo::PathEl::CurveTo(c1, c2, p) => {
                emit_line_run(&line_pts, false, &mut move_emitted, out);
                out.curve_to(*c1, *c2, *p);
                line_pts = vec![*p];
            }
            kurbo::PathEl::QuadTo(c, p) => {
                emit_line_run(&line_pts, false, &mut move_emitted, out);
                out.quad_to(*c, *p);
                line_pts = vec![*p];
            }
            _ => {}
        }
    }

    // Flush any remaining open line run (unclosed subpath).
    if line_pts.len() > 1 && !is_closed {
        emit_line_run(&line_pts, false, &mut move_emitted, out);
    }
}

// ── Tessellation ───────────────────────────────────────────────────────────────

/// A tessellated triangle mesh in local path coordinates.
#[derive(Debug, Default, Clone)]
pub struct Mesh {
    pub vertices: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

impl Mesh {
    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty()
    }
}

/// Subdivide a triangle mesh (1→4 by edge midpoints) until no triangle edge
/// exceeds `max_edge`, up to a triangle budget. Vertex-color renderers use this
/// to make **non-linear** fills (radial/fluid/mesh gradients, patterns) smooth —
/// lyon triangulates convex fills with no interior vertices, so those fills
/// would otherwise be a flat blend of the boundary colors. Shared-edge midpoints
/// are recomputed identically per triangle, so duplicated vertices are safe (no
/// visible seams — same position samples the same color).
pub fn refine_mesh(mesh: &Mesh, max_edge: f32) -> Mesh {
    let max_edge2 = {
        let e = max_edge.max(0.5);
        e * e
    };
    const BUDGET: usize = 60_000;
    let v = &mesh.vertices;

    let mut work: Vec<[[f32; 2]; 3]> = mesh
        .indices
        .chunks_exact(3)
        .filter_map(|t| {
            let (a, b, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
            (a < v.len() && b < v.len() && c < v.len()).then(|| [v[a], v[b], v[c]])
        })
        .collect();

    let mut out: Vec<[[f32; 2]; 3]> = Vec::new();
    let e2 = |a: [f32; 2], b: [f32; 2]| {
        let dx = a[0] - b[0];
        let dy = a[1] - b[1];
        dx * dx + dy * dy
    };
    let mid = |a: [f32; 2], b: [f32; 2]| [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];

    // Longest-edge bisection: split a triangle across the midpoint of its
    // longest edge into two. This converges on lyon's long fan slivers far more
    // efficiently than 1→4 midpoint subdivision (~edge/target triangles per
    // sliver vs (edge/target)²), and keeps triangles well-shaped. Independent
    // (non-conforming) bisection is fine here — a shared edge's midpoint is the
    // same position on both sides and samples the same color, so no seams.
    while let Some(tri) = work.pop() {
        let d01 = e2(tri[0], tri[1]);
        let d12 = e2(tri[1], tri[2]);
        let d20 = e2(tri[2], tri[0]);
        let longest = d01.max(d12).max(d20);
        if longest <= max_edge2 || out.len() + work.len() >= BUDGET {
            out.push(tri);
            continue;
        }
        if d01 >= d12 && d01 >= d20 {
            let m = mid(tri[0], tri[1]);
            work.push([tri[0], m, tri[2]]);
            work.push([m, tri[1], tri[2]]);
        } else if d12 >= d20 {
            let m = mid(tri[1], tri[2]);
            work.push([tri[1], m, tri[0]]);
            work.push([m, tri[2], tri[0]]);
        } else {
            let m = mid(tri[2], tri[0]);
            work.push([tri[2], m, tri[1]]);
            work.push([m, tri[0], tri[1]]);
        }
    }

    let mut vertices = Vec::with_capacity(out.len() * 3);
    let mut indices = Vec::with_capacity(out.len() * 3);
    for tri in out {
        let base = vertices.len() as u32;
        vertices.extend_from_slice(&tri);
        indices.extend_from_slice(&[base, base + 1, base + 2]);
    }
    Mesh { vertices, indices }
}

/// Tessellate a filled `PathData` into a `Mesh` using lyon.
/// Vertices are returned in path-local coordinates (transforms applied by the renderer).
/// When `even_odd` is true, uses the even-odd fill rule (for compound paths with holes).
pub fn tessellate_fill(path: &PathData, even_odd: bool, tolerance: f32) -> Mesh {
    use lyon::tessellation::{
        BuffersBuilder, FillOptions, FillRule, FillTessellator, FillVertex, VertexBuffers,
    };

    let bez = path.to_bez_path();
    if bez.elements().is_empty() {
        return Mesh::default();
    }

    let lyon_path = bezpath_to_lyon(&bez);

    let mut geometry: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    let mut tess = FillTessellator::new();

    let fill_rule = if even_odd {
        FillRule::EvenOdd
    } else {
        FillRule::NonZero
    };
    let opts = FillOptions::default()
        .with_tolerance(tolerance)
        .with_fill_rule(fill_rule);

    if tess
        .tessellate_path(
            &lyon_path,
            &opts,
            &mut BuffersBuilder::new(&mut geometry, |v: FillVertex| {
                [v.position().x, v.position().y]
            }),
        )
        .is_err()
    {
        return Mesh::default();
    }

    Mesh {
        vertices: geometry.vertices,
        indices: geometry.indices,
    }
}

/// Tessellate a stroked `PathData` outline into a `Mesh` using lyon.
pub fn tessellate_stroke(
    path: &PathData,
    width: f32,
    cap: LineCap,
    join: LineJoin,
    miter_limit: f32,
    tolerance: f32,
) -> Mesh {
    use lyon::tessellation::{
        BuffersBuilder, LineCap as LyonCap, LineJoin as LyonJoin, StrokeOptions, StrokeTessellator,
        StrokeVertex, VertexBuffers,
    };

    let bez = path.to_bez_path();
    if bez.elements().is_empty() {
        return Mesh::default();
    }

    let lyon_path = bezpath_to_lyon(&bez);

    let lyon_cap = match cap {
        LineCap::Butt => LyonCap::Butt,
        LineCap::Round => LyonCap::Round,
        LineCap::Square => LyonCap::Square,
    };
    let lyon_join = match join {
        LineJoin::Miter => LyonJoin::Miter,
        LineJoin::Round => LyonJoin::Round,
        LineJoin::Bevel => LyonJoin::Bevel,
    };

    let opts = StrokeOptions::default()
        .with_line_width(width)
        .with_tolerance(tolerance)
        .with_start_cap(lyon_cap)
        .with_end_cap(lyon_cap)
        .with_line_join(lyon_join)
        .with_miter_limit(miter_limit);

    let mut geometry: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    let mut tess = StrokeTessellator::new();

    if tess
        .tessellate_path(
            &lyon_path,
            &opts,
            &mut BuffersBuilder::new(&mut geometry, |v: StrokeVertex| {
                [v.position().x, v.position().y]
            }),
        )
        .is_err()
    {
        return Mesh::default();
    }

    Mesh {
        vertices: geometry.vertices,
        indices: geometry.indices,
    }
}

/// Tessellate a stroked `kurbo::BezPath` (already processed) into a `Mesh`.
/// Used when the path has been pre-transformed (e.g. corner-rounded).
pub fn tessellate_stroke_bez(
    bez: &kurbo::BezPath,
    width: f32,
    cap: LineCap,
    join: LineJoin,
    miter_limit: f32,
    tolerance: f32,
) -> Mesh {
    use lyon::tessellation::{
        BuffersBuilder, LineCap as LyonCap, LineJoin as LyonJoin, StrokeOptions, StrokeTessellator,
        StrokeVertex, VertexBuffers,
    };

    if bez.elements().is_empty() {
        return Mesh::default();
    }

    let lyon_path = bezpath_to_lyon(bez);

    let lyon_cap = match cap {
        LineCap::Butt => LyonCap::Butt,
        LineCap::Round => LyonCap::Round,
        LineCap::Square => LyonCap::Square,
    };
    let lyon_join = match join {
        LineJoin::Miter => LyonJoin::Miter,
        LineJoin::Round => LyonJoin::Round,
        LineJoin::Bevel => LyonJoin::Bevel,
    };

    let opts = StrokeOptions::default()
        .with_line_width(width)
        .with_tolerance(tolerance)
        .with_start_cap(lyon_cap)
        .with_end_cap(lyon_cap)
        .with_line_join(lyon_join)
        .with_miter_limit(miter_limit);

    let mut geometry: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    let mut tess = StrokeTessellator::new();

    if tess
        .tessellate_path(
            &lyon_path,
            &opts,
            &mut BuffersBuilder::new(&mut geometry, |v: StrokeVertex| {
                [v.position().x, v.position().y]
            }),
        )
        .is_err()
    {
        return Mesh::default();
    }

    Mesh {
        vertices: geometry.vertices,
        indices: geometry.indices,
    }
}

/// Linearly sample a width profile at parameter `t ∈ [0, 1]`.
/// `widths` are samples at uniform t intervals (`widths[0]` at t=0,
/// `widths[last]` at t=1). Values are interpolated linearly between samples.
fn sample_width_profile(widths: &[f64], t: f64) -> f64 {
    match widths.len() {
        0 => 1.0,
        1 => widths[0],
        n => {
            let t = t.clamp(0.0, 1.0);
            let scaled = t * (n - 1) as f64;
            let i = scaled.floor() as usize;
            if i >= n - 1 {
                widths[n - 1]
            } else {
                let frac = scaled - i as f64;
                widths[i] * (1.0 - frac) + widths[i + 1] * frac
            }
        }
    }
}

/// Tessellate a variable-width stroke into a filled outline ribbon.
///
/// `widths` are width samples at uniform `t` intervals along the path
/// (t=0 at the start, t=1 at the end). The path is flattened, each vertex is
/// offset by the interpolated half-width along its normal on both sides, and
/// the resulting ribbon is triangulated directly. Unlike [`tessellate_stroke`]
/// this honours a true variable width rather than a single average value.
///
/// Falls back to producing an empty mesh when fewer than two width samples are
/// supplied; callers should use the uniform path in that case.
pub fn tessellate_stroke_variable(path: &PathData, widths: &[f64], tolerance: f64) -> Mesh {
    if widths.len() < 2 {
        return Mesh::default();
    }

    let bez = path.to_bez_path();
    if bez.elements().is_empty() {
        return Mesh::default();
    }

    // Flatten the (possibly curved, multi-subpath) outline into polylines.
    let mut subpaths: Vec<(Vec<kurbo::Point>, bool)> = Vec::new();
    let mut cur: Vec<kurbo::Point> = Vec::new();
    let mut closed = false;
    let flush = |cur: &mut Vec<kurbo::Point>,
                 closed: &mut bool,
                 out: &mut Vec<(Vec<kurbo::Point>, bool)>| {
        if cur.len() >= 2 {
            out.push((std::mem::take(cur), *closed));
        } else {
            cur.clear();
        }
        *closed = false;
    };
    kurbo::flatten(bez.elements().iter().copied(), tolerance, |el| match el {
        kurbo::PathEl::MoveTo(p) => {
            flush(&mut cur, &mut closed, &mut subpaths);
            cur.push(p);
        }
        kurbo::PathEl::LineTo(p) => cur.push(p),
        kurbo::PathEl::ClosePath => closed = true,
        // `flatten` only emits MoveTo / LineTo / ClosePath.
        _ => {}
    });
    flush(&mut cur, &mut closed, &mut subpaths);

    let mut vertices: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    for (mut pts, is_closed) in subpaths {
        if is_closed && pts.first() != pts.last() {
            pts.push(pts[0]);
        }
        let n = pts.len();
        if n < 2 {
            continue;
        }

        // Cumulative arc length → per-vertex t.
        let mut clen = vec![0.0_f64; n];
        for i in 1..n {
            clen[i] = clen[i - 1] + (pts[i] - pts[i - 1]).hypot();
        }
        let total = clen[n - 1];
        if total <= f64::EPSILON {
            continue;
        }

        let base = vertices.len() as u32;
        for i in 0..n {
            // Averaged tangent at the vertex for smooth normals on interior points.
            let tangent = if i == 0 {
                pts[1] - pts[0]
            } else if i == n - 1 {
                pts[n - 1] - pts[n - 2]
            } else {
                (pts[i] - pts[i - 1]).normalize() + (pts[i + 1] - pts[i]).normalize()
            };
            let tan = if tangent.hypot() > f64::EPSILON {
                tangent.normalize()
            } else {
                kurbo::Vec2::new(1.0, 0.0)
            };
            let normal = kurbo::Vec2::new(-tan.y, tan.x);
            let half_w = sample_width_profile(widths, clen[i] / total) * 0.5;
            let l = pts[i] + normal * half_w;
            let r = pts[i] - normal * half_w;
            vertices.push([l.x as f32, l.y as f32]);
            vertices.push([r.x as f32, r.y as f32]);
        }

        for i in 0..n - 1 {
            let l0 = base + (i * 2) as u32;
            let r0 = base + (i * 2 + 1) as u32;
            let l1 = base + ((i + 1) * 2) as u32;
            let r1 = base + ((i + 1) * 2 + 1) as u32;
            indices.extend_from_slice(&[l0, r0, r1, l0, r1, l1]);
        }
    }

    Mesh { vertices, indices }
}

/// Convert a `kurbo::BezPath` into a `lyon::path::Path`.
/// Handles open and closed contours, including multiple subpaths.
fn bezpath_to_lyon(bez: &kurbo::BezPath) -> lyon::path::Path {
    use lyon::math::point;
    use lyon::path::Path as LyonPath;

    let mut builder = LyonPath::builder();
    let mut in_contour = false;

    for el in bez.elements() {
        match el {
            kurbo::PathEl::MoveTo(p) => {
                if in_contour {
                    builder.end(false);
                }
                builder.begin(point(p.x as f32, p.y as f32));
                in_contour = true;
            }
            kurbo::PathEl::LineTo(p) => {
                builder.line_to(point(p.x as f32, p.y as f32));
            }
            kurbo::PathEl::CurveTo(c1, c2, p) => {
                builder.cubic_bezier_to(
                    point(c1.x as f32, c1.y as f32),
                    point(c2.x as f32, c2.y as f32),
                    point(p.x as f32, p.y as f32),
                );
            }
            kurbo::PathEl::QuadTo(c, p) => {
                builder.quadratic_bezier_to(
                    point(c.x as f32, c.y as f32),
                    point(p.x as f32, p.y as f32),
                );
            }
            kurbo::PathEl::ClosePath => {
                builder.end(true);
                in_contour = false;
            }
        }
    }
    if in_contour {
        builder.end(false);
    }

    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refine_mesh_densifies_large_triangles() {
        // One big 100-unit triangle refined to <=10-unit edges.
        let mesh = Mesh {
            vertices: vec![[0.0, 0.0], [100.0, 0.0], [0.0, 100.0]],
            indices: vec![0, 1, 2],
        };
        let refined = refine_mesh(&mesh, 10.0);
        assert!(refined.indices.len() > mesh.indices.len() * 20);
        // Every resulting edge is within the target.
        for t in refined.indices.chunks_exact(3) {
            let p = [
                refined.vertices[t[0] as usize],
                refined.vertices[t[1] as usize],
                refined.vertices[t[2] as usize],
            ];
            for (a, b) in [(0, 1), (1, 2), (2, 0)] {
                let dx = p[a][0] - p[b][0];
                let dy = p[a][1] - p[b][1];
                assert!((dx * dx + dy * dy).sqrt() <= 10.5, "edge too long");
            }
        }
    }

    #[test]
    fn refine_mesh_noop_when_already_fine() {
        let mesh = Mesh {
            vertices: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            indices: vec![0, 1, 2],
        };
        assert_eq!(refine_mesh(&mesh, 10.0).indices.len(), 3);
    }

    #[test]
    fn sample_width_profile_interpolates_linearly() {
        let widths = [2.0, 10.0];
        assert_eq!(sample_width_profile(&widths, 0.0), 2.0);
        assert_eq!(sample_width_profile(&widths, 1.0), 10.0);
        assert_eq!(sample_width_profile(&widths, 0.5), 6.0);
        // Out-of-range t is clamped.
        assert_eq!(sample_width_profile(&widths, -1.0), 2.0);
        assert_eq!(sample_width_profile(&widths, 2.0), 10.0);
    }

    #[test]
    fn variable_stroke_widens_with_profile() {
        // A horizontal line from (0,0) to (100,0); width ramps 2 → 20.
        let path = PathData::line(0.0, 0.0, 100.0, 0.0);
        let mesh = tessellate_stroke_variable(&path, &[2.0, 20.0], 0.1);
        assert!(!mesh.is_empty(), "variable stroke should produce geometry");

        // The ribbon spans the line's normal (the y axis). Vertical extent near
        // the start must be ~2px and near the end ~20px.
        let near_start: Vec<f32> = mesh
            .vertices
            .iter()
            .filter(|v| v[0] < 5.0)
            .map(|v| v[1])
            .collect();
        let near_end: Vec<f32> = mesh
            .vertices
            .iter()
            .filter(|v| v[0] > 95.0)
            .map(|v| v[1])
            .collect();
        let span = |ys: &[f32]| {
            let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
            for &y in ys {
                lo = lo.min(y);
                hi = hi.max(y);
            }
            hi - lo
        };
        let start_span = span(&near_start);
        let end_span = span(&near_end);
        assert!((start_span - 2.0).abs() < 0.5, "start span {start_span}");
        assert!((end_span - 20.0).abs() < 0.5, "end span {end_span}");
    }

    #[test]
    fn variable_stroke_needs_two_samples() {
        let path = PathData::line(0.0, 0.0, 10.0, 0.0);
        assert!(tessellate_stroke_variable(&path, &[5.0], 0.1).is_empty());
        assert!(tessellate_stroke_variable(&path, &[], 0.1).is_empty());
    }

    // Extract the end point of each path element (start for MoveTo, end for
    // LineTo/QuadTo), skipping control points and closes.
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
    fn open_polyline_corner_rounds_past_half_edge() {
        // Open 3-vertex polyline: (0,0) → (10,0) → (10,10). The single interior
        // corner at (10,0) has two endpoint neighbours, so it may retreat almost
        // the full edge instead of being capped at L/2 = 5.
        let mut bez = kurbo::BezPath::new();
        bez.move_to((0.0, 0.0));
        bez.line_to((10.0, 0.0));
        bez.line_to((10.0, 10.0));
        let out = round_corners(&bez, 8.0);

        // The retreat point is the LineTo immediately preceding the corner's QuadTo,
        // whose control point is the original corner (10,0).
        let els: Vec<kurbo::PathEl> = out.elements().to_vec();
        let mut retreat = None;
        for (i, el) in els.iter().enumerate() {
            if let kurbo::PathEl::QuadTo(ctrl, _) = el {
                assert!(
                    (ctrl.x - 10.0).abs() < 1e-6 && ctrl.y.abs() < 1e-6,
                    "quad control should be the corner (10,0), got {ctrl:?}"
                );
                if let kurbo::PathEl::LineTo(p) = els[i - 1] {
                    retreat = Some(p);
                }
            }
        }
        let r = retreat.expect("expected a rounded corner with a preceding LineTo");
        // Retreat from (10,0) toward (0,0) by r=8 ⇒ x=2, past the midpoint x=5.
        // The old unconditional L/2 clamp would have stopped at x=5.
        assert!(
            r.x < 5.0 - 1e-6,
            "retreat x {} should be past the half-edge (5.0)",
            r.x
        );
        assert!((r.x - 2.0).abs() < 1e-6, "retreat x {} expected 2.0", r.x);
    }

    #[test]
    fn closed_rect_adjacent_fillets_never_overlap() {
        // A closed 10×10 square with an oversized radius. Every corner's neighbour
        // is also rounded, so each fillet must still stay clamped to L/2 = 5 and
        // meet its neighbour exactly at the edge midpoint — never crossing it.
        let mut bez = kurbo::BezPath::new();
        bez.move_to((0.0, 0.0));
        bez.line_to((10.0, 0.0));
        bez.line_to((10.0, 10.0));
        bez.line_to((0.0, 10.0));
        bez.close_path();
        let out = round_corners(&bez, 100.0);

        // Each fillet end point must sit at an edge midpoint (distance 5 from the
        // corner), i.e. no point retreats past half of any 10-unit edge.
        for p in end_vertices(&out) {
            let on_bottom = p.y.abs() < 1e-6;
            let on_top = (p.y - 10.0).abs() < 1e-6;
            let on_left = p.x.abs() < 1e-6;
            let on_right = (p.x - 10.0).abs() < 1e-6;
            if on_bottom || on_top {
                assert!(
                    (p.x - 5.0).abs() < 1e-6,
                    "fillet point {p:?} on a horizontal edge crosses the midpoint x=5"
                );
            }
            if on_left || on_right {
                assert!(
                    (p.y - 5.0).abs() < 1e-6,
                    "fillet point {p:?} on a vertical edge crosses the midpoint y=5"
                );
            }
        }
    }
}

/// Split triangles wherever they cross the given axis-aligned lines, so every
/// resulting triangle lies entirely within one grid cell. Used to give mesh
/// (spreadsheet-grid) fills clean, straight cell boundaries on the vertex-color
/// renderers — otherwise the hard color edge zig-zags along the triangulation.
/// `xs`/`ys` are line positions in the same space as the triangle coordinates.
pub fn cut_triangles(tris: &mut Vec<[[f64; 2]; 3]>, xs: &[f64], ys: &[f64]) {
    const CAP: usize = 200_000;
    for &c in xs {
        if tris.len() >= CAP {
            break;
        }
        let mut next = Vec::with_capacity(tris.len() + 8);
        for t in tris.iter() {
            split_axis(*t, c, 0, &mut next);
        }
        *tris = next;
    }
    for &c in ys {
        if tris.len() >= CAP {
            break;
        }
        let mut next = Vec::with_capacity(tris.len() + 8);
        for t in tris.iter() {
            split_axis(*t, c, 1, &mut next);
        }
        *tris = next;
    }
}

/// Split one triangle by the line `axis == c` (`axis` 0 = vertical x=c, 1 =
/// horizontal y=c), appending the 1–3 resulting triangles.
fn split_axis(tri: [[f64; 2]; 3], c: f64, axis: usize, out: &mut Vec<[[f64; 2]; 3]>) {
    let below = [tri[0][axis] < c, tri[1][axis] < c, tri[2][axis] < c];
    let n_below = below.iter().filter(|&&b| b).count();
    if n_below == 0 || n_below == 3 {
        out.push(tri);
        return;
    }
    // The lone vertex sits alone on its side of the line.
    let lone = if n_below == 1 {
        below.iter().position(|&b| b).unwrap()
    } else {
        below.iter().position(|&b| !b).unwrap()
    };
    let a = tri[lone];
    let b = tri[(lone + 1) % 3];
    let d = tri[(lone + 2) % 3];
    let cross = |p: [f64; 2], q: [f64; 2]| -> [f64; 2] {
        let denom = q[axis] - p[axis];
        let t = if denom.abs() < 1e-12 {
            0.5
        } else {
            (c - p[axis]) / denom
        };
        [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t]
    };
    let mab = cross(a, b);
    let mad = cross(a, d);
    out.push([a, mab, mad]);
    out.push([mab, b, d]);
    out.push([mab, d, mad]);
}

#[cfg(test)]
mod cut_tests {
    use super::*;
    #[test]
    fn cut_triangles_no_straddle() {
        let mut tris = vec![[[0.0, 0.0], [10.0, 0.0], [0.0, 10.0]]];
        cut_triangles(&mut tris, &[3.0, 7.0], &[4.0]);
        assert!(tris.len() > 1);
        let straddles = |t: &[[f64; 2]; 3], c: f64, ax: usize| {
            let lo = t.iter().filter(|p| p[ax] < c - 1e-6).count();
            let hi = t.iter().filter(|p| p[ax] > c + 1e-6).count();
            lo > 0 && hi > 0
        };
        for t in &tris {
            for &c in &[3.0_f64, 7.0] {
                assert!(!straddles(t, c, 0), "straddles x={c}");
            }
            assert!(!straddles(t, 4.0, 1), "straddles y=4");
        }
    }
}

#[cfg(test)]
mod refine_diag {
    use super::*;
    use photonic_core::PathData;
    #[test]
    fn refine_circle_reaches_target_within_budget() {
        // A 400px circle (lyon fan, edges up to 400px) must refine so every
        // edge meets the target — no huge leftover triangles (the wedge bug).
        let p = PathData::ellipse(200.0, 200.0, 200.0, 200.0);
        let mesh = tessellate_fill(&p, false, 0.1);
        let target = 400.0 / 48.0; // ~8.3px
        let refined = refine_mesh(&mesh, target);
        let tris = refined.indices.len() / 3;
        let mut maxe = 0f32;
        for t in refined.indices.chunks_exact(3) {
            let q = [
                refined.vertices[t[0] as usize],
                refined.vertices[t[1] as usize],
                refined.vertices[t[2] as usize],
            ];
            for (a, b) in [(0, 1), (1, 2), (2, 0)] {
                let dx = q[a][0] - q[b][0];
                let dy = q[a][1] - q[b][1];
                maxe = maxe.max((dx * dx + dy * dy).sqrt());
            }
        }
        assert!(
            maxe <= target * 1.05,
            "max edge {maxe} exceeds target {target}"
        );
        assert!(tris < 60_000, "triangle count {tris} hit the budget");
    }
}

#[cfg(test)]
mod adaptive_tolerance_tests {
    use super::*;

    #[test]
    fn adaptive_tolerance_tracks_zoom_and_bounds_extremes() {
        let identity = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        assert_eq!(adaptive_tolerance(1.0, &identity), 0.25);
        assert!(adaptive_tolerance(32.0, &identity) < adaptive_tolerance(1.0, &identity));
        assert_eq!(
            adaptive_tolerance(1.0e9, &identity),
            adaptive_tolerance(MAX_EFFECTIVE_SCALE, &identity)
        );
        assert_eq!(
            adaptive_tolerance(0.0, &identity),
            adaptive_tolerance(MIN_EFFECTIVE_SCALE, &identity)
        );
    }

    #[test]
    fn adaptive_tolerance_is_conservative_for_anisotropic_and_shear() {
        let identity = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let anisotropic = [32.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let shear = [1.0, 32.0, 0.0, 1.0, 0.0, 0.0];

        // The transform's largest singular value is at least 32 in both
        // cases. That is below the practical scale cap, so tolerance must be
        // no larger than target / 32.
        let max_safe_tolerance = TARGET_FLATTENING_ERROR_PX as f32 / 32.0;
        assert!(adaptive_tolerance(1.0, &anisotropic) <= max_safe_tolerance);
        assert!(adaptive_tolerance(1.0, &shear) <= max_safe_tolerance);
        assert!(adaptive_tolerance(1.0, &identity) > max_safe_tolerance);
    }

    #[test]
    fn zoomed_out_paths_use_fewer_triangles() {
        let path = PathData::ellipse(0.0, 0.0, 100.0, 100.0);
        let identity = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let zoomed_out = tessellate_fill(&path, false, adaptive_tolerance(0.01, &identity));
        let zoomed_in = tessellate_fill(&path, false, adaptive_tolerance(64.0, &identity));

        assert!(
            zoomed_out.indices.len() < zoomed_in.indices.len(),
            "zoomed-out mesh should have fewer triangles: {} >= {}",
            zoomed_out.indices.len(),
            zoomed_in.indices.len()
        );
    }
}
