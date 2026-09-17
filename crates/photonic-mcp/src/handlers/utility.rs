use crate::handlers::nodes::{generate_name, is_generic_name};
use crate::handlers::shared::{spatial::world_aabb, styling::*};
use crate::protocol::*;
use crate::server::AppState;
use kurbo;
use photonic_core::{
    history::Command,
    layer::BlendMode,
    node::{GroupNode, NodeId, SceneNode, SceneNodeKind},
};

pub async fn update_node(state: &AppState, args: UpdateNodeArgs) -> ToolResult {
    tracing::debug!("tool: update_node {}", args.node_id);
    // Read phase: clone the node, then immediately release the doc lock.
    let old_node = {
        let doc = state.document.lock().await;
        match doc.get_node(&args.node_id) {
            Some(n) => n.clone(),
            None => return ToolResult::error(format!("Node {} not found", args.node_id)),
        }
    }; // doc lock released here

    // Prepare phase: build the updated node — no locks held.
    let mut new_node = old_node.clone();

    if let Some(name) = args.name {
        new_node.name = name;
    }
    if let Some(opacity) = args.opacity {
        new_node.opacity = opacity;
    }
    if let Some(visible) = args.visible {
        new_node.visible = visible;
    }
    if let Some(locked) = args.locked {
        new_node.locked = locked;
    }
    if let Some(blend_mode) = args.blend_mode {
        if blend_mode != BlendMode::Normal {
            return ToolResult::error(
                "Blend modes other than 'normal' are not yet rendered. \
                 Set blend_mode to 'normal' (or omit it) until blend mode \
                 rendering is implemented.",
            );
        }
        new_node.blend_mode = blend_mode;
    }
    if let Some(tags) = args.tags {
        new_node.tags = tags;
    }
    if let Some(og) = args.outer_glow {
        new_node.outer_glow = og.into();
    }
    if let Some(ig) = args.inner_glow {
        new_node.inner_glow = ig.into();
    }
    if let Some(gg) = args.gaussian_glow {
        new_node.gaussian_glow = gg.into();
    }
    if let Some(ds) = args.drop_shadow {
        new_node.drop_shadow = ds.into();
    }
    if let Some(ob) = args.object_blur {
        new_node.object_blur = ob.into();
    }
    if let Some(ft) = args.feather {
        new_node.feather = ft.into();
    }
    if let Some(t_arg) = args.transform {
        new_node.transform = t_arg.to_transform();
    }

    match &mut new_node.kind {
        SceneNodeKind::Path(ref mut path_node) => {
            if let Err(e) = apply_style(path_node, args.fill, args.stroke) {
                return ToolResult::error(e);
            }
        }
        SceneNodeKind::Text(ref mut text_node) => {
            use photonic_core::node::TextAlign;
            if let Some(content) = args.content {
                text_node.content = content;
            }
            if let Some(ff) = args.font_family {
                text_node.font_family = ff;
            }
            if let Some(fs) = args.font_size {
                text_node.font_size = fs;
            }
            if let Some(fw) = args.font_weight {
                text_node.font_weight = fw;
            }
            if let Some(ref a) = args.text_align {
                text_node.align = match a.as_str() {
                    "center" => TextAlign::Center,
                    "right" => TextAlign::Right,
                    _ => TextAlign::Left,
                };
            }
            if let Some(fill_arg) = args.fill {
                match fill_arg.to_fill() {
                    Ok(f) => text_node.fill = f,
                    Err(e) => return ToolResult::error(e),
                }
            }
            if let Some(stroke_arg) = args.stroke {
                match stroke_arg.to_stroke() {
                    Ok(s) => text_node.stroke = s,
                    Err(e) => return ToolResult::error(e),
                }
            }
        }
        SceneNodeKind::Group(_) => {}
        // raster: no vector fill/stroke/text properties to update
        SceneNodeKind::Raster(_) => {}
    }

    // Write phase: acquire both locks, execute synchronously, release both.
    let cmd = Command::UpdateNode {
        old: old_node,
        new: new_node,
    };
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!("Updated node {}", args.node_id))
}
pub async fn delete_nodes(state: &AppState, args: DeleteNodeArgs) -> ToolResult {
    tracing::debug!("tool: delete_nodes (count={})", args.node_ids.len());
    let count = args.node_ids.len();
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    // Deleting a group must remove its whole subtree — a bare RemoveNode would
    // leave the children orphaned in the document (unreachable but still present,
    // and still found by find_nodes). Use RemoveSubtree (undo-safe via its
    // AddSubtree inverse) for any node with descendants; RemoveNode for leaves.
    let mut commands: Vec<Command> = Vec::new();
    for &node_id in &args.node_ids {
        let Some(node) = doc.nodes.get(&node_id) else {
            continue;
        };
        let layer_id = node.layer_id;
        if matches!(node.kind, SceneNodeKind::Group(_)) {
            let nodes: Vec<SceneNode> = crate::handlers::clipboard::collect_subtree(&doc, node_id)
                .into_values()
                .collect();
            commands.push(Command::RemoveSubtree {
                layer_id,
                roots: vec![node_id],
                nodes,
            });
        } else {
            commands.push(Command::RemoveNode { node_id });
        }
    }
    // One Batch = the doc lock is taken once and the whole delete is one undo step.
    history.execute_discrete(Command::Batch(commands), &mut doc);
    ToolResult::text(format!("Deleted {} node(s)", count))
}
pub async fn get_node(state: &AppState, args: GetNodeArgs) -> ToolResult {
    let doc = state.document.lock().await;

    let node = if let Some(id) = args.node_id {
        doc.get_node(&id).cloned()
    } else if let Some(name) = &args.name {
        doc.find_node_by_name(name).cloned()
    } else {
        return ToolResult::error("Provide either node_id or name");
    };

    match node {
        Some(n) => ToolResult::text(format!("Node '{}'", n.name)).with_data(&n),
        None => ToolResult::error("Node not found"),
    }
}
pub async fn group_nodes(state: &AppState, args: GroupNodesArgs) -> ToolResult {
    if args.node_ids.len() < 2 {
        return ToolResult::error("group_nodes requires at least 2 node_ids");
    }

    let mut doc = state.document.lock().await;

    let (layer_id, mut indexed) = match doc.nodes_layer_and_indices(&args.node_ids) {
        Some(v) => v,
        None => return ToolResult::error("All nodes must exist and belong to the same layer"),
    };

    // Sort children bottom-to-top (ascending index)
    indexed.sort_by_key(|(_, idx)| *idx);
    let children: Vec<NodeId> = indexed.iter().map(|(id, _)| *id).collect();
    let insert_index = indexed[0].1; // position of bottom-most child

    let group_name = args.name.unwrap_or_else(|| "Group".to_string());
    let group_kind = SceneNodeKind::Group(GroupNode {
        children: children.clone(),
        clip_children: false,
        clip_node_id: None,
        blend_spine_id: None,
        live_boolean: None,
    });
    let group = SceneNode::new(&group_name, layer_id, group_kind);
    let group_id = group.id;

    let cmd = Command::GroupNodes {
        group,
        layer_id,
        insert_index,
        children,
    };
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!(
        "Grouped {} nodes into '{}' (id: {})",
        args.node_ids.len(),
        group_name,
        group_id
    ))
    .with_data(serde_json::json!({ "group_id": group_id }))
}
pub async fn make_live_boolean(
    state: &AppState,
    args: crate::protocol::args::MakeLiveBooleanArgs,
) -> ToolResult {
    use photonic_core::ops::boolean::BooleanOp;
    if args.node_ids.len() < 2 {
        return ToolResult::error("make_live_boolean requires at least 2 node_ids");
    }
    let op = match args.operation.trim().to_lowercase().as_str() {
        "union" | "add" => BooleanOp::Union,
        "intersect" | "intersection" => BooleanOp::Intersect,
        "subtract" | "minus" | "minus_front" => BooleanOp::Subtract,
        "exclude" | "xor" => BooleanOp::Exclude,
        "divide" => BooleanOp::Divide,
        other => {
            return ToolResult::error(format!(
                "Unknown operation '{other}' (union|intersect|subtract|exclude|divide)"
            ))
        }
    };

    let mut doc = state.document.lock().await;
    // Every operand must be a path node for the boolean to be meaningful.
    for &id in &args.node_ids {
        match doc.nodes.get(&id) {
            Some(n) if matches!(n.kind, SceneNodeKind::Path(_)) => {}
            Some(_) => return ToolResult::error(format!("Node {id} is not a path node")),
            None => return ToolResult::error(format!("Node {id} not found")),
        }
    }

    let (layer_id, mut indexed) = match doc.nodes_layer_and_indices(&args.node_ids) {
        Some(v) => v,
        None => return ToolResult::error("All nodes must exist and belong to the same layer"),
    };
    indexed.sort_by_key(|(_, idx)| *idx);
    let children: Vec<NodeId> = indexed.iter().map(|(id, _)| *id).collect();
    let insert_index = indexed[0].1;

    let group_name = args.name.unwrap_or_else(|| "Live Boolean".to_string());
    let group_kind = SceneNodeKind::Group(GroupNode {
        children: children.clone(),
        clip_children: false,
        clip_node_id: None,
        blend_spine_id: None,
        live_boolean: Some(op),
    });
    let group = SceneNode::new(&group_name, layer_id, group_kind);
    let group_id = group.id;

    let cmd = Command::GroupNodes {
        group,
        layer_id,
        insert_index,
        children,
    };
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!(
        "Made live {op:?} boolean '{group_name}' (id: {group_id}) from {} paths",
        args.node_ids.len()
    ))
    .with_data(serde_json::json!({ "group_id": group_id }))
}

