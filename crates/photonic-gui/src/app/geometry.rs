//! Path geometry helpers extracted from app::mod (bezier math, anchor/handle
//! resolution, corner rounding, and path distortions). Pure functions — no UI.
#![allow(clippy::too_many_arguments)]
use super::*;
use photonic_render::TextLayoutOptions;

pub(crate) fn bez_to_screen_points(bez: &BezPath, view: &CanvasView) -> Vec<egui::Pos2> {
    let mut pts: Vec<egui::Pos2> = Vec::new();
    let mut cur = (0.0f64, 0.0f64);
    for el in bez.elements() {
        match el {
            PathEl::MoveTo(p) => {
                cur = (p.x, p.y);
                let (sx, sy) = view.canvas_to_screen(p.x, p.y);
                pts.push(egui::pos2(sx as f32, sy as f32));
            }
            PathEl::LineTo(p) => {
                cur = (p.x, p.y);
                let (sx, sy) = view.canvas_to_screen(p.x, p.y);
                pts.push(egui::pos2(sx as f32, sy as f32));
            }
            PathEl::CurveTo(c1, c2, p) => {
                let (x0, y0) = cur;
                for i in 1..=16u32 {
                    let t = i as f64 / 16.0;
                    let u = 1.0 - t;
                    let x = u * u * u * x0
                        + 3.0 * u * u * t * c1.x
                        + 3.0 * u * t * t * c2.x
                        + t * t * t * p.x;
                    let y = u * u * u * y0
                        + 3.0 * u * u * t * c1.y
                        + 3.0 * u * t * t * c2.y
                        + t * t * t * p.y;
                    let (sx, sy) = view.canvas_to_screen(x, y);
                    pts.push(egui::pos2(sx as f32, sy as f32));
                }
                cur = (p.x, p.y);
            }
            PathEl::QuadTo(c, p) => {
                let (x0, y0) = cur;
                for i in 1..=8u32 {
                    let t = i as f64 / 8.0;
                    let u = 1.0 - t;
                    let x = u * u * x0 + 2.0 * u * t * c.x + t * t * p.x;
                    let y = u * u * y0 + 2.0 * u * t * c.y + t * t * p.y;
                    let (sx, sy) = view.canvas_to_screen(x, y);
                    pts.push(egui::pos2(sx as f32, sy as f32));
                }
                cur = (p.x, p.y);
            }
            PathEl::ClosePath => {}
        }
    }
    pts
}

pub(crate) fn make_node(
    path: PathData,
    fill_color: [f32; 4],
    stroke: Option<([f32; 4], f32)>,
    label: &str,
    num: usize,
) -> SceneNode {
    let [r, g, b, a] = fill_color;
    let fill = Fill::solid(Color { r, g, b, a });
    let mut path_node = PathNode::new(path).with_fill(fill);
    if let Some(([sr, sg, sb, sa], width)) = stroke {
        path_node = path_node.with_stroke(Stroke::solid(
            Color {
                r: sr,
                g: sg,
                b: sb,
                a: sa,
            },
            width as f64,
        ));
    }
    let kind = SceneNodeKind::Path(path_node);
    SceneNode::new(format!("{} {}", label, num), Default::default(), kind)
}

/// Like `canvas_bounds` but uses glyphon layout for accurate TextNode dimensions.
pub(crate) fn text_aware_canvas_bounds(
    node: &SceneNode,
    renderer: &mut PhotonicRenderer,
) -> Option<(f64, f64, f64, f64)> {
    let local = match &node.kind {
        SceneNodeKind::Text(t) => {
            // Mirror the renderer's advanced character metrics so the selection
            // rectangle / hit-zone tracks the drawn glyphs. Super/subscript shrinks
            // the node (size_scale) and offsets its baseline; an explicit baseline
            // shift raises (positive) or lowers it. The local Y offset matches the
            // renderer's `top_offset` sign convention (Y grows downward → a raise
            // is negative) with zoom factored out. For Normal nodes with no shift
            // this is size_scale()=1.0 and offset=0, leaving bounds byte-identical.
            let effective_font_size = t.font_size * t.script_position.size_scale();
            let (w, h) = renderer.measure_text_with_layout(
                &t.content,
                &t.font_family,
                effective_font_size,
                TextLayoutOptions {
                    font_weight: t.font_weight,
                    font_style: t.font_style,
                    line_height_mul: t.line_height as f32,
                    letter_spacing: t.letter_spacing as f32,
                    vertical: t.vertical,
                },
            );
            let offset_y =
                -(t.script_position.baseline_offset_em() * t.font_size) - t.baseline_shift;
            kurbo::Rect::new(0.0, offset_y, w, offset_y + h)
        }
        _ => node.local_bounds()?,
    };
    let corners = [
        node.transform.apply(local.x0, local.y0),
        node.transform.apply(local.x1, local.y0),
        node.transform.apply(local.x0, local.y1),
        node.transform.apply(local.x1, local.y1),
    ];
    let min_x = corners.iter().map(|&(x, _)| x).fold(f64::MAX, f64::min);
    let min_y = corners.iter().map(|&(_, y)| y).fold(f64::MAX, f64::min);
    let max_x = corners.iter().map(|&(x, _)| x).fold(f64::MIN, f64::max);
    let max_y = corners.iter().map(|&(_, y)| y).fold(f64::MIN, f64::max);
    Some((min_x, min_y, max_x, max_y))
}

/// Returns the axis-aligned bounding box that covers all nodes in `ids`,
/// or `None` if none of them have computable bounds.
pub(crate) fn selection_canvas_bounds(
    doc: &Document,
    ids: &[NodeId],
    renderer: &mut PhotonicRenderer,
) -> Option<(f64, f64, f64, f64)> {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for &id in ids {
        if let Some(node) = doc.nodes.get(&id) {
            if let Some((x0, y0, x1, y1)) = text_aware_canvas_bounds(node, renderer) {
                min_x = min_x.min(x0);
                min_y = min_y.min(y0);
                max_x = max_x.max(x1);
                max_y = max_y.max(y1);
            }
        }
    }
    if min_x.is_finite() {
        Some((min_x, min_y, max_x, max_y))
    } else {
        None
    }
}

// ─── Direct-select helpers ────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SampledSubpath {
    pub(crate) points: Vec<Point>,
    pub(crate) closed: bool,
}

/// Sample each contour in `bez` independently.
///
/// Keeping the contours separate is important for editor overlays: flattening
/// every `MoveTo` into one point list invents a line from the previous contour
/// to the next one (and closing that list invents another line back to the
/// first contour).
pub(crate) fn sample_path_subpaths(bez: &BezPath) -> Vec<SampledSubpath> {
    let mut subpaths = Vec::new();
    let mut points: Vec<Point> = Vec::new();
    let mut cur_local: Option<Point> = None;
    let mut closed = false;

    let flush = |subpaths: &mut Vec<SampledSubpath>, points: &mut Vec<Point>, closed: &mut bool| {
        if !points.is_empty() {
            subpaths.push(SampledSubpath {
                points: std::mem::take(points),
                closed: *closed,
            });
        }
        *closed = false;
    };

    for el in bez.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                flush(&mut subpaths, &mut points, &mut closed);
                points.push(p);
                cur_local = Some(p);
            }
            PathEl::LineTo(p) => {
                if cur_local.is_some() {
                    points.push(p);
                    cur_local = Some(p);
                }
            }
            PathEl::CurveTo(c1, c2, p) => {
                if let Some(cur) = cur_local {
                    for i in 1..=16u32 {
                        let t = i as f64 / 16.0;
                        let u = 1.0 - t;
                        points.push(Point::new(
                            u * u * u * cur.x
                                + 3.0 * u * u * t * c1.x
                                + 3.0 * u * t * t * c2.x
                                + t * t * t * p.x,
                            u * u * u * cur.y
                                + 3.0 * u * u * t * c1.y
                                + 3.0 * u * t * t * c2.y
                                + t * t * t * p.y,
                        ));
                    }
                    cur_local = Some(p);
                }
            }
            PathEl::QuadTo(c, p) => {
                if let Some(cur) = cur_local {
                    for i in 1..=8u32 {
                        let t = i as f64 / 8.0;
                        let u = 1.0 - t;
                        points.push(Point::new(
                            u * u * cur.x + 2.0 * u * t * c.x + t * t * p.x,
                            u * u * cur.y + 2.0 * u * t * c.y + t * t * p.y,
                        ));
                    }
                    cur_local = Some(p);
                }
            }
            PathEl::ClosePath => {
                closed = true;
                cur_local = points.first().copied();
            }
        }
    }
    flush(&mut subpaths, &mut points, &mut closed);
    subpaths
}

/// Sample each contour independently, then apply the node transform and view.
pub(crate) fn bez_to_screen_subpaths_xf(
    bez: &BezPath,
    view: &CanvasView,
    transform: &photonic_core::transform::Transform,
) -> Vec<(Vec<egui::Pos2>, bool)> {
    sample_path_subpaths(bez)
        .into_iter()
        .map(|subpath| {
            let points = subpath
                .points
                .into_iter()
                .map(|p| {
                    let (cx, cy) = transform.apply(p.x, p.y);
                    let (sx, sy) = view.canvas_to_screen(cx, cy);
                    egui::pos2(sx as f32, sy as f32)
                })
                .collect();
            (points, subpath.closed)
        })
        .collect()
}

