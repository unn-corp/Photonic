pub use crate::handlers::charts::{
    create_bar_chart, create_line_chart, create_pie_chart, create_radar_chart, create_scatter_plot,
    create_stacked_bar_chart,
};
pub use crate::handlers::clipping::{
    make_clipping_mask, make_compound_path, release_clipping_mask, release_compound_path,
};
pub use crate::handlers::guides::{
    add_dimension_line, add_guide, clear_guides, list_guides, pin_object_guides, remove_guide,
};
pub use crate::handlers::pathfinder::{
    boolean_operation, divide_objects_below, pathfinder_crop, pathfinder_divide, pathfinder_merge,
    pathfinder_minus_back, pathfinder_minus_front, pathfinder_outline, pathfinder_trim,
};
pub use crate::handlers::selection::{
    deselect_all, find_nodes, find_replace_style, find_replace_text, get_selection, lasso_select,
    magic_wand_select, select_all, select_by_kind, select_inside_group, select_same,
    select_similar, set_selection,
};
pub use crate::handlers::shapes::{
    add_anchor_points, average_anchor_points, build_shape_from_points, convert_anchor_points,
    create_arrow_shape, create_cross, create_curvature_path, create_donut, create_flare,
    create_freehand_path, create_gear, create_grid, create_heart, create_parametric_shape,
    create_path, create_polar_grid, create_qr_code, create_shape, create_speech_bubble,
    create_spiral, create_sunburst, create_truchet_tiling, create_wave_pattern, crystallize_path,
    delete_anchor_point, join_paths, measure_path, noise_deform, offset_path, outline_stroke,
    point_on_path, proportional_move_anchor, pucker_bloat, reverse_path_direction, roughen_path,
    round_corners, scallop_path, scissors_cut, simplify_path, smooth_path, twirl_path,
    warp_envelope, zig_zag_path,
};
pub use crate::handlers::transform::{
    align_nodes, apply_flex_layout, apply_grid_layout, apply_stack_layout, apply_transform,
    center_on_canvas, create_array, distribute_no_overlap, distribute_on_path, duplicate_nodes,
    fit_to_canvas, flatten_group, flip_nodes, layout_nodes, mirror_copy, reorder_node,
    reverse_node_order, rotate_copies, scatter_copies, snap_to_pixel, split_into_grid,
    transform_copies,
};
pub use crate::handlers::typography::{
    apply_character_style, apply_paragraph_style, bind_text_variable, clear_tab_stops,
    clear_text_area, clear_text_path, create_character_style, create_paragraph_style, create_text,
    delete_character_style, delete_paragraph_style, get_opentype_features, link_text_frames,
    list_character_styles, list_paragraph_styles, set_character_metrics, set_font_style,
    set_font_weight, set_opentype_features, set_paragraph_options, set_tab_stops, set_text_area,
    set_text_decoration, set_text_direction, set_text_path, unbind_text_variable,
    unlink_text_frames,
};
use kurbo;
use photonic_core::node::{SceneNode, SceneNodeKind};

pub use crate::handlers::styling::{
    add_drop_shadow, adjust_colors, blend_colors, blend_objects, clear_blend_spine,
    clear_symbol_overrides, convert_to_grayscale, copy_appearance, expand_blend, get_recent_colors,
    hatch_fill, invert_colors, randomize_colors, recolor_artwork, remove_fill, remove_stroke,
    reverse_blend_spine, sample_color_at, set_blend_mode, set_blend_spine, set_opacity, set_paint,
    set_symbol_override, stipple_fill, style_transfer, swap_fill_stroke,
};
pub use crate::handlers::utility::{
    auto_name_nodes, check_style_continuity, clean_up, delete_nodes, enter_isolation_mode,
    exit_isolation_mode, export_tagged_assets, flatten_transparency, get_css_preview, get_node,
    get_node_prompts, group_nodes, inspect_node, make_live_boolean, measure_distance,
    measure_nodes, move_to_layer, set_locked, set_node_prompt, set_node_size, set_visibility,
    tag_node_for_export, tag_nodes, undo_node, ungroup_nodes, update_node,
};