pub async fn ungroup_nodes(state: &AppState, args: UngroupNodesArgs) -> ToolResult {
    let mut doc = state.document.lock().await;

    let group_node = match doc.get_node(&args.group_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Node {} not found", args.group_id)),
    };

    let children = match &group_node.kind {
        SceneNodeKind::Group(g) => g.children.clone(),
        _ => return ToolResult::error("Node is not a group"),
    };

    let (layer_id, group_index) = match doc.node_layer_and_index(&args.group_id) {
        Some(v) => v,
        None => return ToolResult::error("Group node has no layer position"),
    };

    let child_count = children.len();
    let cmd = Command::UngroupNodes {
        group: group_node,
        layer_id,
        group_index,
        children,
    };
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!(
        "Ungrouped {} into {} child node(s)",
        args.group_id, child_count
    ))
}
/// Measure the world-space bounding boxes and spatial relationships of one or
/// more nodes. Applies each node's transform to its local bounds to produce the
/// actual axis-aligned bounding box (AABB) on screen.
///
/// Returns per-node `world_bounds` and `center`, the `combined_bounds` of the
/// entire selection, and — when exactly two nodes are provided — pairwise
/// `center_to_center_distance` and `angle_degrees` (0° = right, 90° = down).
pub async fn measure_nodes(
    state: &AppState,
    args: crate::protocol::MeasureNodesArgs,
) -> ToolResult {
    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    fn r2(v: f64) -> f64 {
        (v * 100.0).round() / 100.0
    }

    // Collect measurements under a single read lock.
    struct Item {
        id: uuid::Uuid,
        name: String,
        aabb: Option<[f64; 4]>,
    }

    let items: Vec<Item> = {
        let doc = state.document.lock().await;
        let mut out = Vec::with_capacity(args.node_ids.len());
        for id in &args.node_ids {
            let Some(node) = doc.get_node(id) else {
                return ToolResult::error(format!("Node not found: {}", id));
            };
            out.push(Item {
                id: *id,
                name: node.name.clone(),
                aabb: world_aabb(node),
            });
        }
        out
    };

    // Combined bounding box over all nodes that have known bounds.
    let combined = {
        let rects: Vec<[f64; 4]> = items.iter().filter_map(|m| m.aabb).collect();
        if rects.is_empty() {
            None
        } else {
            let x0 = rects.iter().map(|r| r[0]).fold(f64::INFINITY, f64::min);
            let y0 = rects.iter().map(|r| r[1]).fold(f64::INFINITY, f64::min);
            let x1 = rects
                .iter()
                .map(|r| r[0] + r[2])
                .fold(f64::NEG_INFINITY, f64::max);
            let y1 = rects
                .iter()
                .map(|r| r[1] + r[3])
                .fold(f64::NEG_INFINITY, f64::max);
            Some([x0, y0, x1 - x0, y1 - y0])
        }
    };

    // Pairwise metrics only when exactly two nodes are given.
    let pairwise = if items.len() == 2 {
        let center = |aabb: [f64; 4]| (aabb[0] + aabb[2] / 2.0, aabb[1] + aabb[3] / 2.0);
        match (items[0].aabb, items[1].aabb) {
            (Some(a), Some(b)) => {
                let (ax, ay) = center(a);
                let (bx, by) = center(b);
                let dx = bx - ax;
                let dy = by - ay;
                let dist = (dx * dx + dy * dy).sqrt();
                let angle = dy.atan2(dx).to_degrees();
                Some(serde_json::json!({
                    "center_to_center_distance": r2(dist),
                    "angle_degrees": r2(angle),
                }))
            }
            _ => None,
        }
    } else {
        None
    };

    // Serialize per-node results.
    let nodes_json: Vec<_> = items
        .iter()
        .map(|m| {
            let bounds_json = m.aabb.map(|[x, y, w, h]| {
                serde_json::json!({ "x": r2(x), "y": r2(y), "width": r2(w), "height": r2(h) })
            });
            let center_json = m.aabb.map(
                |[x, y, w, h]| serde_json::json!({ "x": r2(x + w / 2.0), "y": r2(y + h / 2.0) }),
            );
            serde_json::json!({
                "id": m.id,
                "name": m.name,
                "world_bounds": bounds_json,
                "center": center_json,
            })
        })
        .collect();

    let combined_json = combined.map(|[x, y, w, h]| {
        serde_json::json!({ "x": r2(x), "y": r2(y), "width": r2(w), "height": r2(h) })
    });

    let mut data = serde_json::json!({
        "nodes": nodes_json,
        "combined_bounds": combined_json,
    });
    if let Some(p) = pairwise {
        data["pairwise"] = p;
    }

    ToolResult::text(format!("Measured {} node(s)", items.len())).with_data(data)
}
/// Resize a node to exact pixel dimensions in one step.
///
/// Eliminates the two-round-trip pattern of `measure_nodes` → compute scale →
/// `apply_transform`. The world-space AABB of the node is computed internally;
/// a scale transform is derived and composed onto the node's existing transform
/// so that the result has the requested dimensions.
pub async fn set_node_size(state: &AppState, args: crate::protocol::SetNodeSizeArgs) -> ToolResult {
    use crate::protocol::SizeAnchor;
    use photonic_core::{history::Command, transform::Transform};

    // ── 1. Validate args ─────────────────────────────────────────────────────
    if args.width.is_none() && args.height.is_none() {
        return ToolResult::error("At least one of `width` or `height` must be provided");
    }
    if let Some(w) = args.width {
        if w <= 0.0 {
            return ToolResult::error("`width` must be greater than 0");
        }
    }
    if let Some(h) = args.height {
        if h <= 0.0 {
            return ToolResult::error("`height` must be greater than 0");
        }
    }

    // ── 2. Compute world AABB (same logic as `measure_nodes`) ────────────────
    let (old_node, aabb) = {
        let doc = state.document.lock().await;
        let Some(node) = doc.get_node(&args.node_id) else {
            return ToolResult::error(format!("Node not found: {}", args.node_id));
        };
        let Some(aabb) = world_aabb(node) else {
            return ToolResult::error(
                "Cannot resize this node — it has no computable bounding box (e.g. empty group)",
            );
        };
        (node.clone(), aabb)
    };

    let [ax, ay, cur_w, cur_h] = aabb;

    if cur_w < 1e-9 || cur_h < 1e-9 {
        return ToolResult::error(
            "Cannot resize: the node's bounding box has zero or near-zero dimensions",
        );
    }

    // ── 3. Compute scale factors ─────────────────────────────────────────────
    let (mut sx, mut sy) = match (args.width, args.height) {
        (Some(tw), Some(th)) => (tw / cur_w, th / cur_h),
        (Some(tw), None) => (tw / cur_w, tw / cur_w), // both uniform until aspect check
        (None, Some(th)) => (th / cur_h, th / cur_h),
        (None, None) => unreachable!(),
    };

    // When both dimensions are given and aspect ratio must be maintained, fit
    // inside the requested box (use the smaller of the two scale factors).
    if args.maintain_aspect_ratio {
        if let (Some(tw), Some(th)) = (args.width, args.height) {
            let s = (tw / cur_w).min(th / cur_h);
            sx = s;
            sy = s;
        }
        // single-dimension + maintain_aspect_ratio: already set sx==sy above
    } else if args.width.is_some() && args.height.is_some() {
        // both given, no aspect constraint: scale axes independently (already done above)
    }

    // ── 4. Anchor point in world space ───────────────────────────────────────
    let (origin_x, origin_y) = match args.anchor {
        SizeAnchor::TopLeft => (ax, ay),
        SizeAnchor::TopCenter => (ax + cur_w / 2.0, ay),
        SizeAnchor::TopRight => (ax + cur_w, ay),
        SizeAnchor::LeftCenter => (ax, ay + cur_h / 2.0),
        SizeAnchor::Center => (ax + cur_w / 2.0, ay + cur_h / 2.0),
        SizeAnchor::RightCenter => (ax + cur_w, ay + cur_h / 2.0),
        SizeAnchor::BottomLeft => (ax, ay + cur_h),
        SizeAnchor::BottomCenter => (ax + cur_w / 2.0, ay + cur_h),
        SizeAnchor::BottomRight => (ax + cur_w, ay + cur_h),
    };

    // ── 5. Build new transform ───────────────────────────────────────────────
    // Compose: existing local→world transform, then world-space scale around anchor.
    let scale_t = Transform::scale_around(sx, sy, origin_x, origin_y);
    let new_transform = old_node.transform.then(&scale_t);

    let mut new_node = old_node.clone();
    new_node.transform = new_transform;

    let cmd = Command::UpdateNode {
        old: old_node.clone(),
        new: new_node,
    };
    {
        let mut doc = state.document.lock().await;
        let mut history = state.history.lock().await;
        history.execute_discrete(cmd, &mut doc);
    }

    let new_w = (cur_w * sx * 100.0).round() / 100.0;
    let new_h = (cur_h * sy * 100.0).round() / 100.0;

    ToolResult::text(format!(
        "Resized '{}' to {:.2}×{:.2} px (was {:.2}×{:.2} px)",
        old_node.name, new_w, new_h, cur_w, cur_h
    ))
    .with_data(serde_json::json!({
        "node_id": args.node_id,
        "previous": { "width": (cur_w * 100.0).round() / 100.0, "height": (cur_h * 100.0).round() / 100.0 },
        "new":      { "width": new_w, "height": new_h },
        "scale":    { "sx": (sx * 10000.0).round() / 10000.0, "sy": (sy * 10000.0).round() / 10000.0 },
    }))
}
/// Return computed geometry and structure data for a single node.
pub async fn inspect_node(state: &AppState, args: InspectNodeArgs) -> ToolResult {
    use kurbo::Shape;

    // Resolve node and clone the full node map under a brief lock.
    let (node, node_map) = {
        let doc = state.document.lock().await;
        let found = if let Ok(uuid) = uuid::Uuid::parse_str(&args.id) {
            doc.get_node(&uuid).cloned()
        } else {
            doc.find_node_by_name(&args.id).cloned()
        };
        let Some(node) = found else {
            return ToolResult::error(format!("Node not found: {}", args.id));
        };
        let node_map = doc.nodes.clone();
        (node, node_map)
    };

    // ── shared helpers ────────────────────────────────────────────────────────

    fn union_aabb(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
        let x0 = a[0].min(b[0]);
        let y0 = a[1].min(b[1]);
        let x1 = (a[0] + a[2]).max(b[0] + b[2]);
        let y1 = (a[1] + a[3]).max(b[1] + b[3]);
        [x0, y0, x1 - x0, y1 - y0]
    }

    fn r2(v: f64) -> f64 {
        (v * 100.0).round() / 100.0
    }

    fn aabb_to_json(aabb: [f64; 4]) -> serde_json::Value {
        serde_json::json!({ "x": aabb[0], "y": aabb[1], "width": aabb[2], "height": aabb[3] })
    }

    let id_str = node.id.to_string();
    let name = node.name.clone();

    // ── per-kind computation ──────────────────────────────────────────────────

    match &node.kind {
        SceneNodeKind::Path(path_node) => {
            let bez = path_node.path_data.to_bez_path();

            let anchor_count = bez
                .elements()
                .iter()
                .filter(|e| !matches!(e, kurbo::PathEl::ClosePath))
                .count();

            let area = r2(bez.area().abs());
            let perimeter = r2(bez.perimeter(1e-3));

            let (centroid_x, centroid_y) = if let Some(local) = node.local_bounds() {
                let cx = (local.x0 + local.x1) / 2.0;
                let cy = (local.y0 + local.y1) / 2.0;
                let p = node.transform.to_kurbo() * kurbo::Point::new(cx, cy);
                (r2(p.x), r2(p.y))
            } else {
                (0.0, 0.0)
            };

            let world_bounds = world_aabb(&node).map(aabb_to_json);
            let local_bounds = node.local_bounds().map(|r| {
                serde_json::json!({
                    "x": r2(r.x0), "y": r2(r.y0),
                    "width": r2(r.x1 - r.x0), "height": r2(r.y1 - r.y0)
                })
            });

            let data = serde_json::json!({
                "id": id_str,
                "name": name,
                "type": "path",
                "world_bounds": world_bounds,
                "local_bounds": local_bounds,
                "perimeter": perimeter,
                "area": area,
                "centroid": { "x": centroid_x, "y": centroid_y },
                "anchor_count": anchor_count,
                "is_compound": path_node.is_compound,
            });

            ToolResult::text(format!(
                "inspect_node '{}': path with {} anchor(s), area={}, perimeter={}, compound={}",
                name, anchor_count, area, perimeter, path_node.is_compound
            ))
            .with_data(data)
        }

        SceneNodeKind::Group(group_node) => {
            let child_count = group_node.children.len();

            // DFS to collect all descendant node IDs.
            let mut stack: Vec<NodeId> = group_node.children.clone();
            let mut descendants: Vec<NodeId> = Vec::new();
            while let Some(id) = stack.pop() {
                descendants.push(id);
                if let Some(n) = node_map.get(&id) {
                    if let SceneNodeKind::Group(g) = &n.kind {
                        stack.extend(g.children.iter().copied());
                    }
                }
            }
            let descendant_count = descendants.len();

            // Collect stats from all descendants.
            let mut total_anchor_count: usize = 0;
            let mut fill_colors: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            let mut stroke_colors: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            let mut world_bounds: Option<[f64; 4]> = None;

            for id in &descendants {
                let Some(n) = node_map.get(id) else { continue };
                match &n.kind {
                    SceneNodeKind::Path(p) => {
                        let bez = p.path_data.to_bez_path();
                        total_anchor_count += bez
                            .elements()
                            .iter()
                            .filter(|e| !matches!(e, kurbo::PathEl::ClosePath))
                            .count();
                        if p.fill.enabled {
                            if let photonic_core::style::FillKind::Solid(color) = &p.fill.kind {
                                fill_colors.insert(color.to_hex());
                            }
                        }
                        if p.stroke.enabled {
                            stroke_colors.insert(p.stroke.color.to_hex());
                        }
                        if let Some(aabb) = world_aabb(n) {
                            world_bounds = Some(match world_bounds {
                                None => aabb,
                                Some(r) => union_aabb(r, aabb),
                            });
                        }
                    }
                    SceneNodeKind::Text(t) => {
                        if t.fill.enabled {
                            if let photonic_core::style::FillKind::Solid(color) = &t.fill.kind {
                                fill_colors.insert(color.to_hex());
                            }
                        }
                        if t.stroke.enabled {
                            stroke_colors.insert(t.stroke.color.to_hex());
                        }
                    }
                    SceneNodeKind::Group(_) => {} // handled by DFS stack
                    // raster: no anchors/fill/stroke to aggregate
                    SceneNodeKind::Raster(_) => {}
                }
            }

            let mut fill_list: Vec<String> = fill_colors.into_iter().collect();
            fill_list.sort();
            let mut stroke_list: Vec<String> = stroke_colors.into_iter().collect();
            stroke_list.sort();

            let data = serde_json::json!({
                "id": id_str,
                "name": name,
                "type": "group",
                "world_bounds": world_bounds.map(aabb_to_json),
                "child_count": child_count,
                "descendant_count": descendant_count,
                "total_anchor_count": total_anchor_count,
                "unique_fill_colors": fill_list,
                "unique_stroke_colors": stroke_list,
            });

            ToolResult::text(format!(
                "inspect_node '{}': group, {} child(ren), {} descendant(s), {} total anchor(s)",
                name, child_count, descendant_count, total_anchor_count
            ))
            .with_data(data)
        }

        SceneNodeKind::Text(text_node) => {
            let line_count = text_node.content.lines().count().max(1);
            let char_count = text_node.content.chars().count();
            let world_bounds = world_aabb(&node).map(aabb_to_json);

            let data = serde_json::json!({
                "id": id_str,
                "name": name,
                "type": "text",
                "world_bounds": world_bounds,
                "line_count": line_count,
                "char_count": char_count,
                "font_family": text_node.font_family,
                "font_size": text_node.font_size,
                "font_weight": text_node.font_weight,
                "baseline_shift": text_node.baseline_shift,
                "script_position": text_node.script_position.as_str(),
            });

            ToolResult::text(format!(
                "inspect_node '{}': text, {} char(s), {} line(s), font '{}'",
                name, char_count, line_count, text_node.font_family
            ))
            .with_data(data)
        }

        // raster: pixel layer — no vector geometry, fill, or stroke
        SceneNodeKind::Raster(_) => {
            let world_bounds = world_aabb(&node).map(aabb_to_json);

            let data = serde_json::json!({
                "id": id_str,
                "name": name,
                "type": "raster",
                "world_bounds": world_bounds,
            });

            ToolResult::text(format!("inspect_node '{}': raster (pixel layer)", name))
                .with_data(data)
        }
    }
}
pub async fn auto_name_nodes(state: &AppState, args: AutoNameNodesArgs) -> ToolResult {
    tracing::debug!("tool: auto_name_nodes");

    // ── Phase 1: collect target node IDs and clone nodes ─────────────────────
    let (_target_ids, nodes_snapshot) = {
        let doc = state.document.lock().await;
        let scope = args.scope.as_deref().unwrap_or("document");
        let ids: Vec<NodeId> = if scope == "selection" {
            doc.selection.ids().copied().collect()
        } else {
            doc.nodes.keys().copied().collect()
        };
        let snapshot: Vec<SceneNode> = ids
            .iter()
            .filter_map(|id| doc.nodes.get(id).cloned())
            .collect();
        (ids, snapshot)
    }; // lock released

    if nodes_snapshot.is_empty() {
        return ToolResult::text("No nodes to rename");
    }

    // ── Phase 2: compute renames ──────────────────────────────────────────────
    let renames: Vec<(SceneNode, String)> = nodes_snapshot
        .into_iter()
        .filter(|n| args.overwrite || is_generic_name(&n.name))
        .map(|n| {
            let new_name = generate_name(&n);
            (n, new_name)
        })
        .collect();

    if renames.is_empty() {
        return ToolResult::text(
            "No nodes with generic names found. Pass overwrite:true to rename all nodes.",
        );
    }

    let rename_list: Vec<serde_json::Value> = renames
        .iter()
        .map(|(n, new_name)| {
            serde_json::json!({
                "id": n.id.to_string(),
                "old_name": n.name,
                "new_name": new_name,
            })
        })
        .collect();

    if args.dry_run {
        return ToolResult::text(format!("dry_run: would rename {} node(s)", renames.len()))
            .with_data(serde_json::json!({
                "renamed": renames.len(),
                "dry_run": true,
                "renames": rename_list,
            }));
    }

    // ── Phase 3: apply renames ────────────────────────────────────────────────
    let commands: Vec<Command> = renames
        .into_iter()
        .map(|(old_node, new_name)| {
            let mut new_node = old_node.clone();
            new_node.name = new_name;
            Command::UpdateNode {
                old: old_node,
                new: new_node,
            }
        })
        .collect();

    let count = commands.len();
    let batch = Command::Batch(commands);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(batch, &mut doc);

    ToolResult::text(format!("Renamed {} node(s)", count)).with_data(serde_json::json!({
        "renamed": count,
        "dry_run": false,
        "renames": rename_list,
    }))
}
/// Return a CSS representation of a node's visual properties for developer
/// handoff. Read-only — does not modify the document.
pub async fn get_css_preview(state: &AppState, args: GetCssPreviewArgs) -> ToolResult {
    use photonic_core::{
        style::{Fill, FillKind, GradientKind, Stroke},
        transform::Transform,
    };

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Format a color as `rgba(r, g, b, a)` or `#rrggbb` when fully opaque.
    fn color_css(r: f32, g: f32, b: f32, a: f32) -> String {
        if (a - 1.0).abs() < 0.004 {
            format!(
                "#{:02x}{:02x}{:02x}",
                (r * 255.0).round() as u8,
                (g * 255.0).round() as u8,
                (b * 255.0).round() as u8,
            )
        } else {
            format!(
                "rgba({}, {}, {}, {:.3})",
                (r * 255.0).round() as u8,
                (g * 255.0).round() as u8,
                (b * 255.0).round() as u8,
                a,
            )
        }
    }

    /// Convert a `Fill` to one or two CSS lines and an optional note.
    fn fill_to_css(fill: &Fill, lines: &mut Vec<String>, notes: &mut Vec<String>) {
        if !fill.enabled {
            return;
        }
        let opacity = fill.opacity;
        match &fill.kind {
            FillKind::None => {}
            FillKind::Solid(c) => {
                let a = c.a * opacity;
                lines.push(format!(
                    "background-color: {};",
                    color_css(c.r, c.g, c.b, a)
                ));
            }
            FillKind::Gradient(g) => {
                if g.stops.is_empty() {
                    return;
                }
                let stops: Vec<String> = g
                    .stops
                    .iter()
                    .map(|s| {
                        let a = s.color.a * opacity;
                        format!(
                            "{} {:.1}%",
                            color_css(s.color.r, s.color.g, s.color.b, a),
                            s.offset * 100.0
                        )
                    })
                    .collect();
                let stops_str = stops.join(", ");
                match g.kind {
                    GradientKind::Linear => {
                        let (dx, dy) = if g.coords.len() >= 4 {
                            (g.coords[2] - g.coords[0], g.coords[3] - g.coords[1])
                        } else {
                            (1.0, 0.0)
                        };
                        // CSS gradient angle: 0deg = upward, increases clockwise.
                        // atan2(dx, -dy) converts vector direction to CSS convention.
                        let angle = dy.atan2(dx).to_degrees() + 90.0;
                        lines.push(format!(
                            "background: linear-gradient({:.1}deg, {});",
                            angle, stops_str
                        ));
                    }
                    GradientKind::Radial => {
                        let (cx, cy) = if g.coords.len() >= 2 {
                            (g.coords[0], g.coords[1])
                        } else {
                            (0.0, 0.0)
                        };
                        lines.push(format!(
                            "background: radial-gradient(circle at {:.1}px {:.1}px, {});",
                            cx, cy, stops_str
                        ));
                    }
                }
            }
            FillKind::FluidGradient(fg) => {
                if let Some(first) = fg.points.first() {
                    let c = &first.color;
                    let a = c.a * opacity;
                    lines.push(format!(
                        "background-color: {}; /* approximated from fluid gradient */",
                        color_css(c.r, c.g, c.b, a)
                    ));
                    notes.push(
                        "Fluid gradient has no direct CSS equivalent — shown as approximated solid from the first control point."
                            .to_string(),
                    );
                }
            }
            FillKind::MeshGradient(mg) => {
                if let Some(c) = mg.cell_colors.first() {
                    let a = c.a * opacity;
                    lines.push(format!(
                        "background-color: {}; /* approximated from mesh gradient */",
                        color_css(c.r, c.g, c.b, a)
                    ));
                    notes.push(
                        "Mesh gradient has no direct CSS equivalent — shown as approximated solid from the first cell."
                            .to_string(),
                    );
                }
            }
            FillKind::Pattern(p) => {
                use base64::Engine;
                let png = p.tile.to_png();
                let b64 = base64::engine::general_purpose::STANDARD.encode(png);
                let size = (p.tile.width.max(1) as f64 + p.spacing.max(0.0)) * p.scale.max(0.001);
                lines.push(format!(
                    "background-image: url(data:image/png;base64,{b64});"
                ));
                lines.push("background-repeat: repeat;".to_string());
                lines.push(format!("background-size: {:.1}px;", size));
                notes.push(
                    "Pattern fill exported as a repeating CSS background image (grid layout); brick/hex staggers are approximated."
                        .to_string(),
                );
            }
        }
    }

    /// Convert a `Stroke` to a CSS `outline` line (preserves layout dimensions).
    fn stroke_to_css(stroke: &Stroke) -> Option<String> {
        if !stroke.enabled || stroke.width <= 0.0 {
            return None;
        }
        let a = stroke.color.a * stroke.opacity;
        let color = color_css(stroke.color.r, stroke.color.g, stroke.color.b, a);
        // Use outline so the stroke does not affect the element's box dimensions.
        Some(format!("outline: {:.2}px solid {};", stroke.width, color))
    }

    /// Convert a `Transform` to a CSS `transform` line, or `None` if identity.
    fn transform_to_css(t: &Transform) -> Option<String> {
        if t.is_identity() {
            return None;
        }
        let m = t.matrix;
        // CSS matrix(a, b, c, d, e, f) matches SVG / affine conventions.
        Some(format!(
            "transform: matrix({:.6}, {:.6}, {:.6}, {:.6}, {:.6}, {:.6});",
            m[0], m[1], m[2], m[3], m[4], m[5]
        ))
    }

    // ── Resolve node ──────────────────────────────────────────────────────────

    let node = {
        let doc = state.document.lock().await;
        if let Some(id_str) = &args.id {
            if let Ok(uuid) = uuid::Uuid::parse_str(id_str) {
                doc.get_node(&uuid).cloned()
            } else {
                doc.find_node_by_name(id_str).cloned()
            }
        } else {
            doc.nodes.values().next().cloned()
        }
    };

    let Some(node) = node else {
        let desc = args.id.as_deref().unwrap_or("<first node>");
        return ToolResult::error(format!("Node not found: {}", desc));
    };

    // ── Build CSS lines ───────────────────────────────────────────────────────

    let mut lines: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    // Size from world bounding box (ignoring rotation for width/height).
    if let Some([_x, _y, w, h]) = world_aabb(&node) {
        lines.push(format!("width: {:.2}px;", w));
        lines.push(format!("height: {:.2}px;", h));
    }

    // Node-kind–specific properties.
    match &node.kind {
        SceneNodeKind::Path(p) => {
            fill_to_css(&p.fill, &mut lines, &mut notes);
            if let Some(s) = stroke_to_css(&p.stroke) {
                lines.push(s);
            }
        }
        SceneNodeKind::Text(t) => {
            // Text colour from fill.
            if t.fill.enabled {
                match &t.fill.kind {
                    FillKind::Solid(c) => {
                        let a = c.a * t.fill.opacity;
                        lines.push(format!("color: {};", color_css(c.r, c.g, c.b, a)));
                    }
                    _ => {
                        fill_to_css(&t.fill, &mut lines, &mut notes);
                    }
                }
            }
            if let Some(s) = stroke_to_css(&t.stroke) {
                lines.push(s);
            }
            lines.push(format!("font-family: \"{}\";", t.font_family));
            lines.push(format!("font-size: {}px;", t.font_size));
            lines.push(format!("font-weight: {};", t.font_weight));
            let align_str = match t.align {
                photonic_core::node::TextAlign::Left => "left",
                photonic_core::node::TextAlign::Center => "center",
                photonic_core::node::TextAlign::Right => "right",
            };
            lines.push(format!("text-align: {};", align_str));
        }
        SceneNodeKind::Group(_) => {
            notes.push(
                "Group nodes have no fill or stroke — CSS shown covers size and positioning only."
                    .to_string(),
            );
        }
        // raster: no vector fill or stroke
        SceneNodeKind::Raster(_) => {
            notes.push(
                "Raster nodes have no fill or stroke — CSS shown covers size and positioning only."
                    .to_string(),
            );
        }
    }

    // Opacity (node-level).
    if (node.opacity - 1.0).abs() > 1e-4 {
        lines.push(format!("opacity: {:.3};", node.opacity));
    }

    // Blend mode.
    if node.blend_mode != BlendMode::Normal {
        let bm = format!("{:?}", node.blend_mode);
        // Convert PascalCase to kebab-case (e.g. ColorDodge → color-dodge).
        let kebab = bm
            .chars()
            .enumerate()
            .flat_map(|(i, c)| {
                if c.is_uppercase() && i > 0 {
                    vec!['-', c.to_lowercase().next().unwrap()]
                } else {
                    vec![c.to_lowercase().next().unwrap()]
                }
            })
            .collect::<String>();
        lines.push(format!("mix-blend-mode: {};", kebab));
    }

    // Transform (only if non-identity).
    if let Some(t) = transform_to_css(&node.transform) {
        lines.push(t);
    }

    // ── Assemble CSS block ────────────────────────────────────────────────────

    let node_type = match &node.kind {
        SceneNodeKind::Path(_) => "path",
        SceneNodeKind::Text(_) => "text",
        SceneNodeKind::Group(_) => "group",
        SceneNodeKind::Raster(_) => "raster",
    };

    let css_block = if lines.is_empty() {
        format!("/* Photonic node: \"{}\" — no CSS properties */", node.name)
    } else {
        format!(
            "/* Photonic node: \"{}\" */\n{}",
            node.name,
            lines.join("\n")
        )
    };

    ToolResult::text(format!("CSS preview for '{}'", node.name)).with_data(serde_json::json!({
        "node_id":   node.id.to_string(),
        "node_name": node.name,
        "node_type": node_type,
        "css":       css_block,
        "notes":     notes,
    }))
}
/// Analyse style consistency across the document or a node subset.
/// Returns a structured report identifying dominant values and outliers per
/// checked property (fill color, stroke width, opacity, font family).
/// Read-only — makes no changes to the document.
pub async fn check_style_continuity(
    state: &AppState,
    args: CheckStyleContinuityArgs,
) -> ToolResult {
    use photonic_core::style::FillKind;
    use std::collections::HashMap;

    let doc = state.document.lock().await;

    // ── Build the node list ───────────────────────────────────────────────────
    let nodes: Vec<&photonic_core::node::SceneNode> = if args.node_ids.is_empty() {
        doc.nodes.values().collect()
    } else {
        args.node_ids
            .iter()
            .filter_map(|id| doc.nodes.get(id))
            .collect()
    };

    // Determine which property groups to check (default: all four).
    let all_checks = args.checks.is_empty();
    let check_fill = all_checks || args.checks.iter().any(|c| c == "fill");
    let check_stroke = all_checks || args.checks.iter().any(|c| c == "stroke");
    let check_opacity = all_checks || args.checks.iter().any(|c| c == "opacity");
    let check_font = all_checks || args.checks.iter().any(|c| c == "font");

    let threshold = args.outlier_threshold.unwrap_or(2);

    // ── Property buckets: value → Vec<(node_id_str, node_name)> ──────────────
    // Each bucket accumulates (string_value, node_id, node_name) entries.
    let mut fill_bucket: Vec<(String, String, String)> = Vec::new();
    let mut stroke_bucket: Vec<(String, String, String)> = Vec::new();
    let mut opacity_bucket: Vec<(String, String, String)> = Vec::new();
    let mut font_bucket: Vec<(String, String, String)> = Vec::new();

    for node in &nodes {
        let nid = node.id.to_string();
        let nname = node.name.clone();

        match &node.kind {
            SceneNodeKind::Path(p) => {
                if check_fill && p.fill.enabled {
                    if let FillKind::Solid(c) = &p.fill.kind {
                        fill_bucket.push((c.to_hex(), nid.clone(), nname.clone()));
                    }
                }
                if check_stroke && p.stroke.enabled {
                    let w = format!("{:.2}", p.stroke.width);
                    stroke_bucket.push((w, nid.clone(), nname.clone()));
                }
                if check_opacity {
                    let op = format!("{:.2}", node.opacity);
                    opacity_bucket.push((op, nid.clone(), nname.clone()));
                }
            }
            SceneNodeKind::Text(t) => {
                if check_fill && t.fill.enabled {
                    if let FillKind::Solid(c) = &t.fill.kind {
                        fill_bucket.push((c.to_hex(), nid.clone(), nname.clone()));
                    }
                }
                if check_stroke && t.stroke.enabled {
                    let w = format!("{:.2}", t.stroke.width);
                    stroke_bucket.push((w, nid.clone(), nname.clone()));
                }
                if check_opacity {
                    let op = format!("{:.2}", node.opacity);
                    opacity_bucket.push((op, nid.clone(), nname.clone()));
                }
                if check_font {
                    font_bucket.push((t.font_family.clone(), nid.clone(), nname.clone()));
                }
            }
            SceneNodeKind::Group(_) => {
                // Groups are included only for opacity analysis, not fill/stroke/font.
                if check_opacity {
                    let op = format!("{:.2}", node.opacity);
                    opacity_bucket.push((op, nid.clone(), nname.clone()));
                }
            }
            // raster: no vector fill/stroke/font — opacity analysis only
            SceneNodeKind::Raster(_) => {
                if check_opacity {
                    let op = format!("{:.2}", node.opacity);
                    opacity_bucket.push((op, nid.clone(), nname.clone()));
                }
            }
        }
    }

    // ── Analyse a bucket: return (dominant_values, outliers) ─────────────────
    // outliers: Vec<(value, node_id, node_name)>
    fn analyse_bucket(
        bucket: &[(String, String, String)],
        threshold: usize,
    ) -> (Vec<String>, Vec<(String, String, String)>) {
        if bucket.is_empty() {
            return (vec![], vec![]);
        }
        // Count frequency per value.
        let mut freq: HashMap<&str, usize> = HashMap::new();
        for (val, _, _) in bucket {
            *freq.entry(val.as_str()).or_insert(0) += 1;
        }
        let dominant: Vec<String> = freq
            .iter()
            .filter(|(_, &count)| count >= threshold)
            .map(|(v, _)| v.to_string())
            .collect();

        // Only flag outliers when at least one dominant value exists.
        if dominant.is_empty() {
            return (vec![], vec![]);
        }
        let outliers: Vec<(String, String, String)> = bucket
            .iter()
            .filter(|(val, _, _)| freq[val.as_str()] < threshold)
            .map(|(v, id, name)| (v.clone(), id.clone(), name.clone()))
            .collect();
        (dominant, outliers)
    }

    // ── Run analysis ─────────────────────────────────────────────────────────
    let (fill_dominant, fill_outliers) = analyse_bucket(&fill_bucket, threshold);
    let (stroke_dominant, stroke_outliers) = analyse_bucket(&stroke_bucket, threshold);
    let (opacity_dominant, opacity_outliers) = analyse_bucket(&opacity_bucket, threshold);
    let (font_dominant, font_outliers) = analyse_bucket(&font_bucket, threshold);

    // ── Build consistent summary ──────────────────────────────────────────────
    let mut consistent = serde_json::Map::new();
    let count_dominant = |bucket: &[(String, String, String)], dominant: &[String]| {
        bucket
            .iter()
            .filter(|(v, _, _)| dominant.contains(v))
            .count()
    };
    if !fill_dominant.is_empty() {
        consistent.insert(
            "fill_color".to_string(),
            serde_json::json!({
                "dominant_values": fill_dominant,
                "node_count": count_dominant(&fill_bucket, &fill_dominant),
            }),
        );
    }
    if !stroke_dominant.is_empty() {
        consistent.insert(
            "stroke_width".to_string(),
            serde_json::json!({
                "dominant_values": stroke_dominant,
                "node_count": count_dominant(&stroke_bucket, &stroke_dominant),
            }),
        );
    }
    if !opacity_dominant.is_empty() {
        consistent.insert(
            "opacity".to_string(),
            serde_json::json!({
                "dominant_values": opacity_dominant,
                "node_count": count_dominant(&opacity_bucket, &opacity_dominant),
            }),
        );
    }
    if !font_dominant.is_empty() {
        consistent.insert(
            "font_family".to_string(),
            serde_json::json!({
                "dominant_values": font_dominant,
                "node_count": count_dominant(&font_bucket, &font_dominant),
            }),
        );
    }

    // ── Build outlier list ────────────────────────────────────────────────────
    let mut outlier_items: Vec<serde_json::Value> = Vec::new();

    let mut push_outliers = |property: &str,
                             outliers: &[(String, String, String)],
                             dominant: &[String],
                             total: usize| {
        for (val, nid, nname) in outliers {
            let dominant_str = dominant.first().map(String::as_str).unwrap_or("?");
            let message = match property {
                "fill_color" => format!(
                    "Fill color {} is used by 1 node; {} other(s) use dominant values",
                    val,
                    total - 1
                ),
                "stroke_width" => format!(
                    "Stroke width {} px; {} other node(s) use {}",
                    val,
                    total - 1,
                    dominant_str
                ),
                "opacity" => format!(
                    "Opacity {}; {} other node(s) use {}",
                    val,
                    total - 1,
                    dominant_str
                ),
                "font_family" => format!(
                    "Font \"{}\" differs from dominant \"{}\" (used by {} node(s))",
                    val,
                    dominant_str,
                    total - 1
                ),
                _ => format!("{} value {} is an outlier", property, val),
            };
            outlier_items.push(serde_json::json!({
                "property":      property,
                "node_id":       nid,
                "node_name":     nname,
                "value":         val,
                "dominant_value": dominant_str,
                "message":       message,
            }));
        }
    };

    let fill_total = fill_bucket.len();
    let stroke_total = stroke_bucket.len();
    let opacity_total = opacity_bucket.len();
    let font_total = font_bucket.len();

    // Retrieve dominant slices before moving into closure (borrow checker).
    let fill_dom_snap: Vec<String> = consistent
        .get("fill_color")
        .and_then(|v| v["dominant_values"].as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let stroke_dom_snap: Vec<String> = consistent
        .get("stroke_width")
        .and_then(|v| v["dominant_values"].as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let opacity_dom_snap: Vec<String> = consistent
        .get("opacity")
        .and_then(|v| v["dominant_values"].as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let font_dom_snap: Vec<String> = consistent
        .get("font_family")
        .and_then(|v| v["dominant_values"].as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    push_outliers("fill_color", &fill_outliers, &fill_dom_snap, fill_total);
    push_outliers(
        "stroke_width",
        &stroke_outliers,
        &stroke_dom_snap,
        stroke_total,
    );
    push_outliers(
        "opacity",
        &opacity_outliers,
        &opacity_dom_snap,
        opacity_total,
    );
    push_outliers("font_family", &font_outliers, &font_dom_snap, font_total);

    let outlier_count = outlier_items.len();
    let nodes_analysed = nodes.len();

    let summary = if outlier_count == 0 {
        format!(
            "Style is consistent across {} nodes — no outliers found.",
            nodes_analysed
        )
    } else {
        format!(
            "{} style outlier(s) found across {} nodes.",
            outlier_count, nodes_analysed
        )
    };

    ToolResult::text(summary).with_data(serde_json::json!({
        "nodes_analysed": nodes_analysed,
        "outlier_count":  outlier_count,
        "consistent":     consistent,
        "outliers":       outlier_items,
    }))
}
pub async fn tag_nodes(state: &AppState, args: TagNodesArgs) -> ToolResult {
    tracing::debug!("tool: tag_nodes");

    if args.add.is_empty() && args.remove.is_empty() {
        return ToolResult::error("Specify at least one tag to add or remove");
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let node_ids: Vec<NodeId> = if args.node_ids.is_empty() {
        doc.selection.node_ids.iter().copied().collect()
    } else {
        args.node_ids
            .iter()
            .filter_map(|s| {
                uuid::Uuid::parse_str(s)
                    .ok()
                    .or_else(|| doc.find_node_by_name(s).map(|n| n.id))
            })
            .collect()
    };

    if node_ids.is_empty() {
        return ToolResult::error("No nodes specified and nothing selected");
    }

    let mut modified = 0usize;
    for nid in &node_ids {
        let node = match doc.nodes.get(nid) {
            Some(n) => n.clone(),
            None => continue,
        };
        let mut new_node = node.clone();
        // Remove specified tags.
        for tag in &args.remove {
            new_node.tags.retain(|t| t != tag);
        }
        // Add specified tags (avoid duplicates).
        for tag in &args.add {
            if !new_node.tags.contains(tag) {
                new_node.tags.push(tag.clone());
            }
        }
        if new_node.tags != node.tags {
            history.execute_discrete(
                Command::UpdateNode {
                    old: node,
                    new: new_node,
                },
                &mut doc,
            );
            modified += 1;
        }
    }

    ToolResult::text(format!(
        "Tagged {modified} node(s) — added [{}], removed [{}]",
        args.add.join(", "),
        args.remove.join(", ")
    ))
    .with_data(serde_json::json!({ "modified": modified }))
}
pub async fn move_to_layer(state: &AppState, args: MoveToLayerArgs) -> ToolResult {
    tracing::debug!("tool: move_to_layer");

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    // Resolve target layer.
    let target_lid = if let Ok(uuid) = uuid::Uuid::parse_str(&args.target_layer) {
        uuid
    } else {
        match doc.layers.values().find(|l| l.name == args.target_layer) {
            Some(l) => l.id,
            None => return ToolResult::error(format!("Layer not found: {}", args.target_layer)),
        }
    };

    if !doc.layers.contains_key(&target_lid) {
        return ToolResult::error("Target layer not found");
    }

    let node_ids: Vec<NodeId> = if args.node_ids.is_empty() {
        doc.selection.node_ids.iter().copied().collect()
    } else {
        args.node_ids
            .iter()
            .filter_map(|s| {
                uuid::Uuid::parse_str(s)
                    .ok()
                    .or_else(|| doc.find_node_by_name(s).map(|n| n.id))
            })
            .collect()
    };

    if node_ids.is_empty() {
        return ToolResult::error("No nodes specified and nothing selected");
    }

    let mut moved = 0usize;
    for nid in &node_ids {
        let node = match doc.nodes.get(nid) {
            Some(n) => n,
            None => continue,
        };
        let old_layer_id = node.layer_id;
        if old_layer_id == target_lid {
            continue;
        }

        let old_index = doc
            .layers
            .get(&old_layer_id)
            .and_then(|l| l.node_ids.iter().position(|id| id == nid))
            .unwrap_or(0);

        let new_index = doc
            .layers
            .get(&target_lid)
            .map(|l| l.node_ids.len())
            .unwrap_or(0);

        history.execute_discrete(
            Command::MoveNodeToLayer {
                node_id: *nid,
                old_layer_id,
                new_layer_id: target_lid,
                old_index,
                new_index,
            },
            &mut doc,
        );
        moved += 1;
    }

    ToolResult::text(format!(
        "Moved {moved} node(s) to layer '{}'",
        args.target_layer
    ))
    .with_data(serde_json::json!({ "moved": moved, "target_layer": target_lid }))
}
pub async fn set_visibility(state: &AppState, args: SetVisibilityArgs) -> ToolResult {
    tracing::debug!("tool: set_visibility");

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let node_ids: Vec<NodeId> = if args.node_ids.is_empty() {
        doc.selection.node_ids.iter().copied().collect()
    } else {
        args.node_ids
            .iter()
            .filter_map(|s| {
                uuid::Uuid::parse_str(s)
                    .ok()
                    .or_else(|| doc.find_node_by_name(s).map(|n| n.id))
            })
            .collect()
    };

    if node_ids.is_empty() {
        return ToolResult::error("No nodes specified and nothing selected");
    }

    let mut modified = 0usize;
    for nid in &node_ids {
        let node = match doc.nodes.get(nid) {
            Some(n) => n.clone(),
            None => continue,
        };
        let mut new_node = node.clone();
        new_node.visible = args.visible.unwrap_or(!node.visible);
        history.execute_discrete(
            Command::UpdateNode {
                old: node,
                new: new_node,
            },
            &mut doc,
        );
        modified += 1;
    }

    let state_label = if args.visible == Some(true) {
        "visible"
    } else if args.visible == Some(false) {
        "hidden"
    } else {
        "toggled"
    };
    ToolResult::text(format!("Set {modified} node(s) to {state_label}"))
        .with_data(serde_json::json!({ "modified": modified }))
}
pub async fn set_locked(state: &AppState, args: SetLockedArgs) -> ToolResult {
    tracing::debug!("tool: set_locked");

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let node_ids: Vec<NodeId> = if args.node_ids.is_empty() {
        doc.selection.node_ids.iter().copied().collect()
    } else {
        args.node_ids
            .iter()
            .filter_map(|s| {
                uuid::Uuid::parse_str(s)
                    .ok()
                    .or_else(|| doc.find_node_by_name(s).map(|n| n.id))
            })
            .collect()
    };

    if node_ids.is_empty() {
        return ToolResult::error("No nodes specified and nothing selected");
    }

    let mut modified = 0usize;
    for nid in &node_ids {
        let node = match doc.nodes.get(nid) {
            Some(n) => n.clone(),
            None => continue,
        };
        let mut new_node = node.clone();
        new_node.locked = args.locked.unwrap_or(!node.locked);
        history.execute_discrete(
            Command::UpdateNode {
                old: node,
                new: new_node,
            },
            &mut doc,
        );
        modified += 1;
    }

    let state_label = if args.locked == Some(true) {
        "locked"
    } else if args.locked == Some(false) {
        "unlocked"
    } else {
        "toggled"
    };
    ToolResult::text(format!("Set {modified} node(s) to {state_label}"))
        .with_data(serde_json::json!({ "modified": modified }))
}
pub async fn measure_distance(state: &AppState, args: MeasureDistanceArgs) -> ToolResult {
    tracing::debug!("tool: measure_distance");

    let doc = state.document.lock().await;

    let resolve = |target: &MeasureTarget| -> Result<kurbo::Point, String> {
        match target {
            MeasureTarget::Point(p) => Ok(kurbo::Point::new(p[0], p[1])),
            MeasureTarget::NodeId(id_str) => {
                let nid = uuid::Uuid::parse_str(id_str)
                    .ok()
                    .or_else(|| doc.find_node_by_name(id_str).map(|n| n.id));
                let nid = nid.ok_or_else(|| format!("Node not found: {id_str}"))?;
                let node = doc
                    .nodes
                    .get(&nid)
                    .ok_or_else(|| format!("Node not found: {id_str}"))?;
                // Compute center from path bounding box or transform translation.
                match &node.kind {
                    SceneNodeKind::Path(pn) => {
                        use kurbo::Shape;
                        let bez = pn.path_data.to_bez_path();
                        let b = bez.bounding_box();
                        Ok(kurbo::Point::new(
                            b.x0 + b.width() / 2.0 + node.transform.matrix[4],
                            b.y0 + b.height() / 2.0 + node.transform.matrix[5],
                        ))
                    }
                    _ => Ok(kurbo::Point::new(
                        node.transform.matrix[4],
                        node.transform.matrix[5],
                    )),
                }
            }
        }
    };

    let p1 = match resolve(&args.from) {
        Ok(p) => p,
        Err(e) => return ToolResult::error(e),
    };
    let p2 = match resolve(&args.to) {
        Ok(p) => p,
        Err(e) => return ToolResult::error(e),
    };

    let dx = p2.x - p1.x;
    let dy = p2.y - p1.y;
    let distance = (dx * dx + dy * dy).sqrt();
    let angle = dy.atan2(dx).to_degrees();

    ToolResult::text(format!(
        "Distance: {:.2} — from ({:.1},{:.1}) to ({:.1},{:.1}), Δx={:.1}, Δy={:.1}, angle={:.1}°",
        distance, p1.x, p1.y, p2.x, p2.y, dx, dy, angle
    ))
    .with_data(serde_json::json!({
        "distance": distance,
        "dx": dx,
        "dy": dy,
        "angle_degrees": angle,
        "from": [p1.x, p1.y],
        "to": [p2.x, p2.y],
    }))
}
/// Remove degenerate content from the document:
/// - stray points: paths with no drawing segments (only MoveTo or empty)
/// - unpainted objects: paths with no visible fill and no visible stroke
/// - empty text: text nodes with whitespace-only content
pub async fn clean_up(state: &AppState, args: CleanUpArgs) -> ToolResult {
    use kurbo::PathEl;
    use photonic_core::style::FillKind;

    tracing::debug!("tool: clean_up");

    let remove_stray = args.remove_stray_points.unwrap_or(true);
    let remove_unpaint = args.remove_unpainted.unwrap_or(true);
    let remove_empty = args.remove_empty_text.unwrap_or(true);
    let dry_run = args.dry_run.unwrap_or(false);

    // ── Phase 1: identify nodes to remove (read-only, single lock acquisition) ──
    let to_delete: Vec<(NodeId, &'static str)> = {
        let doc = state.document.lock().await;
        let mut found: Vec<(NodeId, &'static str)> = Vec::new();

        for node in doc.nodes.values() {
            match &node.kind {
                SceneNodeKind::Path(path_node) => {
                    // Stray point: path with no drawing segments
                    if remove_stray {
                        let bez = path_node.path_data.to_bez_path();
                        let has_segment = bez.elements().iter().any(|el| {
                            matches!(
                                el,
                                PathEl::LineTo(_) | PathEl::CurveTo(..) | PathEl::QuadTo(..)
                            )
                        });
                        if !has_segment {
                            found.push((node.id, "stray_point"));
                            continue;
                        }
                    }
                    // Unpainted: no visible fill and no visible stroke
                    if remove_unpaint {
                        let has_fill = path_node.fill.enabled
                            && !matches!(path_node.fill.kind, FillKind::None)
                            && path_node.fill.opacity > 0.0;
                        let has_stroke = path_node.stroke.enabled
                            && path_node.stroke.width > 0.0
                            && path_node.stroke.opacity > 0.0;
                        if !has_fill && !has_stroke {
                            found.push((node.id, "unpainted"));
                        }
                    }
                }
                SceneNodeKind::Text(text_node) => {
                    if remove_empty && text_node.content.trim().is_empty() {
                        found.push((node.id, "empty_text"));
                    }
                }
                SceneNodeKind::Group(_) => {}
                // raster: not subject to stray/unpainted/empty-text cleanup
                SceneNodeKind::Raster(_) => {}
            }
        }
        found
    }; // doc lock released

    let count = to_delete.len();
    let items: Vec<serde_json::Value> = to_delete
        .iter()
        .map(|(id, reason)| serde_json::json!({ "id": id, "reason": reason }))
        .collect();

    if count == 0 {
        return ToolResult::text("Nothing to clean up").with_data(serde_json::json!({
            "dry_run": dry_run,
            "removed": 0,
            "items":   [],
        }));
    }

    if dry_run {
        return ToolResult::text(format!("Dry run — {} node(s) would be removed", count))
            .with_data(serde_json::json!({
                "dry_run":      true,
                "would_remove": count,
                "items":        items,
            }));
    }

    // ── Phase 2: delete (acquire both locks) ─────────────────────────────────
    let ids: Vec<NodeId> = to_delete.iter().map(|(id, _)| *id).collect();
    let cmd = Command::Batch(
        ids.iter()
            .map(|&node_id| Command::RemoveNode { node_id })
            .collect(),
    );
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!("Cleaned up {} node(s)", count)).with_data(serde_json::json!({
        "dry_run": false,
        "removed": count,
        "items":   items,
    }))
}
/// Select all children of the group — the MCP-observable effect of entering Isolation Mode.
pub async fn enter_isolation_mode(state: &AppState, args: EnterIsolationModeArgs) -> ToolResult {
    let mut doc = state.document.lock().await;
    let children = match doc.nodes.get(&args.group_id) {
        Some(node) => {
            if let SceneNodeKind::Group(g) = &node.kind {
                if g.children.is_empty() {
                    return ToolResult::text(format!("Group {} has no children", args.group_id));
                }
                g.children.clone()
            } else {
                return ToolResult::error(format!("Node {} is not a group", args.group_id));
            }
        }
        None => return ToolResult::error(format!("No node found with id {}", args.group_id)),
    };

    doc.selection.clear();
    for cid in &children {
        doc.selection.add(*cid);
    }

    ToolResult::text(format!(
        "Entered isolation mode for group {} — {} child node(s) selected",
        args.group_id,
        children.len()
    ))
    .with_data(serde_json::json!({
        "group_id": args.group_id,
        "child_count": children.len(),
        "children": children,
    }))
}
/// Exit Isolation Mode — clears the current selection.
pub async fn exit_isolation_mode(state: &AppState, _args: ExitIsolationModeArgs) -> ToolResult {
    let mut doc = state.document.lock().await;
    doc.selection.clear();
    ToolResult::text("Exited isolation mode. Selection cleared.")
}
/// Record an AI prompt on a node's prompt_history field for provenance tracking.
pub async fn set_node_prompt(state: &AppState, args: SetNodePromptArgs) -> ToolResult {
    tracing::debug!("tool: set_node_prompt");

    if args.prompt.trim().is_empty() {
        return ToolResult::error("prompt must not be empty");
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let nid = match uuid::Uuid::parse_str(&args.node_id) {
        Ok(id) => id,
        Err(_) => match doc.find_node_by_name(&args.node_id) {
            Some(n) => n.id,
            None => return ToolResult::error(format!("Node '{}' not found", args.node_id)),
        },
    };

    let node = match doc.nodes.get(&nid) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Node {} not found", nid)),
    };

    let mut new_node = node.clone();
    let mode = args.mode.as_deref().unwrap_or("append");
    match mode {
        "replace" => {
            new_node.prompt_history = vec![args.prompt.clone()];
        }
        "prepend" => {
            new_node.prompt_history.insert(0, args.prompt.clone());
        }
        _ => {
            // "append" and anything else
            new_node.prompt_history.push(args.prompt.clone());
        }
    }

    let entry_count = new_node.prompt_history.len();
    history.execute_discrete(
        Command::UpdateNode {
            old: node,
            new: new_node,
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Recorded prompt on node '{}' ({} mode). History length: {}.",
        args.node_id, mode, entry_count
    ))
}
/// Return the full prompt history for a node.
pub async fn get_node_prompts(state: &AppState, args: GetNodePromptsArgs) -> ToolResult {
    tracing::debug!("tool: get_node_prompts");

    let doc = state.document.lock().await;
    let nid = match uuid::Uuid::parse_str(&args.node_id) {
        Ok(id) => id,
        Err(_) => match doc.find_node_by_name(&args.node_id) {
            Some(n) => n.id,
            None => return ToolResult::error(format!("Node '{}' not found", args.node_id)),
        },
    };

    let node = match doc.nodes.get(&nid) {
        Some(n) => n,
        None => return ToolResult::error(format!("Node {} not found", nid)),
    };

    if node.prompt_history.is_empty() {
        return ToolResult::text(format!("Node '{}' has no prompt history.", node.name));
    }

    let prompts: Vec<serde_json::Value> = node
        .prompt_history
        .iter()
        .enumerate()
        .map(|(i, p)| serde_json::json!({ "index": i, "prompt": p }))
        .collect();

    ToolResult::text(format!(
        "Node '{}' has {} prompt(s) in history.",
        node.name,
        prompts.len()
    ))
    .with_data(serde_json::json!({
        "node_id": nid.to_string(),
        "node_name": node.name,
        "prompts": prompts,
    }))
}
/// Tag a node for inclusion in batch asset exports.  Passing an empty `name`
/// removes the tag entirely.
pub async fn tag_node_for_export(state: &AppState, args: TagNodeForExportArgs) -> ToolResult {
    tracing::debug!("tool: tag_node_for_export");
    use photonic_core::history::Command;
    use photonic_core::AssetExportSpec;

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let nid = uuid::Uuid::parse_str(&args.node_id).ok().or_else(|| {
        doc.nodes
            .values()
            .find(|n| n.name == args.node_id)
            .map(|n| n.id)
    });

    let nid = match nid {
        Some(id) => id,
        None => return ToolResult::error(format!("Node not found: {}", args.node_id)),
    };

    let node = match doc.nodes.get(&nid).cloned() {
        Some(n) => n,
        None => return ToolResult::error(format!("Node not found: {}", args.node_id)),
    };

    let mut new_node = node.clone();
    if args.name.trim().is_empty() {
        new_node.export_spec = None;
        history.execute_discrete(
            Command::UpdateNode {
                old: node,
                new: new_node,
            },
            &mut doc,
        );
        return ToolResult::text(format!("Removed export tag from node '{}'.", args.node_id));
    }

    let format = args.format.as_deref().unwrap_or("svg").to_lowercase();
    if !matches!(format.as_str(), "svg" | "png" | "jpeg" | "jpg" | "webp") {
        return ToolResult::error(format!(
            "Unsupported format '{}'. Use svg, png, jpeg, or webp.",
            format
        ));
    }

    let scales = if args.scales.is_empty() {
        vec![1.0]
    } else {
        args.scales.clone()
    };

    new_node.export_spec = Some(AssetExportSpec {
        name: args.name.trim().to_string(),
        format: format.clone(),
        scales: scales.clone(),
    });

    history.execute_discrete(
        Command::UpdateNode {
            old: node,
            new: new_node,
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Tagged node '{}' for export as '{}' ({}, {} scale(s)).",
        args.node_id,
        args.name.trim(),
        format,
        scales.len()
    ))
    .with_data(serde_json::json!({
        "node_id": nid.to_string(),
        "asset_name": args.name.trim(),
        "format": format,
        "scales": scales,
    }))
}
/// Export all nodes tagged with `tag_node_for_export`.  Returns a JSON array
/// of export results, one entry per (node × scale) combination.
pub async fn export_tagged_assets(state: &AppState, args: ExportTaggedAssetsArgs) -> ToolResult {
    tracing::debug!("tool: export_tagged_assets");

    let doc = state.document.lock().await;

    let tagged: Vec<_> = doc
        .nodes
        .values()
        .filter(|n| {
            n.export_spec.is_some()
                && args
                    .filter
                    .as_deref()
                    .map(|f| n.export_spec.as_ref().unwrap().name.contains(f))
                    .unwrap_or(true)
        })
        .collect();

    if tagged.is_empty() {
        return ToolResult::text("No nodes tagged for export. Use tag_node_for_export first.");
    }

    let mut results: Vec<serde_json::Value> = Vec::new();

    for node in &tagged {
        let spec = node.export_spec.as_ref().unwrap();
        match spec.format.as_str() {
            "svg" => {
                let svg = photonic_core::export::export_nodes_as_svg(&doc, &[node.id]);
                results.push(serde_json::json!({
                    "asset_name": spec.name,
                    "node_id": node.id.to_string(),
                    "node_name": node.name,
                    "format": "svg",
                    "scale": 1.0,
                    "filename": format!("{}.svg", spec.name),
                    "svg": svg,
                    "bytes": svg.len(),
                }));
            }
            _ => {
                // For raster formats, record intent (actual raster requires render thread).
                for &scale in &spec.scales {
                    let suffix = if (scale - 1.0).abs() < 0.001 {
                        String::new()
                    } else {
                        format!("@{}x", scale as u32)
                    };
                    results.push(serde_json::json!({
                        "asset_name": spec.name,
                        "node_id": node.id.to_string(),
                        "node_name": node.name,
                        "format": spec.format,
                        "scale": scale,
                        "filename": format!("{}{}.{}", spec.name, suffix, spec.format),
                        "note": "Raster export requires render thread — use export_raster MCP tool with the returned node_id",
                    }));
                }
            }
        }
    }

    ToolResult::text(format!(
        "Exported {} asset(s) from {} tagged node(s).",
        results.len(),
        tagged.len()
    ))
    .with_data(serde_json::json!({
        "asset_count": results.len(),
        "tagged_node_count": tagged.len(),
        "assets": results,
    }))
}
/// Flatten transparency — bake node opacity and fill/stroke opacity into color
/// alpha values, then set all opacity fields to 1.0 for print-ready output.
pub async fn flatten_transparency(state: &AppState, args: FlattenTransparencyArgs) -> ToolResult {
    tracing::debug!("tool: flatten_transparency");
    use photonic_core::style::{Fill, FillKind, Stroke};

    let mut doc = state.document.lock().await;

    // Collect target node IDs
    let target_ids: Vec<NodeId> = if args.node_ids.is_empty() {
        doc.nodes.keys().cloned().collect()
    } else {
        args.node_ids
            .iter()
            .filter_map(|id_str| {
                uuid::Uuid::parse_str(id_str)
                    .ok()
                    .or_else(|| doc.find_node_by_name(id_str).map(|n| n.id))
            })
            .collect()
    };

    /// Premultiply a fill's own opacity and the node's opacity into color alphas.
    fn bake_fill(fill: &Fill, node_opacity: f32) -> Fill {
        let combined = fill.opacity * node_opacity;
        let kind = match &fill.kind {
            FillKind::Solid(c) => FillKind::Solid(photonic_core::color::Color {
                r: c.r,
                g: c.g,
                b: c.b,
                a: c.a * combined,
            }),
            FillKind::Gradient(g) => {
                let mut g2 = g.clone();
                for stop in g2.stops.iter_mut() {
                    stop.color.a *= combined;
                }
                FillKind::Gradient(g2)
            }
            other => other.clone(),
        };
        Fill {
            kind,
            opacity: 1.0,
            enabled: fill.enabled,
        }
    }

    fn bake_stroke(stroke: &Stroke, node_opacity: f32) -> Stroke {
        let combined = node_opacity;
        let mut s = stroke.clone();
        s.color.a *= combined;
        s.opacity = 1.0;
        s
    }

    let mut commands = Vec::new();
    let mut processed = 0usize;

    for nid in target_ids {
        let node = match doc.nodes.get(&nid) {
            Some(n)
                if n.opacity < 1.0 - f32::EPSILON
                    || matches!(n.kind, SceneNodeKind::Path(ref pn) if pn.fill.opacity < 1.0 - f32::EPSILON) =>
            {
                n.clone()
            }
            Some(n) if matches!(n.kind, SceneNodeKind::Text(ref tn) if tn.fill.opacity < 1.0 - f32::EPSILON) => {
                n.clone()
            }
            _ => continue,
        };

        let node_opacity = node.opacity;
        let mut new_node = node.clone();
        new_node.opacity = 1.0;

        match &mut new_node.kind {
            SceneNodeKind::Path(pn) => {
                pn.fill = bake_fill(&pn.fill, node_opacity);
                pn.stroke = bake_stroke(&pn.stroke, node_opacity);
            }
            SceneNodeKind::Text(tn) => {
                tn.fill = bake_fill(&tn.fill, node_opacity);
            }
            SceneNodeKind::Group(_) => {
                // Group opacity baking is skipped — children are processed individually
            }
            // raster: no vector fill/stroke to bake
            SceneNodeKind::Raster(_) => {}
        }

        commands.push(Command::UpdateNode {
            old: node,
            new: new_node,
        });
        processed += 1;
    }

    if commands.is_empty() {
        return ToolResult::text("No nodes with transparency found — nothing to flatten.")
            .with_data(serde_json::json!({ "processed": 0 }));
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(Command::Batch(commands), &mut doc);
    drop(history);

    ToolResult::text(format!("Flattened transparency on {} node(s).", processed))
        .with_data(serde_json::json!({ "processed": processed }))
}
pub async fn undo_node(state: &AppState, args: UndoNodeArgs) -> ToolResult {
    let mut doc = state.document.lock().await;

    let uid = match uuid::Uuid::parse_str(&args.node_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.node_id).map(|n| n.id))
    {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    if !doc.nodes.contains_key(&uid) {
        return ToolResult::error(format!("Node '{}' not found.", args.node_id));
    }

    let steps = args.steps.unwrap_or(1).max(1);
    let mut history = state.history.lock().await;

    match history.revert_node_steps(uid, steps, &mut doc) {
        Some(actual) => ToolResult::text(format!(
            "Reverted node '{}' by {} history step(s).",
            args.node_id, actual
        ))
        .with_data(serde_json::json!({
            "node_id": uid.to_string(),
            "steps_reverted": actual,
        })),
        None => ToolResult::text(format!(
            "Node '{}' has no edits in history — nothing to revert.",
            args.node_id
        ))
        .with_data(serde_json::json!({ "node_id": uid.to_string(), "steps_reverted": 0 })),
    }
}
