use crate::handlers::nodes::{
    apply_crystallize, apply_scallop, apply_warp_envelope, catmull_rom_to_bezier, path_centroid,
    reverse_bez, subdivide_bez,
};
use crate::handlers::shared::{
    paths::{apply_pucker_bloat, apply_roughen, apply_round_corners, apply_twirl, apply_zig_zag},
    styling::apply_style,
};
use crate::protocol::*;
use crate::server::AppState;
use kurbo;
use photonic_core::{
    history::Command,
    node::{GroupNode, NodeId, PathNode, SceneNode, SceneNodeKind},
    path::PathData,
};

pub async fn create_shape(state: &AppState, args: CreateShapeArgs) -> ToolResult {
    tracing::debug!("tool: create_shape {:?}", args.shape_type);
    let path_data = match args.shape_type {
        ShapeType::Rectangle => PathData::rect(args.x, args.y, args.width, args.height),
        ShapeType::RoundedRect => PathData::rounded_rect(
            args.x,
            args.y,
            args.width,
            args.height,
            args.corner_radius.unwrap_or(10.0),
        ),
        ShapeType::Ellipse => {
            let cx = args.x + args.width / 2.0;
            let cy = args.y + args.height / 2.0;
            PathData::ellipse(cx, cy, args.width / 2.0, args.height / 2.0)
        }
        ShapeType::Polygon => {
            let cx = args.x + args.width / 2.0;
            let cy = args.y + args.height / 2.0;
            let r = args.width.min(args.height) / 2.0;
            PathData::regular_polygon(cx, cy, r, args.sides.unwrap_or(6))
        }
        ShapeType::Star => {
            let cx = args.x + args.width / 2.0;
            let cy = args.y + args.height / 2.0;
            let outer = args.width.min(args.height) / 2.0;
            let inner = outer * args.inner_radius.unwrap_or(0.4);
            PathData::star(cx, cy, outer, inner, args.sides.unwrap_or(5))
        }
        ShapeType::Line => {
            PathData::line(args.x, args.y, args.x + args.width, args.y + args.height)
        }
        ShapeType::Arc => {
            let cx = args.x + args.width / 2.0;
            let cy = args.y + args.height / 2.0;
            let rx = args.width.abs() / 2.0;
            let ry = args.height.abs() / 2.0;
            let start = args.arc_start_angle.unwrap_or(0.0);
            let end = args.arc_end_angle.unwrap_or(270.0);
            let open = args.arc_open.unwrap_or(false);
            PathData::arc(cx, cy, rx, ry, start, end, !open)
        }
    };

    let mut path_node = PathNode::new(path_data);
    let has_explicit_fill = args.fill.is_some();
    if let Err(e) = apply_style(&mut path_node, args.fill, args.stroke) {
        return ToolResult::error(e);
    }
    // `color` is a convenience shorthand for a solid fill. An explicit `fill`
    // takes priority; a bad hex string is reported rather than silently ignored.
    if !has_explicit_fill {
        if let Some(hex) = &args.color {
            match photonic_core::color::Color::from_hex(hex) {
                Some(c) => path_node.fill = photonic_core::style::Fill::solid(c),
                None => {
                    return ToolResult::error(format!("Invalid color '{hex}' (expected #rrggbb)"))
                }
            }
        }
    }

    let shape_name = args
        .name
        .unwrap_or_else(|| format!("{:?}", args.shape_type));

    let mut doc = state.document.lock().await;
    let mut node = SceneNode::new(
        &shape_name,
        uuid::Uuid::nil(),
        SceneNodeKind::Path(path_node),
    );
    if !args.tags.is_empty() {
        node.tags = args.tags;
    }

    let node_id = node.id;
    let cmd = Command::AddNode {
        node,
        layer_id: args.layer_id,
    };
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd.clone(), &mut doc);

    ToolResult::text(format!(
        "Created {} '{}' (id: {})",
        format!("{:?}", args.shape_type).to_lowercase(),
        shape_name,
        node_id
    ))
    .with_data(serde_json::json!({ "node_id": node_id }))
}
pub async fn create_path(state: &AppState, args: CreatePathArgs) -> ToolResult {
    tracing::debug!("tool: create_path (data len={})", args.path_data.len());
    let path_data = match PathData::from_svg(&args.path_data) {
        Ok(p) => p,
        Err(e) => return ToolResult::error(format!("Invalid SVG path data: {}", e)),
    };
    if !path_data.has_drawable_geometry() {
        return ToolResult::error(
            "Invalid SVG path data: path contains no drawable segments".to_string(),
        );
    }

    let mut path_node = PathNode::new(path_data);
    if let Err(e) = apply_style(&mut path_node, args.fill, args.stroke) {
        return ToolResult::error(e);
    }

    let name = args.name.unwrap_or_else(|| "Path".to_string());
    let mut doc = state.document.lock().await;
    let mut node = SceneNode::new(&name, uuid::Uuid::nil(), SceneNodeKind::Path(path_node));

    if let Some(t_arg) = args.transform {
        node.transform = t_arg.to_transform();
    }
    if !args.tags.is_empty() {
        node.tags = args.tags;
    }

    let node_id = node.id;
    let cmd = Command::AddNode {
        node,
        layer_id: args.layer_id,
    };
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!("Created path '{}' (id: {})", name, node_id))
        .with_data(serde_json::json!({ "node_id": node_id }))
}
pub async fn create_curvature_path(state: &AppState, args: CreateCurvaturePathArgs) -> ToolResult {
    tracing::debug!("tool: create_curvature_path (points={})", args.points.len());

    if args.points.len() < 2 {
        return ToolResult::error("At least 2 points are required");
    }

    // Build a smooth cubic bezier path through the points using Catmull-Rom interpolation.
    let pts: Vec<kurbo::Point> = args
        .points
        .iter()
        .map(|p| kurbo::Point::new(p[0], p[1]))
        .collect();
    let bez = catmull_rom_to_bezier(&pts, args.closed);

    let path_data = PathData::from_bez_path(&bez);
    let mut path_node = PathNode::new(path_data);
    if let Err(e) = apply_style(&mut path_node, args.fill, args.stroke) {
        return ToolResult::error(e);
    }

    let layer_id = args.layer_id.and_then(|s| uuid::Uuid::parse_str(&s).ok());

    let name = "Curvature Path".to_string();
    let mut doc = state.document.lock().await;
    let node = SceneNode::new(&name, uuid::Uuid::nil(), SceneNodeKind::Path(path_node));
    let node_id = node.id;
    let cmd = Command::AddNode { node, layer_id };
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!(
        "Created smooth curve through {} points (id: {})",
        pts.len(),
        node_id
    ))
    .with_data(serde_json::json!({ "node_id": node_id, "point_count": pts.len() }))
}
pub async fn create_flare(state: &AppState, args: CreateFlareArgs) -> ToolResult {
    tracing::debug!("tool: create_flare");
    use kurbo::Shape;
    use photonic_core::color::Color;
    use photonic_core::style::{Fill, FillKind, Stroke};

    let cx = args.cx;
    let cy = args.cy;
    let halo_r = args.halo_radius.unwrap_or(50.0);
    let ray_count = args.ray_count.unwrap_or(12).max(2);
    let ray_len = args.ray_length.unwrap_or(80.0);
    let ring_count = args.ring_count.unwrap_or(3);
    let ray_opacity = args.ray_opacity.unwrap_or(0.3);

    let generated_nodes = match 2usize
        .checked_add(ray_count)
        .and_then(|count| count.checked_add(ring_count))
    {
        Some(count) => count,
        None => return ToolResult::error("Lens flare generated-node count overflow"),
    };
    if generated_nodes > MAX_GENERATED_WORK {
        return ToolResult::error(format!(
            "create_flare may generate at most {MAX_GENERATED_WORK} nodes, including the halo and group"
        ));
    }

    let halo_color = args.halo_color.as_deref().unwrap_or("#fffbe6");
    let halo_c = Color::from_hex(halo_color).unwrap_or(Color::new(1.0, 0.98, 0.9, 0.6));

    let layer_id = args.layer_id.and_then(|s| uuid::Uuid::parse_str(&s).ok());

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let actual_layer = layer_id
        .or(doc.active_layer_id)
        .unwrap_or(uuid::Uuid::nil());
    let mut child_ids = Vec::with_capacity(generated_nodes - 1);

    // 1. Create halo circle (semi-transparent filled ellipse).
    {
        let path = kurbo::Ellipse::new((cx, cy), (halo_r, halo_r), 0.0).to_path(0.1);
        let mut pn = PathNode::new(PathData::from_bez_path(&path));
        pn.fill = Fill {
            kind: FillKind::Solid(Color::new(halo_c.r, halo_c.g, halo_c.b, 0.6)),
            ..Default::default()
        };
        pn.stroke = Stroke::none();
        let node = SceneNode::new("Flare Halo", actual_layer, SceneNodeKind::Path(pn));
        let nid = node.id;
        child_ids.push(nid);
        history.execute_discrete(
            Command::AddNode {
                node,
                layer_id: Some(actual_layer),
            },
            &mut doc,
        );
    }

    // 2. Create radiating rays (thin triangles).
    for i in 0..ray_count {
        let angle = std::f64::consts::TAU * i as f64 / ray_count as f64;
        let half_width = std::f64::consts::TAU / ray_count as f64 * 0.15; // thin ray

        let tip_x = cx + (halo_r + ray_len) * angle.cos();
        let tip_y = cy + (halo_r + ray_len) * angle.sin();
        let base_l_x = cx + halo_r * 0.8 * (angle - half_width).cos();
        let base_l_y = cy + halo_r * 0.8 * (angle - half_width).sin();
        let base_r_x = cx + halo_r * 0.8 * (angle + half_width).cos();
        let base_r_y = cy + halo_r * 0.8 * (angle + half_width).sin();

        let mut bez = kurbo::BezPath::new();
        bez.move_to((base_l_x, base_l_y));
        bez.line_to((tip_x, tip_y));
        bez.line_to((base_r_x, base_r_y));
        bez.close_path();

        let mut pn = PathNode::new(PathData::from_bez_path(&bez));
        pn.fill = Fill {
            kind: FillKind::Solid(Color::new(halo_c.r, halo_c.g, halo_c.b, ray_opacity)),
            ..Default::default()
        };
        pn.stroke = Stroke::none();
        let mut node = SceneNode::new(
            &format!("Flare Ray {}", i + 1),
            actual_layer,
            SceneNodeKind::Path(pn),
        );
        node.opacity = ray_opacity;
        let nid = node.id;
        child_ids.push(nid);
        history.execute_discrete(
            Command::AddNode {
                node,
                layer_id: Some(actual_layer),
            },
            &mut doc,
        );
    }

    // 3. Create concentric rings.
    for i in 0..ring_count {
        let ring_r = halo_r * (1.5 + i as f64 * 0.8);
        let ring_opacity = 0.15 / (i as f32 + 1.0);
        let path = kurbo::Ellipse::new((cx, cy), (ring_r, ring_r), 0.0).to_path(0.1);
        let mut pn = PathNode::new(PathData::from_bez_path(&path));
        pn.fill = Fill::none();
        pn.stroke = Stroke {
            color: Color::new(halo_c.r, halo_c.g, halo_c.b, ring_opacity),
            width: 1.5,
            ..Default::default()
        };
        let node = SceneNode::new(
            &format!("Flare Ring {}", i + 1),
            actual_layer,
            SceneNodeKind::Path(pn),
        );
        let nid = node.id;
        child_ids.push(nid);
        history.execute_discrete(
            Command::AddNode {
                node,
                layer_id: Some(actual_layer),
            },
            &mut doc,
        );
    }

    // 4. Group all flare parts.
    let group = SceneNode::new(
        "Lens Flare",
        actual_layer,
        SceneNodeKind::Group(photonic_core::node::GroupNode::new()),
    );
    let group_id = group.id;
    history.execute_discrete(
        Command::GroupNodes {
            group,
            layer_id: actual_layer,
            insert_index: 0,
            children: child_ids.clone(),
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Created lens flare at ({cx}, {cy}) — {} rays, {} rings, halo r={halo_r}",
        ray_count, ring_count
    ))
    .with_data(serde_json::json!({
        "group_id": group_id,
        "child_count": child_ids.len(),
    }))
}
pub async fn create_spiral(state: &AppState, args: CreateSpiralArgs) -> ToolResult {
    tracing::debug!("tool: create_spiral turns={}", args.turns);

    if args.outer_radius <= 0.0 {
        return ToolResult::error("outer_radius must be greater than 0");
    }
    if !args.turns.is_finite() || args.turns <= 0.0 {
        return ToolResult::error("turns must be a finite number greater than 0");
    }

    // PathData::spiral applies these same minimums before deriving its loop
    // count. Check the resulting rounded segment count before path creation.
    let effective_turns = args.turns.max(0.01);
    let effective_segments_per_turn = args.segments_per_turn.max(4);
    let generated_segments = (effective_turns * effective_segments_per_turn as f64).round();
    if !generated_segments.is_finite() || generated_segments > MAX_GENERATED_WORK as f64 {
        return ToolResult::error(format!(
            "create_spiral may generate at most {MAX_GENERATED_WORK} Bézier segments"
        ));
    }

    let path_data = PathData::spiral(
        args.x,
        args.y,
        args.outer_radius,
        args.inner_radius,
        args.turns,
        args.segments_per_turn,
    );

    let mut path_node = PathNode::new(path_data);
    if let Err(e) = apply_style(&mut path_node, args.fill, args.stroke) {
        return ToolResult::error(e);
    }

    let name = args.name.unwrap_or_else(|| "Spiral".to_string());
    let mut doc = state.document.lock().await;
    let node = SceneNode::new(&name, uuid::Uuid::nil(), SceneNodeKind::Path(path_node));
    let node_id = node.id;
    let cmd = Command::AddNode {
        node,
        layer_id: args.layer_id,
    };
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!(
        "Created spiral '{}' ({} turns, outer_r={}, inner_r={}) id: {}",
        name, args.turns, args.outer_radius, args.inner_radius, node_id
    ))
    .with_data(serde_json::json!({ "node_id": node_id }))
}
pub async fn create_polar_grid(state: &AppState, args: CreatePolarGridArgs) -> ToolResult {
    if args.outer_radius <= 0.0 {
        return ToolResult::error("outer_radius must be greater than 0");
    }
    let inner_r = args.inner_radius.unwrap_or(0.0).max(0.0);
    let rings = args.rings.unwrap_or(4).max(1);
    let sectors = args.sectors.unwrap_or(8).max(1);

    let path_data =
        PathData::polar_grid(args.x, args.y, args.outer_radius, inner_r, rings, sectors);

    let mut path_node = PathNode::new(path_data);
    if let Err(e) = apply_style(&mut path_node, args.fill, args.stroke) {
        return ToolResult::error(e);
    }

    let name = args
        .name
        .unwrap_or_else(|| format!("Polar Grid {}r {}s", rings, sectors));
    let mut doc = state.document.lock().await;
    let node = SceneNode::new(&name, uuid::Uuid::nil(), SceneNodeKind::Path(path_node));
    let node_id = node.id;
    let cmd = Command::AddNode {
        node,
        layer_id: args.layer_id,
    };
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!(
        "Created polar grid '{}' ({} rings, {} sectors, outer_r={}) id: {}",
        name, rings, sectors, args.outer_radius, node_id
    ))
    .with_data(serde_json::json!({ "node_id": node_id, "rings": rings, "sectors": sectors }))
}
pub async fn create_grid(state: &AppState, args: CreateGridArgs) -> ToolResult {
    if args.width <= 0.0 || args.height <= 0.0 {
        return ToolResult::error("width and height must be greater than 0");
    }
    let cols = args.cols.unwrap_or(4).max(1);
    let rows = args.rows.unwrap_or(4).max(1);

    let path_data = PathData::grid(args.x, args.y, args.width, args.height, cols, rows);

    let mut path_node = PathNode::new(path_data);
    if let Err(e) = apply_style(&mut path_node, args.fill, args.stroke) {
        return ToolResult::error(e);
    }

    let name = args
        .name
        .unwrap_or_else(|| format!("Grid {}×{}", cols, rows));
    let mut doc = state.document.lock().await;
    let node = SceneNode::new(&name, uuid::Uuid::nil(), SceneNodeKind::Path(path_node));
    let node_id = node.id;
    let cmd = Command::AddNode {
        node,
        layer_id: args.layer_id,
    };
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!(
        "Created grid '{}' ({}×{} cells, {}×{} size) id: {}",
        name, cols, rows, args.width, args.height, node_id
    ))
    .with_data(serde_json::json!({ "node_id": node_id, "cols": cols, "rows": rows }))
}
pub async fn build_shape_from_points(
    state: &AppState,
    args: BuildShapeFromPointsArgs,
) -> ToolResult {
    if args.points.len() < 2 {
        return ToolResult::error("At least 2 points are required");
    }

    // Build the ordered index sequence to traverse
    let order: Vec<usize> = match &args.connection_order {
        Some(o) => o.clone(),
        None => (0..args.points.len()).collect(),
    };

    if order.is_empty() {
        return ToolResult::error("connection_order must contain at least one index");
    }

    // Validate all indices
    for &idx in &order {
        if idx >= args.points.len() {
            return ToolResult::error(format!(
                "connection_order index {} is out of bounds (have {} points)",
                idx,
                args.points.len()
            ));
        }
    }

    // Build SVG path string from ordered points
    let first = args.points[order[0]];
    let mut svg = format!("M {} {}", first[0], first[1]);
    for &idx in order.iter().skip(1) {
        let p = args.points[idx];
        svg.push_str(&format!(" L {} {}", p[0], p[1]));
    }
    if args.closed {
        svg.push_str(" Z");
    }

    let path_data = match PathData::from_svg(&svg) {
        Ok(p) => p,
        Err(e) => return ToolResult::error(format!("Failed to build path: {}", e)),
    };

    let mut path_node = PathNode::new(path_data);
    if let Err(e) = apply_style(&mut path_node, args.fill, args.stroke) {
        return ToolResult::error(e);
    }

    let name = args.name.unwrap_or_else(|| "Custom Shape".to_string());
    let mut doc = state.document.lock().await;
    let mut node = SceneNode::new(&name, uuid::Uuid::nil(), SceneNodeKind::Path(path_node));
    if !args.tags.is_empty() {
        node.tags = args.tags;
    }

    let node_id = node.id;
    let cmd = Command::AddNode {
        node,
        layer_id: args.layer_id,
    };
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!(
        "Created '{}' from {} points (id: {})",
        name,
        args.points.len(),
        node_id
    ))
    .with_data(serde_json::json!({
        "node_id": node_id,
    }))
}
/// Insert a new anchor point at the midpoint of every path segment for each
/// supplied node. Non-path nodes are silently skipped.
pub async fn add_anchor_points(state: &AppState, args: AddAnchorPointsArgs) -> ToolResult {
    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let passes = args.passes.unwrap_or(1).min(8).max(1);

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let mut commands = Vec::new();
    let mut modified = 0usize;
    let mut skipped = 0usize;

    for node_id in &args.node_ids {
        let node = match doc.nodes.get(node_id) {
            Some(n) => n.clone(),
            None => {
                skipped += 1;
                continue;
            }
        };

        match &node.kind {
            SceneNodeKind::Path(pn) => {
                let new_path = pn.path_data.subdivide(passes);
                let mut new_node = node.clone();
                if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                    new_pn.path_data = new_path;
                }
                commands.push(Command::UpdateNode {
                    old: node,
                    new: new_node,
                });
                modified += 1;
            }
            _ => {
                skipped += 1;
            }
        }
    }

    if commands.is_empty() {
        return ToolResult::error("No path nodes found in node_ids");
    }

    for cmd in commands {
        history.execute_discrete(cmd, &mut doc);
    }

    let summary = format!(
        "Added anchor points to {} node(s) ({} pass{}){}",
        modified,
        passes,
        if passes == 1 { "" } else { "es" },
        if skipped > 0 {
            format!(" — {} non-path node(s) skipped", skipped)
        } else {
            String::new()
        },
    );
    ToolResult::text(summary).with_data(serde_json::json!({
        "modified": modified,
        "skipped":  skipped,
        "passes":   passes,
    }))
}
pub async fn delete_anchor_point(state: &AppState, args: DeleteAnchorPointArgs) -> ToolResult {
    tracing::debug!("tool: delete_anchor_point");

    if args.anchor_indices.is_empty() {
        return ToolResult::error("anchor_indices must not be empty");
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    // Resolve node by UUID or name.
    let nid = if let Ok(uuid) = uuid::Uuid::parse_str(&args.node_id) {
        uuid
    } else {
        match doc.find_node_by_name(&args.node_id) {
            Some(n) => n.id,
            None => return ToolResult::error(format!("Node not found: {}", args.node_id)),
        }
    };

    let node = match doc.nodes.get(&nid) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Node not found: {}", args.node_id)),
    };

    let pn = match &node.kind {
        SceneNodeKind::Path(pn) => pn,
        _ => return ToolResult::error("Node is not a path"),
    };

    let bez = pn.path_data.to_bez_path();
    let el_count = bez.elements().len();

    // Validate indices.
    for &idx in &args.anchor_indices {
        if idx >= el_count {
            return ToolResult::error(format!(
                "Anchor index {idx} out of range (path has {el_count} elements)"
            ));
        }
    }

    // Remove elements (same algorithm as GUI's bez_remove_elements).
    let remove_set: std::collections::HashSet<usize> =
        args.anchor_indices.iter().copied().collect();
    let mut result = kurbo::BezPath::new();
    let mut needs_move = true;
    for (i, el) in bez.elements().iter().enumerate() {
        if remove_set.contains(&i) {
            needs_move = true;
            continue;
        }
        if needs_move {
            let endpoint = match el {
                kurbo::PathEl::MoveTo(p) | kurbo::PathEl::LineTo(p) => Some(*p),
                kurbo::PathEl::CurveTo(_, _, p) => Some(*p),
                kurbo::PathEl::QuadTo(_, p) => Some(*p),
                kurbo::PathEl::ClosePath => None,
            };
            if let Some(p) = endpoint {
                result.push(kurbo::PathEl::MoveTo(p));
                needs_move = false;
                if !matches!(el, kurbo::PathEl::MoveTo(_)) {
                    result.push(*el);
                }
            }
        } else {
            result.push(*el);
        }
    }

    let mut new_node = node.clone();
    if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
        new_pn.path_data = PathData::from_bez_path(&result);
    }

    let removed_count = remove_set.len();
    let new_count = result.elements().len();
    history.execute_discrete(
        Command::UpdateNode {
            old: node,
            new: new_node,
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Deleted {removed_count} anchor(s) — {el_count} → {new_count} elements"
    ))
    .with_data(serde_json::json!({
        "removed": removed_count,
        "elements_before": el_count,
        "elements_after": new_count,
    }))
}
pub async fn zig_zag_path(state: &AppState, args: ZigZagPathArgs) -> ToolResult {
    tracing::debug!("tool: zig_zag_path");

    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let size = args.size.unwrap_or(10.0);
    let ridges = args.ridges_per_segment.unwrap_or(4).max(1);
    let smooth = args.smooth;

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let mut commands = Vec::new();
    let mut modified = 0usize;
    let mut skipped = 0usize;

    for node_id_str in &args.node_ids {
        let nid = match uuid::Uuid::parse_str(node_id_str) {
            Ok(id) => id,
            Err(_) => match doc.find_node_by_name(node_id_str) {
                Some(n) => n.id,
                None => {
                    skipped += 1;
                    continue;
                }
            },
        };
        let node = match doc.nodes.get(&nid) {
            Some(n) => n.clone(),
            None => {
                skipped += 1;
                continue;
            }
        };
        let pn = match &node.kind {
            SceneNodeKind::Path(pn) => pn,
            _ => {
                skipped += 1;
                continue;
            }
        };

        let bez = pn.path_data.to_bez_path();
        let new_bez = apply_zig_zag(&bez, size, ridges, smooth);

        let mut new_node = node.clone();
        if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
            new_pn.path_data = PathData::from_bez_path(&new_bez);
        }
        commands.push(Command::UpdateNode {
            old: node,
            new: new_node,
        });
        modified += 1;
    }

    if commands.is_empty() {
        return ToolResult::error("No path nodes found in node_ids");
    }

    for cmd in commands {
        history.execute_discrete(cmd, &mut doc);
    }

    ToolResult::text(format!(
        "Applied zig-zag to {} node(s) (size={size}, ridges={ridges}, smooth={smooth}){}",
        modified,
        if skipped > 0 {
            format!(" — {} skipped", skipped)
        } else {
            String::new()
        },
    ))
    .with_data(serde_json::json!({ "modified": modified, "skipped": skipped }))
}
pub async fn pucker_bloat(state: &AppState, args: PuckerBloatArgs) -> ToolResult {
    tracing::debug!("tool: pucker_bloat");

    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let strength = args.strength.unwrap_or(0.5);

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let mut commands = Vec::new();
    let mut modified = 0usize;
    let mut skipped = 0usize;

    for node_id_str in &args.node_ids {
        let nid = match uuid::Uuid::parse_str(node_id_str) {
            Ok(id) => id,
            Err(_) => match doc.find_node_by_name(node_id_str) {
                Some(n) => n.id,
                None => {
                    skipped += 1;
                    continue;
                }
            },
        };
        let node = match doc.nodes.get(&nid) {
            Some(n) => n.clone(),
            None => {
                skipped += 1;
                continue;
            }
        };
        let pn = match &node.kind {
            SceneNodeKind::Path(pn) => pn,
            _ => {
                skipped += 1;
                continue;
            }
        };

        let bez = pn.path_data.to_bez_path();

        // Determine center — use args or compute centroid.
        let center = if let (Some(cx), Some(cy)) = (args.center_x, args.center_y) {
            kurbo::Point::new(cx, cy)
        } else {
            path_centroid(&bez)
        };

        let new_bez = apply_pucker_bloat(&bez, strength, center);

        let mut new_node = node.clone();
        if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
            new_pn.path_data = PathData::from_bez_path(&new_bez);
        }
        commands.push(Command::UpdateNode {
            old: node,
            new: new_node,
        });
        modified += 1;
    }

    if commands.is_empty() {
        return ToolResult::error("No path nodes found in node_ids");
    }

    for cmd in commands {
        history.execute_discrete(cmd, &mut doc);
    }

    let label = if strength >= 0.0 { "bloat" } else { "pucker" };
    ToolResult::text(format!(
        "Applied {label} (strength={strength}) to {modified} node(s){}",
        if skipped > 0 {
            format!(" — {skipped} skipped")
        } else {
            String::new()
        },
    ))
    .with_data(serde_json::json!({ "modified": modified, "skipped": skipped }))
}
pub async fn roughen_path(state: &AppState, args: RoughenPathArgs) -> ToolResult {
    tracing::debug!("tool: roughen_path");

    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let size = args.size.unwrap_or(5.0);
    let detail = args.detail.unwrap_or(0);
    let seed = args.seed.unwrap_or(42);

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let mut commands = Vec::new();
    let mut modified = 0usize;
    let mut skipped = 0usize;

    for node_id_str in &args.node_ids {
        let nid = match uuid::Uuid::parse_str(node_id_str) {
            Ok(id) => id,
            Err(_) => match doc.find_node_by_name(node_id_str) {
                Some(n) => n.id,
                None => {
                    skipped += 1;
                    continue;
                }
            },
        };
        let node = match doc.nodes.get(&nid) {
            Some(n) => n.clone(),
            None => {
                skipped += 1;
                continue;
            }
        };
        let pn = match &node.kind {
            SceneNodeKind::Path(pn) => pn,
            _ => {
                skipped += 1;
                continue;
            }
        };

        let mut bez = pn.path_data.to_bez_path();

        // Subdivide for extra detail before roughening.
        for _ in 0..detail {
            bez = subdivide_bez(&bez);
        }

        let new_bez = apply_roughen(&bez, size, seed + modified as u64);

        let mut new_node = node.clone();
        if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
            new_pn.path_data = PathData::from_bez_path(&new_bez);
        }
        commands.push(Command::UpdateNode {
            old: node,
            new: new_node,
        });
        modified += 1;
    }

    if commands.is_empty() {
        return ToolResult::error("No path nodes found in node_ids");
    }

    for cmd in commands {
        history.execute_discrete(cmd, &mut doc);
    }

    ToolResult::text(format!(
        "Roughened {} node(s) (size={size}, detail={detail}){}",
        modified,
        if skipped > 0 {
            format!(" — {skipped} skipped")
        } else {
            String::new()
        },
    ))
    .with_data(serde_json::json!({ "modified": modified, "skipped": skipped }))
}
pub async fn twirl_path(state: &AppState, args: TwirlPathArgs) -> ToolResult {
    tracing::debug!("tool: twirl_path");

    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let angle_deg = args.angle.unwrap_or(90.0);
    let angle_rad = angle_deg.to_radians();

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let mut commands = Vec::new();
    let mut modified = 0usize;
    let mut skipped = 0usize;

    for node_id_str in &args.node_ids {
        let nid = match uuid::Uuid::parse_str(node_id_str) {
            Ok(id) => id,
            Err(_) => match doc.find_node_by_name(node_id_str) {
                Some(n) => n.id,
                None => {
                    skipped += 1;
                    continue;
                }
            },
        };
        let node = match doc.nodes.get(&nid) {
            Some(n) => n.clone(),
            None => {
                skipped += 1;
                continue;
            }
        };
        let pn = match &node.kind {
            SceneNodeKind::Path(pn) => pn,
            _ => {
                skipped += 1;
                continue;
            }
        };

        let bez = pn.path_data.to_bez_path();
        let center = if let (Some(cx), Some(cy)) = (args.center_x, args.center_y) {
            kurbo::Point::new(cx, cy)
        } else {
            path_centroid(&bez)
        };

        let new_bez = apply_twirl(&bez, angle_rad, center);

        let mut new_node = node.clone();
        if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
            new_pn.path_data = PathData::from_bez_path(&new_bez);
        }
        commands.push(Command::UpdateNode {
            old: node,
            new: new_node,
        });
        modified += 1;
    }

    if commands.is_empty() {
        return ToolResult::error("No path nodes found in node_ids");
    }

    for cmd in commands {
        history.execute_discrete(cmd, &mut doc);
    }

    ToolResult::text(format!(
        "Twirled {} node(s) by {angle_deg}°{}",
        modified,
        if skipped > 0 {
            format!(" — {skipped} skipped")
        } else {
            String::new()
        },
    ))
    .with_data(serde_json::json!({ "modified": modified, "skipped": skipped }))
}