/// Extract `(element_index, local_point)` for every element that has an endpoint.
/// `ClosePath` is excluded (no anchor).
pub(crate) fn path_anchor_points(bez: &BezPath) -> Vec<(usize, Point)> {
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

/// Screen-space positions of a path's on-curve anchor points (endpoints only,
/// no sampled points along curves). Applies `transform` (local → canvas) then
/// `view` (canvas → screen).
pub(crate) fn anchor_screen_points_xf(
    bez: &BezPath,
    view: &CanvasView,
    transform: &photonic_core::transform::Transform,
) -> Vec<egui::Pos2> {
    path_anchor_points(bez)
        .into_iter()
        .map(|(_, p)| {
            let (cx, cy) = transform.apply(p.x, p.y);
            let (sx, sy) = view.canvas_to_screen(cx, cy);
            egui::pos2(sx as f32, sy as f32)
        })
        .collect()
}

/// Find the element index of the anchor point nearest to `(cursor_cx, cursor_cy)`
/// in canvas space, within `threshold_px` pixels on screen.
pub(crate) fn nearest_anchor_screen(
    bez: &BezPath,
    transform: &photonic_core::transform::Transform,
    view: &CanvasView,
    cursor_cx: f64,
    cursor_cy: f64,
    threshold_px: f64,
) -> Option<usize> {
    let (cursor_sx, cursor_sy) = view.canvas_to_screen(cursor_cx, cursor_cy);
    let mut best: Option<(usize, f64)> = None;
    for (idx, local_pt) in path_anchor_points(bez) {
        let (cx, cy) = transform.apply(local_pt.x, local_pt.y);
        let (sx, sy) = view.canvas_to_screen(cx, cy);
        let dist = ((sx - cursor_sx).powi(2) + (sy - cursor_sy).powi(2)).sqrt();
        if dist < threshold_px {
            if best.map_or(true, |(_, d)| dist < d) {
                best = Some((idx, dist));
            }
        }
    }
    best.map(|(idx, _)| idx)
}

/// Invert a node's affine transform to map a canvas-space point into the node's
/// local path space.
pub(crate) fn canvas_to_local(
    transform: &photonic_core::transform::Transform,
    cx: f64,
    cy: f64,
) -> (f64, f64) {
    let [a, b, c, d, e, f] = transform.matrix;
    let det = a * d - b * c;
    if det.abs() < 1e-12 {
        return (cx, cy);
    }
    let x = cx - e;
    let y = cy - f;
    ((d * x - c * y) / det, (-b * x + a * y) / det)
}

/// Screen position of a local path point through a node's transform.
pub(crate) fn local_to_screen(
    transform: &photonic_core::transform::Transform,
    view: &CanvasView,
    p: Point,
) -> (f64, f64) {
    let (cx, cy) = transform.apply(p.x, p.y);
    view.canvas_to_screen(cx, cy)
}

/// Find the bezier control handle (of a selected anchor) nearest the cursor,
/// within `threshold_px` on screen. Handles take priority over anchors so a
/// curve can be reshaped without moving its anchor.
pub(crate) fn ds_find_handle(
    node: &SceneNode,
    view: &CanvasView,
    selected: &[usize],
    cursor_cx: f64,
    cursor_cy: f64,
    threshold_px: f64,
) -> Option<(usize, HandleKind)> {
    let SceneNodeKind::Path(pn) = &node.kind else {
        return None;
    };
    let bez = pn.path_data.to_bez_path();
    let (csx, csy) = view.canvas_to_screen(cursor_cx, cursor_cy);
    let mut best: Option<(usize, HandleKind, f64)> = None;
    for &i in selected {
        let (in_h, out_h) = anchor_handle_pair(&bez, i);
        for h in [in_h, out_h].into_iter().flatten() {
            let (kind, hp) = h;
            let (hsx, hsy) = local_to_screen(&node.transform, view, hp);
            let d = ((hsx - csx).powi(2) + (hsy - csy).powi(2)).sqrt();
            if d < threshold_px && best.map_or(true, |(_, _, bd)| d < bd) {
                best = Some((i, kind, d));
            }
        }
    }
    best.map(|(i, k, _)| (i, k))
}

/// Screen position of the Live-Corners rounding widget for a corner, offset
/// along the interior angle bisector by `inset_px`.
pub(crate) fn ds_corner_widget_screen(
    transform: &photonic_core::transform::Transform,
    view: &CanvasView,
    prev: Point,
    curr: Point,
    next: Point,
    inset_px: f64,
) -> (f64, f64) {
    let (px, py) = local_to_screen(transform, view, prev);
    let (cx, cy) = local_to_screen(transform, view, curr);
    let (nx, ny) = local_to_screen(transform, view, next);
    let (mut bx, mut by) = (px - cx, py - cy);
    let l1 = (bx * bx + by * by).sqrt();
    let (mut ox, mut oy) = (nx - cx, ny - cy);
    let l2 = (ox * ox + oy * oy).sqrt();
    if l1 > 1e-6 {
        bx /= l1;
        by /= l1;
    }
    if l2 > 1e-6 {
        ox /= l2;
        oy /= l2;
    }
    let (mut dx, mut dy) = (bx + ox, by + oy);
    let dl = (dx * dx + dy * dy).sqrt();
    if dl > 1e-6 {
        dx /= dl;
        dy /= dl;
    } else {
        dx = 0.0;
        dy = 0.0;
    }
    (cx + dx * inset_px, cy + dy * inset_px)
}

/// Find the Live-Corners widget (of a selected straight corner) under the
/// cursor, within `threshold_px` on screen.
pub(crate) fn ds_find_corner_widget(
    node: &SceneNode,
    view: &CanvasView,
    selected: &[usize],
    corners: &std::collections::HashMap<usize, (Point, Point, Point)>,
    cursor_cx: f64,
    cursor_cy: f64,
    inset_px: f64,
    threshold_px: f64,
) -> Option<usize> {
    let (csx, csy) = view.canvas_to_screen(cursor_cx, cursor_cy);
    let mut best: Option<(usize, f64)> = None;
    for &i in selected {
        if let Some((prev, curr, next)) = corners.get(&i) {
            let (wsx, wsy) =
                ds_corner_widget_screen(&node.transform, view, *prev, *curr, *next, inset_px);
            let d = ((wsx - csx).powi(2) + (wsy - csy).powi(2)).sqrt();
            if d < threshold_px && best.map_or(true, |(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
    }
    best.map(|(i, _)| i)
}

/// Move the selected anchor points in a `BezPath` by `(dx, dy)` in local space.
///
/// Implemented as a single membership pass over the elements so that each point
/// is written exactly once — this makes rigidly translating a *set* of adjacent
/// (or all) anchors correct, which the old two-write approach corrupted via
/// overwrites and its `!sel_set.contains(&next)` guard.
///
/// For each element `j`, a point moves iff the anchor it belongs to is selected:
/// - endpoint `p` and incoming handle `c2` belong to anchor `j` — move iff `j`
///   is selected;
/// - outgoing handle `c1` belongs to the *previous* anchor `j-1` (the segment
///   leaves that anchor) — move iff `j-1` is selected (and `j-1` is a real
///   anchor, i.e. not a `ClosePath`);
/// - a `QuadTo`'s single control is shared by both endpoints, so it moves iff
///   *either* `j` or `j-1` is selected.
///
/// Single-anchor behaviour is identical to before (the outgoing handle of the
/// selected anchor lives on the next element, which sees `j-1` selected).
pub(crate) fn bez_move_anchors(bez: &BezPath, selected: &[usize], dx: f64, dy: f64) -> BezPath {
    let els: Vec<PathEl> = bez.elements().iter().copied().collect();
    let sel_set: std::collections::HashSet<usize> = selected.iter().copied().collect();
    let shift = |p: Point| Point::new(p.x + dx, p.y + dy);

    let mut result = BezPath::new();
    for (j, el) in els.iter().enumerate() {
        // This element's own anchor (owns endpoint + incoming handle).
        let anchor_sel = sel_set.contains(&j);
        // The previous anchor (owns this element's outgoing handle `c1`), unless
        // the previous element is a `ClosePath` (no anchor there).
        let prev_sel =
            j > 0 && !matches!(els[j - 1], PathEl::ClosePath) && sel_set.contains(&(j - 1));
        let new_el = match *el {
            PathEl::MoveTo(p) => PathEl::MoveTo(if anchor_sel { shift(p) } else { p }),
            PathEl::LineTo(p) => PathEl::LineTo(if anchor_sel { shift(p) } else { p }),
            PathEl::CurveTo(c1, c2, p) => PathEl::CurveTo(
                if prev_sel { shift(c1) } else { c1 },
                if anchor_sel { shift(c2) } else { c2 },
                if anchor_sel { shift(p) } else { p },
            ),
            PathEl::QuadTo(c, p) => PathEl::QuadTo(
                if anchor_sel || prev_sel { shift(c) } else { c },
                if anchor_sel { shift(p) } else { p },
            ),
            PathEl::ClosePath => PathEl::ClosePath,
        };
        result.push(new_el);
    }
    result
}

/// The local-space position of the bezier control handle on `kind` side of the
/// anchor at element index `i`, or `None` if that side is not curved.
///
/// - `In`  → the `c2` control of the `CurveTo` element *at* `i` (the curve
///   arriving at this anchor).
/// - `Out` → the `c1` control of the `CurveTo` element *after* `i` (the curve
///   leaving this anchor).
// Retained as a small seam-unaware handle accessor exercised by the anchor
// conversion tests; the conversion path itself now decomposes handles directly.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn anchor_handle_point(els: &[PathEl], i: usize, kind: HandleKind) -> Option<Point> {
    match kind {
        HandleKind::In => match els.get(i) {
            Some(PathEl::CurveTo(_, c2, _)) => Some(*c2),
            _ => None,
        },
        HandleKind::Out => match els.get(i + 1) {
            Some(PathEl::CurveTo(c1, _, _)) => Some(*c1),
            _ => None,
        },
    }
}

/// A control-handle location: element index, plus whether it is the `c1`
/// (outgoing) control of that `CurveTo` (`true`) or the `c2` (incoming, `false`).
pub(crate) type HandleLoc = (usize, bool);

pub(crate) fn handle_loc_point(els: &[PathEl], loc: HandleLoc) -> Option<Point> {
    match els.get(loc.0) {
        Some(PathEl::CurveTo(c1, c2, _)) => Some(if loc.1 { *c1 } else { *c2 }),
        _ => None,
    }
}

pub(crate) fn set_handle_loc(els: &mut [PathEl], loc: HandleLoc, pt: Point) {
    if let Some(PathEl::CurveTo(c1, c2, p)) = els.get(loc.0).copied() {
        els[loc.0] = if loc.1 {
            PathEl::CurveTo(pt, c2, p)
        } else {
            PathEl::CurveTo(c1, pt, p)
        };
    }
}

/// Resolve the In/Out handle locations of the *logical* anchor at element index
/// `i`, following the closed-path seam: a closed shape lists its start point
/// twice (the `MoveTo` and the closing `CurveTo` endpoint), so that one logical
/// anchor's two handles live on different elements. This maps both to a single
/// anchor so smooth-mirroring, hit-testing and rendering treat the seam as one.
pub(crate) fn logical_handles(bez: &BezPath, i: usize) -> (Option<HandleLoc>, Option<HandleLoc>) {
    let els = bez.elements();
    let n = els.len();
    if i >= n {
        return (None, None);
    }
    // Subpath start: nearest MoveTo at or before i.
    let mut start = 0usize;
    for j in (0..=i).rev() {
        if matches!(els[j], PathEl::MoveTo(_)) {
            start = j;
            break;
        }
    }
    // Subpath end: element before the next MoveTo, else the last element.
    let mut end = n - 1;
    for j in (start + 1)..n {
        if matches!(els[j], PathEl::MoveTo(_)) {
            end = j - 1;
            break;
        }
    }
    let closed = matches!(els[end], PathEl::ClosePath);
    let last_geom = if closed { end.saturating_sub(1) } else { end };

    // In = c2 of the curve ending at this anchor (or the closing curve at seam).
    let in_loc = if matches!(els.get(i), Some(PathEl::CurveTo(..))) {
        Some((i, false))
    } else if closed && i == start && matches!(els.get(last_geom), Some(PathEl::CurveTo(..))) {
        Some((last_geom, false))
    } else {
        None
    };
    // Out = c1 of the curve leaving this anchor (or the first curve at seam).
    let out_loc = if i + 1 < n && matches!(els.get(i + 1), Some(PathEl::CurveTo(..))) {
        Some((i + 1, true))
    } else if closed && i == last_geom && matches!(els.get(start + 1), Some(PathEl::CurveTo(..))) {
        Some((start + 1, true))
    } else {
        None
    };
    (in_loc, out_loc)
}

/// The In/Out handle points of the logical anchor at `i`, seam-aware.
pub(crate) fn anchor_handle_pair(
    bez: &BezPath,
    i: usize,
) -> (Option<(HandleKind, Point)>, Option<(HandleKind, Point)>) {
    let els = bez.elements();
    let (in_l, out_l) = logical_handles(bez, i);
    (
        in_l.and_then(|l| handle_loc_point(els, l))
            .map(|p| (HandleKind::In, p)),
        out_l
            .and_then(|l| handle_loc_point(els, l))
            .map(|p| (HandleKind::Out, p)),
    )
}

/// True when the anchor has both handles and they are roughly collinear
/// (opposite directions through the anchor) — a smooth point, not a cusp.
/// Used to decide whether dragging one handle should mirror the other.
pub(crate) fn is_smooth_anchor(bez: &BezPath, i: usize) -> bool {
    let anchor = match path_anchor_points(bez)
        .into_iter()
        .find(|(idx, _)| *idx == i)
    {
        Some((_, p)) => p,
        None => return false,
    };
    let (in_h, out_h) = anchor_handle_pair(bez, i);
    if let (Some((_, ip)), Some((_, op))) = (in_h, out_h) {
        let v1 = (ip.x - anchor.x, ip.y - anchor.y);
        let v2 = (op.x - anchor.x, op.y - anchor.y);
        let l1 = (v1.0 * v1.0 + v1.1 * v1.1).sqrt();
        let l2 = (v2.0 * v2.0 + v2.1 * v2.1).sqrt();
        if l1 > 1e-6 && l2 > 1e-6 {
            let dot = (v1.0 * v2.0 + v1.1 * v2.1) / (l1 * l2);
            return dot < -0.985; // within ~10° of straight → smooth
        }
    }
    false
}

/// Move the bezier control handle on `kind` side of anchor `i` to local point
/// `target`. When `mirror` is true (a smooth anchor dragged without Alt), the
/// opposite handle is kept collinear through the anchor, preserving its own
/// length — Illustrator's smooth-point behaviour.
pub(crate) fn bez_set_handle(
    bez: &BezPath,
    i: usize,
    kind: HandleKind,
    target: Point,
    mirror: bool,
) -> BezPath {
    let mut els: Vec<PathEl> = bez.elements().to_vec();
    let anchor = match path_anchor_points(bez)
        .into_iter()
        .find(|(idx, _)| *idx == i)
    {
        Some((_, p)) => p,
        None => return bez.clone(),
    };

    // Resolve the dragged and opposite handle locations (seam-aware).
    let (in_l, out_l) = logical_handles(bez, i);
    let (dragged, opposite) = match kind {
        HandleKind::In => (in_l, out_l),
        HandleKind::Out => (out_l, in_l),
    };

    if let Some(loc) = dragged {
        set_handle_loc(&mut els, loc, target);
    }

    // Mirror the opposite handle through the anchor (smooth point).
    if mirror {
        if let (Some(_), Some(opp)) = (dragged, opposite) {
            if let Some(opp_pt) = handle_loc_point(&els, opp) {
                let dx = target.x - anchor.x;
                let dy = target.y - anchor.y;
                let len = (dx * dx + dy * dy).sqrt();
                let olen = ((opp_pt.x - anchor.x).powi(2) + (opp_pt.y - anchor.y).powi(2)).sqrt();
                if len > 1e-9 && olen > 1e-9 {
                    let new_opp =
                        Point::new(anchor.x + (-dx / len) * olen, anchor.y + (-dy / len) * olen);
                    set_handle_loc(&mut els, opp, new_opp);
                }
            }
        }
    }

    let mut result = BezPath::new();
    for el in els {
        result.push(el);
    }
    result
}

/// Move the single anchor at element index `i` so its endpoint sits at local
/// `(x, y)`, dragging its attached handles along with it.
pub(crate) fn bez_set_anchor_position(bez: &BezPath, i: usize, x: f64, y: f64) -> BezPath {
    match path_anchor_points(bez)
        .into_iter()
        .find(|(idx, _)| *idx == i)
    {
        Some((_, p)) => bez_move_anchors(bez, &[i], x - p.x, y - p.y),
        None => bez.clone(),
    }
}

/// Subpath split used by the straight-corner helpers.
pub(crate) struct CornerSub {
    /// (element index, endpoint) for every anchor in draw order.
    verts: Vec<(usize, Point)>,
    /// `straight[k]` is true when the segment *arriving* at `verts[k]` is a
    /// straight line (`LineTo`). `straight[0]` is unused (MoveTo).
    straight: Vec<bool>,
    closed: bool,
}

/// Split a path into subpaths, tagging each segment as straight or curved.
pub(crate) fn corner_subpaths(bez: &BezPath) -> Vec<CornerSub> {
    let mut subs: Vec<CornerSub> = Vec::new();
    let mut cur: Option<CornerSub> = None;
    for (i, el) in bez.elements().iter().enumerate() {
        match el {
            PathEl::MoveTo(p) => {
                if let Some(s) = cur.take() {
                    subs.push(s);
                }
                cur = Some(CornerSub {
                    verts: vec![(i, *p)],
                    straight: vec![false],
                    closed: false,
                });
            }
            PathEl::LineTo(p) => {
                if let Some(s) = cur.as_mut() {
                    s.verts.push((i, *p));
                    s.straight.push(true);
                }
            }
            PathEl::CurveTo(_, _, p) | PathEl::QuadTo(_, p) => {
                if let Some(s) = cur.as_mut() {
                    s.verts.push((i, *p));
                    s.straight.push(false);
                }
            }
            PathEl::ClosePath => {
                if let Some(s) = cur.as_mut() {
                    s.closed = true;
                }
            }
        }
    }
    if let Some(s) = cur.take() {
        subs.push(s);
    }
    subs
}

/// Map each anchor that is a *roundable straight corner* — both adjacent
/// segments are straight lines and the turn is significant — to its
/// `(prev, corner, next)` local-space points. These are the only anchors that
/// get a Live-Corners widget.
pub(crate) fn straight_corners(
    bez: &BezPath,
) -> std::collections::HashMap<usize, (Point, Point, Point)> {
    let mut out = std::collections::HashMap::new();
    for s in corner_subpaths(bez) {
        let n = s.verts.len();
        if n < 3 {
            continue;
        }
        for k in 0..n {
            let (idx, curr) = s.verts[k];
            // The closing segment (verts[n-1] -> verts[0]) is a straight line
            // when the subpath is closed.
            let in_straight = if k > 0 { s.straight[k] } else { s.closed };
            let out_straight = if k + 1 < n {
                s.straight[k + 1]
            } else {
                s.closed
            };
            if !(in_straight && out_straight) {
                continue;
            }
            let prev = if k > 0 {
                s.verts[k - 1].1
            } else if s.closed {
                s.verts[n - 1].1
            } else {
                continue;
            };
            let next = if k + 1 < n {
                s.verts[k + 1].1
            } else if s.closed {
                s.verts[0].1
            } else {
                continue;
            };
            let v1 = (curr.x - prev.x, curr.y - prev.y);
            let v2 = (next.x - curr.x, next.y - curr.y);
            let l1 = (v1.0 * v1.0 + v1.1 * v1.1).sqrt();
            let l2 = (v2.0 * v2.0 + v2.1 * v2.1).sqrt();
            if l1 < 1e-6 || l2 < 1e-6 {
                continue;
            }
            let cosang = (v1.0 * v2.0 + v1.1 * v2.1) / (l1 * l2);
            if cosang > 0.999 {
                continue; // nearly collinear — nothing to round
            }
            out.insert(idx, (prev, curr, next));
        }
    }
    out
}

/// Round the selected *straight* corners of `bez` by `radius`, replacing each
/// with a quadratic arc fillet. Non-selected vertices — including every curve
/// segment — are preserved verbatim. Mirrors `gui_round_corners` but applies
/// only to the chosen anchors.
pub(crate) fn round_selected_corners(
    bez: &BezPath,
    selected: &std::collections::HashSet<usize>,
    radius: f64,
) -> BezPath {
    if radius <= 0.0 {
        return bez.clone();
    }
    let subs = corner_subpaths(bez);
    let mut result = BezPath::new();

    for s in &subs {
        let n = s.verts.len();
        if n == 0 {
            continue;
        }
        // Original element for each vertex (for verbatim re-emit of curves).
        let orig: Vec<PathEl> = s.verts.iter().map(|(i, _)| bez.elements()[*i]).collect();

        let neighbours = |k: usize| -> Option<(Point, Point)> {
            let prev = if k > 0 {
                Some(s.verts[k - 1].1)
            } else if s.closed {
                Some(s.verts[n - 1].1)
            } else {
                None
            };
            let next = if k + 1 < n {
                Some(s.verts[k + 1].1)
            } else if s.closed {
                Some(s.verts[0].1)
            } else {
                None
            };
            match (prev, next) {
                (Some(p), Some(q)) => Some((p, q)),
                _ => None,
            }
        };

        // A position is a *roundable straight corner* when both adjacent
        // segments are straight lines (`straight_corners` uses the same rule).
        // `straight[0]` is unused, so the arriving side of vertex 0 is the
        // closing segment, which is straight iff the subpath is closed.
        let is_straight_corner = |k: usize| -> bool {
            let in_straight = if k > 0 { s.straight[k] } else { s.closed };
            let out_straight = if k + 1 < n {
                s.straight[k + 1]
            } else {
                s.closed
            };
            in_straight && out_straight
        };

        // Per-position predicate: this corner will actually be filleted. Used
        // both to emit the fillet and to make each side's retreat clamp aware of
        // whether the neighbour sharing that edge is being rounded too.
        let rounded: Vec<bool> = (0..n)
            .map(|k| {
                let (idx, _) = s.verts[k];
                selected.contains(&idx) && is_straight_corner(k) && neighbours(k).is_some()
            })
            .collect();

        // Fillet endpoints for corner `k`: retreat `r` along each adjacent edge.
        // The retreat on each side is bounded by whether that side's neighbour
        // is itself being rounded — if so the shared edge is split 50/50 so the
        // two fillets never overlap (deterministic for multi-select); otherwise
        // the retreat may reach up to the full edge (minus a tiny epsilon so a
        // degenerate zero-length segment is never emitted at the adjacent
        // vertex). This removes the artificial per-corner half-edge cap.
        let do_round = |k: usize| -> Option<(Point, Point)> {
            if !rounded[k] {
                return None;
            }
            let (_, curr) = s.verts[k];
            let (prev, next) = neighbours(k)?;
            let din = (curr.x - prev.x, curr.y - prev.y);
            let dout = (next.x - curr.x, next.y - curr.y);
            let lin = (din.0 * din.0 + din.1 * din.1).sqrt();
            let lout = (dout.0 * dout.0 + dout.1 * dout.1).sqrt();
            if lin < 1e-9 || lout < 1e-9 {
                return None;
            }
            let kprev = if k > 0 { k - 1 } else { n - 1 };
            let knext = if k + 1 < n { k + 1 } else { 0 };
            const EPS: f64 = 1e-3;
            let max_in = if rounded[kprev] {
                lin / 2.0
            } else {
                lin * (1.0 - EPS)
            };
            let max_out = if rounded[knext] {
                lout / 2.0
            } else {
                lout * (1.0 - EPS)
            };
            let r = radius.min(max_in).min(max_out);
            let fs = Point::new(curr.x - din.0 / lin * r, curr.y - din.1 / lin * r);
            let fe = Point::new(curr.x + dout.0 / lout * r, curr.y + dout.1 / lout * r);
            Some((fs, fe))
        };

        let mut started = false;
        for k in 0..n {
            let (_, curr) = s.verts[k];
            if let Some((fs, fe)) = do_round(k) {
                if !started {
                    result.move_to(fs);
                    started = true;
                } else {
                    result.line_to(fs);
                }
                result.quad_to(curr, fe);
            } else if !started {
                result.move_to(curr);
                started = true;
            } else {
                // Re-emit the original arriving segment, preserving curves.
                match orig[k] {
                    PathEl::MoveTo(_) => result.move_to(curr),
                    PathEl::LineTo(p) => result.line_to(p),
                    PathEl::CurveTo(c1, c2, p) => result.curve_to(c1, c2, p),
                    PathEl::QuadTo(c, p) => result.quad_to(c, p),
                    PathEl::ClosePath => {}
                }
            }
        }
        if s.closed && started {
            result.close_path();
        }
    }
    result
}

/// Convert the selected anchors to smooth (collinear handles) or corner
/// (retracted handles) by surgically rewriting their adjacent control points.
/// Unselected anchors and the rest of the path are untouched.
pub(crate) fn bez_convert_anchors(
    bez: &BezPath,
    selected: &std::collections::HashSet<usize>,
    smooth: bool,
) -> BezPath {
    // One decomposed anchor: its endpoint element index (for matching against
    // `selected`), its local-space point, the incoming (`c2`) and outgoing (`c1`)
    // cubic handles as `Option`s, and the original element that *arrives* at it
    // (kept so `QuadTo` and other segments we don't touch re-emit verbatim).
    struct Anchor {
        el_idx: usize,
        p: Point,
        in_h: Option<Point>,
        out_h: Option<Point>,
        arriving: PathEl,
    }
    struct Sub {
        anchors: Vec<Anchor>,
        closed: bool,
    }

    // --- Decompose the path into per-subpath anchor records. ---------------
    // A closed subpath's final seam edge (last geometric anchor → start anchor)
    // is *implicit* in kurbo's `ClosePath` form, so it carries no handles here;
    // synthesizing them materializes the seam as an explicit `CurveTo` on
    // re-emit, with no index bookkeeping.
    let mut subs: Vec<Sub> = Vec::new();
    let mut cur: Option<Sub> = None;
    for (i, el) in bez.elements().iter().enumerate() {
        match *el {
            PathEl::MoveTo(p) => {
                if let Some(s) = cur.take() {
                    subs.push(s);
                }
                cur = Some(Sub {
                    anchors: vec![Anchor {
                        el_idx: i,
                        p,
                        in_h: None,
                        out_h: None,
                        arriving: *el,
                    }],
                    closed: false,
                });
            }
            PathEl::LineTo(p) => {
                if let Some(s) = cur.as_mut() {
                    s.anchors.push(Anchor {
                        el_idx: i,
                        p,
                        in_h: None,
                        out_h: None,
                        arriving: *el,
                    });
                }
            }
            PathEl::CurveTo(c1, c2, p) => {
                if let Some(s) = cur.as_mut() {
                    if let Some(prev) = s.anchors.last_mut() {
                        prev.out_h = Some(c1);
                    }
                    s.anchors.push(Anchor {
                        el_idx: i,
                        p,
                        in_h: Some(c2),
                        out_h: None,
                        arriving: *el,
                    });
                }
            }
            PathEl::QuadTo(_, p) => {
                // Quadratic segments are preserved verbatim; we never smooth
                // across them (the previous implementation ignored them too).
                if let Some(s) = cur.as_mut() {
                    s.anchors.push(Anchor {
                        el_idx: i,
                        p,
                        in_h: None,
                        out_h: None,
                        arriving: *el,
                    });
                }
            }
            PathEl::ClosePath => {
                if let Some(s) = cur.as_mut() {
                    s.closed = true;
                }
            }
        }
    }
    if let Some(s) = cur.take() {
        subs.push(s);
    }

    // Epsilon for treating two seam points as coincident.
    const SEAM_EPS: f64 = 1e-6;
    let coincident = |a: Point, b: Point| -> bool {
        (a.x - b.x).abs() < SEAM_EPS && (a.y - b.y).abs() < SEAM_EPS
    };

    // --- Reunify the explicit-close seam form. -----------------------------
    // A closed subpath may arrive in kurbo's implicit-close form (start point
    // appears once, as the `MoveTo`) or in the *explicit-close* form where the
    // closing edge is a real `CurveTo`/`LineTo` whose endpoint equals the start,
    // so the start point appears as TWO anchor records. This feature's own seam
    // materialization (and round-tripping through SVG) produces the explicit
    // form. Left as-is, the closed-wrap neighbour lookup for the seam anchor
    // (k == 0) would pick this coincident duplicate as `prev_p` (length 0 → a
    // collapsed handle) instead of the true incoming neighbour, and a second
    // smooth would append a degenerate zero-length `CurveTo`. Fold the trailing
    // duplicate back onto the implicit seam — drop it and carry its incoming
    // handle onto `anchors[0]` — exactly as `logical_handles` reunifies the seam
    // for rendering and hit-testing.
    for s in &mut subs {
        if !s.closed {
            continue;
        }
        let n = s.anchors.len();
        if n < 2 {
            continue;
        }
        if coincident(s.anchors[n - 1].p, s.anchors[0].p) {
            let dup = s.anchors.pop().expect("n >= 2");
            // The dropped closing curve's `c2` is the seam's true incoming
            // handle; fold it onto the implicit-seam start anchor.
            s.anchors[0].in_h = dup.in_h;
        }
    }

    let norm = |dx: f64, dy: f64| -> Option<(f64, f64)> {
        let l = (dx * dx + dy * dy).sqrt();
        (l > 1e-9).then(|| (dx / l, dy / l))
    };

    // --- Synthesize handles on the selected anchors. -----------------------
    for s in &mut subs {
        let count = s.anchors.len();
        let closed = s.closed;
        for k in 0..count {
            if !selected.contains(&s.anchors[k].el_idx) {
                continue;
            }
            let p = s.anchors[k].p;

            if !smooth {
                // Corner: retract both handles to the anchor. Re-emit turns a
                // retracted side into `CurveTo(anchor, …)` when the neighbour
                // stays curved, or a straight `LineTo` when both sides are flat
                // — matching the previous surgical behaviour exactly.
                s.anchors[k].in_h = None;
                s.anchors[k].out_h = None;
                continue;
            }

            // Seam-aware neighbours (wrap across the seam only when closed).
            let prev_p = if k > 0 {
                Some(s.anchors[k - 1].p)
            } else if closed {
                Some(s.anchors[count - 1].p)
            } else {
                None
            };
            let next_p = if k + 1 < count {
                Some(s.anchors[k + 1].p)
            } else if closed {
                Some(s.anchors[0].p)
            } else {
                None
            };

            // A side is writable when it has a real (line/curve/seam) edge that
            // we can reconstruct — never a `QuadTo`.
            let in_quad = k > 0 && matches!(s.anchors[k].arriving, PathEl::QuadTo(..));
            let out_quad = k + 1 < count && matches!(s.anchors[k + 1].arriving, PathEl::QuadTo(..));
            let can_in = prev_p.is_some() && (k > 0 || closed) && !in_quad;
            let can_out = next_p.is_some() && (k + 1 < count || closed) && !out_quad;

            match (s.anchors[k].in_h, s.anchors[k].out_h) {
                (Some(ip), Some(op)) => {
                    // Both sides already curved: average the two handle
                    // directions, keeping each handle's own length (unchanged
                    // legacy smooth-point behaviour).
                    let ilen = ((ip.x - p.x).powi(2) + (ip.y - p.y).powi(2)).sqrt();
                    let olen = ((op.x - p.x).powi(2) + (op.y - p.y).powi(2)).sqrt();
                    let tin = (p.x - ip.x, p.y - ip.y);
                    let tout = (op.x - p.x, op.y - p.y);
                    if let Some((tx, ty)) = norm(tin.0 + tout.0, tin.1 + tout.1) {
                        s.anchors[k].in_h = Some(Point::new(p.x - tx * ilen, p.y - ty * ilen));
                        s.anchors[k].out_h = Some(Point::new(p.x + tx * olen, p.y + ty * olen));
                    }
                }
                (Some(ip), None) => {
                    // Reflect the existing incoming handle onto the outgoing
                    // side (collinear), length = 1/3 of the outgoing edge.
                    if can_out {
                        if let (Some(b), Some((tx, ty))) = (next_p, norm(p.x - ip.x, p.y - ip.y)) {
                            let len = ((b.x - p.x).powi(2) + (b.y - p.y).powi(2)).sqrt() / 3.0;
                            s.anchors[k].out_h = Some(Point::new(p.x + tx * len, p.y + ty * len));
                        }
                    }
                }
                (None, Some(op)) => {
                    // Reflect the existing outgoing handle onto the incoming side.
                    if can_in {
                        if let (Some(a), Some((tx, ty))) = (prev_p, norm(p.x - op.x, p.y - op.y)) {
                            let len = ((a.x - p.x).powi(2) + (a.y - p.y).powi(2)).sqrt() / 3.0;
                            s.anchors[k].in_h = Some(Point::new(p.x + tx * len, p.y + ty * len));
                        }
                    }
                }
                (None, None) => {
                    // Straight corner: synthesize a Catmull-Rom tangent from the
                    // two neighbours and pull 1/3-length handles along it.
                    if can_in && can_out {
                        if let (Some(a), Some(b)) = (prev_p, next_p) {
                            if let Some((tx, ty)) = norm(b.x - a.x, b.y - a.y) {
                                let len_in =
                                    ((p.x - a.x).powi(2) + (p.y - a.y).powi(2)).sqrt() / 3.0;
                                let len_out =
                                    ((b.x - p.x).powi(2) + (b.y - p.y).powi(2)).sqrt() / 3.0;
                                s.anchors[k].in_h =
                                    Some(Point::new(p.x - tx * len_in, p.y - ty * len_in));
                                s.anchors[k].out_h =
                                    Some(Point::new(p.x + tx * len_out, p.y + ty * len_out));
                            }
                        }
                    }
                }
            }
        }
    }

    // --- Re-emit. ----------------------------------------------------------
    // An edge with no handles on either end is a straight `LineTo`; a `QuadTo`
    // is copied verbatim; otherwise a `CurveTo` whose missing control retracts
    // to the shared anchor.
    let mut result = BezPath::new();
    for s in &subs {
        let count = s.anchors.len();
        if count == 0 {
            continue;
        }
        result.move_to(s.anchors[0].p);
        let emit_edge = |result: &mut BezPath, prev: &Anchor, cur: &Anchor| {
            if let PathEl::QuadTo(..) = cur.arriving {
                result.push(cur.arriving);
                return;
            }
            match (prev.out_h, cur.in_h) {
                (None, None) => result.line_to(cur.p),
                (c1, c2) => result.curve_to(c1.unwrap_or(prev.p), c2.unwrap_or(cur.p), cur.p),
            }
        };
        for k in 1..count {
            let (a, b) = (&s.anchors[k - 1], &s.anchors[k]);
            emit_edge(&mut result, a, b);
        }
        if s.closed {
            let last = &s.anchors[count - 1];
            let first = &s.anchors[0];
            match (last.out_h, first.in_h) {
                (None, None) => {}
                (c1, c2) => {
                    let sc1 = c1.unwrap_or(last.p);
                    let sc2 = c2.unwrap_or(first.p);
                    // Skip a degenerate zero-length closing curve — endpoints and
                    // both controls all coincident. `ClosePath` already draws the
                    // straight seam back to the start, so emitting this cubic would
                    // only inject an invalid zero-length segment.
                    let degenerate = coincident(last.p, first.p)
                        && coincident(sc1, last.p)
                        && coincident(sc2, first.p);
                    if !degenerate {
                        result.curve_to(sc1, sc2, first.p);
                    }
                }
            }
            result.close_path();
        }
    }
    result
}

/// Remove the elements at `indices` from a `BezPath`, rebuilding a valid path.
/// Apply zig-zag distortion to a BezPath (GUI version, mirrors MCP logic).
pub(crate) fn gui_zig_zag(bez: &BezPath, size: f64, ridges: usize, smooth: bool) -> BezPath {
    use kurbo::{PathEl, Point};

    let mut result = BezPath::new();
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
                if current != subpath_start {
                    gui_zig_zag_segment(&mut result, current, subpath_start, size, ridges, smooth);
                }
                result.close_path();
                current = subpath_start;
            }
            _ => {
                let endpoint = match *el {
                    PathEl::LineTo(p) | PathEl::CurveTo(_, _, p) | PathEl::QuadTo(_, p) => p,
                    _ => unreachable!(),
                };
                // Find previous endpoint.
                let start = {
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
                };
                gui_zig_zag_segment(&mut result, start, endpoint, size, ridges, smooth);
                current = endpoint;
            }
        }
    }
    result
}