/// Convert a sequence of points to a smooth cubic bezier path using Catmull-Rom interpolation.
/// The tension parameter is fixed at 0 (uniform Catmull-Rom = smooth interpolation).
pub(crate) fn catmull_rom_to_bezier(points: &[kurbo::Point], closed: bool) -> kurbo::BezPath {
    let n = points.len();
    let mut path = kurbo::BezPath::new();

    if n < 2 {
        if n == 1 {
            path.move_to(points[0]);
        }
        return path;
    }

    if n == 2 {
        // Straight line for 2 points.
        path.move_to(points[0]);
        path.line_to(points[1]);
        if closed {
            path.close_path();
        }
        return path;
    }

    // For Catmull-Rom → cubic bezier conversion:
    // Given four points P0, P1, P2, P3, the cubic bezier between P1 and P2 has:
    //   cp1 = P1 + (P2 - P0) / 6
    //   cp2 = P2 - (P3 - P1) / 6
    //
    // For endpoints of an open curve, we mirror the missing point.

    let get_point = |i: isize| -> kurbo::Point {
        if closed {
            points[((i % n as isize) + n as isize) as usize % n]
        } else {
            if i < 0 {
                // Mirror: P[-1] = 2*P[0] - P[1]
                kurbo::Point::new(
                    2.0 * points[0].x - points[1].x,
                    2.0 * points[0].y - points[1].y,
                )
            } else if i >= n as isize {
                // Mirror: P[n] = 2*P[n-1] - P[n-2]
                kurbo::Point::new(
                    2.0 * points[n - 1].x - points[n - 2].x,
                    2.0 * points[n - 1].y - points[n - 2].y,
                )
            } else {
                points[i as usize]
            }
        }
    };

    path.move_to(points[0]);

    let segments = if closed { n } else { n - 1 };
    for i in 0..segments {
        let p0 = get_point(i as isize - 1);
        let p1 = get_point(i as isize);
        let p2 = get_point(i as isize + 1);
        let p3 = get_point(i as isize + 2);

        let cp1 = kurbo::Point::new(p1.x + (p2.x - p0.x) / 6.0, p1.y + (p2.y - p0.y) / 6.0);
        let cp2 = kurbo::Point::new(p2.x - (p3.x - p1.x) / 6.0, p2.y - (p3.y - p1.y) / 6.0);

        path.curve_to(cp1, cp2, p2);
    }

    if closed {
        path.close_path();
    }

    path
}

/// Returns true if `prop` should be copied given the optional property filter list.
/// An absent or empty list means "copy everything".
pub(crate) fn style_prop_enabled(properties: &Option<Vec<String>>, prop: &str) -> bool {
    match properties {
        None => true,
        Some(v) if v.is_empty() => true,
        Some(v) => v.iter().any(|p| p == prop),
    }
}

// ─── find_replace_text ───────────────────────────────────────────────────────

// ─── layout_nodes ────────────────────────────────────────────────────────────

// ─── auto_name_nodes ──────────────────────────────────────────────────────────

/// Returns true if `name` looks like an auto-generated default (should be renamed).
pub(crate) fn is_generic_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    let generic_prefixes = [
        "path",
        "ellipse",
        "rectangle",
        "rect",
        "polygon",
        "star",
        "line",
        "group",
        "text",
        "shape",
        "node",
        "layer",
    ];
    if generic_prefixes.iter().any(|p| lower.starts_with(p)) {
        return true;
    }
    uuid::Uuid::parse_str(name).is_ok()
}

/// Map an RGB colour (0..1 linear sRGB) to a short English label.
pub(crate) fn color_label(r: f32, g: f32, b: f32) -> &'static str {
    if r > 0.85 && g > 0.85 && b > 0.85 {
        return "white";
    }
    if r < 0.15 && g < 0.15 && b < 0.15 {
        return "black";
    }
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let chroma = max - min;
    if chroma < 0.12 {
        return if max > 0.6 { "light gray" } else { "gray" };
    }
    if r > 0.5 && g > 0.35 && b < 0.25 {
        return "orange";
    }
    if r > 0.6 && g > 0.6 && b < 0.3 {
        return "yellow";
    }
    if r > 0.6 && b > 0.6 && g < 0.3 {
        return "magenta";
    }
    if g > 0.5 && b > 0.5 && r < 0.3 {
        return "cyan";
    }
    if r >= g && r >= b && r > 0.5 && g < 0.5 {
        return "red";
    }
    if g >= r && g >= b && g > 0.5 && r < 0.5 {
        return "green";
    }
    if b >= r && b >= g && b > 0.4 {
        return "blue";
    }
    if max < 0.4 {
        return "dark";
    }
    "colored"
}