pub async fn proportional_move_anchor(
    state: &AppState,
    args: ProportionalMoveAnchorArgs,
) -> ToolResult {
    tracing::debug!("tool: proportional_move_anchor");
    use photonic_core::ops::proportional;

    if args.anchor_indices.is_empty() {
        return ToolResult::error("anchor_indices must not be empty");
    }
    let spread = args.spread.unwrap_or(proportional::DEFAULT_SPREAD);
    let curve = args.curve.unwrap_or(proportional::DEFAULT_CURVE);

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    // Resolve the node id (UUID or name).
    let nid = match uuid::Uuid::parse_str(&args.node_id) {
        Ok(id) => id,
        Err(_) => match doc.find_node_by_name(&args.node_id) {
            Some(n) => n.id,
            None => return ToolResult::error(format!("Node not found: {}", args.node_id)),
        },
    };
    let node = match doc.nodes.get(&nid) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Node not found: {}", args.node_id)),
    };
    let pn = match &node.kind {
        SceneNodeKind::Path(pn) => pn,
        _ => return ToolResult::error("Node is not a path"),
    };

    // Validate the primary indices against the path's actual anchor indices.
    let bez = pn.path_data.to_bez_path();
    let valid: Vec<usize> = proportional::anchor_points(&bez)
        .into_iter()
        .map(|(i, _)| i)
        .collect();
    let invalid: Vec<usize> = args
        .anchor_indices
        .iter()
        .copied()
        .filter(|i| !valid.contains(i))
        .collect();
    if !invalid.is_empty() {
        return ToolResult::error(format!(
            "Invalid anchor_indices {invalid:?}; this path's anchor element indices are {valid:?}"
        ));
    }

    // How many anchors will actually move (weight > 0), for the report.
    let affected = proportional::compute_weights(
        &bez,
        &args.anchor_indices,
        spread,
        curve,
        proportional::DistanceMetric::Euclidean,
    )
    .len();

    let new_path = proportional::proportional_move(
        &pn.path_data,
        &args.anchor_indices,
        args.dx,
        args.dy,
        spread,
        curve,
        proportional::DistanceMetric::Euclidean,
    );

    let mut new_node = node.clone();
    if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
        new_pn.path_data = new_path;
    }
    history.execute_discrete(
        Command::UpdateNode {
            old: node,
            new: new_node,
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Proportionally moved {} primary anchor(s) by ({}, {}); {affected} anchor(s) affected within spread {spread} (curve {curve})",
        args.anchor_indices.len(),
        args.dx,
        args.dy,
    ))
    .with_data(serde_json::json!({
        "node_id": nid.to_string(),
        "primary": args.anchor_indices,
        "affected": affected,
        "spread": spread,
        "curve": curve,
    }))
}