pub(crate) fn gui_zig_zag_segment(
    path: &mut BezPath,
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
    let tx = dx / len;
    let ty = dy / len;
    let nx = -ty;
    let ny = tx;
    let steps = ridges * 2;
    let step_len = len / steps as f64;

    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        let px = from.x + dx * t;
        let py = from.y + dy * t;
        let disp = if i == steps {
            0.0
        } else if i % 2 == 1 {
            size / 2.0
        } else {
            -size / 2.0
        };
        let pt = kurbo::Point::new(px + nx * disp, py + ny * disp);

        if smooth && i < steps {
            let handle_len = step_len * 0.3;
            let prev_disp = if i == 1 {
                0.0
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

pub(crate) fn gui_path_centroid(bez: &BezPath) -> kurbo::Point {
    let mut sx = 0.0;
    let mut sy = 0.0;
    let mut n = 0usize;
    for el in bez.elements() {
        let pt = match *el {
            PathEl::MoveTo(p)
            | PathEl::LineTo(p)
            | PathEl::CurveTo(_, _, p)
            | PathEl::QuadTo(_, p) => Some(p),
            PathEl::ClosePath => None,
        };
        if let Some(p) = pt {
            sx += p.x;
            sy += p.y;
            n += 1;
        }
    }
    if n == 0 {
        kurbo::Point::ZERO
    } else {
        kurbo::Point::new(sx / n as f64, sy / n as f64)
    }
}

pub(crate) fn gui_pucker_bloat(bez: &BezPath, strength: f64, center: kurbo::Point) -> BezPath {
    let displace = |p: kurbo::Point| -> kurbo::Point {
        let dx = p.x - center.x;
        let dy = p.y - center.y;
        let dist = (dx * dx + dy * dy).sqrt();
        if dist < 1e-9 {
            return p;
        }
        let factor = 1.0 + strength;
        kurbo::Point::new(center.x + dx * factor, center.y + dy * factor)
    };
    let mut result = BezPath::new();
    for el in bez.elements() {
        match *el {
            PathEl::MoveTo(p) => result.move_to(displace(p)),
            PathEl::LineTo(p) => result.line_to(displace(p)),
            PathEl::CurveTo(c1, c2, p) => result.curve_to(displace(c1), displace(c2), displace(p)),
            PathEl::QuadTo(c, p) => result.quad_to(displace(c), displace(p)),
            PathEl::ClosePath => result.close_path(),
        }
    }
    result
}

pub(crate) fn gui_round_corners(bez: &BezPath, radius: f64) -> BezPath {
    let elements = bez.elements();
    if elements.is_empty() || radius <= 0.0 {
        return bez.clone();
    }

    let mut result = BezPath::new();
    let mut subpath: Vec<kurbo::Point> = Vec::new();
    let mut is_closed = false;

    let flush = |result: &mut BezPath, pts: &[kurbo::Point], closed: bool, radius: f64| {
        if pts.len() < 2 {
            if let Some(&p) = pts.first() {
                result.move_to(p);
            }
            return;
        }
        let n = pts.len();
        for i in 0..n {
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
            let is_ep = !closed && (i == 0 || i == n - 1);
            if is_ep {
                if i == 0 {
                    result.move_to(curr);
                } else {
                    result.line_to(curr);
                }
            } else {
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
                let r = radius.min(len_in / 2.0).min(len_out / 2.0);
                let fs =
                    kurbo::Point::new(curr.x - (dx_in / len_in) * r, curr.y - (dy_in / len_in) * r);
                let fe = kurbo::Point::new(
                    curr.x + (dx_out / len_out) * r,
                    curr.y + (dy_out / len_out) * r,
                );
                if i == 0 {
                    result.move_to(fs);
                } else {
                    result.line_to(fs);
                }
                result.quad_to(curr, fe);
            }
        }
        if closed {
            result.close_path();
        }
    };

    for el in elements {
        match *el {
            PathEl::MoveTo(p) => {
                if !subpath.is_empty() {
                    flush(&mut result, &subpath, is_closed, radius);
                }
                subpath.clear();
                subpath.push(p);
                is_closed = false;
            }
            PathEl::LineTo(p) => {
                subpath.push(p);
            }
            PathEl::CurveTo(_, _, p) | PathEl::QuadTo(_, p) => {
                subpath.push(p);
            }
            PathEl::ClosePath => {
                is_closed = true;
            }
        }
    }
    if !subpath.is_empty() {
        flush(&mut result, &subpath, is_closed, radius);
    }
    result
}

pub(crate) fn gui_warp_envelope(bez: &BezPath, warp_type: &str, bend: f64) -> BezPath {
    // Compute bounding box.
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;
    for el in bez.elements() {
        let pts: Vec<kurbo::Point> = match *el {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => vec![p],
            PathEl::CurveTo(c1, c2, p) => vec![c1, c2, p],
            PathEl::QuadTo(c, p) => vec![c, p],
            PathEl::ClosePath => vec![],
        };
        for p in pts {
            min_x = min_x.min(p.x);
            min_y = min_y.min(p.y);
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
        }
    }
    let w = max_x - min_x;
    let h = max_y - min_y;
    if w < 1e-9 || h < 1e-9 {
        return bez.clone();
    }

    let warp = |p: kurbo::Point| -> kurbo::Point {
        let nx = (p.x - min_x) / w;
        let ny = (p.y - min_y) / h;
        let (dx, dy) = match warp_type {
            "arc" => (0.0, bend * (nx * (1.0 - nx) * 4.0) * h * 0.25),
            "bulge" => {
                let cx = nx - 0.5;
                let cy = ny - 0.5;
                let r = (cx * cx + cy * cy).sqrt().min(0.5);
                let f = bend * (1.0 - r * 2.0).max(0.0);
                (cx * f * w, cy * f * h)
            }
            "wave" => (
                0.0,
                bend * (std::f64::consts::PI * 2.0 * nx).sin() * h * 0.25,
            ),
            "flag" => (
                0.0,
                bend * nx * (std::f64::consts::PI * 2.0 * ny).sin() * h * 0.25,
            ),
            _ => (0.0, 0.0),
        };
        kurbo::Point::new(p.x + dx, p.y + dy)
    };

    let mut result = BezPath::new();
    for el in bez.elements() {
        match *el {
            PathEl::MoveTo(p) => result.move_to(warp(p)),
            PathEl::LineTo(p) => result.line_to(warp(p)),
            PathEl::CurveTo(c1, c2, p) => result.curve_to(warp(c1), warp(c2), warp(p)),
            PathEl::QuadTo(c, p) => result.quad_to(warp(c), warp(p)),
            PathEl::ClosePath => result.close_path(),
        }
    }
    result
}

pub(crate) fn gui_crystallize(bez: &BezPath, size: f64, count: usize) -> BezPath {
    let mut result = BezPath::new();
    let mut current = kurbo::Point::ZERO;
    let mut subpath_start = kurbo::Point::ZERO;
    for el in bez.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                result.move_to(p);
                current = p;
                subpath_start = p;
            }
            PathEl::ClosePath => {
                if current != subpath_start {
                    gui_crystallize_seg(&mut result, current, subpath_start, size, count);
                }
                result.close_path();
                current = subpath_start;
            }
            _ => {
                let endpoint = match *el {
                    PathEl::LineTo(p) | PathEl::CurveTo(_, _, p) | PathEl::QuadTo(_, p) => p,
                    _ => unreachable!(),
                };
                let start = {
                    let els = result.elements();
                    let mut pt = kurbo::Point::ZERO;
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
                };
                gui_crystallize_seg(&mut result, start, endpoint, size, count);
                current = endpoint;
            }
        }
    }
    result
}