/// Generate a descriptive name for a node based on its type and properties.
pub(crate) fn generate_name(node: &SceneNode) -> String {
    use photonic_core::style::FillKind;

    match &node.kind {
        SceneNodeKind::Text(t) => {
            let preview: String = t.content.chars().take(24).collect();
            let preview = preview.trim().to_string();
            if preview.is_empty() {
                "empty text".to_string()
            } else {
                format!("text: {}", preview)
            }
        }
        SceneNodeKind::Group(g) => {
            format!("group ({} items)", g.children.len())
        }
        SceneNodeKind::Path(p) => {
            // ── color part ────────────────────────────────────────────────────
            let color_part: String = if !p.fill.enabled {
                if p.stroke.enabled {
                    "outline".to_string()
                } else {
                    "empty".to_string()
                }
            } else {
                match &p.fill.kind {
                    FillKind::Solid(c) => color_label(c.r, c.g, c.b).to_string(),
                    FillKind::Gradient(_)
                    | FillKind::FluidGradient(_)
                    | FillKind::MeshGradient(_) => "gradient".to_string(),
                    FillKind::Pattern(_) => "pattern".to_string(),
                    FillKind::None => "outline".to_string(),
                }
            };
            // ── geometry part ─────────────────────────────────────────────────
            let geo_part: String = match p.path_data.bounding_box() {
                None => "shape".to_string(),
                Some(bb) => {
                    let w = (bb.x1 - bb.x0).abs();
                    let h = (bb.y1 - bb.y0).abs();
                    let area = w * h;
                    let size = if area < 2500.0 {
                        "small"
                    } else if area < 22500.0 {
                        "medium"
                    } else {
                        "large"
                    };
                    let ratio = if h > 0.0 { w / h } else { 1.0 };
                    let shape = if ratio > 2.5 {
                        "wide bar"
                    } else if ratio < 0.4 {
                        "tall bar"
                    } else if (0.85..=1.18).contains(&ratio) {
                        "square"
                    } else {
                        "shape"
                    };
                    format!("{} {}", size, shape)
                }
            };
            format!("{} {}", color_part, geo_part)
        }
        // raster: pixel layer — no fill/geometry to describe
        SceneNodeKind::Raster(_) => "raster".to_string(),
    }
}

// ─── CSS Preview ──────────────────────────────────────────────────────────────

// ─── check_style_continuity ───────────────────────────────────────────────────

/// Compute the centroid of all on-curve points in a BezPath.
pub(crate) fn path_centroid(bez: &kurbo::BezPath) -> kurbo::Point {
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut count = 0usize;
    for el in bez.elements() {
        let pt = match *el {
            kurbo::PathEl::MoveTo(p) | kurbo::PathEl::LineTo(p) => Some(p),
            kurbo::PathEl::CurveTo(_, _, p) => Some(p),
            kurbo::PathEl::QuadTo(_, p) => Some(p),
            kurbo::PathEl::ClosePath => None,
        };
        if let Some(p) = pt {
            sum_x += p.x;
            sum_y += p.y;
            count += 1;
        }
    }
    if count == 0 {
        kurbo::Point::ZERO
    } else {
        kurbo::Point::new(sum_x / count as f64, sum_y / count as f64)
    }
}

/// Subdivide every segment of a BezPath once (insert midpoints).
pub(crate) fn subdivide_bez(bez: &kurbo::BezPath) -> kurbo::BezPath {
    let mut result = kurbo::BezPath::new();
    let mut current = kurbo::Point::ZERO;

    for el in bez.elements() {
        match *el {
            kurbo::PathEl::MoveTo(p) => {
                result.move_to(p);
                current = p;
            }
            kurbo::PathEl::LineTo(p) => {
                let mid = kurbo::Point::new((current.x + p.x) / 2.0, (current.y + p.y) / 2.0);
                result.line_to(mid);
                result.line_to(p);
                current = p;
            }
            kurbo::PathEl::CurveTo(c1, c2, p) => {
                // De Casteljau subdivision at t=0.5
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
            kurbo::PathEl::QuadTo(c, p) => {
                let mc0 = mid(current, c);
                let mc1 = mid(c, p);
                let m = mid(mc0, mc1);
                result.quad_to(mc0, m);
                result.quad_to(mc1, p);
                current = p;
            }
            kurbo::PathEl::ClosePath => {
                result.close_path();
            }
        }
    }
    result
}

pub(crate) fn mid(a: kurbo::Point, b: kurbo::Point) -> kurbo::Point {
    kurbo::Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0)
}