pub async fn create_parametric_shape(
    state: &AppState,
    args: CreateParametricShapeArgs,
) -> ToolResult {
    tracing::debug!("tool: create_parametric_shape");

    let cx = args.cx;
    let cy = args.cy;
    let radius = args.radius.unwrap_or(80.0);
    let n_pts = args.points.unwrap_or(360).max(3).min(4096);
    let rx = radius * args.ratio_x.unwrap_or(1.0);
    let ry = radius * args.ratio_y.unwrap_or(1.0);

    // Generate (x, y) sample points in object space.
    let pts: Vec<(f64, f64)> = match args.shape_type {
        ParametricShapeType::Lissajous => {
            let freq_a = args.freq_a.unwrap_or(3.0);
            let freq_b = args.freq_b.unwrap_or(2.0);
            let delta = args.delta_deg.unwrap_or(90.0).to_radians();
            (0..n_pts)
                .map(|i| {
                    let t = i as f64 / n_pts as f64 * std::f64::consts::TAU;
                    (rx * (freq_a * t + delta).sin(), ry * (freq_b * t).sin())
                })
                .collect()
        }
        ParametricShapeType::Superellipse => {
            let n = args.exponent.unwrap_or(2.5).max(0.1);
            (0..n_pts)
                .map(|i| {
                    let t = i as f64 / n_pts as f64 * std::f64::consts::TAU;
                    let cos_t = t.cos();
                    let sin_t = t.sin();
                    // |x/rx|^n + |y/ry|^n = 1 → parameterized as x = rx·sgn(cos)·|cos|^(2/n)
                    let x = rx * cos_t.signum() * cos_t.abs().powf(2.0 / n);
                    let y = ry * sin_t.signum() * sin_t.abs().powf(2.0 / n);
                    (x, y)
                })
                .collect()
        }
        ParametricShapeType::Rose => {
            let k = args.petals.unwrap_or(5.0);
            (0..n_pts)
                .map(|i| {
                    // Integrate over 2π (even k) or π (odd k) for a closed rose.
                    let t_max = if (k.round() as i64 % 2 == 0) && k.fract() < 1e-9 {
                        std::f64::consts::TAU
                    } else {
                        std::f64::consts::PI
                    };
                    let t = i as f64 / n_pts as f64 * t_max;
                    let r = radius * (k * t).cos();
                    (r * t.cos(), r * t.sin())
                })
                .collect()
        }
        ParametricShapeType::Hypotrochoid => {
            let r_ratio = args.inner_ratio.unwrap_or(0.4).clamp(0.01, 0.99);
            let pen_r = args.pen_ratio.unwrap_or(1.0);
            let big_r = radius / (1.0 + r_ratio * (pen_r - 1.0).abs().max(1.0) + r_ratio);
            let r = big_r * r_ratio;
            let d = r * pen_r;
            (0..n_pts)
                .map(|i| {
                    let t =
                        i as f64 / n_pts as f64 * std::f64::consts::TAU * r_ratio.recip().ceil();
                    let x = (big_r - r) * t.cos() + d * ((big_r - r) / r * t).cos();
                    let y = (big_r - r) * t.sin() - d * ((big_r - r) / r * t).sin();
                    (x, y)
                })
                .collect()
        }
        ParametricShapeType::Epicycloid => {
            let r_ratio = args.inner_ratio.unwrap_or(0.3).clamp(0.01, 0.99);
            let big_r = radius / (1.0 + r_ratio);
            let r = big_r * r_ratio;
            let d = r * args.pen_ratio.unwrap_or(1.0);
            let loops = (1.0 / r_ratio).round().max(1.0) as usize;
            (0..n_pts)
                .map(|i| {
                    let t = i as f64 / n_pts as f64 * std::f64::consts::TAU * loops as f64;
                    let x = (big_r + r) * t.cos() - d * ((big_r + r) / r * t).cos();
                    let y = (big_r + r) * t.sin() - d * ((big_r + r) / r * t).sin();
                    (x, y)
                })
                .collect()
        }
    };

    if pts.is_empty() {
        return ToolResult::error("no points generated");
    }

    // Build BezPath from sample points (polyline, closed).
    let mut bez = kurbo::BezPath::new();
    for (i, (px, py)) in pts.iter().enumerate() {
        let pt = kurbo::Point::new(cx + px, cy + py);
        if i == 0 {
            bez.move_to(pt);
        } else {
            bez.line_to(pt);
        }
    }
    bez.close_path();

    let mut pn = PathNode::new(PathData::from_bez_path(&bez));
    if let Err(e) = apply_style(&mut pn, args.fill, args.stroke) {
        return ToolResult::error(e);
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let layer_id = args
        .layer_id
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .or(doc.active_layer_id)
        .unwrap_or(uuid::Uuid::nil());

    let shape_name = match args.shape_type {
        ParametricShapeType::Lissajous => "Lissajous Curve",
        ParametricShapeType::Superellipse => "Superellipse",
        ParametricShapeType::Rose => "Rose Curve",
        ParametricShapeType::Hypotrochoid => "Hypotrochoid",
        ParametricShapeType::Epicycloid => "Epicycloid",
    };
    let node = SceneNode::new(shape_name, layer_id, SceneNodeKind::Path(pn));
    let node_id = node.id;
    history.execute_discrete(
        Command::AddNode {
            node,
            layer_id: Some(layer_id),
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Created {shape_name} at ({cx},{cy}) with {n_pts} points"
    ))
    .with_data(serde_json::json!({ "node_id": node_id }))
}
pub async fn create_truchet_tiling(state: &AppState, args: CreateTruchetTilingArgs) -> ToolResult {
    tracing::debug!("tool: create_truchet_tiling");

    let x = args.x;
    let y = args.y;
    let width = args.width.unwrap_or(200.0).max(4.0);
    let height = args.height.unwrap_or(200.0).max(4.0);
    let ts = args.tile_size.unwrap_or(40.0).clamp(4.0, 400.0);
    let seed = args.seed.unwrap_or(42);
    let style = args.style.as_deref().unwrap_or("arcs");
    let sw = args.stroke_width.unwrap_or(2.0).max(0.1);

    // Parse colors.
    let tile_color = args
        .color
        .as_deref()
        .and_then(|s| photonic_core::Color::from_hex(s))
        .unwrap_or(photonic_core::Color::new(0.10, 0.10, 0.18, 1.0));

    let cols = (width / ts).ceil() as usize;
    let rows = (height / ts).ceil() as usize;

    // Cap at 50×50 to avoid creating thousands of nodes.
    let cols = cols.min(50);
    let rows = rows.min(50);

    // Simple LCG pseudo-random number generator (no external deps).
    let mut rng_state = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let mut next_bool = move || -> bool {
        rng_state = rng_state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (rng_state >> 33) & 1 == 0
    };

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let layer_id = args
        .layer_id
        .as_deref()
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .or(doc.active_layer_id)
        .unwrap_or(uuid::Uuid::nil());

    let mut child_ids: Vec<photonic_core::node::NodeId> = Vec::new();

    // Optional background rectangle.
    if let Some(bg_hex) = &args.background {
        if let Some(bg_color) = photonic_core::Color::from_hex(bg_hex) {
            let mut bg_bez = kurbo::BezPath::new();
            bg_bez.move_to(kurbo::Point::new(x, y));
            bg_bez.line_to(kurbo::Point::new(x + width, y));
            bg_bez.line_to(kurbo::Point::new(x + width, y + height));
            bg_bez.line_to(kurbo::Point::new(x, y + height));
            bg_bez.close_path();
            let mut bg_pn = PathNode::new(photonic_core::path::PathData::from_bez_path(&bg_bez));
            bg_pn.fill = photonic_core::style::Fill::solid(bg_color);
            bg_pn.stroke = photonic_core::style::Stroke::none();
            let bg_node = SceneNode::new("background", layer_id, SceneNodeKind::Path(bg_pn));
            let bg_id = bg_node.id;
            history.execute_discrete(
                photonic_core::history::Command::AddNode {
                    node: bg_node,
                    layer_id: Some(layer_id),
                },
                &mut doc,
            );
            child_ids.push(bg_id);
        }
    }

    for row in 0..rows {
        for col in 0..cols {
            let tx = x + col as f64 * ts;
            let ty = y + row as f64 * ts;
            let flip = next_bool(); // each tile is one of 2 orientations

            let mut bez = kurbo::BezPath::new();

            match style {
                "diagonals" => {
                    // Two diagonal lines per tile — one of two crossing patterns.
                    if flip {
                        // top-left to bottom-right
                        bez.move_to(kurbo::Point::new(tx, ty));
                        bez.line_to(kurbo::Point::new(tx + ts, ty + ts));
                    } else {
                        // top-right to bottom-left
                        bez.move_to(kurbo::Point::new(tx + ts, ty));
                        bez.line_to(kurbo::Point::new(tx, ty + ts));
                    }
                }
                "triangles" => {
                    // Filled triangle (one of two orientations).
                    if flip {
                        bez.move_to(kurbo::Point::new(tx, ty));
                        bez.line_to(kurbo::Point::new(tx + ts, ty));
                        bez.line_to(kurbo::Point::new(tx, ty + ts));
                    } else {
                        bez.move_to(kurbo::Point::new(tx + ts, ty));
                        bez.line_to(kurbo::Point::new(tx + ts, ty + ts));
                        bez.line_to(kurbo::Point::new(tx, ty + ts));
                    }
                    bez.close_path();
                }
                _ => {
                    // "arcs" (default): two quarter-circle arcs connecting mid-edge pairs.
                    let mid = ts / 2.0;
                    let r = mid; // arc radius = half tile side
                    if flip {
                        // Arc: top-mid → left-mid  AND  bottom-mid → right-mid
                        // Approximate arc with a cubic Bézier (kappa ≈ 0.5523).
                        let k = r * 0.5523;
                        // Arc 1: top-mid → left-mid (curves through top-left corner)
                        let p0 = kurbo::Point::new(tx + mid, ty);
                        let p3 = kurbo::Point::new(tx, ty + mid);
                        bez.move_to(p0);
                        bez.curve_to(
                            kurbo::Point::new(tx + mid - k, ty),
                            kurbo::Point::new(tx, ty + mid - k),
                            p3,
                        );
                        // Arc 2: bottom-mid → right-mid (curves through bottom-right corner)
                        let q0 = kurbo::Point::new(tx + mid, ty + ts);
                        let q3 = kurbo::Point::new(tx + ts, ty + mid);
                        bez.move_to(q0);
                        bez.curve_to(
                            kurbo::Point::new(tx + mid + k, ty + ts),
                            kurbo::Point::new(tx + ts, ty + mid + k),
                            q3,
                        );
                    } else {
                        // Arc: top-mid → right-mid  AND  bottom-mid → left-mid
                        let k = r * 0.5523;
                        // Arc 1: top-mid → right-mid (curves through top-right corner)
                        let p0 = kurbo::Point::new(tx + mid, ty);
                        let p3 = kurbo::Point::new(tx + ts, ty + mid);
                        bez.move_to(p0);
                        bez.curve_to(
                            kurbo::Point::new(tx + mid + k, ty),
                            kurbo::Point::new(tx + ts, ty + mid - k),
                            p3,
                        );
                        // Arc 2: bottom-mid → left-mid (curves through bottom-left corner)
                        let q0 = kurbo::Point::new(tx + mid, ty + ts);
                        let q3 = kurbo::Point::new(tx, ty + mid);
                        bez.move_to(q0);
                        bez.curve_to(
                            kurbo::Point::new(tx + mid - k, ty + ts),
                            kurbo::Point::new(tx, ty + mid + k),
                            q3,
                        );
                    }
                }
            }

            let mut pn = PathNode::new(photonic_core::path::PathData::from_bez_path(&bez));
            match style {
                "triangles" => {
                    pn.fill = photonic_core::style::Fill::solid(tile_color);
                    pn.stroke = photonic_core::style::Stroke::none();
                }
                _ => {
                    pn.fill = photonic_core::style::Fill::none();
                    pn.stroke = photonic_core::style::Stroke::solid(tile_color, sw);
                }
            }

            let label = format!("tile_{row}_{col}");
            let node = SceneNode::new(&label, layer_id, SceneNodeKind::Path(pn));
            let nid = node.id;
            history.execute_discrete(
                photonic_core::history::Command::AddNode {
                    node,
                    layer_id: Some(layer_id),
                },
                &mut doc,
            );
            child_ids.push(nid);
        }
    }

    // Group all tiles.
    let group = SceneNode::new(
        "Truchet Tiling",
        layer_id,
        SceneNodeKind::Group(GroupNode::new()),
    );
    let group_id = group.id.to_string();
    history.execute_discrete(
        photonic_core::history::Command::GroupNodes {
            group,
            layer_id,
            insert_index: 0,
            children: child_ids,
        },
        &mut doc,
    );

    ToolResult::text(format!("Created Truchet tiling: {cols}×{rows} tiles")).with_data(
        serde_json::json!({
            "group_id": group_id,
            "cols": cols,
            "rows": rows,
            "tiles": cols * rows,
        }),
    )
}
pub async fn create_heart(state: &AppState, args: CreateHeartArgs) -> ToolResult {
    tracing::debug!("tool: create_heart");

    let s = args.size.unwrap_or(60.0);
    let cx = args.cx;
    let cy = args.cy;
    let half = s / 2.0;

    // Heart shape using cubic bezier curves.
    // Bottom tip at (cx, cy), top center dip, two rounded lobes.
    let mut bez = kurbo::BezPath::new();

    // Start at bottom tip.
    bez.move_to((cx, cy));

    // Left lobe: bottom tip → left side → top-left lobe → center dip
    bez.curve_to(
        (cx - half * 0.3, cy - half * 0.6), // cp1
        (cx - half, cy - half * 0.9),       // cp2
        (cx - half, cy - half * 1.2),       // left peak
    );
    bez.curve_to(
        (cx - half, cy - half * 1.6),       // cp1
        (cx - half * 0.4, cy - half * 1.7), // cp2
        (cx, cy - half * 1.4),              // center dip
    );

    // Right lobe: center dip → top-right lobe → right side → bottom tip
    bez.curve_to(
        (cx + half * 0.4, cy - half * 1.7), // cp1
        (cx + half, cy - half * 1.6),       // cp2
        (cx + half, cy - half * 1.2),       // right peak
    );
    bez.curve_to(
        (cx + half, cy - half * 0.9),       // cp1
        (cx + half * 0.3, cy - half * 0.6), // cp2
        (cx, cy),                           // back to bottom tip
    );
    bez.close_path();

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let layer_id = args
        .layer_id
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .or(doc.active_layer_id)
        .unwrap_or(uuid::Uuid::nil());

    let mut pn = PathNode::new(PathData::from_bez_path(&bez));
    // Default to red fill if none specified.
    if args.fill.is_none() && args.stroke.is_none() {
        pn.fill = photonic_core::style::Fill {
            kind: photonic_core::style::FillKind::Solid(photonic_core::color::Color::new(
                0.9, 0.1, 0.2, 1.0,
            )),
            ..Default::default()
        };
        pn.stroke = photonic_core::style::Stroke::none();
    } else {
        if let Err(e) = apply_style(&mut pn, args.fill, args.stroke) {
            return ToolResult::error(e);
        }
    }

    let node = SceneNode::new("Heart", layer_id, SceneNodeKind::Path(pn));
    let node_id = node.id;
    history.execute_discrete(
        Command::AddNode {
            node,
            layer_id: Some(layer_id),
        },
        &mut doc,
    );

    ToolResult::text(format!("Created heart at ({cx},{cy}), size={s}"))
        .with_data(serde_json::json!({ "node_id": node_id }))
}
pub async fn create_gear(state: &AppState, args: CreateGearArgs) -> ToolResult {
    tracing::debug!("tool: create_gear");
    use kurbo::Shape;

    let outer_r = args.outer_radius.unwrap_or(50.0);
    let inner_r = args.inner_radius.unwrap_or(35.0);
    let hole_r = args.hole_radius.unwrap_or(10.0);
    let teeth = args.teeth.unwrap_or(12).max(3);
    let cx = args.cx;
    let cy = args.cy;

    let mut bez = kurbo::BezPath::new();
    let tooth_angle = std::f64::consts::TAU / teeth as f64;

    // Each tooth has 4 points: inner-left, outer-left, outer-right, inner-right.
    // The tooth occupies half the angular span, gap occupies the other half.
    let tooth_frac = 0.4; // fraction of tooth_angle occupied by tooth top
    let gap_frac = 1.0 - tooth_frac;

    for i in 0..teeth {
        let base_a = tooth_angle * i as f64;
        let a0 = base_a; // start of gap (inner)
        let a1 = base_a + tooth_angle * gap_frac * 0.5; // start of tooth (inner→outer)
        let a2 = base_a + tooth_angle * (gap_frac * 0.5 + tooth_frac * 0.25); // outer left
        let a3 = base_a + tooth_angle * (1.0 - gap_frac * 0.5 - tooth_frac * 0.25); // outer right
        let a4 = base_a + tooth_angle * (1.0 - gap_frac * 0.5); // end of tooth (outer→inner)

        let pts = [
            (inner_r, a0),
            (inner_r, a1),
            (outer_r, a2),
            (outer_r, a3),
            (inner_r, a4),
        ];

        for (j, &(r, a)) in pts.iter().enumerate() {
            let px = cx + r * a.cos();
            let py = cy + r * a.sin();
            if i == 0 && j == 0 {
                bez.move_to((px, py));
            } else {
                bez.line_to((px, py));
            }
        }
    }
    bez.close_path();

    // Add center hole as reversed circle.
    if hole_r > 0.0 {
        let hole = kurbo::Ellipse::new((cx, cy), (hole_r, hole_r), 0.0).to_path(0.1);
        let hole_els: Vec<_> = hole.elements().to_vec();
        let reversed = reverse_bez(&hole_els);
        for el in &reversed {
            bez.push(*el);
        }
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let layer_id = args
        .layer_id
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .or(doc.active_layer_id)
        .unwrap_or(uuid::Uuid::nil());

    let mut pn = PathNode::new(PathData::from_bez_path(&bez));
    if let Err(e) = apply_style(&mut pn, args.fill, args.stroke) {
        return ToolResult::error(e);
    }

    let node = SceneNode::new("Gear", layer_id, SceneNodeKind::Path(pn));
    let node_id = node.id;
    history.execute_discrete(
        Command::AddNode {
            node,
            layer_id: Some(layer_id),
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Created gear at ({cx},{cy}) — {teeth} teeth, outer={outer_r}, inner={inner_r}"
    ))
    .with_data(serde_json::json!({ "node_id": node_id, "teeth": teeth }))
}

pub async fn create_qr_code(state: &AppState, args: CreateQrCodeArgs) -> ToolResult {
    tracing::debug!("tool: create_qr_code");
    use photonic_core::color::Color;
    use photonic_core::ops::qr::{build_qr, QrEcc, QrModuleShape, QrOptions};
    use photonic_core::style::Fill;
    use photonic_core::transform::Transform;

    let shape = match args.module_shape.as_deref() {
        Some(s) => match QrModuleShape::parse(s) {
            Some(v) => v,
            None => {
                return ToolResult::error("module_shape must be square, rounded, dot, or connected")
            }
        },
        None => QrModuleShape::Square,
    };
    let ecc = match args.ecc.as_deref() {
        Some(s) => match QrEcc::parse(s) {
            Some(v) => v,
            None => return ToolResult::error("ecc must be l, m, q, or h"),
        },
        None => QrEcc::Medium,
    };

    let opts = QrOptions {
        data: args.data.clone(),
        ecc,
        shape,
        radius: args.radius.unwrap_or(0.4),
        size: args.size.unwrap_or(200.0),
        quiet_zone: args.quiet_zone.unwrap_or(4),
    };
    let art = match build_qr(&opts) {
        Ok(a) => a,
        Err(e) => return ToolResult::error(e),
    };

    // Dark-module fill (solid or gradient); default solid black.
    let fg_fill = match &args.fill {
        Some(f) => match f.to_fill() {
            Ok(x) => x,
            Err(e) => return ToolResult::error(e),
        },
        None => Fill::solid(Color::BLACK),
    };
    // Background: default white; "none" = transparent; else a solid hex.
    let bg_fill: Option<Fill> = match args.background.as_deref() {
        Some(s) if s.eq_ignore_ascii_case("none") => None,
        Some(hex) => match Color::from_hex(hex) {
            Some(c) => Some(Fill::solid(c)),
            None => {
                return ToolResult::error(format!(
                    "Invalid background '{hex}' (expected #rrggbb or 'none')"
                ))
            }
        },
        None => Some(Fill::solid(Color::WHITE)),
    };

    let tf = Transform::new(
        1.0,
        0.0,
        0.0,
        1.0,
        args.x.unwrap_or(0.0),
        args.y.unwrap_or(0.0),
    );

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let layer_id = args
        .layer_id
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .or(doc.active_layer_id)
        .unwrap_or(uuid::Uuid::nil());

    let mut commands: Vec<Command> = Vec::new();
    let mut child_ids: Vec<NodeId> = Vec::new();

    // Background rect (behind the modules), if any — first child = bottom of group.
    if let Some(bg) = bg_fill {
        let mut pn = PathNode::new(PathData::rect(0.0, 0.0, art.size, art.size));
        pn.fill = bg;
        let mut node = SceneNode::new("QR Background", layer_id, SceneNodeKind::Path(pn));
        node.transform = tf;
        child_ids.push(node.id);
        commands.push(Command::AddNode {
            node,
            layer_id: Some(layer_id),
        });
    }

    // The compound path of every dark module (top of the group).
    let mut mod_pn = PathNode::new(art.modules);
    mod_pn.fill = fg_fill;
    let mut mod_node = SceneNode::new("QR Modules", layer_id, SceneNodeKind::Path(mod_pn));
    mod_node.transform = tf;
    child_ids.push(mod_node.id);
    commands.push(Command::AddNode {
        node: mod_node,
        layer_id: Some(layer_id),
    });

    // Wrap in a group so the code is one movable/recolourable unit. The group
    // node must ALREADY list its children — `GroupNodes` only detaches them from
    // the layer and inserts the group (it does not populate the child list).
    // Insert at the top of z-order (usize::MAX clamps to the layer length) so the
    // QR is visible without a manual reorder. One Batch = one undo step, and the
    // whole group deletes as a unit (no orphaned "QR Modules" paths).
    let group = SceneNode::new(
        "QR Code",
        layer_id,
        SceneNodeKind::Group(GroupNode {
            children: child_ids.clone(),
            clip_children: false,
            clip_node_id: None,
            blend_spine_id: None,
            live_boolean: None,
        }),
    );
    let group_id = group.id;
    commands.push(Command::GroupNodes {
        group,
        layer_id,
        insert_index: usize::MAX,
        children: child_ids,
    });

    history.execute_discrete(Command::Batch(commands), &mut doc);

    ToolResult::text(format!(
        "Created QR code — {}×{} modules, {shape:?} style, encoding: {}",
        art.matrix_size, art.matrix_size, opts.data
    ))
    .with_data(serde_json::json!({
        "group_id": group_id,
        "matrix_size": art.matrix_size,
        "module_size": art.module_size
    }))
}

pub async fn point_on_path(state: &AppState, args: PointOnPathArgs) -> ToolResult {
    tracing::debug!("tool: point_on_path");
    use kurbo::{ParamCurve, ParamCurveArclen};

    let doc = state.document.lock().await;

    let nid = match uuid::Uuid::parse_str(&args.node_id) {
        Ok(id) => id,
        Err(_) => match doc.find_node_by_name(&args.node_id) {
            Some(n) => n.id,
            None => return ToolResult::error(format!("Node not found: {}", args.node_id)),
        },
    };
    let node = match doc.nodes.get(&nid) {
        Some(n) => n,
        None => return ToolResult::error("Node not found"),
    };
    let pn = match &node.kind {
        SceneNodeKind::Path(pn) => pn,
        _ => return ToolResult::error("Node is not a path"),
    };

    let bez = pn.path_data.to_bez_path();
    let segments: Vec<kurbo::PathSeg> = bez.segments().collect();
    if segments.is_empty() {
        return ToolResult::error("Path has no segments");
    }

    let accuracy = 0.5;
    let seg_lengths: Vec<f64> = segments.iter().map(|s| s.arclen(accuracy)).collect();
    let total_length: f64 = seg_lengths.iter().sum();
    if total_length < 1e-9 {
        return ToolResult::error("Path has zero length");
    }

    let mut results = Vec::new();

    for &t_val in &args.t {
        let t = t_val.clamp(0.0, 1.0);
        let target_len = t * total_length;

        let mut accum = 0.0;
        let mut pt = kurbo::Point::ZERO;
        let mut tangent_angle: f64 = 0.0;

        for (seg, &seg_len) in segments.iter().zip(seg_lengths.iter()) {
            if accum + seg_len >= target_len || seg_len < 1e-9 {
                let local_t = if seg_len > 1e-9 {
                    (target_len - accum) / seg_len
                } else {
                    0.5
                };
                pt = seg.eval(local_t.clamp(0.0, 1.0));
                let dt = 0.001;
                let p0 = seg.eval((local_t - dt).max(0.0));
                let p1 = seg.eval((local_t + dt).min(1.0));
                tangent_angle = (p1.y - p0.y).atan2(p1.x - p0.x);
                break;
            }
            accum += seg_len;
        }

        results.push(serde_json::json!({
            "t": t_val,
            "x": pt.x,
            "y": pt.y,
            "tangent_degrees": tangent_angle.to_degrees(),
        }));
    }

    let summary = if results.len() == 1 {
        let r = &results[0];
        format!(
            "t={}: ({:.1}, {:.1}) angle={:.1}°",
            r["t"], r["x"], r["y"], r["tangent_degrees"]
        )
    } else {
        format!("{} points sampled along path", results.len())
    };

    ToolResult::text(summary).with_data(serde_json::json!({
        "points": results,
        "total_length": total_length,
    }))
}
pub async fn create_speech_bubble(state: &AppState, args: CreateSpeechBubbleArgs) -> ToolResult {
    tracing::debug!("tool: create_speech_bubble");

    let w = args.width.unwrap_or(120.0);
    let h = args.height.unwrap_or(60.0);
    let r = args.corner_radius.unwrap_or(15.0).min(w / 2.0).min(h / 2.0);
    let tail_x = args.tail_x.unwrap_or(args.cx - 10.0);
    let tail_y = args.tail_y.unwrap_or(args.cy + h / 2.0 + 30.0);
    let tail_w = args.tail_width.unwrap_or(20.0);

    let left = args.cx - w / 2.0;
    let right = args.cx + w / 2.0;
    let top = args.cy - h / 2.0;
    let bottom = args.cy + h / 2.0;

    // Rounded rectangle with tail integrated into the bottom edge.
    // Tail connects at the bottom edge between two points.
    let tail_base_left = (args.cx - tail_w / 2.0).max(left + r);
    let tail_base_right = (args.cx + tail_w / 2.0).min(right - r);

    let mut bez = kurbo::BezPath::new();

    // Start at top-left corner after the radius.
    bez.move_to((left + r, top));
    bez.line_to((right - r, top));
    // Top-right corner.
    bez.quad_to((right, top), (right, top + r));
    bez.line_to((right, bottom - r));
    // Bottom-right corner.
    bez.quad_to((right, bottom), (right - r, bottom));
    // Bottom edge → tail.
    bez.line_to((tail_base_right, bottom));
    bez.line_to((tail_x, tail_y));
    bez.line_to((tail_base_left, bottom));
    // Continue bottom edge.
    bez.line_to((left + r, bottom));
    // Bottom-left corner.
    bez.quad_to((left, bottom), (left, bottom - r));
    bez.line_to((left, top + r));
    // Top-left corner.
    bez.quad_to((left, top), (left + r, top));
    bez.close_path();

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let layer_id = args
        .layer_id
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .or(doc.active_layer_id)
        .unwrap_or(uuid::Uuid::nil());

    let mut pn = PathNode::new(PathData::from_bez_path(&bez));
    if args.fill.is_none() && args.stroke.is_none() {
        pn.fill = photonic_core::style::Fill {
            kind: photonic_core::style::FillKind::Solid(photonic_core::color::Color::WHITE),
            ..Default::default()
        };
        pn.stroke = photonic_core::style::Stroke {
            color: photonic_core::color::Color::BLACK,
            width: 2.0,
            enabled: true,
            ..Default::default()
        };
    } else if let Err(e) = apply_style(&mut pn, args.fill, args.stroke) {
        return ToolResult::error(e);
    }

    let node = SceneNode::new("Speech Bubble", layer_id, SceneNodeKind::Path(pn));
    let node_id = node.id;
    history.execute_discrete(
        Command::AddNode {
            node,
            layer_id: Some(layer_id),
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Created speech bubble at ({},{}), {w}×{h}",
        args.cx, args.cy
    ))
    .with_data(serde_json::json!({ "node_id": node_id }))
}
pub async fn create_cross(state: &AppState, args: CreateCrossArgs) -> ToolResult {
    tracing::debug!("tool: create_cross");

    let size = args.size.unwrap_or(60.0);
    let thick = args.thickness.unwrap_or(20.0).min(size);
    let rot_deg = args.rotation.unwrap_or(0.0);

    let half_s = size / 2.0;
    let half_t = thick / 2.0;

    // Cross shape centered at origin, 12-point polygon:
    //   -half_t,-half_s → half_t,-half_s → half_t,-half_t → half_s,-half_t →
    //   half_s,half_t → half_t,half_t → half_t,half_s → -half_t,half_s →
    //   -half_t,half_t → -half_s,half_t → -half_s,-half_t → -half_t,-half_t
    let pts = [
        (-half_t, -half_s),
        (half_t, -half_s),
        (half_t, -half_t),
        (half_s, -half_t),
        (half_s, half_t),
        (half_t, half_t),
        (half_t, half_s),
        (-half_t, half_s),
        (-half_t, half_t),
        (-half_s, half_t),
        (-half_s, -half_t),
        (-half_t, -half_t),
    ];

    let rad = rot_deg.to_radians();
    let cos_r = rad.cos();
    let sin_r = rad.sin();

    let mut bez = kurbo::BezPath::new();
    for (i, &(px, py)) in pts.iter().enumerate() {
        let rx = px * cos_r - py * sin_r + args.cx;
        let ry = px * sin_r + py * cos_r + args.cy;
        if i == 0 {
            bez.move_to((rx, ry));
        } else {
            bez.line_to((rx, ry));
        }
    }
    bez.close_path();

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let layer_id = args
        .layer_id
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .or(doc.active_layer_id)
        .unwrap_or(uuid::Uuid::nil());

    let mut pn = PathNode::new(PathData::from_bez_path(&bez));
    if let Err(e) = apply_style(&mut pn, args.fill, args.stroke) {
        return ToolResult::error(e);
    }

    let node = SceneNode::new("Cross", layer_id, SceneNodeKind::Path(pn));
    let node_id = node.id;
    history.execute_discrete(
        Command::AddNode {
            node,
            layer_id: Some(layer_id),
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Created cross at ({},{}), size={size}, thickness={thick}",
        args.cx, args.cy
    ))
    .with_data(serde_json::json!({ "node_id": node_id }))
}
pub async fn measure_path(state: &AppState, args: MeasurePathArgs) -> ToolResult {
    tracing::debug!("tool: measure_path");
    use kurbo::{ParamCurveArclen, Shape};

    let doc = state.document.lock().await;

    let nid = match uuid::Uuid::parse_str(&args.node_id) {
        Ok(id) => id,
        Err(_) => match doc.find_node_by_name(&args.node_id) {
            Some(n) => n.id,
            None => return ToolResult::error(format!("Node not found: {}", args.node_id)),
        },
    };

    let node = match doc.nodes.get(&nid) {
        Some(n) => n,
        None => return ToolResult::error("Node not found"),
    };

    let pn = match &node.kind {
        SceneNodeKind::Path(pn) => pn,
        _ => return ToolResult::error("Node is not a path"),
    };

    let bez = pn.path_data.to_bez_path();
    let el_count = bez.elements().len();

    // Count segments and compute arc length.
    let segments: Vec<kurbo::PathSeg> = bez.segments().collect();
    let seg_count = segments.len();
    let total_length: f64 = segments.iter().map(|s| s.arclen(0.5)).sum();

    // Bounding box.
    let bbox = bez.bounding_box();
    let is_closed = bez
        .elements()
        .iter()
        .any(|e| matches!(e, kurbo::PathEl::ClosePath));

    // Count anchor points (MoveTo + LineTo + CurveTo + QuadTo endpoints).
    let anchor_count = bez
        .elements()
        .iter()
        .filter(|e| !matches!(e, kurbo::PathEl::ClosePath))
        .count();

    ToolResult::text(format!(
        "Path '{}': length={:.1}, {anchor_count} anchors, {seg_count} segments, {}",
        node.name,
        total_length,
        if is_closed { "closed" } else { "open" },
    ))
    .with_data(serde_json::json!({
        "total_length": total_length,
        "element_count": el_count,
        "segment_count": seg_count,
        "anchor_count": anchor_count,
        "closed": is_closed,
        "bounding_box": {
            "x": bbox.x0,
            "y": bbox.y0,
            "width": bbox.width(),
            "height": bbox.height(),
        },
    }))
}
pub async fn create_arrow_shape(state: &AppState, args: CreateArrowShapeArgs) -> ToolResult {
    tracing::debug!("tool: create_arrow_shape");

    let length = args.length.unwrap_or(100.0);
    let head_w = args.head_width.unwrap_or(40.0);
    let head_depth_frac = args.head_depth.unwrap_or(0.4).clamp(0.1, 0.9);
    let shaft_w = args.shaft_width.unwrap_or(16.0);
    let dir_deg = args.direction.unwrap_or(0.0);

    let head_len = length * head_depth_frac;
    let _shaft_len = length - head_len;
    let half_head = head_w / 2.0;
    let half_shaft = shaft_w / 2.0;

    // Build arrow pointing right (direction=0), then rotate.
    // Tip at origin, shaft extends to the left.
    //
    //        (0,0) ← tip
    //       / |
    //      /  |  head_len
    //     /   |
    //    (-head_len, -half_head)  ← top wing
    //    |    (-head_len, -half_shaft) ← shaft top
    //    |    |
    //    |    (-length, -half_shaft) ← shaft end top
    //    |    (-length, +half_shaft) ← shaft end bottom
    //    |    (-head_len, +half_shaft) ← shaft bottom
    //    (-head_len, +half_head) ← bottom wing
    //     \   |
    //      \  |
    //       \ |

    let pts = [
        (0.0, 0.0),               // tip
        (-head_len, -half_head),  // top wing
        (-head_len, -half_shaft), // shaft top start
        (-length, -half_shaft),   // shaft top end
        (-length, half_shaft),    // shaft bottom end
        (-head_len, half_shaft),  // shaft bottom start
        (-head_len, half_head),   // bottom wing
    ];

    // Rotate all points by direction.
    let rad = dir_deg.to_radians();
    let cos_d = rad.cos();
    let sin_d = rad.sin();

    let mut bez = kurbo::BezPath::new();
    for (i, &(px, py)) in pts.iter().enumerate() {
        let rx = px * cos_d - py * sin_d + args.x;
        let ry = px * sin_d + py * cos_d + args.y;
        if i == 0 {
            bez.move_to((rx, ry));
        } else {
            bez.line_to((rx, ry));
        }
    }
    bez.close_path();

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let layer_id = args
        .layer_id
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .or(doc.active_layer_id)
        .unwrap_or(uuid::Uuid::nil());

    let mut pn = PathNode::new(PathData::from_bez_path(&bez));
    if let Err(e) = apply_style(&mut pn, args.fill, args.stroke) {
        return ToolResult::error(e);
    }

    let node = SceneNode::new("Arrow", layer_id, SceneNodeKind::Path(pn));
    let node_id = node.id;
    history.execute_discrete(
        Command::AddNode {
            node,
            layer_id: Some(layer_id),
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Created arrow shape at ({},{}) length={length} dir={dir_deg}°",
        args.x, args.y
    ))
    .with_data(serde_json::json!({ "node_id": node_id }))
}
pub async fn create_donut(state: &AppState, args: CreateDonutArgs) -> ToolResult {
    tracing::debug!("tool: create_donut");
    use kurbo::Shape;

    let outer_r = args.outer_radius.unwrap_or(50.0).max(1.0);
    let inner_r = args
        .inner_radius
        .unwrap_or(25.0)
        .max(0.0)
        .min(outer_r - 0.1);
    let start_deg = args.start_angle.unwrap_or(0.0);
    let end_deg = args.end_angle.unwrap_or(360.0);
    let cx = args.cx;
    let cy = args.cy;

    let is_full = (end_deg - start_deg).abs() >= 359.9;

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let layer_id = args
        .layer_id
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .or(doc.active_layer_id)
        .unwrap_or(uuid::Uuid::nil());

    let mut bez = kurbo::BezPath::new();

    if is_full {
        // Full donut: outer circle CW, inner circle CCW (for even-odd fill rule).
        let outer = kurbo::Ellipse::new((cx, cy), (outer_r, outer_r), 0.0).to_path(0.1);
        for el in outer.elements() {
            bez.push(*el);
        }

        // Inner circle: reverse direction for hole.
        let inner = kurbo::Ellipse::new((cx, cy), (inner_r, inner_r), 0.0).to_path(0.1);
        let inner_els: Vec<_> = inner.elements().to_vec();
        // Reverse the inner path.
        let reversed = reverse_bez(&inner_els);
        for el in &reversed {
            bez.push(*el);
        }
    } else {
        // Partial donut (arc segment).
        let start_rad = start_deg.to_radians();
        let end_rad = end_deg.to_radians();
        let n_segs = 32;

        // Outer arc from start to end.
        for i in 0..=n_segs {
            let t = i as f64 / n_segs as f64;
            let a = start_rad + (end_rad - start_rad) * t;
            let pt = kurbo::Point::new(cx + outer_r * a.cos(), cy + outer_r * a.sin());
            if i == 0 {
                bez.move_to(pt);
            } else {
                bez.line_to(pt);
            }
        }
        // Line to inner arc end.
        let inner_end =
            kurbo::Point::new(cx + inner_r * end_rad.cos(), cy + inner_r * end_rad.sin());
        bez.line_to(inner_end);
        // Inner arc from end back to start.
        for i in (0..=n_segs).rev() {
            let t = i as f64 / n_segs as f64;
            let a = start_rad + (end_rad - start_rad) * t;
            let pt = kurbo::Point::new(cx + inner_r * a.cos(), cy + inner_r * a.sin());
            bez.line_to(pt);
        }
        bez.close_path();
    }

    let mut pn = PathNode::new(PathData::from_bez_path(&bez));
    if let Err(e) = apply_style(&mut pn, args.fill, args.stroke) {
        return ToolResult::error(e);
    }

    let node = SceneNode::new("Donut", layer_id, SceneNodeKind::Path(pn));
    let node_id = node.id;
    history.execute_discrete(
        Command::AddNode {
            node,
            layer_id: Some(layer_id),
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Created donut at ({cx},{cy}) — outer={outer_r}, inner={inner_r}"
    ))
    .with_data(serde_json::json!({ "node_id": node_id }))
}
pub async fn create_sunburst(state: &AppState, args: CreateSunburstArgs) -> ToolResult {
    tracing::debug!("tool: create_sunburst");
    use photonic_core::color::Color;
    use photonic_core::style::{Fill, FillKind, Stroke};

    let inner_r = args.inner_radius.unwrap_or(20.0).max(0.0);
    let outer_r = args.outer_radius.unwrap_or(100.0).max(1.0);
    let rays = args.rays.unwrap_or(24).max(4);
    let cx = args.cx;
    let cy = args.cy;

    let ray_color = args.color.as_deref().unwrap_or("#FFD700");
    let color = Color::from_hex(ray_color).unwrap_or(Color::new(1.0, 0.84, 0.0, 1.0));

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let layer_id = args
        .layer_id
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .or(doc.active_layer_id)
        .unwrap_or(uuid::Uuid::nil());

    // Build alternating wedges as a single compound path.
    let mut bez = kurbo::BezPath::new();
    let wedge_angle = std::f64::consts::TAU / rays as f64;

    for i in (0..rays).step_by(2) {
        let a0 = wedge_angle * i as f64;
        let a1 = wedge_angle * (i + 1) as f64;

        // Inner arc start → outer arc start → outer arc end → inner arc end → close.
        let i0 = kurbo::Point::new(cx + inner_r * a0.cos(), cy + inner_r * a0.sin());
        let o0 = kurbo::Point::new(cx + outer_r * a0.cos(), cy + outer_r * a0.sin());
        let o1 = kurbo::Point::new(cx + outer_r * a1.cos(), cy + outer_r * a1.sin());
        let i1 = kurbo::Point::new(cx + inner_r * a1.cos(), cy + inner_r * a1.sin());

        // Approximate the arc with a line (for simplicity — each wedge is ~15° which is fine).
        bez.move_to(i0);
        bez.line_to(o0);
        // Outer arc (approximate with a quadratic through the midpoint).
        let mid_a = (a0 + a1) / 2.0;
        let outer_mid = kurbo::Point::new(cx + outer_r * mid_a.cos(), cy + outer_r * mid_a.sin());
        // Control point for quadratic arc approximation:
        let arc_cp = kurbo::Point::new(
            2.0 * outer_mid.x - 0.5 * (o0.x + o1.x),
            2.0 * outer_mid.y - 0.5 * (o0.y + o1.y),
        );
        bez.quad_to(arc_cp, o1);
        bez.line_to(i1);
        // Inner arc back.
        let inner_mid = kurbo::Point::new(cx + inner_r * mid_a.cos(), cy + inner_r * mid_a.sin());
        let inner_cp = kurbo::Point::new(
            2.0 * inner_mid.x - 0.5 * (i1.x + i0.x),
            2.0 * inner_mid.y - 0.5 * (i1.y + i0.y),
        );
        bez.quad_to(inner_cp, i0);
        bez.close_path();
    }

    let mut pn = PathNode::new(PathData::from_bez_path(&bez));
    pn.fill = Fill {
        kind: FillKind::Solid(color),
        ..Default::default()
    };
    pn.stroke = Stroke::none();

    let node = SceneNode::new("Sunburst", layer_id, SceneNodeKind::Path(pn));
    let node_id = node.id;
    history.execute_discrete(
        Command::AddNode {
            node,
            layer_id: Some(layer_id),
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Created sunburst at ({cx},{cy}) — {rays} rays, inner={inner_r}, outer={outer_r}"
    ))
    .with_data(serde_json::json!({ "node_id": node_id, "rays": rays }))
}
pub async fn create_wave_pattern(state: &AppState, args: CreateWavePatternArgs) -> ToolResult {
    tracing::debug!("tool: create_wave_pattern");

    let lines = args.lines.unwrap_or(8).max(1);
    let wavelength = args.wavelength.unwrap_or(40.0).max(1.0);
    let amplitude = args.amplitude.unwrap_or(10.0);

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let layer_id = args
        .layer_id
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .or(doc.active_layer_id)
        .unwrap_or(uuid::Uuid::nil());

    let mut bez = kurbo::BezPath::new();
    let line_spacing = args.height / lines as f64;
    let points_per_wave = 20; // subdivision for smooth sine
    let total_points = (args.width / wavelength * points_per_wave as f64) as usize + 1;

    for line_idx in 0..lines {
        let base_y = args.y + line_spacing * (line_idx as f64 + 0.5);

        // Generate sine wave points.
        let mut wave_pts: Vec<kurbo::Point> = Vec::with_capacity(total_points);
        for i in 0..=total_points {
            let t = i as f64 / total_points as f64;
            let wx = args.x + t * args.width;
            let phase = t * args.width / wavelength * std::f64::consts::TAU;
            let wy = base_y + amplitude * phase.sin();
            wave_pts.push(kurbo::Point::new(wx, wy));
        }

        // Convert to smooth bezier using Catmull-Rom.
        if wave_pts.len() >= 2 {
            bez.move_to(wave_pts[0]);
            for i in 0..wave_pts.len() - 1 {
                let p0 = if i > 0 {
                    wave_pts[i - 1]
                } else {
                    kurbo::Point::new(
                        2.0 * wave_pts[0].x - wave_pts[1].x,
                        2.0 * wave_pts[0].y - wave_pts[1].y,
                    )
                };
                let p1 = wave_pts[i];
                let p2 = wave_pts[i + 1];
                let p3 = if i + 2 < wave_pts.len() {
                    wave_pts[i + 2]
                } else {
                    let n = wave_pts.len();
                    kurbo::Point::new(
                        2.0 * wave_pts[n - 1].x - wave_pts[n - 2].x,
                        2.0 * wave_pts[n - 1].y - wave_pts[n - 2].y,
                    )
                };
                let cp1 = kurbo::Point::new(p1.x + (p2.x - p0.x) / 6.0, p1.y + (p2.y - p0.y) / 6.0);
                let cp2 = kurbo::Point::new(p2.x - (p3.x - p1.x) / 6.0, p2.y - (p3.y - p1.y) / 6.0);
                bez.curve_to(cp1, cp2, p2);
            }
        }
    }

    let mut pn = PathNode::new(PathData::from_bez_path(&bez));
    pn.fill = photonic_core::style::Fill::none();

    if let Some(stroke_arg) = args.stroke {
        match stroke_arg.to_stroke() {
            Ok(s) => pn.stroke = s,
            Err(e) => return ToolResult::error(e),
        }
    } else {
        pn.stroke = photonic_core::style::Stroke {
            color: photonic_core::color::Color::new(0.2, 0.4, 0.8, 1.0),
            width: 1.5,
            ..Default::default()
        };
    }

    let node = SceneNode::new("Wave Pattern", layer_id, SceneNodeKind::Path(pn));
    let node_id = node.id;
    history.execute_discrete(
        Command::AddNode {
            node,
            layer_id: Some(layer_id),
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Created wave pattern — {} lines, wavelength={wavelength}, amplitude={amplitude}",
        lines
    ))
    .with_data(serde_json::json!({ "node_id": node_id, "lines": lines }))
}
pub async fn round_corners(state: &AppState, args: RoundCornersArgs) -> ToolResult {
    tracing::debug!("tool: round_corners");

    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let radius = args.radius.unwrap_or(10.0).max(0.0);

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let mut commands = Vec::new();
    let mut modified = 0usize;
    let mut skipped = 0usize;

    for node_id_str in &args.node_ids {
        let nid = match uuid::Uuid::parse_str(node_id_str) {
            Ok(id) => id,
            Err(_) => match doc.find_node_by_name(node_id_str) {
                Some(n) => n.id,
                None => {
                    skipped += 1;
                    continue;
                }
            },
        };
        let node = match doc.nodes.get(&nid) {
            Some(n) => n.clone(),
            None => {
                skipped += 1;
                continue;
            }
        };
        let pn = match &node.kind {
            SceneNodeKind::Path(pn) => pn,
            _ => {
                skipped += 1;
                continue;
            }
        };

        let bez = pn.path_data.to_bez_path();
        let new_bez = apply_round_corners(&bez, radius);

        let mut new_node = node.clone();
        if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
            new_pn.path_data = PathData::from_bez_path(&new_bez);
        }
        commands.push(Command::UpdateNode {
            old: node,
            new: new_node,
        });
        modified += 1;
    }

    if commands.is_empty() {
        return ToolResult::error("No path nodes found in node_ids");
    }

    for cmd in commands {
        history.execute_discrete(cmd, &mut doc);
    }

    ToolResult::text(format!(
        "Rounded corners on {} node(s) (radius={radius}){}",
        modified,
        if skipped > 0 {
            format!(" — {skipped} skipped")
        } else {
            String::new()
        },
    ))
    .with_data(serde_json::json!({ "modified": modified, "skipped": skipped }))
}
pub async fn warp_envelope(state: &AppState, args: WarpEnvelopeArgs) -> ToolResult {
    tracing::debug!("tool: warp_envelope");

    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let valid_types = [
        "arc",
        "arc_lower",
        "arc_upper",
        "arch",
        "bulge",
        "wave",
        "flag",
        "squeeze",
        "inflate",
        "fisheye",
        "shell_lower",
        "shell_upper",
        "fish",
        "rise",
        "twist",
    ];
    if !valid_types.contains(&args.warp_type.as_str()) {
        return ToolResult::error(format!(
            "Unknown warp_type: '{}'. Use one of: {}",
            args.warp_type,
            valid_types.join(", ")
        ));
    }

    let bend = args.bend.unwrap_or(0.5);
    let dh = args.distort_h.unwrap_or(0.0);
    let dv = args.distort_v.unwrap_or(0.0);

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let mut commands = Vec::new();
    let mut modified = 0usize;
    let mut skipped = 0usize;

    for node_id_str in &args.node_ids {
        let nid = match uuid::Uuid::parse_str(node_id_str) {
            Ok(id) => id,
            Err(_) => match doc.find_node_by_name(node_id_str) {
                Some(n) => n.id,
                None => {
                    skipped += 1;
                    continue;
                }
            },
        };
        let node = match doc.nodes.get(&nid) {
            Some(n) => n.clone(),
            None => {
                skipped += 1;
                continue;
            }
        };
        let pn = match &node.kind {
            SceneNodeKind::Path(pn) => pn,
            _ => {
                skipped += 1;
                continue;
            }
        };

        let bez = pn.path_data.to_bez_path();
        let new_bez = apply_warp_envelope(&bez, &args.warp_type, bend, dh, dv);

        let mut new_node = node.clone();
        if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
            new_pn.path_data = PathData::from_bez_path(&new_bez);
        }
        commands.push(Command::UpdateNode {
            old: node,
            new: new_node,
        });
        modified += 1;
    }

    if commands.is_empty() {
        return ToolResult::error("No path nodes found in node_ids");
    }

    for cmd in commands {
        history.execute_discrete(cmd, &mut doc);
    }

    ToolResult::text(format!(
        "Applied '{}' warp to {} node(s) (bend={bend}){}",
        args.warp_type, modified,
        if skipped > 0 { format!(" — {skipped} skipped") } else { String::new() },
    ))
    .with_data(serde_json::json!({ "modified": modified, "skipped": skipped, "warp_type": args.warp_type }))
}
pub async fn scallop_path(state: &AppState, args: ScallopPathArgs) -> ToolResult {
    tracing::debug!("tool: scallop_path");

    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let depth = args.depth.unwrap_or(10.0);
    let count = args.count.unwrap_or(1).max(1);

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let mut commands = Vec::new();
    let mut modified = 0usize;
    let mut skipped = 0usize;

    for node_id_str in &args.node_ids {
        let nid = match uuid::Uuid::parse_str(node_id_str) {
            Ok(id) => id,
            Err(_) => match doc.find_node_by_name(node_id_str) {
                Some(n) => n.id,
                None => {
                    skipped += 1;
                    continue;
                }
            },
        };
        let node = match doc.nodes.get(&nid) {
            Some(n) => n.clone(),
            None => {
                skipped += 1;
                continue;
            }
        };
        let pn = match &node.kind {
            SceneNodeKind::Path(pn) => pn,
            _ => {
                skipped += 1;
                continue;
            }
        };

        let bez = pn.path_data.to_bez_path();
        let new_bez = apply_scallop(&bez, depth, count);

        let mut new_node = node.clone();
        if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
            new_pn.path_data = PathData::from_bez_path(&new_bez);
        }
        commands.push(Command::UpdateNode {
            old: node,
            new: new_node,
        });
        modified += 1;
    }

    if commands.is_empty() {
        return ToolResult::error("No path nodes found in node_ids");
    }

    for cmd in commands {
        history.execute_discrete(cmd, &mut doc);
    }

    ToolResult::text(format!(
        "Applied scallop to {} node(s) (depth={depth}, count={count}){}",
        modified,
        if skipped > 0 {
            format!(" — {skipped} skipped")
        } else {
            String::new()
        },
    ))
    .with_data(serde_json::json!({ "modified": modified, "skipped": skipped }))
}
pub async fn crystallize_path(state: &AppState, args: CrystallizePathArgs) -> ToolResult {
    tracing::debug!("tool: crystallize_path");

    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let size = args.size.unwrap_or(10.0);
    let count = args.count.unwrap_or(3).max(1);

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let mut commands = Vec::new();
    let mut modified = 0usize;
    let mut skipped = 0usize;

    for node_id_str in &args.node_ids {
        let nid = match uuid::Uuid::parse_str(node_id_str) {
            Ok(id) => id,
            Err(_) => match doc.find_node_by_name(node_id_str) {
                Some(n) => n.id,
                None => {
                    skipped += 1;
                    continue;
                }
            },
        };
        let node = match doc.nodes.get(&nid) {
            Some(n) => n.clone(),
            None => {
                skipped += 1;
                continue;
            }
        };
        let pn = match &node.kind {
            SceneNodeKind::Path(pn) => pn,
            _ => {
                skipped += 1;
                continue;
            }
        };

        let bez = pn.path_data.to_bez_path();
        let new_bez = apply_crystallize(&bez, size, count);

        let mut new_node = node.clone();
        if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
            new_pn.path_data = PathData::from_bez_path(&new_bez);
        }
        commands.push(Command::UpdateNode {
            old: node,
            new: new_node,
        });
        modified += 1;
    }

    if commands.is_empty() {
        return ToolResult::error("No path nodes found in node_ids");
    }

    for cmd in commands {
        history.execute_discrete(cmd, &mut doc);
    }

    ToolResult::text(format!(
        "Applied crystallize to {} node(s) (size={size}, count={count}){}",
        modified,
        if skipped > 0 {
            format!(" — {skipped} skipped")
        } else {
            String::new()
        },
    ))
    .with_data(serde_json::json!({ "modified": modified, "skipped": skipped }))
}
pub async fn simplify_path(state: &AppState, args: SimplifyPathArgs) -> ToolResult {
    use photonic_core::ops::simplify::{count_points, simplify_path as do_simplify};

    if args.tolerance <= 0.0 {
        return ToolResult::error("tolerance must be > 0");
    }

    let mut doc = state.document.lock().await;
    let old_node = match doc.get_node(&args.node_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Node not found: {}", args.node_id)),
    };

    let path_node = match &old_node.kind {
        SceneNodeKind::Path(p) => p.clone(),
        _ => return ToolResult::error("Node must be a path node"),
    };

    let original_count = count_points(&path_node.path_data);
    let simplified_data = do_simplify(&path_node.path_data, args.tolerance);
    let simplified_count = count_points(&simplified_data);
    let pct = 100.0 * (1.0 - simplified_count as f64 / original_count.max(1) as f64);

    if args.dry_run {
        return ToolResult::text(format!(
            "Dry run: '{}' — {} points → {} points ({:.0}% reduction)",
            old_node.name, original_count, simplified_count, pct
        ))
        .with_data(serde_json::json!({
            "node_id": args.node_id,
            "node_name": old_node.name,
            "original_points": original_count,
            "simplified_points": simplified_count,
            "applied": false,
        }));
    }

    let mut new_path_node = PathNode::new(simplified_data);
    new_path_node.fill = path_node.fill.clone();
    new_path_node.stroke = path_node.stroke.clone();
    new_path_node.is_compound = path_node.is_compound;

    let mut new_node = old_node.clone();
    new_node.kind = SceneNodeKind::Path(new_path_node);

    let cmd = Command::UpdateNode {
        old: old_node,
        new: new_node.clone(),
    };
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!(
        "Simplified '{}': {} → {} points ({:.0}% reduction)",
        new_node.name, original_count, simplified_count, pct
    ))
    .with_data(serde_json::json!({
        "node_id": args.node_id,
        "node_name": new_node.name,
        "original_points": original_count,
        "simplified_points": simplified_count,
        "applied": true,
    }))
}
/// Reverse the winding direction of path node(s). Non-path nodes are silently skipped.
pub async fn reverse_path_direction(
    state: &AppState,
    args: ReversePathDirectionArgs,
) -> ToolResult {
    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let mut commands = Vec::new();
    let mut modified = 0usize;
    let mut skipped = 0usize;

    for node_id in &args.node_ids {
        let node = match doc.nodes.get(node_id) {
            Some(n) => n.clone(),
            None => {
                skipped += 1;
                continue;
            }
        };

        match &node.kind {
            SceneNodeKind::Path(pn) => {
                let new_path = pn.path_data.reverse();
                let mut new_node = node.clone();
                if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                    new_pn.path_data = new_path;
                }
                commands.push(Command::UpdateNode {
                    old: node,
                    new: new_node,
                });
                modified += 1;
            }
            _ => {
                skipped += 1;
            }
        }
    }

    if commands.is_empty() {
        return ToolResult::error("No path nodes found in node_ids");
    }

    for cmd in commands {
        history.execute_discrete(cmd, &mut doc);
    }

    let summary = format!(
        "Reversed path direction on {} node(s){}",
        modified,
        if skipped > 0 {
            format!(" — {} non-path node(s) skipped", skipped)
        } else {
            String::new()
        },
    );
    ToolResult::text(summary).with_data(serde_json::json!({
        "modified": modified,
        "skipped":  skipped,
    }))
}
pub async fn average_anchor_points(state: &AppState, args: AverageAnchorPointsArgs) -> ToolResult {
    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let (avg_x, avg_y) = match args.axis.as_deref().unwrap_or("both") {
        "horizontal" => (true, false),
        "vertical" => (false, true),
        _ => (true, true), // "both" or any unrecognised value
    };

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let mut commands = Vec::new();
    let mut modified = 0usize;
    let mut skipped = 0usize;

    for node_id in &args.node_ids {
        let node = match doc.nodes.get(node_id) {
            Some(n) => n.clone(),
            None => {
                skipped += 1;
                continue;
            }
        };

        let pn = match &node.kind {
            SceneNodeKind::Path(p) => p.clone(),
            _ => {
                skipped += 1;
                continue;
            }
        };

        let new_path = pn.path_data.average_anchor_points(avg_x, avg_y);
        let mut new_node = node.clone();
        if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
            new_pn.path_data = new_path;
        }
        commands.push(Command::UpdateNode {
            old: node,
            new: new_node,
        });
        modified += 1;
    }

    if commands.is_empty() {
        return ToolResult::error("No path nodes found in node_ids");
    }

    for cmd in commands {
        history.execute_discrete(cmd, &mut doc);
    }

    let summary = format!(
        "Averaged anchor points on {} node(s){}",
        modified,
        if skipped > 0 {
            format!(" — {} non-path node(s) skipped", skipped)
        } else {
            String::new()
        },
    );
    ToolResult::text(summary).with_data(serde_json::json!({
        "modified": modified,
        "skipped":  skipped,
        "axis":     args.axis.as_deref().unwrap_or("both"),
    }))
}
pub async fn outline_stroke(state: &AppState, args: OutlineStrokeArgs) -> ToolResult {
    use photonic_core::ops::stroke_outline::{
        outline_stroke_with_scale as do_outline, transform_uniform_scale,
    };
    use photonic_core::style::{Fill, FillKind, Stroke};

    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let mut commands = Vec::new();
    let mut outlined_ids = Vec::new();
    let mut original_ids = Vec::new();
    let mut skipped = 0usize;

    for node_id in &args.node_ids {
        let node = match doc.nodes.get(node_id) {
            Some(n) => n.clone(),
            None => {
                skipped += 1;
                continue;
            }
        };

        let pn = match &node.kind {
            SceneNodeKind::Path(p) => p.clone(),
            _ => {
                skipped += 1;
                continue;
            }
        };

        // Non-scaling stroke: divide width by the object's transform scale so
        // the outline matches the drawn stroke (the outline keeps this transform).
        let obj_scale = transform_uniform_scale(&node.transform.matrix);
        let outline_data = match do_outline(&pn.path_data, &pn.stroke, obj_scale) {
            Ok(d) => d,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let layer_id = node.layer_id;

        // Build outlined path node: fill = stroke color, stroke disabled.
        let mut outline_pn = PathNode::new(outline_data);
        outline_pn.fill = Fill {
            kind: FillKind::Solid(pn.stroke.color),
            opacity: pn.stroke.opacity,
            enabled: true,
        };
        outline_pn.stroke = Stroke::none();

        let outline_node = SceneNode::new(
            &format!("{} outline", node.name),
            layer_id,
            SceneNodeKind::Path(outline_pn),
        );
        let outline_id = outline_node.id;

        // Disable stroke on the original node.
        let mut updated_orig = node.clone();
        if let SceneNodeKind::Path(ref mut op) = updated_orig.kind {
            op.stroke.enabled = false;
        }

        commands.push(Command::Batch(vec![
            Command::AddNode {
                node: outline_node,
                layer_id: Some(layer_id),
            },
            Command::UpdateNode {
                old: node.clone(),
                new: updated_orig,
            },
        ]));

        outlined_ids.push(outline_id.to_string());
        original_ids.push(node_id.to_string());
    }

    if commands.is_empty() {
        return ToolResult::error(
            "No eligible path nodes found — each node must be a path with an enabled stroke",
        );
    }

    let modified = outlined_ids.len();
    let batch = if commands.len() == 1 {
        commands.remove(0)
    } else {
        Command::Batch(commands)
    };
    history.execute_discrete(batch, &mut doc);

    let summary = format!(
        "Outlined stroke on {} node(s){}",
        modified,
        if skipped > 0 {
            format!(" — {} node(s) skipped", skipped)
        } else {
            String::new()
        },
    );
    ToolResult::text(summary).with_data(serde_json::json!({
        "outlined_ids": outlined_ids,
        "original_ids": original_ids,
        "skipped":      skipped,
    }))
}
/// Offset (expand or inset) one or more path nodes by a fixed distance.
pub async fn offset_path(state: &AppState, args: OffsetPathArgs) -> ToolResult {
    use kurbo::Join;
    use photonic_core::ops::offset::offset_path as do_offset;

    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let join = match args.join_style.as_deref().unwrap_or("miter") {
        "round" => Join::Round,
        "bevel" => Join::Bevel,
        _ => Join::Miter,
    };
    let create_copy = args.create_copy.unwrap_or(true);

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let mut commands = Vec::new();
    let mut processed = Vec::new();
    let mut skipped = 0usize;

    for node_id in &args.node_ids {
        let node = match doc.nodes.get(node_id) {
            Some(n) => n.clone(),
            None => {
                skipped += 1;
                continue;
            }
        };

        let pn = match &node.kind {
            SceneNodeKind::Path(p) => p.clone(),
            _ => {
                skipped += 1;
                continue;
            }
        };

        let offset_data = match do_offset(&pn.path_data, args.distance, join) {
            Ok(d) => d,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let layer_id = node.layer_id;

        if create_copy {
            let mut new_pn = pn.clone();
            new_pn.path_data = offset_data;
            let new_node = SceneNode::new(
                &format!("{} offset", node.name),
                layer_id,
                SceneNodeKind::Path(new_pn),
            );
            let new_id = new_node.id.to_string();
            commands.push(Command::AddNode {
                node: new_node,
                layer_id: Some(layer_id),
            });
            processed.push(new_id);
        } else {
            let mut new_node = node.clone();
            if let SceneNodeKind::Path(ref mut p) = new_node.kind {
                p.path_data = offset_data;
            }
            commands.push(Command::UpdateNode {
                old: node,
                new: new_node,
            });
            processed.push(node_id.to_string());
        }
    }

    if commands.is_empty() {
        return ToolResult::error("No path nodes found or all offsets failed (path may be too small to inset by this distance)");
    }

    let batch = if commands.len() == 1 {
        commands.remove(0)
    } else {
        Command::Batch(commands)
    };
    history.execute_discrete(batch, &mut doc);

    ToolResult::text(format!(
        "Offset {} path(s) by {}{:.1} units{}",
        processed.len(),
        if args.distance >= 0.0 { "+" } else { "" },
        args.distance,
        if skipped > 0 {
            format!(" — {} node(s) skipped", skipped)
        } else {
            String::new()
        },
    ))
    .with_data(serde_json::json!({
        "processed": processed,
        "skipped":   skipped,
    }))
}
/// Close or join path nodes.
///
/// * **1 node_id** — appends `ClosePath` to every open subpath in the node.
/// * **2 node_ids** — merges the two paths into one by connecting their nearest
///   open endpoints with a straight line; the result replaces the first node
///   and the second node is deleted.
pub async fn join_paths(state: &AppState, args: JoinPathsArgs) -> ToolResult {
    use photonic_core::ops::join::{close_open_paths, join_two_paths};

    let n = args.node_ids.len();
    if n == 0 || n > 2 {
        return ToolResult::error("node_ids must contain 1 or 2 path node IDs");
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    if n == 1 {
        // ── Close a single path ──────────────────────────────────────────────
        let nid = args.node_ids[0];
        let node = match doc.nodes.get(&nid) {
            Some(n) => n.clone(),
            None => return ToolResult::error(format!("node {} not found", nid)),
        };
        let pn = match &node.kind {
            SceneNodeKind::Path(p) => p.clone(),
            _ => return ToolResult::error("node is not a path node"),
        };

        let new_path = close_open_paths(&pn.path_data);
        let mut new_node = node.clone();
        if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
            new_pn.path_data = new_path;
        }
        history.execute_discrete(
            Command::UpdateNode {
                old: node,
                new: new_node.clone(),
            },
            &mut doc,
        );

        ToolResult::text("Closed open subpaths.").with_data(serde_json::json!({
            "operation":  "closed",
            "result_id":  new_node.id,
        }))
    } else {
        // ── Join two paths ───────────────────────────────────────────────────
        let id_a = args.node_ids[0];
        let id_b = args.node_ids[1];

        let node_a = match doc.nodes.get(&id_a) {
            Some(n) => n.clone(),
            None => return ToolResult::error(format!("node {} not found", id_a)),
        };
        let node_b = match doc.nodes.get(&id_b) {
            Some(n) => n.clone(),
            None => return ToolResult::error(format!("node {} not found", id_b)),
        };

        let pn_a = match &node_a.kind {
            SceneNodeKind::Path(p) => p.clone(),
            _ => return ToolResult::error(format!("node {} is not a path node", id_a)),
        };
        let pn_b = match &node_b.kind {
            SceneNodeKind::Path(p) => p.clone(),
            _ => return ToolResult::error(format!("node {} is not a path node", id_b)),
        };

        let merged = join_two_paths(&pn_a.path_data, &pn_b.path_data);
        let mut result_node = node_a.clone();
        if let SceneNodeKind::Path(ref mut new_pn) = result_node.kind {
            new_pn.path_data = merged;
        }

        history.execute_discrete(
            Command::Batch(vec![
                Command::UpdateNode {
                    old: node_a,
                    new: result_node.clone(),
                },
                Command::RemoveNode { node_id: id_b },
            ]),
            &mut doc,
        );

        ToolResult::text("Joined two paths into one.").with_data(serde_json::json!({
            "operation":  "joined",
            "result_id":  result_node.id,
            "removed_id": id_b,
        }))
    }
}
/// Cut a path node at the point on it nearest to `(canvas_x, canvas_y)`,
/// producing two new open path nodes with the same style as the original.
pub async fn scissors_cut(state: &AppState, args: ScissorsCutArgs) -> ToolResult {
    let mut doc = state.document.lock().await;

    let node = match doc.nodes.get(&args.node_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Node {} not found", args.node_id)),
    };

    let pn = match &node.kind {
        SceneNodeKind::Path(pn) => pn.clone(),
        _ => return ToolResult::error("scissors_cut only works on path nodes"),
    };

    if pn.path_data.is_empty() {
        return ToolResult::error("Path has no segments to cut");
    }

    // Transform the canvas point into the node's local coordinate space.
    let inv = node.transform.to_kurbo().inverse();
    let local_pt = inv * kurbo::Point::new(args.canvas_x, args.canvas_y);
    let (lx, ly) = (local_pt.x, local_pt.y);

    let (path_before, path_after) =
        match pn.path_data.split_at_point(lx, ly) {
            Some(pair) => pair,
            None => return ToolResult::error(
                "Could not split path — cut point may be at an endpoint or the path is degenerate",
            ),
        };

    let layer_id = node.layer_id;

    // Build two new nodes inheriting the original's style.
    let mut node_before = SceneNode::new(
        format!("{} (1/2)", node.name),
        layer_id,
        SceneNodeKind::Path(PathNode {
            path_data: path_before,
            ..pn.clone()
        }),
    );
    node_before.transform = node.transform.clone();
    node_before.opacity = node.opacity;
    node_before.blend_mode = node.blend_mode;

    let mut node_after = SceneNode::new(
        format!("{} (2/2)", node.name),
        layer_id,
        SceneNodeKind::Path(PathNode {
            path_data: path_after,
            ..pn.clone()
        }),
    );
    node_after.transform = node.transform.clone();
    node_after.opacity = node.opacity;
    node_after.blend_mode = node.blend_mode;

    let id_before = node_before.id;
    let id_after = node_after.id;

    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::Batch(vec![
            Command::RemoveNode {
                node_id: args.node_id,
            },
            Command::AddNode {
                node: node_before,
                layer_id: Some(layer_id),
            },
            Command::AddNode {
                node: node_after,
                layer_id: Some(layer_id),
            },
        ]),
        &mut doc,
    );

    ToolResult::text(format!("Cut path into 2 open paths")).with_data(serde_json::json!({
        "node_before_id": id_before,
        "node_after_id": id_after,
    }))
}
/// Convert all cubic anchor points in selected path nodes to smooth or corner joins.
pub async fn convert_anchor_points(state: &AppState, args: ConvertAnchorPointsArgs) -> ToolResult {
    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let mut cmds: Vec<Command> = Vec::new();
    let mut skipped = 0usize;
    let mut converted = 0usize;

    for &nid in &args.node_ids {
        let node = match doc.nodes.get(&nid) {
            Some(n) => n.clone(),
            None => {
                skipped += 1;
                continue;
            }
        };
        let pn = match &node.kind {
            SceneNodeKind::Path(p) => p.clone(),
            _ => {
                skipped += 1;
                continue;
            }
        };

        let new_path = match args.mode {
            ConvertAnchorMode::Smooth => pn.path_data.convert_to_smooth(),
            ConvertAnchorMode::Corner => pn.path_data.convert_to_corner(),
        };

        let mut new_node = node.clone();
        if let SceneNodeKind::Path(ref mut np) = new_node.kind {
            np.path_data = new_path;
        }
        cmds.push(Command::UpdateNode {
            old: node,
            new: new_node,
        });
        converted += 1;
    }

    if cmds.is_empty() {
        return ToolResult::error("No path nodes found in node_ids");
    }

    let cmd = if cmds.len() == 1 {
        cmds.remove(0)
    } else {
        Command::Batch(cmds)
    };
    history.execute_discrete(cmd, &mut doc);

    let mode_label = match args.mode {
        ConvertAnchorMode::Smooth => "smooth",
        ConvertAnchorMode::Corner => "corner",
    };
    ToolResult::text(format!(
        "Converted {} node(s) to {} anchors ({} skipped).",
        converted, mode_label, skipped
    ))
    .with_data(serde_json::json!({
        "converted": converted,
        "skipped": skipped,
        "mode": mode_label,
    }))
}
/// Create a freehand polyline path from an ordered list of canvas-space points.
pub async fn create_freehand_path(state: &AppState, args: CreateFreehandPathArgs) -> ToolResult {
    if args.points.len() < 2 {
        return ToolResult::error("create_freehand_path requires at least 2 points");
    }

    // Build SVG path string.
    let first = args.points[0];
    let mut svg = format!("M {:.4} {:.4}", first[0], first[1]);
    for pt in &args.points[1..] {
        svg.push_str(&format!(" L {:.4} {:.4}", pt[0], pt[1]));
    }
    let path_data = match PathData::from_svg(&svg) {
        Ok(p) => p,
        Err(e) => return ToolResult::error(format!("Failed to build path: {}", e)),
    };

    let mut path_node = PathNode::new(path_data);
    if let Err(e) = apply_style(&mut path_node, args.fill, args.stroke) {
        return ToolResult::error(e);
    }

    let name = args.name.unwrap_or_else(|| "Pencil".to_string());
    let mut doc = state.document.lock().await;
    let node_id;
    {
        let node = SceneNode::new(&name, uuid::Uuid::nil(), SceneNodeKind::Path(path_node));
        node_id = node.id;
        let cmd = Command::AddNode {
            node,
            layer_id: None,
        };
        let mut history = state.history.lock().await;
        history.execute_discrete(cmd, &mut doc);
    }

    ToolResult::text(format!(
        "Created freehand path '{}' ({} points, id: {})",
        name,
        args.points.len(),
        node_id
    ))
    .with_data(serde_json::json!({ "node_id": node_id }))
}
/// Smooth path nodes using Chaikin's corner-cutting algorithm.
pub async fn smooth_path(state: &AppState, args: SmoothPathArgs) -> ToolResult {
    let mut doc = state.document.lock().await;
    let factor = args.factor.clamp(0.0, 0.5);
    let iterations = args.iterations.min(8);

    let ids: Vec<NodeId> = if args.node_ids.is_empty() {
        doc.selection.ids().copied().collect()
    } else {
        args.node_ids
    };

    if ids.is_empty() {
        return ToolResult::text("No nodes specified or selected.");
    }

    let mut cmds = Vec::new();
    let mut smoothed = 0usize;
    for id in &ids {
        if let Some(node) = doc.nodes.get(id) {
            if let SceneNodeKind::Path(pn) = &node.kind {
                let new_path = pn.path_data.smooth(factor, iterations);
                let mut new_node = node.clone();
                if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                    new_pn.path_data = new_path;
                }
                cmds.push(Command::UpdateNode {
                    old: node.clone(),
                    new: new_node,
                });
                smoothed += 1;
            }
        }
    }

    if cmds.is_empty() {
        return ToolResult::text("No path nodes found in the specified IDs.");
    }

    let batch = if cmds.len() == 1 {
        cmds.remove(0)
    } else {
        Command::Batch(cmds)
    };
    let mut history = state.history.lock().await;
    history.execute_discrete(batch, &mut doc);

    ToolResult::text(format!(
        "Smoothed {} path node(s) with factor={:.2}, iterations={}.",
        smoothed, factor, iterations
    ))
}
/// Displace every anchor point and control point in selected paths using
/// a smooth sinusoidal field, producing organic wave-like deformation.
pub async fn noise_deform(state: &AppState, args: NoiseDeformArgs) -> ToolResult {
    tracing::debug!("tool: noise_deform");

    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let amplitude = args.amplitude.unwrap_or(8.0);
    let frequency = args.frequency.unwrap_or(0.05);
    let seed = args.seed.unwrap_or(0.0);
    let axis = args.axis.as_deref().unwrap_or("both");

    let deform_x = axis == "both" || axis == "x";
    let deform_y = axis == "both" || axis == "y";

    // Displace a single point using two-octave sinusoidal noise.
    let displace = |pt: kurbo::Point| -> kurbo::Point {
        let dx = if deform_x {
            amplitude * (pt.y * frequency + seed).sin()
                + (amplitude * 0.5) * (pt.y * frequency * 2.1 + seed * 1.3).sin()
        } else {
            0.0
        };
        let dy = if deform_y {
            amplitude * (pt.x * frequency + seed + std::f64::consts::FRAC_PI_2).sin()
                + (amplitude * 0.5) * (pt.x * frequency * 2.1 + seed * 1.7).sin()
        } else {
            0.0
        };
        kurbo::Point::new(pt.x + dx, pt.y + dy)
    };

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let mut commands = Vec::new();
    let mut modified = 0usize;
    let mut skipped = 0usize;

    for node_id_str in &args.node_ids {
        let nid = match uuid::Uuid::parse_str(node_id_str) {
            Ok(id) => id,
            Err(_) => match doc.find_node_by_name(node_id_str) {
                Some(n) => n.id,
                None => {
                    skipped += 1;
                    continue;
                }
            },
        };
        let node = match doc.nodes.get(&nid) {
            Some(n) => n.clone(),
            None => {
                skipped += 1;
                continue;
            }
        };
        let pn = match &node.kind {
            SceneNodeKind::Path(pn) => pn,
            _ => {
                skipped += 1;
                continue;
            }
        };

        let bez = pn.path_data.to_bez_path();

        let new_els: Vec<kurbo::PathEl> = bez
            .iter()
            .map(|el| match el {
                kurbo::PathEl::MoveTo(p) => kurbo::PathEl::MoveTo(displace(p)),
                kurbo::PathEl::LineTo(p) => kurbo::PathEl::LineTo(displace(p)),
                kurbo::PathEl::QuadTo(p1, p2) => kurbo::PathEl::QuadTo(displace(p1), displace(p2)),
                kurbo::PathEl::CurveTo(p1, p2, p3) => {
                    kurbo::PathEl::CurveTo(displace(p1), displace(p2), displace(p3))
                }
                kurbo::PathEl::ClosePath => kurbo::PathEl::ClosePath,
            })
            .collect();

        let new_bez = kurbo::BezPath::from_vec(new_els);
        let mut new_node = node.clone();
        if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
            new_pn.path_data = PathData::from_bez_path(&new_bez);
        }
        commands.push(Command::UpdateNode {
            old: node,
            new: new_node,
        });
        modified += 1;
    }

    if commands.is_empty() {
        return ToolResult::error("No path nodes found in node_ids");
    }

    for cmd in commands {
        history.execute_discrete(cmd, &mut doc);
    }

    ToolResult::text(format!(
        "Noise-deformed {} path node(s) (amplitude={:.1}, frequency={:.4}, axis={}, seed={:.2}). Skipped: {}.",
        modified, amplitude, frequency, axis, seed, skipped
    ))
}