pub(crate) fn gui_crystallize_seg(
    path: &mut BezPath,
    from: kurbo::Point,
    to: kurbo::Point,
    size: f64,
    count: usize,
) {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-9 {
        path.line_to(to);
        return;
    }
    let nx = -dy / len;
    let ny = dx / len;
    for i in 0..count {
        let t_peak = (i as f64 + 0.5) / count as f64;
        let t_end = (i + 1) as f64 / count as f64;
        let peak = kurbo::Point::new(
            from.x + dx * t_peak + nx * size,
            from.y + dy * t_peak + ny * size,
        );
        let base_end = kurbo::Point::new(from.x + dx * t_end, from.y + dy * t_end);
        path.line_to(peak);
        path.line_to(base_end);
    }
}

pub(crate) fn gui_scallop(bez: &BezPath, depth: f64, count: usize) -> BezPath {
    let mut result = BezPath::new();
    let mut current = kurbo::Point::ZERO;
    let mut subpath_start = kurbo::Point::ZERO;

    for el in bez.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                result.move_to(p);
                current = p;
                subpath_start = p;
            }
            PathEl::ClosePath => {
                if current != subpath_start {
                    gui_scallop_seg(&mut result, current, subpath_start, depth, count);
                }
                result.close_path();
                current = subpath_start;
            }
            _ => {
                let endpoint = match *el {
                    PathEl::LineTo(p) | PathEl::CurveTo(_, _, p) | PathEl::QuadTo(_, p) => p,
                    _ => unreachable!(),
                };
                let start = {
                    let els = result.elements();
                    let mut pt = kurbo::Point::ZERO;
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
                };
                gui_scallop_seg(&mut result, start, endpoint, depth, count);
                current = endpoint;
            }
        }
    }
    result
}