pub(crate) fn lerp_point(a: kurbo::Point, b: kurbo::Point, t: f64) -> kurbo::Point {
    kurbo::Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

/// Reverse a sequence of BezPath elements.
pub(crate) fn reverse_bez(els: &[kurbo::PathEl]) -> Vec<kurbo::PathEl> {
    // Collect endpoints in reverse, rebuild path.
    let mut points: Vec<kurbo::Point> = Vec::new();
    for el in els {
        match *el {
            kurbo::PathEl::MoveTo(p) | kurbo::PathEl::LineTo(p) => points.push(p),
            kurbo::PathEl::CurveTo(_, _, p) | kurbo::PathEl::QuadTo(_, p) => points.push(p),
            kurbo::PathEl::ClosePath => {}
        }
    }
    points.reverse();
    let mut result = Vec::new();
    for (i, &p) in points.iter().enumerate() {
        if i == 0 {
            result.push(kurbo::PathEl::MoveTo(p));
        } else {
            result.push(kurbo::PathEl::LineTo(p));
        }
    }
    result.push(kurbo::PathEl::ClosePath);
    result
}

/// Apply a named warp envelope to a BezPath.
/// Points are normalized to [0,1] based on bounding box, warped, then scaled back.
pub(crate) fn apply_warp_envelope(
    bez: &kurbo::BezPath,
    warp_type: &str,
    bend: f64,
    dh: f64,
    dv: f64,
) -> kurbo::BezPath {
    // Compute bounding box.
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;

    for el in bez.elements() {
        let pts: Vec<kurbo::Point> = match *el {
            kurbo::PathEl::MoveTo(p) | kurbo::PathEl::LineTo(p) => vec![p],
            kurbo::PathEl::CurveTo(c1, c2, p) => vec![c1, c2, p],
            kurbo::PathEl::QuadTo(c, p) => vec![c, p],
            kurbo::PathEl::ClosePath => vec![],
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

    let warp_point = |p: kurbo::Point| -> kurbo::Point {
        // Normalize to [0,1].
        let nx = (p.x - min_x) / w;
        let ny = (p.y - min_y) / h;

        let (dx, dy) = match warp_type {
            "arc" => {
                // Bend along an arc: vertical displacement follows sin(π*x).
                (
                    dh * (ny - 0.5) * w,
                    bend * (nx * (1.0 - nx) * 4.0) * h * 0.25,
                )
            }
            "bulge" => {
                // Horizontal expansion in the middle.
                let cx = nx - 0.5;
                let cy = ny - 0.5;
                let r = (cx * cx + cy * cy).sqrt().min(0.5);
                let factor = bend * (1.0 - r * 2.0).max(0.0);
                (cx * factor * w, cy * factor * h)
            }
            "wave" => {
                // Sinusoidal wave.
                (
                    dh * (std::f64::consts::PI * 2.0 * ny).sin() * w * 0.1,
                    bend * (std::f64::consts::PI * 2.0 * nx).sin() * h * 0.25,
                )
            }
            "flag" => {
                // Flag wave: amplitude increases with x.
                (
                    0.0,
                    bend * nx * (std::f64::consts::PI * 2.0 * ny).sin() * h * 0.25,
                )
            }
            "squeeze" => {
                // Compress horizontally in the middle, expand at edges.
                let cy = ny - 0.5;
                (
                    bend * cy * cy * (nx - 0.5) * w * -2.0,
                    dv * (nx - 0.5) * h * 0.1,
                )
            }
            "inflate" => {
                // Expand everything from center.
                let cx = nx - 0.5;
                let cy = ny - 0.5;
                let dist = (cx * cx + cy * cy).sqrt();
                let factor = bend * (1.0 - dist * 2.0).max(0.0);
                (cx * factor * w * 0.5, cy * factor * h * 0.5)
            }
            "fisheye" => {
                // Fisheye lens distortion.
                let cx = nx - 0.5;
                let cy = ny - 0.5;
                let r = (cx * cx + cy * cy).sqrt();
                if r < 1e-9 {
                    (0.0, 0.0)
                } else {
                    let factor = bend * r;
                    (cx * factor * w * 0.5, cy * factor * h * 0.5)
                }
            }
            "arc_lower" => {
                // Bend only the bottom edge.
                (0.0, bend * ny * (nx * (1.0 - nx) * 4.0) * h * 0.25)
            }
            "arc_upper" => {
                // Bend only the top edge.
                (0.0, bend * (1.0 - ny) * (nx * (1.0 - nx) * 4.0) * h * 0.25)
            }
            "arch" => {
                // Arch: arc on top, flat on bottom (semicircular arch).
                let arch_amt = (1.0 - ny) * bend * (nx * (1.0 - nx) * 4.0) * h * 0.25;
                (0.0, -arch_amt)
            }
            "shell_lower" => {
                // Shell: curl the bottom inward.
                let t = ny;
                (bend * t * (nx - 0.5) * w * 0.5, bend * t * t * h * 0.2)
            }
            "shell_upper" => {
                // Shell: curl the top inward.
                let t = 1.0 - ny;
                (bend * t * (nx - 0.5) * w * 0.5, -bend * t * t * h * 0.2)
            }
            "fish" => {
                // Fish: pinch horizontally at top and bottom, expand at middle.
                let cy = ny - 0.5;
                let factor = bend * (1.0 - 4.0 * cy * cy);
                (factor * (nx - 0.5) * w * 0.3, 0.0)
            }
            "rise" => {
                // Rise: progressive vertical displacement increasing left to right.
                (0.0, bend * nx * nx * h * 0.3)
            }
            "twist" => {
                // Twist: rotate progressively from bottom to top.
                let angle = bend * (ny - 0.5) * std::f64::consts::PI;
                let cx = nx - 0.5;
                let cos_a = angle.cos();
                let sin_a = angle.sin();
                ((cx * cos_a - cx) * w, (cx * sin_a) * w)
            }
            _ => (0.0, 0.0),
        };

        kurbo::Point::new(p.x + dx, p.y + dy)
    };

    let mut result = kurbo::BezPath::new();
    for el in bez.elements() {
        match *el {
            kurbo::PathEl::MoveTo(p) => result.move_to(warp_point(p)),
            kurbo::PathEl::LineTo(p) => result.line_to(warp_point(p)),
            kurbo::PathEl::CurveTo(c1, c2, p) => {
                result.curve_to(warp_point(c1), warp_point(c2), warp_point(p))
            }
            kurbo::PathEl::QuadTo(c, p) => result.quad_to(warp_point(c), warp_point(p)),
            kurbo::PathEl::ClosePath => result.close_path(),
        }
    }
    result
}

/// Replace each line/curve segment with scallop arcs (smooth inward curves).
pub(crate) fn apply_scallop(bez: &kurbo::BezPath, depth: f64, count: usize) -> kurbo::BezPath {
    let mut result = kurbo::BezPath::new();
    let mut current = kurbo::Point::ZERO;
    let mut subpath_start = kurbo::Point::ZERO;

    for el in bez.elements() {
        match *el {
            kurbo::PathEl::MoveTo(p) => {
                result.move_to(p);
                current = p;
                subpath_start = p;
            }
            kurbo::PathEl::ClosePath => {
                if current != subpath_start {
                    scallop_segment(&mut result, current, subpath_start, depth, count);
                }
                result.close_path();
                current = subpath_start;
            }
            _ => {
                let endpoint = match *el {
                    kurbo::PathEl::LineTo(p)
                    | kurbo::PathEl::CurveTo(_, _, p)
                    | kurbo::PathEl::QuadTo(_, p) => p,
                    _ => unreachable!(),
                };
                let start = {
                    let els = result.elements();
                    let mut pt = kurbo::Point::ZERO;
                    for e in els.iter().rev() {
                        match e {
                            kurbo::PathEl::MoveTo(p)
                            | kurbo::PathEl::LineTo(p)
                            | kurbo::PathEl::CurveTo(_, _, p)
                            | kurbo::PathEl::QuadTo(_, p) => {
                                pt = *p;
                                break;
                            }
                            kurbo::PathEl::ClosePath => {}
                        }
                    }
                    pt
                };
                scallop_segment(&mut result, start, endpoint, depth, count);
                current = endpoint;
            }
        }
    }
    result
}

/// Emit scallop arcs between `from` and `to`.
pub(crate) fn scallop_segment(
    path: &mut kurbo::BezPath,
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

    // Normal points inward (to the right of the direction).
    let nx = dy / len;
    let ny = -dx / len;

    for i in 0..count {
        let t0 = i as f64 / count as f64;
        let t1 = (i + 1) as f64 / count as f64;
        let tmid = (t0 + t1) / 2.0;

        let p0 = kurbo::Point::new(from.x + dx * t0, from.y + dy * t0);
        let p1 = kurbo::Point::new(from.x + dx * t1, from.y + dy * t1);
        let pmid = kurbo::Point::new(
            from.x + dx * tmid + nx * depth,
            from.y + dy * tmid + ny * depth,
        );

        // Quadratic bezier through the midpoint creates a smooth arc.
        // Control point for quadratic that passes through pmid at t=0.5:
        // Q = 2*pmid - 0.5*(p0 + p1)
        let qx = 2.0 * pmid.x - 0.5 * (p0.x + p1.x);
        let qy = 2.0 * pmid.y - 0.5 * (p0.y + p1.y);

        path.quad_to(kurbo::Point::new(qx, qy), p1);
    }
}

/// Add sharp outward spikes along each segment.
pub(crate) fn apply_crystallize(bez: &kurbo::BezPath, size: f64, count: usize) -> kurbo::BezPath {
    let mut result = kurbo::BezPath::new();
    let mut current = kurbo::Point::ZERO;
    let mut subpath_start = kurbo::Point::ZERO;

    for el in bez.elements() {
        match *el {
            kurbo::PathEl::MoveTo(p) => {
                result.move_to(p);
                current = p;
                subpath_start = p;
            }
            kurbo::PathEl::ClosePath => {
                if current != subpath_start {
                    crystallize_segment(&mut result, current, subpath_start, size, count);
                }
                result.close_path();
                current = subpath_start;
            }
            _ => {
                let endpoint = match *el {
                    kurbo::PathEl::LineTo(p)
                    | kurbo::PathEl::CurveTo(_, _, p)
                    | kurbo::PathEl::QuadTo(_, p) => p,
                    _ => unreachable!(),
                };
                let start = {
                    let els = result.elements();
                    let mut pt = kurbo::Point::ZERO;
                    for e in els.iter().rev() {
                        match e {
                            kurbo::PathEl::MoveTo(p)
                            | kurbo::PathEl::LineTo(p)
                            | kurbo::PathEl::CurveTo(_, _, p)
                            | kurbo::PathEl::QuadTo(_, p) => {
                                pt = *p;
                                break;
                            }
                            kurbo::PathEl::ClosePath => {}
                        }
                    }
                    pt
                };
                crystallize_segment(&mut result, start, endpoint, size, count);
                current = endpoint;
            }
        }
    }
    result
}

/// Emit sharp triangular spikes between `from` and `to`.
pub(crate) fn crystallize_segment(
    path: &mut kurbo::BezPath,
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

    // Normal points outward (opposite to scallop).
    let nx = -dy / len;
    let ny = dx / len;

    // Each spike is a triangle: base_start → peak → base_end.
    for i in 0..count {
        let t_peak = (i as f64 + 0.5) / count as f64;
        let t_base_end = (i + 1) as f64 / count as f64;

        // Spike peak displaced outward.
        let peak = kurbo::Point::new(
            from.x + dx * t_peak + nx * size,
            from.y + dy * t_peak + ny * size,
        );
        let base_end = kurbo::Point::new(from.x + dx * t_base_end, from.y + dy * t_base_end);

        path.line_to(peak);
        path.line_to(base_end);
    }
}

pub(crate) fn solid_fill_of(
    fill: &photonic_core::style::Fill,
) -> Option<photonic_core::color::Color> {
    match &fill.kind {
        photonic_core::style::FillKind::Solid(c) => Some(*c),
        _ => None,
    }
}

// ── simplify_path ─────────────────────────────────────────────────────────────

// ── invert_colors ─────────────────────────────────────────────────────────────

// ─── adjust_colors ─────────────────────────────────────────────────────────────

// ── average_anchor_points ───────────────────────────────────────────────────────

// ── outline_stroke ─────────────────────────────────────────────────────────────

// ─── split_into_grid ─────────────────────────────────────────────────────────

// ─── blend_colors ─────────────────────────────────────────────────────────────

// ─── join_paths ───────────────────────────────────────────────────────────────

// ─── pathfinder_crop ─────────────────────────────────────────────────────────

// ─── pathfinder_minus_back ────────────────────────────────────────────────────

// ─── pathfinder_minus_front ───────────────────────────────────────────────────

// ─── pathfinder_trim ──────────────────────────────────────────────────────────

// ─── pathfinder_outline ───────────────────────────────────────────────────────

// ─── pathfinder_divide ────────────────────────────────────────────────────────

// ─── divide_objects_below ─────────────────────────────────────────────────────

// ─── pathfinder_merge ────────────────────────────────────────────────────────

// ─── select_same ─────────────────────────────────────────────────────────────

/// Extract the solid fill color from a node, or None if it has no solid fill.
pub(crate) fn solid_fill_color(node: &SceneNode) -> Option<photonic_core::color::Color> {
    use photonic_core::style::FillKind;
    if let SceneNodeKind::Path(pn) = &node.kind {
        if pn.fill.enabled {
            if let FillKind::Solid(c) = pn.fill.kind {
                return Some(c);
            }
        }
    }
    None
}

/// Euclidean distance between two RGBA colors in [0,1] space.
pub(crate) fn color_distance(
    a: photonic_core::color::Color,
    b: photonic_core::color::Color,
) -> f32 {
    let dr = a.r - b.r;
    let dg = a.g - b.g;
    let db = a.b - b.b;
    let da = a.a - b.a;
    (dr * dr + dg * dg + db * db + da * da).sqrt()
}

/// Returns the horizontal center of a path node's bounding box (local space).
pub(crate) fn path_center_x(node: &SceneNode) -> f32 {
    if let SceneNodeKind::Path(p) = &node.kind {
        if let Some(bb) = p.path_data.bounding_box() {
            return ((bb.x0 + bb.x1) / 2.0) as f32;
        }
    }
    0.0
}

/// Returns the vertical center of a path node's bounding box (local space).
pub(crate) fn path_center_y(node: &SceneNode) -> f32 {
    if let SceneNodeKind::Path(p) = &node.kind {
        if let Some(bb) = p.path_data.bounding_box() {
            return ((bb.y0 + bb.y1) / 2.0) as f32;
        }
    }
    0.0
}

// ─── make_compound_path ───────────────────────────────────────────────────────

// ─── release_compound_path ────────────────────────────────────────────────────

// ─── Guide tools ─────────────────────────────────────────────────────────────

// ─── magic_wand_select ────────────────────────────────────────────────────────

/// Compute the world-space axis-aligned bounding box of a node using its
/// transform and path bounding box (or a text fallback of 1×1 at origin).
pub(crate) fn node_world_aabb(node: &SceneNode) -> Option<(f64, f64, f64, f64)> {
    let (lx0, ly0, lx1, ly1) = match &node.kind {
        SceneNodeKind::Path(pn) => {
            let r = pn.path_data.bounding_box()?;
            (r.x0, r.y0, r.x1, r.y1)
        }
        SceneNodeKind::Text(_) => (0.0, 0.0, 1.0, 1.0),
        SceneNodeKind::Group(_) => (0.0, 0.0, 1.0, 1.0),
        // raster: no path geometry — fallback local AABB
        SceneNodeKind::Raster(_) => (0.0, 0.0, 1.0, 1.0),
    };
    // Transform all four corners of the local AABB and compute the world AABB.
    let fwd = node.transform.to_kurbo();
    let corners = [
        fwd * kurbo::Point::new(lx0, ly0),
        fwd * kurbo::Point::new(lx1, ly0),
        fwd * kurbo::Point::new(lx0, ly1),
        fwd * kurbo::Point::new(lx1, ly1),
    ];
    let wx0 = corners.iter().map(|p| p.x).fold(f64::MAX, f64::min);
    let wy0 = corners.iter().map(|p| p.y).fold(f64::MAX, f64::min);
    let wx1 = corners.iter().map(|p| p.x).fold(f64::MIN, f64::max);
    let wy1 = corners.iter().map(|p| p.y).fold(f64::MIN, f64::max);
    Some((wx0, wy0, wx1, wy1))
}

// ─── convert_anchor_points ────────────────────────────────────────────────────

// ─── lasso_select ─────────────────────────────────────────────────────────────

// ─── select_by_kind ──────────────────────────────────────────────────────────

// ─── create_freehand_path ────────────────────────────────────────────────────

// ─── Isolation Mode ──────────────────────────────────────────────────────────

// ─── select_inside_group ─────────────────────────────────────────────────────

// ─── get_recent_colors ───────────────────────────────────────────────────────

/// Ray-casting point-in-polygon test (Jordan curve theorem).
/// Returns true when `(px, py)` is strictly inside the polygon.
pub(crate) fn point_in_polygon(px: f64, py: f64, poly: &[[f64; 2]]) -> bool {
    let n = poly.len();
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let xi = poly[i][0];
        let yi = poly[i][1];
        let xj = poly[j][0];
        let yj = poly[j][1];
        if ((yi > py) != (yj > py)) && (px < (xj - xi) * (py - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

// ─── smooth_path ─────────────────────────────────────────────────────────────

// ─── noise_deform ─────────────────────────────────────────────────────────────

// ─── mirror_copy ──────────────────────────────────────────────────────────────

// ─── pin_object_guides ────────────────────────────────────────────────────────

// ─── reverse_node_order ───────────────────────────────────────────────────────

// ─── prompt history ───────────────────────────────────────────────────────────

// ─── Select Similar ───────────────────────────────────────────────────────────

// ─── Asset Export ─────────────────────────────────────────────────────────────

// ─── Character Styles ─────────────────────────────────────────────────────────

// ─── Paragraph Styles ─────────────────────────────────────────────────────────

// ─── Clipping Mask ────────────────────────────────────────────────────────────

// ─── Type on a Path ───────────────────────────────────────────────────────────

// ─── Text Direction ────────────────────────────────────────────────────────────

// ─── Area Type ────────────────────────────────────────────────────────────────

// ─── Text Frame Threading ─────────────────────────────────────────────────────

// ─── Text Variable Binding ────────────────────────────────────────────────────

#[cfg(test)]
mod create_shape_color_tests {
    use super::*;
    use crate::server::{AppState, McpServerConfig};
    use photonic_core::style::FillKind;
    use photonic_core::{AuditLog, Document};
    use serde_json::json;
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::Mutex;

    fn test_state() -> AppState {
        let (tx, _rx) = std::sync::mpsc::channel();
        AppState {
            document: Arc::new(Mutex::new(Document::new("t", 100.0, 100.0))),
            history: Arc::new(Mutex::new(photonic_core::history::CommandHistory::new(100))),
            document_path: Arc::new(StdMutex::new(None)),
            capture_tx: Arc::new(StdMutex::new(tx)),
            config: McpServerConfig::default(),
            path_policy: photonic_core::PathPolicy::test_default(),
            audit_log: Arc::new(StdMutex::new(AuditLog::new())),
            clipboard_ring: Arc::new(crate::handlers::clipboard::new_clipboard_ring()),
            video_engine: Arc::new(crate::handlers::video_jobs::VideoEngineHandle::new()),
            video_jobs: Arc::new(StdMutex::new(
                crate::handlers::video_jobs::JobRegistry::new(),
            )),
        }
    }

    async fn only_fill(state: &AppState) -> photonic_core::style::Fill {
        let doc = state.document.lock().await;
        let node = doc
            .nodes
            .values()
            .find(|n| matches!(n.kind, SceneNodeKind::Path(_)))
            .expect("a path node");
        match &node.kind {
            SceneNodeKind::Path(p) => p.fill.clone(),
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn color_shorthand_sets_solid_fill() {
        let state = test_state();
        let args = serde_json::from_value(json!({
            "shape_type": "rectangle", "x": 0.0, "y": 0.0, "width": 10.0, "height": 10.0,
            "color": "#2277ff"
        }))
        .unwrap();
        create_shape(&state, args).await;

        let fill = only_fill(&state).await;
        match fill.kind {
            FillKind::Solid(c) => {
                assert!((c.r - 0.133).abs() < 0.02, "r={}", c.r);
                assert!((c.g - 0.467).abs() < 0.02, "g={}", c.g);
                assert!((c.b - 1.0).abs() < 0.02, "b={}", c.b);
            }
            other => panic!("expected solid fill, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn explicit_fill_wins_over_color() {
        let state = test_state();
        let args = serde_json::from_value(json!({
            "shape_type": "rectangle", "x": 0.0, "y": 0.0, "width": 10.0, "height": 10.0,
            "color": "#2277ff",
            "fill": { "type": "solid", "color": "#ff0000" }
        }))
        .unwrap();
        create_shape(&state, args).await;

        let fill = only_fill(&state).await;
        match fill.kind {
            FillKind::Solid(c) => {
                assert!(
                    c.r > 0.9 && c.g < 0.1 && c.b < 0.1,
                    "expected red, got {c:?}"
                );
            }
            other => panic!("expected solid fill, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn invalid_color_is_an_error() {
        let state = test_state();
        let args = serde_json::from_value(json!({
            "shape_type": "rectangle", "x": 0.0, "y": 0.0, "width": 10.0, "height": 10.0,
            "color": "not-a-color"
        }))
        .unwrap();
        let result = create_shape(&state, args).await;
        assert_eq!(result.is_error, Some(true), "invalid color should error");
    }
}