pub(crate) fn gui_scallop_seg(
    path: &mut BezPath,
    from: kurbo::Point,
    to: kurbo::Point,
    depth: f64,
    count: usize,
) {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-9 {
        path.line_to(to);
        return;
    }
    let nx = dy / len;
    let ny = -dx / len;
    for i in 0..count {
        let t0 = i as f64 / count as f64;
        let t1 = (i + 1) as f64 / count as f64;
        let tmid = (t0 + t1) / 2.0;
        let p1 = kurbo::Point::new(from.x + dx * t1, from.y + dy * t1);
        let p0 = kurbo::Point::new(from.x + dx * t0, from.y + dy * t0);
        let pmid = kurbo::Point::new(
            from.x + dx * tmid + nx * depth,
            from.y + dy * tmid + ny * depth,
        );
        let qx = 2.0 * pmid.x - 0.5 * (p0.x + p1.x);
        let qy = 2.0 * pmid.y - 0.5 * (p0.y + p1.y);
        path.quad_to(kurbo::Point::new(qx, qy), p1);
    }
}

pub(crate) fn gui_blend_objects(
    nid_a: NodeId,
    nid_b: NodeId,
    steps: usize,
    doc: &mut Document,
    history: &mut CommandHistory,
    doc_modified: &mut bool,
) {
    use photonic_core::color::Color;
    use photonic_core::style::{Fill, FillKind};

    let (node_a, node_b) = match (
        doc.nodes.get(&nid_a).cloned(),
        doc.nodes.get(&nid_b).cloned(),
    ) {
        (Some(a), Some(b)) => (a, b),
        _ => return,
    };
    let (pn_a, pn_b) = match (&node_a.kind, &node_b.kind) {
        (SceneNodeKind::Path(a), SceneNodeKind::Path(b)) => (a.clone(), b.clone()),
        _ => return,
    };
    let bez_a = pn_a.path_data.to_bez_path();
    let bez_b = pn_b.path_data.to_bez_path();
    if bez_a.elements().len() != bez_b.elements().len() {
        return;
    }

    let color_a = match &pn_a.fill.kind {
        FillKind::Solid(c) => Some(*c),
        _ => None,
    };
    let color_b = match &pn_b.fill.kind {
        FillKind::Solid(c) => Some(*c),
        _ => None,
    };
    let tx_a = (node_a.transform.matrix[4], node_a.transform.matrix[5]);
    let tx_b = (node_b.transform.matrix[4], node_b.transform.matrix[5]);
    let layer_id = node_a.layer_id;

    let lerp_pt = |a: kurbo::Point, b: kurbo::Point, t: f64| {
        kurbo::Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
    };

    for i in 1..=steps {
        let t = i as f64 / (steps + 1) as f64;
        let mut interp = BezPath::new();
        for (ea, eb) in bez_a.elements().iter().zip(bez_b.elements().iter()) {
            match (*ea, *eb) {
                (PathEl::MoveTo(a), PathEl::MoveTo(b)) => interp.move_to(lerp_pt(a, b, t)),
                (PathEl::LineTo(a), PathEl::LineTo(b)) => interp.line_to(lerp_pt(a, b, t)),
                (PathEl::CurveTo(a1, a2, a3), PathEl::CurveTo(b1, b2, b3)) => {
                    interp.curve_to(lerp_pt(a1, b1, t), lerp_pt(a2, b2, t), lerp_pt(a3, b3, t))
                }
                (PathEl::QuadTo(a1, a2), PathEl::QuadTo(b1, b2)) => {
                    interp.quad_to(lerp_pt(a1, b1, t), lerp_pt(a2, b2, t))
                }
                (PathEl::ClosePath, PathEl::ClosePath) => interp.close_path(),
                _ => interp.push(*ea),
            }
        }
        let mut new_pn = pn_a.clone();
        new_pn.path_data = PathData::from_bez_path(&interp);
        if let (Some(ca), Some(cb)) = (&color_a, &color_b) {
            new_pn.fill = Fill {
                kind: FillKind::Solid(Color::new(
                    ca.r + (cb.r - ca.r) * t as f32,
                    ca.g + (cb.g - ca.g) * t as f32,
                    ca.b + (cb.b - ca.b) * t as f32,
                    ca.a + (cb.a - ca.a) * t as f32,
                )),
                ..pn_a.fill.clone()
            };
        }
        let opacity = node_a.opacity + (node_b.opacity - node_a.opacity) * t as f32;
        let name = format!("Blend {}/{}", i, steps);
        let mut node = SceneNode::new(&name, layer_id, SceneNodeKind::Path(new_pn));
        node.opacity = opacity;
        let itx = (
            tx_a.0 + (tx_b.0 - tx_a.0) * t,
            tx_a.1 + (tx_b.1 - tx_a.1) * t,
        );
        node.transform = photonic_core::transform::Transform::translate(itx.0, itx.1);
        history.execute(
            Command::AddNode {
                node,
                layer_id: Some(layer_id),
            },
            doc,
        );
    }
    *doc_modified = true;
}

/// Blend using Smooth Color mode: auto-compute steps from color distance.
pub(crate) fn gui_blend_objects_smooth_color(
    nid_a: NodeId,
    nid_b: NodeId,
    doc: &mut Document,
    history: &mut CommandHistory,
    doc_modified: &mut bool,
) {
    use photonic_core::style::FillKind;
    let (node_a, node_b) = match (
        doc.nodes.get(&nid_a).cloned(),
        doc.nodes.get(&nid_b).cloned(),
    ) {
        (Some(a), Some(b)) => (a, b),
        _ => return,
    };
    let (pn_a, pn_b) = match (&node_a.kind, &node_b.kind) {
        (SceneNodeKind::Path(a), SceneNodeKind::Path(b)) => (a.clone(), b.clone()),
        _ => return,
    };
    let color_a = match &pn_a.fill.kind {
        FillKind::Solid(c) => Some(*c),
        _ => None,
    };
    let color_b = match &pn_b.fill.kind {
        FillKind::Solid(c) => Some(*c),
        _ => None,
    };
    let steps = if let (Some(ca), Some(cb)) = (&color_a, &color_b) {
        let dr = ((cb.r - ca.r).abs() * 255.0) as f64;
        let dg = ((cb.g - ca.g).abs() * 255.0) as f64;
        let db = ((cb.b - ca.b).abs() * 255.0) as f64;
        (dr.max(dg).max(db).ceil() as usize).max(1)
    } else {
        5
    };
    gui_blend_objects(nid_a, nid_b, steps, doc, history, doc_modified);
}

/// Blend using Specified Distance mode: space steps by pixel distance.
pub(crate) fn gui_blend_objects_spacing(
    nid_a: NodeId,
    nid_b: NodeId,
    spacing: f64,
    doc: &mut Document,
    history: &mut CommandHistory,
    doc_modified: &mut bool,
) {
    if spacing <= 0.0 {
        return;
    }
    let (node_a, node_b) = match (
        doc.nodes.get(&nid_a).cloned(),
        doc.nodes.get(&nid_b).cloned(),
    ) {
        (Some(a), Some(b)) => (a, b),
        _ => return,
    };
    let tx_a = (node_a.transform.matrix[4], node_a.transform.matrix[5]);
    let tx_b = (node_b.transform.matrix[4], node_b.transform.matrix[5]);
    let dx = tx_b.0 - tx_a.0;
    let dy = tx_b.1 - tx_a.1;
    let dist = (dx * dx + dy * dy).sqrt();
    let steps = ((dist / spacing).ceil() as usize).saturating_sub(1).max(1);
    gui_blend_objects(nid_a, nid_b, steps, doc, history, doc_modified);
}

pub(crate) fn gui_twirl(bez: &BezPath, angle_rad: f64, center: kurbo::Point) -> BezPath {
    let mut max_dist = 0.0f64;
    for el in bez.elements() {
        let pts: Vec<kurbo::Point> = match *el {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => vec![p],
            PathEl::CurveTo(c1, c2, p) => vec![c1, c2, p],
            PathEl::QuadTo(c, p) => vec![c, p],
            PathEl::ClosePath => vec![],
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

    let twirl = |p: kurbo::Point| -> kurbo::Point {
        let dx = p.x - center.x;
        let dy = p.y - center.y;
        let dist = (dx * dx + dy * dy).sqrt();
        let t = 1.0 - (dist / max_dist).min(1.0);
        let a = angle_rad * t;
        kurbo::Point::new(
            center.x + dx * a.cos() - dy * a.sin(),
            center.y + dx * a.sin() + dy * a.cos(),
        )
    };

    let mut result = BezPath::new();
    for el in bez.elements() {
        match *el {
            PathEl::MoveTo(p) => result.move_to(twirl(p)),
            PathEl::LineTo(p) => result.line_to(twirl(p)),
            PathEl::CurveTo(c1, c2, p) => result.curve_to(twirl(c1), twirl(c2), twirl(p)),
            PathEl::QuadTo(c, p) => result.quad_to(twirl(c), twirl(p)),
            PathEl::ClosePath => result.close_path(),
        }
    }
    result
}

pub(crate) fn gui_xorshift64(state: &mut u64) -> f64 {
    let mut s = *state;
    s ^= s << 13;
    s ^= s >> 7;
    s ^= s << 17;
    *state = s;
    (s as f64 / u64::MAX as f64) * 2.0 - 1.0
}

pub(crate) fn gui_subdivide_bez(bez: &BezPath) -> BezPath {
    let mut result = BezPath::new();
    let mut current = kurbo::Point::ZERO;
    let mid =
        |a: kurbo::Point, b: kurbo::Point| kurbo::Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
    for el in bez.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                result.move_to(p);
                current = p;
            }
            PathEl::LineTo(p) => {
                result.line_to(mid(current, p));
                result.line_to(p);
                current = p;
            }
            PathEl::CurveTo(c1, c2, p) => {
                let m01 = mid(current, c1);
                let m12 = mid(c1, c2);
                let m23 = mid(c2, p);
                let m012 = mid(m01, m12);
                let m123 = mid(m12, m23);
                let m0123 = mid(m012, m123);
                result.curve_to(m01, m012, m0123);
                result.curve_to(m123, m23, p);
                current = p;
            }
            PathEl::QuadTo(c, p) => {
                let mc0 = mid(current, c);
                let mc1 = mid(c, p);
                let m = mid(mc0, mc1);
                result.quad_to(mc0, m);
                result.quad_to(mc1, p);
                current = p;
            }
            PathEl::ClosePath => {
                result.close_path();
            }
        }
    }
    result
}

pub(crate) fn gui_roughen(bez: &BezPath, size: f64, seed: u64) -> BezPath {
    let mut rng = seed.max(1);
    let displace = |p: kurbo::Point, rng: &mut u64| -> kurbo::Point {
        kurbo::Point::new(
            p.x + gui_xorshift64(rng) * size,
            p.y + gui_xorshift64(rng) * size,
        )
    };
    let mut result = BezPath::new();
    for el in bez.elements() {
        match *el {
            PathEl::MoveTo(p) => result.move_to(displace(p, &mut rng)),
            PathEl::LineTo(p) => result.line_to(displace(p, &mut rng)),
            PathEl::CurveTo(c1, c2, p) => result.curve_to(
                displace(c1, &mut rng),
                displace(c2, &mut rng),
                displace(p, &mut rng),
            ),
            PathEl::QuadTo(c, p) => result.quad_to(displace(c, &mut rng), displace(p, &mut rng)),
            PathEl::ClosePath => result.close_path(),
        }
    }
    result
}

pub(crate) fn bez_remove_elements(bez: &BezPath, indices: &[usize]) -> BezPath {
    let remove_set: std::collections::HashSet<usize> = indices.iter().copied().collect();
    let mut result = BezPath::new();
    let mut needs_move = true;
    for (i, el) in bez.elements().iter().enumerate() {
        if remove_set.contains(&i) {
            needs_move = true;
            continue;
        }
        if needs_move {
            // Patch: replace a non-MoveTo element that follows a gap with a MoveTo
            let endpoint = match el {
                PathEl::MoveTo(p) | PathEl::LineTo(p) => Some(*p),
                PathEl::CurveTo(_, _, p) => Some(*p),
                PathEl::QuadTo(_, p) => Some(*p),
                PathEl::ClosePath => None,
            };
            if let Some(p) = endpoint {
                result.push(PathEl::MoveTo(p));
                needs_move = false;
                // Skip emitting the original element if it was already a MoveTo
                if !matches!(el, PathEl::MoveTo(_)) {
                    result.push(*el);
                }
            }
        } else {
            result.push(*el);
        }
    }
    result
}

#[cfg(test)]
mod convert_anchor_tests {
    use super::*;
    use std::collections::HashSet;

    /// A closed unit-ish rectangle in kurbo's implicit-close form:
    /// `MoveTo, LineTo, LineTo, LineTo, ClosePath` (indices 0..=4).
    fn rect() -> BezPath {
        let mut b = BezPath::new();
        b.move_to(Point::new(0.0, 0.0)); // idx 0
        b.line_to(Point::new(100.0, 0.0)); // idx 1
        b.line_to(Point::new(100.0, 100.0)); // idx 2
        b.line_to(Point::new(0.0, 100.0)); // idx 3
        b.close_path(); // idx 4
        b
    }

    fn sel(indices: &[usize]) -> HashSet<usize> {
        indices.iter().copied().collect()
    }

    fn append_closed_contour(bez: &mut BezPath, x0: f64, y0: f64, x1: f64, y1: f64) {
        bez.move_to(Point::new(x0, y0));
        bez.curve_to(
            Point::new(x0 + (x1 - x0) / 3.0, y0 - 2.0),
            Point::new(x0 + 2.0 * (x1 - x0) / 3.0, y0 - 2.0),
            Point::new(x1, y0),
        );
        bez.line_to(Point::new(x1, y1));
        bez.curve_to(
            Point::new(x1 - (x1 - x0) / 3.0, y1 + 2.0),
            Point::new(x0 + (x1 - x0) / 3.0, y1 + 2.0),
            Point::new(x0, y1),
        );
        bez.close_path();
    }

    #[test]
    fn compound_selection_overlay_never_connects_contours() {
        let mut bez = BezPath::new();
        append_closed_contour(&mut bez, 0.0, 0.0, 100.0, 100.0);
        append_closed_contour(&mut bez, 20.0, 20.0, 35.0, 40.0);
        append_closed_contour(&mut bez, 60.0, 55.0, 80.0, 80.0);

        // This is the exact point data used to build the direct-selection
        // overlay. Each MoveTo must start a fresh contour, and each ClosePath
        // may only close back to that contour's own first point.
        let subpaths = sample_path_subpaths(&bez);
        assert_eq!(subpaths.len(), 3);
        assert!(subpaths.iter().all(|subpath| subpath.closed));

        let segments: Vec<(usize, Point, Point)> = subpaths
            .iter()
            .enumerate()
            .flat_map(|(contour_id, subpath)| {
                let mut contour_segments: Vec<_> = subpath
                    .points
                    .windows(2)
                    .map(|pair| (contour_id, pair[0], pair[1]))
                    .collect();
                contour_segments.push((
                    contour_id,
                    *subpath.points.last().unwrap(),
                    subpath.points[0],
                ));
                contour_segments
            })
            .collect();

        for contour_id in 0..2 {
            let from = *subpaths[contour_id].points.last().unwrap();
            let to = subpaths[contour_id + 1].points[0];
            assert!(
                !segments
                    .iter()
                    .any(|(_, segment_from, segment_to)| *segment_from == from && *segment_to == to),
                "overlay contains a synthetic edge from contour {contour_id} to contour {}",
                contour_id + 1
            );
        }

        // Handles are resolved by element location. Verify every handle used by
        // an anchor stays within the same MoveTo..ClosePath element range.
        let contour_ranges = [(0usize, 4usize), (5, 9), (10, 14)];
        for (anchor_index, _) in path_anchor_points(&bez) {
            let (contour_id, &(start, end)) = contour_ranges
                .iter()
                .enumerate()
                .find(|(_, (start, end))| (*start..=*end).contains(&anchor_index))
                .expect("anchor belongs to one contour");
            let (in_handle, out_handle) = logical_handles(&bez, anchor_index);
            for (handle_index, _) in [in_handle, out_handle].into_iter().flatten() {
                assert!(
                    (start..=end).contains(&handle_index),
                    "anchor {anchor_index} in contour {contour_id} borrowed handle {handle_index} from another contour"
                );
            }
        }

        // Applying a node transform must not merge the contour lists either.
        let view = CanvasView::default();
        let transform = photonic_core::transform::Transform::translate(56.0, 14.0);
        let screen_subpaths = bez_to_screen_subpaths_xf(&bez, &view, &transform);
        assert_eq!(screen_subpaths.len(), 3);
        assert!(screen_subpaths.iter().all(|(_, closed)| *closed));
    }

    /// Cosine of the angle between the In and Out handle directions at anchor
    /// `i` (seam-aware). ~-1 means the handles are collinear → a smooth point.
    fn handle_collinearity(bez: &BezPath, i: usize) -> f64 {
        let anchor = path_anchor_points(bez)
            .into_iter()
            .find(|(idx, _)| *idx == i)
            .map(|(_, p)| p)
            .expect("anchor exists");
        let (in_h, out_h) = anchor_handle_pair(bez, i);
        let (_, ip) = in_h.expect("in handle present");
        let (_, op) = out_h.expect("out handle present");
        let v1 = (ip.x - anchor.x, ip.y - anchor.y);
        let v2 = (op.x - anchor.x, op.y - anchor.y);
        let l1 = (v1.0 * v1.0 + v1.1 * v1.1).sqrt();
        let l2 = (v2.0 * v2.0 + v2.1 * v2.1).sqrt();
        (v1.0 * v2.0 + v1.1 * v2.1) / (l1 * l2)
    }

    #[test]
    fn smooth_straight_corner_synthesizes_handles() {
        // Select the interior vertex at (100,0); both adjacent LineTo edges
        // must become CurveTo and the synthesized handles must be collinear.
        let out = bez_convert_anchors(&rect(), &sel(&[1]), true);
        let els = out.elements();
        assert!(
            matches!(els[1], PathEl::CurveTo(..)),
            "incoming edge should be a curve, got {:?}",
            els[1]
        );
        assert!(
            matches!(els[2], PathEl::CurveTo(..)),
            "outgoing edge should be a curve, got {:?}",
            els[2]
        );
        // Untouched sides stay straight.
        assert!(matches!(els[3], PathEl::LineTo(_)));
        assert!((handle_collinearity(&out, 1) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn smooth_seam_corner_synthesizes_handles() {
        // Select the MoveTo start vertex (idx 0) and the last pre-ClosePath
        // vertex (idx 3). The implicit closing edge must be materialized as an
        // explicit CurveTo, and both corners must become smooth.
        let out = bez_convert_anchors(&rect(), &sel(&[0, 3]), true);
        let els = out.elements();
        // One extra element: the seam CurveTo inserted before ClosePath.
        assert_eq!(els.len(), 6, "seam should be materialized, got {els:?}");
        assert!(matches!(els[0], PathEl::MoveTo(_)));
        assert!(
            matches!(els[4], PathEl::CurveTo(..)),
            "closing seam edge should be an explicit curve, got {:?}",
            els[4]
        );
        assert!(matches!(els[5], PathEl::ClosePath));
        // Both selected corners are smooth (seam-aware handle pair collinear).
        assert!((handle_collinearity(&out, 0) + 1.0).abs() < 1e-6);
        assert!((handle_collinearity(&out, 3) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn corner_is_noop_on_straight() {
        // Corner on an already-straight vertex must leave the path identical.
        let before = rect();
        let after = bez_convert_anchors(&before, &sel(&[1]), false);
        assert_eq!(before.elements(), after.elements());
    }

    #[test]
    fn smooth_then_corner_roundtrips() {
        // Smooth a corner, then Corner it: the handles adjacent to that anchor
        // must retract back to the anchor point (geometrically straight again).
        let smoothed = bez_convert_anchors(&rect(), &sel(&[1]), true);
        let cornered = bez_convert_anchors(&smoothed, &sel(&[1]), false);
        let anchor = Point::new(100.0, 0.0);
        let (in_h, out_h) = anchor_handle_pair(&cornered, 1);
        let (_, ip) = in_h.expect("in control present (retracted CurveTo)");
        let (_, op) = out_h.expect("out control present (retracted CurveTo)");
        assert!((ip.x - anchor.x).abs() < 1e-6 && (ip.y - anchor.y).abs() < 1e-6);
        assert!((op.x - anchor.x).abs() < 1e-6 && (op.y - anchor.y).abs() < 1e-6);
        assert!(!is_smooth_anchor(&cornered, 1));
    }

    #[test]
    fn some_some_averaging_preserved() {
        // A vertex that already has both handles keeps the legacy averaging:
        // handles stay curved and are made collinear, lengths preserved.
        let mut b = BezPath::new();
        b.move_to(Point::new(0.0, 0.0));
        // Arrive at (100,0) with an incoming handle, leave toward (100,100).
        b.curve_to(
            Point::new(20.0, 30.0),
            Point::new(60.0, 20.0),
            Point::new(100.0, 0.0),
        );
        b.curve_to(
            Point::new(140.0, -10.0),
            Point::new(110.0, 60.0),
            Point::new(100.0, 100.0),
        );
        b.line_to(Point::new(0.0, 100.0));
        b.close_path();
        let in_len_before = ((60.0 - 100.0f64).powi(2) + (20.0 - 0.0f64).powi(2)).sqrt();
        let out_len_before = ((140.0 - 100.0f64).powi(2) + (-10.0 - 0.0f64).powi(2)).sqrt();
        let out = bez_convert_anchors(&b, &sel(&[1]), true);
        assert!(matches!(out.elements()[1], PathEl::CurveTo(..)));
        assert!(matches!(out.elements()[2], PathEl::CurveTo(..)));
        assert!((handle_collinearity(&out, 1) + 1.0).abs() < 1e-6);
        let anchor = Point::new(100.0, 0.0);
        let (in_h, out_h) = anchor_handle_pair(&out, 1);
        let (_, ip) = in_h.unwrap();
        let (_, op) = out_h.unwrap();
        let in_len_after = ((ip.x - anchor.x).powi(2) + (ip.y - anchor.y).powi(2)).sqrt();
        let out_len_after = ((op.x - anchor.x).powi(2) + (op.y - anchor.y).powi(2)).sqrt();
        assert!((in_len_after - in_len_before).abs() < 1e-6);
        assert!((out_len_after - out_len_before).abs() < 1e-6);
    }

    /// True if any `CurveTo` in `bez` is a degenerate zero-length cubic — its
    /// endpoint coincides with the previous point and both controls collapse to
    /// it. Such elements are invalid geometry the seam path must never emit.
    fn has_degenerate_curve(bez: &BezPath) -> bool {
        let close = |a: Point, b: Point| (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6;
        let mut prev = Point::ZERO;
        for el in bez.elements() {
            match *el {
                PathEl::MoveTo(p) => prev = p,
                PathEl::LineTo(p) => prev = p,
                PathEl::QuadTo(_, p) => prev = p,
                PathEl::CurveTo(c1, c2, p) => {
                    if close(p, prev) && close(c1, prev) && close(c2, prev) {
                        return true;
                    }
                    prev = p;
                }
                PathEl::ClosePath => {}
            }
        }
        false
    }

    #[test]
    fn smooth_seam_is_idempotent() {
        // Regression (blocker): smoothing the seam anchor materializes an
        // explicit closing `CurveTo` whose endpoint equals the `MoveTo` start
        // (start listed twice). Feeding that back and smoothing the same seam
        // anchor again must NOT append a degenerate zero-length cubic or collapse
        // the seam in-handle — Smooth is idempotent on an already-smooth point.
        let once = bez_convert_anchors(&rect(), &sel(&[0]), true);
        assert!(
            !has_degenerate_curve(&once),
            "first smooth degenerate: {once:?}"
        );
        assert!(
            is_smooth_anchor(&once, 0),
            "seam not smooth after 1st: {once:?}"
        );

        // Second application on the explicit-close (duplicate-start) form.
        let twice = bez_convert_anchors(&once, &sel(&[0]), true);
        assert!(
            !has_degenerate_curve(&twice),
            "second smooth produced a degenerate zero-length CurveTo: {twice:?}"
        );
        assert!(
            is_smooth_anchor(&twice, 0),
            "seam anchor lost smoothness on re-smooth: {twice:?}"
        );
        // No element growth on the idempotent re-application.
        assert_eq!(
            once.elements().len(),
            twice.elements().len(),
            "re-smoothing the seam must not grow the element count: {twice:?}"
        );
    }

    #[test]
    fn seam_materialization_shifts_compound_indices() {
        // Regression (major): materializing a seam in an EARLIER subpath of a
        // compound path inserts an element, shifting every later subpath's
        // element indices. The GUI handler relies on detecting this count change
        // to drop the now-stale point selection. Assert the count grows so that
        // signal is reliable.
        let mut b = BezPath::new();
        // Subpath A (indices 0..=4): a closed rect.
        b.move_to(Point::new(0.0, 0.0)); // 0
        b.line_to(Point::new(100.0, 0.0)); // 1
        b.line_to(Point::new(100.0, 100.0)); // 2
        b.line_to(Point::new(0.0, 100.0)); // 3
        b.close_path(); // 4
                        // Subpath B (indices 5..=9): a second closed rect.
        b.move_to(Point::new(200.0, 0.0)); // 5
        b.line_to(Point::new(300.0, 0.0)); // 6
        b.line_to(Point::new(300.0, 100.0)); // 7
        b.line_to(Point::new(200.0, 100.0)); // 8
        b.close_path(); // 9

        let before = b.elements().len();
        // Smooth the seam anchor of subpath A (idx 0).
        let out = bez_convert_anchors(&b, &sel(&[0]), true);
        assert!(
            out.elements().len() > before,
            "seam materialization must grow the element count so the caller can \
             detect the index shift and clear the stale selection: {out:?}"
        );
    }
}
