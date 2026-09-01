use crate::handlers::shared::{cloning::*, random::*};
use crate::protocol::*;
use crate::server::AppState;
use kurbo;
use photonic_core::{
    history::Command,
    node::{GroupNode, NodeId, PathNode, SceneNode, SceneNodeKind},
    path::PathData,
    transform::Transform,
};

pub async fn reorder_node(state: &AppState, args: ReorderNodeArgs) -> ToolResult {
    let mut doc = state.document.lock().await;

    let (layer_id, current_index) = match doc.node_layer_and_index(&args.node_id) {
        Some(v) => v,
        None => return ToolResult::error(format!("Node {} not found", args.node_id)),
    };

    let layer_len = doc
        .layers
        .get(&layer_id)
        .map(|l| l.node_ids.len())
        .unwrap_or(0);
    if layer_len == 0 {
        return ToolResult::error("Layer is empty");
    }

    let new_index = match args.operation {
        ReorderOperation::SendToBack => 0,
        ReorderOperation::BringToFront => layer_len - 1,
        ReorderOperation::SendBackward => current_index.saturating_sub(1),
        ReorderOperation::BringForward => (current_index + 1).min(layer_len - 1),
        ReorderOperation::MoveAbove | ReorderOperation::MoveBelow => {
            let rel_id = match args.relative_id {
                Some(id) => id,
                None => {
                    return ToolResult::error("relative_id is required for move_above / move_below")
                }
            };
            let (rel_layer, rel_index) = match doc.node_layer_and_index(&rel_id) {
                Some(v) => v,
                None => return ToolResult::error(format!("Relative node {} not found", rel_id)),
            };
            if rel_layer != layer_id {
                return ToolResult::error("Nodes must be in the same layer");
            }
            // Compute position in the post-removal list (removing our node first)
            let adj_rel = if current_index < rel_index {
                rel_index - 1
            } else {
                rel_index
            };
            match args.operation {
                ReorderOperation::MoveAbove => (adj_rel + 1).min(layer_len - 1),
                ReorderOperation::MoveBelow => adj_rel,
                _ => unreachable!(),
            }
        }
    };

    let cmd = Command::ReorderNode {
        layer_id,
        node_id: args.node_id,
        old_index: current_index,
        new_index,
    };
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!(
        "Reordered node {} from z-index {} to {}",
        args.node_id, current_index, new_index
    ))
}
pub async fn apply_transform(state: &AppState, args: ApplyTransformArgs) -> ToolResult {
    tracing::debug!(
        "tool: apply_transform {:?} on {} nodes",
        args.operation,
        args.node_ids.len()
    );
    use photonic_core::{ops::transform_ops, transform::Transform};

    let missing_parameter = match args.operation {
        TransformOperation::Translate if args.translate.is_none() => Some("translate"),
        TransformOperation::Rotate if args.rotate.is_none() => Some("rotate"),
        TransformOperation::Scale if args.scale.is_none() => Some("scale"),
        TransformOperation::Matrix if args.matrix.is_none() => Some("matrix"),
        TransformOperation::Shear if args.shear.is_none() => Some("shear"),
        _ => None,
    };
    if let Some(parameter) = missing_parameter {
        return ToolResult::error(format!(
            "apply_transform operation requires the '{parameter}' parameter"
        ));
    }
    if let Some(matrix) = args.matrix {
        if !matrix.iter().all(|coefficient| coefficient.is_finite()) {
            return ToolResult::error("Transform matrix coefficients must be finite");
        }
    }

    // Read phase: collect the nodes we need, then release the doc lock immediately.
    // Holding a tokio MutexGuard across `.await` blocks the render thread's
    // blocking_lock() call for the entire duration of the loop — causing the
    // window to appear frozen / "Not Responding".
    let old_nodes: Vec<_> = {
        let doc = state.document.lock().await;
        let ids: Vec<_> = if args.node_ids.is_empty() {
            doc.selection.ids().copied().collect()
        } else {
            args.node_ids.clone()
        };
        if ids.is_empty() {
            return ToolResult::error("No nodes specified and no active selection");
        }
        ids.iter()
            .filter_map(|id| doc.get_node(id).cloned())
            .collect()
    }; // doc lock released here

    if old_nodes.is_empty() {
        return ToolResult::error("No nodes specified and no active selection");
    }

    // Prepare phase: compute every transform in-place — no locks held.
    let mut commands: Vec<Command> = Vec::with_capacity(old_nodes.len());
    for node in old_nodes {
        if let SceneNodeKind::Path(path) = &node.kind {
            let parsed = match path.path_data.try_to_bez_path() {
                Ok(parsed) => parsed,
                Err(error) => {
                    return ToolResult::error(format!(
                        "Cannot transform path node {}: invalid geometry: {}",
                        node.id, error
                    ))
                }
            };
            if parsed.segments().next().is_none() {
                return ToolResult::error(format!(
                    "Cannot transform path node {}: path has no drawable geometry",
                    node.id
                ));
            }
        }
        let old = node.clone();
        let mut new_node = node;
        match &args.operation {
            crate::protocol::TransformOperation::Translate => {
                if let Some(t) = &args.translate {
                    transform_ops::translate(&mut new_node, t.x, t.y);
                }
            }
            crate::protocol::TransformOperation::Rotate => {
                if let Some(r) = &args.rotate {
                    transform_ops::rotate(&mut new_node, r.angle_degrees, r.origin_x, r.origin_y);
                }
            }
            crate::protocol::TransformOperation::Scale => {
                if let Some(s) = &args.scale {
                    transform_ops::scale(&mut new_node, s.sx, s.sy, s.origin_x, s.origin_y);
                }
            }
            crate::protocol::TransformOperation::Matrix => {
                if let Some(m) = args.matrix {
                    transform_ops::set_transform(&mut new_node, Transform { matrix: m });
                }
            }
            crate::protocol::TransformOperation::ReflectHorizontal => {
                let cx = new_node
                    .local_bounds()
                    .map(|b| b.x0 + b.width() / 2.0)
                    .unwrap_or(0.0);
                transform_ops::reflect_horizontal(&mut new_node, cx);
            }
            crate::protocol::TransformOperation::ReflectVertical => {
                let cy = new_node
                    .local_bounds()
                    .map(|b| b.y0 + b.height() / 2.0)
                    .unwrap_or(0.0);
                transform_ops::reflect_vertical(&mut new_node, cy);
            }
            crate::protocol::TransformOperation::Shear => {
                if let Some(s) = &args.shear {
                    transform_ops::shear(
                        &mut new_node,
                        s.shear_x,
                        s.shear_y,
                        s.origin_x,
                        s.origin_y,
                    );
                }
            }
        }
        commands.push(Command::UpdateNode { old, new: new_node });
    }

    // Write phase: acquire both locks once, apply all updates as a single
    // batch, then release both immediately. No `.await` between the two lock
    // acquisitions so the render thread is unblocked as quickly as possible.
    let node_count = commands.len();
    let cmd = Command::Batch(commands);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!("Transformed {} node(s)", node_count))
}
/// Align or distribute multiple nodes by their world-space bounding boxes.
///
/// Alignment snaps each node's edge/center to the reference edge/center.
/// Distribution evenly spaces nodes between the two extreme nodes (which stay fixed).
pub async fn align_nodes(state: &AppState, args: AlignNodesArgs) -> ToolResult {
    use photonic_core::transform::Transform;

    if args.node_ids.len() < 2 {
        return ToolResult::error("align_nodes requires at least 2 node IDs");
    }

    // Read phase: clone nodes and capture canvas dimensions under a brief lock.
    let (nodes, canvas_w, canvas_h) = {
        let doc = state.document.lock().await;
        let nodes: Vec<SceneNode> = args
            .node_ids
            .iter()
            .filter_map(|id| doc.nodes.get(id).cloned())
            .collect();
        (nodes, doc.width, doc.height)
    };

    if nodes.len() < 2 {
        return ToolResult::error(format!(
            "Could not find enough nodes — requested {}, found {}",
            args.node_ids.len(),
            nodes.len()
        ));
    }

    // Compute the world-space axis-aligned bounding box for a node.
    // The node's transform is applied to all four corners of the local bbox.
    let world_bounds = |node: &SceneNode| -> Option<(f64, f64, f64, f64)> {
        let local = node.local_bounds()?;
        let corners = [
            (local.x0, local.y0),
            (local.x1, local.y0),
            (local.x1, local.y1),
            (local.x0, local.y1),
        ];
        let pts: Vec<(f64, f64)> = corners
            .iter()
            .map(|(x, y)| node.transform.apply(*x, *y))
            .collect();
        let min_x = pts.iter().map(|(x, _)| *x).fold(f64::INFINITY, f64::min);
        let min_y = pts.iter().map(|(_, y)| *y).fold(f64::INFINITY, f64::min);
        let max_x = pts
            .iter()
            .map(|(x, _)| *x)
            .fold(f64::NEG_INFINITY, f64::max);
        let max_y = pts
            .iter()
            .map(|(_, y)| *y)
            .fold(f64::NEG_INFINITY, f64::max);
        Some((min_x, min_y, max_x, max_y))
    };

    // Pair each node with its world bounds, skipping nodes without computable bounds (groups).
    let node_bounds: Vec<(SceneNode, (f64, f64, f64, f64))> = nodes
        .iter()
        .filter_map(|n| world_bounds(n).map(|b| (n.clone(), b)))
        .collect();

    if node_bounds.is_empty() {
        return ToolResult::error(
            "None of the specified nodes have computable bounds (groups are not supported)",
        );
    }

    // Reference rectangle used as the alignment target.
    let (ref_x0, ref_y0, ref_x1, ref_y1) = match args.anchor {
        AlignAnchor::Canvas => (0.0, 0.0, canvas_w, canvas_h),
        AlignAnchor::KeyObject => {
            // Use the bounds of the designated key object as the fixed reference.
            // Fall back to selection bounds if key_object_id is absent or not found.
            if let Some(key_id) = args.key_object_id {
                if let Some((_, b)) = node_bounds.iter().find(|(n, _)| n.id == key_id) {
                    (b.0, b.1, b.2, b.3)
                } else {
                    // Key object not in the resolved set — fall back to selection.
                    let x0 = node_bounds
                        .iter()
                        .map(|(_, b)| b.0)
                        .fold(f64::INFINITY, f64::min);
                    let y0 = node_bounds
                        .iter()
                        .map(|(_, b)| b.1)
                        .fold(f64::INFINITY, f64::min);
                    let x1 = node_bounds
                        .iter()
                        .map(|(_, b)| b.2)
                        .fold(f64::NEG_INFINITY, f64::max);
                    let y1 = node_bounds
                        .iter()
                        .map(|(_, b)| b.3)
                        .fold(f64::NEG_INFINITY, f64::max);
                    (x0, y0, x1, y1)
                }
            } else {
                // No key_object_id — treat as selection.
                let x0 = node_bounds
                    .iter()
                    .map(|(_, b)| b.0)
                    .fold(f64::INFINITY, f64::min);
                let y0 = node_bounds
                    .iter()
                    .map(|(_, b)| b.1)
                    .fold(f64::INFINITY, f64::min);
                let x1 = node_bounds
                    .iter()
                    .map(|(_, b)| b.2)
                    .fold(f64::NEG_INFINITY, f64::max);
                let y1 = node_bounds
                    .iter()
                    .map(|(_, b)| b.3)
                    .fold(f64::NEG_INFINITY, f64::max);
                (x0, y0, x1, y1)
            }
        }
        AlignAnchor::Selection => {
            let x0 = node_bounds
                .iter()
                .map(|(_, b)| b.0)
                .fold(f64::INFINITY, f64::min);
            let y0 = node_bounds
                .iter()
                .map(|(_, b)| b.1)
                .fold(f64::INFINITY, f64::min);
            let x1 = node_bounds
                .iter()
                .map(|(_, b)| b.2)
                .fold(f64::NEG_INFINITY, f64::max);
            let y1 = node_bounds
                .iter()
                .map(|(_, b)| b.3)
                .fold(f64::NEG_INFINITY, f64::max);
            (x0, y0, x1, y1)
        }
    };

    // Compute phase: build UpdateNode commands for each affected node.
    let commands: Vec<Command> = match args.operation {
        AlignOperation::DistributeHorizontal => {
            let mut sorted = node_bounds.clone();
            sorted.sort_by(|(_, a), (_, b)| {
                a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
            });
            let n = sorted.len();
            let gap = if let Some(s) = args.spacing {
                s
            } else {
                let total_w: f64 = sorted.iter().map(|(_, b)| b.2 - b.0).sum();
                let avail = sorted[n - 1].1 .2 - sorted[0].1 .0;
                (avail - total_w) / (n - 1).max(1) as f64
            };
            // First node is always the anchor; subsequent nodes are placed relative to it.
            let mut cursor = sorted[0].1 .0;
            let mut cmds = Vec::new();
            for (node, bounds) in &sorted {
                let w = bounds.2 - bounds.0;
                let dx = cursor - bounds.0;
                cursor += w + gap;
                if dx.abs() > 1e-9 {
                    let old = node.clone();
                    let mut new = old.clone();
                    new.transform = new.transform.then(&Transform::translate(dx, 0.0));
                    cmds.push(Command::UpdateNode { old, new });
                }
            }
            cmds
        }
        AlignOperation::DistributeVertical => {
            let mut sorted = node_bounds.clone();
            sorted.sort_by(|(_, a), (_, b)| {
                a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
            });
            let n = sorted.len();
            let gap = if let Some(s) = args.spacing {
                s
            } else {
                let total_h: f64 = sorted.iter().map(|(_, b)| b.3 - b.1).sum();
                let avail = sorted[n - 1].1 .3 - sorted[0].1 .1;
                (avail - total_h) / (n - 1).max(1) as f64
            };
            // First node is always the anchor; subsequent nodes are placed relative to it.
            let mut cursor = sorted[0].1 .1;
            let mut cmds = Vec::new();
            for (node, bounds) in &sorted {
                let h = bounds.3 - bounds.1;
                let dy = cursor - bounds.1;
                cursor += h + gap;
                if dy.abs() > 1e-9 {
                    let old = node.clone();
                    let mut new = old.clone();
                    new.transform = new.transform.then(&Transform::translate(0.0, dy));
                    cmds.push(Command::UpdateNode { old, new });
                }
            }
            cmds
        }
        _ => {
            // Positional alignments: snap each node to one edge or center of the reference rect.
            let ref_cx = (ref_x0 + ref_x1) / 2.0;
            let ref_cy = (ref_y0 + ref_y1) / 2.0;
            node_bounds
                .iter()
                .filter_map(|(node, bounds)| {
                    let (nx0, ny0, nx1, ny1) = *bounds;
                    let ncx = (nx0 + nx1) / 2.0;
                    let ncy = (ny0 + ny1) / 2.0;
                    let (dx, dy) = match args.operation {
                        AlignOperation::Left => (ref_x0 - nx0, 0.0),
                        AlignOperation::CenterHorizontal => (ref_cx - ncx, 0.0),
                        AlignOperation::Right => (ref_x1 - nx1, 0.0),
                        AlignOperation::Top => (0.0, ref_y0 - ny0),
                        AlignOperation::CenterVertical => (0.0, ref_cy - ncy),
                        AlignOperation::Bottom => (0.0, ref_y1 - ny1),
                        _ => unreachable!(),
                    };
                    if dx.abs() < 1e-9 && dy.abs() < 1e-9 {
                        return None;
                    }
                    let old = node.clone();
                    let mut new = old.clone();
                    new.transform = new.transform.then(&Transform::translate(dx, dy));
                    Some(Command::UpdateNode { old, new })
                })
                .collect()
        }
    };

    if commands.is_empty() {
        return ToolResult::text("All nodes are already aligned — no changes made");
    }

    let moved = commands.len();
    let batch = Command::Batch(commands);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(batch, &mut doc);

    let op_name = match args.operation {
        AlignOperation::Left => "left",
        AlignOperation::CenterHorizontal => "center_horizontal",
        AlignOperation::Right => "right",
        AlignOperation::Top => "top",
        AlignOperation::CenterVertical => "center_vertical",
        AlignOperation::Bottom => "bottom",
        AlignOperation::DistributeHorizontal => "distribute_horizontal",
        AlignOperation::DistributeVertical => "distribute_vertical",
    };
    let anchor_name = match args.anchor {
        AlignAnchor::Selection => "selection",
        AlignAnchor::Canvas => "canvas",
        AlignAnchor::KeyObject => "key_object",
    };
    let spacing_note = match args.spacing {
        Some(s)
            if matches!(
                args.operation,
                AlignOperation::DistributeHorizontal | AlignOperation::DistributeVertical
            ) =>
        {
            format!(", spacing: {}px", s)
        }
        _ => String::new(),
    };
    ToolResult::text(format!(
        "Aligned {} node(s) — operation: {}, anchor: {}{}",
        moved, op_name, anchor_name, spacing_note
    ))
}
/// Duplicate one or more nodes, optionally creating multiple offset copies.
///
/// Each copy is a full deep clone (groups and all descendants get fresh UUIDs).
/// Copy N is shifted by N × offset from the original position.
/// All copies land in a single undoable batch.
pub async fn duplicate_nodes(state: &AppState, args: DuplicateNodesArgs) -> ToolResult {
    let count = args.count.unwrap_or(1).clamp(1, 100);
    let offset_x = args.offset.as_ref().map(|o| o.x).unwrap_or(10.0);
    let offset_y = args.offset.as_ref().map(|o| o.y).unwrap_or(10.0);

    // Read phase: validate IDs and collect source layer_ids.
    let source_info: Vec<(uuid::Uuid, uuid::Uuid)> = {
        let doc = state.document.lock().await;
        let mut out = Vec::new();
        for id in &args.node_ids {
            match doc.nodes.get(id) {
                Some(n) => out.push((*id, n.layer_id)),
                None => return ToolResult::error(format!("Node {} not found", id)),
            }
        }
        out
    };

    // Clone phase: build all AddNode commands without holding any lock.
    let mut commands: Vec<Command> = Vec::new();
    let mut root_ids: Vec<uuid::Uuid> = Vec::new();

    for copy_idx in 1..=count {
        let dx = offset_x * copy_idx as f64;
        let dy = offset_y * copy_idx as f64;

        // Acquire a read-only snapshot of the document for this copy pass.
        let doc = state.document.lock().await;

        for (src_id, src_layer) in &source_info {
            let target_layer = args.layer_id.unwrap_or(*src_layer);
            let nodes = clone_subtree(&doc, *src_id, target_layer, dx, dy);
            if let Some(root) = nodes.first() {
                root_ids.push(root.id);
            }
            for node in nodes {
                commands.push(Command::AddNode {
                    layer_id: Some(node.layer_id),
                    node,
                });
            }
        }

        drop(doc); // Release before next iteration
    }

    if commands.is_empty() {
        return ToolResult::error("Nothing to duplicate");
    }

    // Write phase: execute as a single batch for a clean one-step undo.
    let cmd = Command::Batch(commands);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    let total_roots = root_ids.len();
    ToolResult::text(format!(
        "Duplicated {} source node(s) × {} copies = {} new root node(s)",
        source_info.len(),
        count,
        total_roots
    ))
    .with_data(serde_json::json!({ "node_ids": root_ids }))
}
/// Repeat a node in a grid or radial pattern, producing a single undoable batch.
///
/// **Grid mode**: The source is treated as cell (row 0, col 0). A new clone is
/// created for every other cell, translated by (col × col_stride, row × row_stride).
///
/// **Radial mode**: The source is treated as instance 0. Each additional instance i
/// is the source rotated around (center_x, center_y) by (i × 360 / count) degrees,
/// so the total visual count (source + copies) equals `count`.
///
/// When `group_result` is true the source and all copies are wrapped into a new
/// group node as part of the same undo step.
pub async fn create_array(state: &AppState, args: CreateArrayArgs) -> ToolResult {
    use photonic_core::transform::Transform;

    // ── Read phase: validate source ───────────────────────────────────────
    let (src_id, src_layer, src_name, src_z) = {
        let doc = state.document.lock().await;
        match doc.nodes.get(&args.node_id) {
            Some(n) => {
                let z = doc.node_layer_and_index(&n.id).map(|(_, i)| i).unwrap_or(0);
                (n.id, n.layer_id, n.name.clone(), z)
            }
            None => return ToolResult::error(format!("Source node {} not found", args.node_id)),
        }
    };

    let target_layer = args.layer_id.unwrap_or(src_layer);
    let prefix = args.name_prefix.unwrap_or_else(|| src_name.clone());

    // ── Compute per-copy transforms ───────────────────────────────────────
    // Each transform is applied on top of the source's existing transform.
    // The source is NOT moved — it is implicitly "instance 0".
    let copy_transforms: Vec<(String, Transform)> = match args.mode {
        ArrayMode::Grid => {
            let rows = args.rows.unwrap_or(2).max(1);
            let cols = args.cols.unwrap_or(2).max(1);
            let cell_count = match rows.checked_mul(cols) {
                Some(cell_count) => cell_count,
                None => {
                    return ToolResult::error(
                        "Grid dimensions overflow before array allocation (rows × cols)",
                    )
                }
            };
            if cell_count > MAX_ARRAY_GRID_CELLS {
                return ToolResult::error(format!(
                    "Grid must have at most {MAX_ARRAY_GRID_CELLS} cells (rows × cols)"
                ));
            }
            if cell_count < 2 {
                return ToolResult::error("Grid must have at least 2 cells (rows × cols ≥ 2)");
            }
            let dx = args.col_stride.unwrap_or(100.0);
            let dy = args.row_stride.unwrap_or(100.0);

            let mut out = Vec::with_capacity(cell_count - 1);
            let mut n = 1usize;
            for r in 0..rows {
                for c in 0..cols {
                    if r == 0 && c == 0 {
                        continue; // source already occupies (0, 0)
                    }
                    out.push((
                        format!("{} {}", prefix, n),
                        Transform::translate(c as f64 * dx, r as f64 * dy),
                    ));
                    n += 1;
                }
            }
            out
        }

        ArrayMode::Radial => {
            let count = args.count.unwrap_or(6);
            if count < 2 {
                return ToolResult::error("Radial count must be ≥ 2");
            }
            let cx = args.center_x.unwrap_or(0.0);
            let cy = args.center_y.unwrap_or(0.0);
            let start_deg = args.start_angle_degrees.unwrap_or(0.0);
            let step_deg = 360.0 / count as f64;

            (1..count)
                .map(|i| {
                    let angle_rad = (start_deg + i as f64 * step_deg).to_radians();
                    (
                        format!("{} {}", prefix, i),
                        Transform::rotate_around(angle_rad, cx, cy),
                    )
                })
                .collect()
        }
    };

    if copy_transforms.is_empty() {
        return ToolResult::error("No copies to create");
    }

    // ── Clone phase ────────────────────────────────────────────────────────
    let mut commands: Vec<Command> = Vec::new();
    let mut new_root_ids: Vec<uuid::Uuid> = Vec::new();

    for (copy_name, extra_transform) in &copy_transforms {
        // Acquire a fresh snapshot for each clone pass (matches duplicate_nodes pattern).
        let doc = state.document.lock().await;
        // clone_subtree with (dx=0, dy=0) preserves the source's transform exactly;
        // we then compose our extra_transform on top of it.
        let mut nodes = clone_subtree(&doc, src_id, target_layer, 0.0, 0.0);
        drop(doc);

        if let Some(root) = nodes.first_mut() {
            root.name = copy_name.clone();
            root.transform = root.transform.then(extra_transform);
            new_root_ids.push(root.id);
        }

        for node in nodes {
            commands.push(Command::AddNode {
                layer_id: Some(node.layer_id),
                node,
            });
        }
    }

    // ── Optional group ─────────────────────────────────────────────────────
    // Runs AFTER all AddNodes so every child already exists in the document.
    let mut group_id: Option<uuid::Uuid> = None;
    if args.group_result {
        let gid = uuid::Uuid::new_v4();
        let mut all_children = vec![src_id];
        all_children.extend_from_slice(&new_root_ids);

        let group_kind = SceneNodeKind::Group(GroupNode {
            children: all_children.clone(),
            clip_children: false,
            clip_node_id: None,
            blend_spine_id: None,
            live_boolean: None,
        });
        let group_name = format!("{} Array", src_name);
        let mut group_node = SceneNode::new(&group_name, target_layer, group_kind);
        group_node.id = gid;

        // insert_index: place the group where the source currently lives.
        // After GroupNodes removes source + copies from the layer and inserts the
        // group at src_z, the result sits at the same z-stack position.
        commands.push(Command::GroupNodes {
            group: group_node,
            layer_id: target_layer,
            insert_index: src_z,
            children: all_children,
        });

        group_id = Some(gid);
    }

    // ── Write phase ────────────────────────────────────────────────────────
    let cmd = Command::Batch(commands);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    let mode_label = match args.mode {
        ArrayMode::Grid => "grid",
        ArrayMode::Radial => "radial",
    };
    ToolResult::text(format!(
        "Created {} {} array: {} new copies of '{}'{}",
        mode_label,
        mode_label,
        new_root_ids.len(),
        src_name,
        if group_id.is_some() { " (grouped)" } else { "" },
    ))
    .with_data(serde_json::json!({
        "source_id": src_id,
        "node_ids":  new_root_ids,
        "group_id":  group_id,
    }))
}
/// Rearrange a set of existing nodes according to a spatial layout algorithm.
///
/// Supports four layouts:
/// - `grid`             — left-to-right, wrapping rows
/// - `circle`           — evenly spaced around a circle
/// - `stack_horizontal` — left-to-right with a gap
/// - `stack_vertical`   — top-to-bottom with a gap
pub async fn layout_nodes(state: &AppState, args: LayoutNodesArgs) -> ToolResult {
    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    // ── 1. Read phase: collect nodes + their world-space AABB ────────────────
    struct NodeItem {
        node: SceneNode,
        /// world-space AABB: (x0, y0, x1, y1)
        bounds: (f64, f64, f64, f64),
    }

    let world_bounds = |node: &SceneNode| -> Option<(f64, f64, f64, f64)> {
        let local = node.local_bounds()?;
        let corners = [
            (local.x0, local.y0),
            (local.x1, local.y0),
            (local.x1, local.y1),
            (local.x0, local.y1),
        ];
        let pts: Vec<(f64, f64)> = corners
            .iter()
            .map(|(x, y)| node.transform.apply(*x, *y))
            .collect();
        let x0 = pts.iter().map(|(x, _)| *x).fold(f64::INFINITY, f64::min);
        let y0 = pts.iter().map(|(_, y)| *y).fold(f64::INFINITY, f64::min);
        let x1 = pts
            .iter()
            .map(|(x, _)| *x)
            .fold(f64::NEG_INFINITY, f64::max);
        let y1 = pts
            .iter()
            .map(|(_, y)| *y)
            .fold(f64::NEG_INFINITY, f64::max);
        Some((x0, y0, x1, y1))
    };

    let items: Vec<NodeItem> = {
        let doc = state.document.lock().await;
        let mut out = Vec::with_capacity(args.node_ids.len());
        for id in &args.node_ids {
            let Some(node) = doc.nodes.get(id).cloned() else {
                return ToolResult::error(format!("Node not found: {}", id));
            };
            let Some(bounds) = world_bounds(&node) else {
                return ToolResult::error(format!(
                    "Node '{}' has no computable bounds (groups are not supported)",
                    node.name
                ));
            };
            out.push(NodeItem { node, bounds });
        }
        out
    };

    let n = items.len();

    // Combined bounding box of the current selection.
    let sel_x0 = items
        .iter()
        .map(|i| i.bounds.0)
        .fold(f64::INFINITY, f64::min);
    let sel_y0 = items
        .iter()
        .map(|i| i.bounds.1)
        .fold(f64::INFINITY, f64::min);
    let sel_x1 = items
        .iter()
        .map(|i| i.bounds.2)
        .fold(f64::NEG_INFINITY, f64::max);
    let sel_y1 = items
        .iter()
        .map(|i| i.bounds.3)
        .fold(f64::NEG_INFINITY, f64::max);

    // ── 2. Compute target positions per layout ────────────────────────────────
    // Returns (target_x, target_y) for the top-left corner of each node's AABB.
    let targets: Vec<(f64, f64)> = match args.layout {
        // ── Grid ─────────────────────────────────────────────────────────────
        LayoutMode::Grid => {
            let cols = args
                .columns
                .unwrap_or_else(|| (n as f64).sqrt().ceil() as usize)
                .max(1);
            let gap_x = args.gap_x.unwrap_or(20.0);
            let gap_y = args.gap_y.unwrap_or(20.0);

            // Default cell size = widest / tallest node; overridable per axis.
            let cell_w = args.cell_width.unwrap_or_else(|| {
                items
                    .iter()
                    .map(|i| i.bounds.2 - i.bounds.0)
                    .fold(0.0_f64, f64::max)
            });
            let cell_h = args.cell_height.unwrap_or_else(|| {
                items
                    .iter()
                    .map(|i| i.bounds.3 - i.bounds.1)
                    .fold(0.0_f64, f64::max)
            });

            let origin_x = args.x.unwrap_or(sel_x0);
            let origin_y = args.y.unwrap_or(sel_y0);

            items
                .iter()
                .enumerate()
                .map(|(idx, item)| {
                    let col = idx % cols;
                    let row = idx / cols;
                    let cell_x = origin_x + col as f64 * (cell_w + gap_x);
                    let cell_y = origin_y + row as f64 * (cell_h + gap_y);
                    // Centre the node inside its cell.
                    let node_w = item.bounds.2 - item.bounds.0;
                    let node_h = item.bounds.3 - item.bounds.1;
                    (
                        cell_x + (cell_w - node_w) / 2.0,
                        cell_y + (cell_h - node_h) / 2.0,
                    )
                })
                .collect()
        }

        // ── Circle ────────────────────────────────────────────────────────────
        LayoutMode::Circle => {
            let centre_x = args.cx.unwrap_or((sel_x0 + sel_x1) / 2.0);
            let centre_y = args.cy.unwrap_or((sel_y0 + sel_y1) / 2.0);
            let radius = args.radius.unwrap_or(200.0);
            let start_deg = args.start_angle.unwrap_or(0.0);
            let angle_step = 360.0 / n as f64;

            items
                .iter()
                .enumerate()
                .map(|(idx, item)| {
                    let angle = (start_deg + idx as f64 * angle_step).to_radians();
                    let node_cx = centre_x + radius * angle.cos();
                    let node_cy = centre_y + radius * angle.sin();
                    let node_w = item.bounds.2 - item.bounds.0;
                    let node_h = item.bounds.3 - item.bounds.1;
                    (node_cx - node_w / 2.0, node_cy - node_h / 2.0)
                })
                .collect()
        }

        // ── Stack horizontal ──────────────────────────────────────────────────
        LayoutMode::StackHorizontal => {
            let gap = args.gap.unwrap_or(20.0);
            let origin_x = args.x.unwrap_or(sel_x0);
            let origin_y = args.y.unwrap_or(sel_y0);

            // Cross-axis (Y) reference.
            let tallest = items
                .iter()
                .map(|i| i.bounds.3 - i.bounds.1)
                .fold(0.0_f64, f64::max);
            let cross_ref_y = match args.align {
                CrossAxisAlign::Start => origin_y,
                CrossAxisAlign::Center => origin_y + tallest / 2.0,
                CrossAxisAlign::End => origin_y + tallest,
            };

            let mut cursor = origin_x;
            items
                .iter()
                .map(|item| {
                    let w = item.bounds.2 - item.bounds.0;
                    let h = item.bounds.3 - item.bounds.1;
                    let tx = cursor;
                    let ty = match args.align {
                        CrossAxisAlign::Start => cross_ref_y,
                        CrossAxisAlign::Center => cross_ref_y - h / 2.0,
                        CrossAxisAlign::End => cross_ref_y - h,
                    };
                    cursor += w + gap;
                    (tx, ty)
                })
                .collect()
        }

        // ── Stack vertical ────────────────────────────────────────────────────
        LayoutMode::StackVertical => {
            let gap = args.gap.unwrap_or(20.0);
            let origin_x = args.x.unwrap_or(sel_x0);
            let origin_y = args.y.unwrap_or(sel_y0);

            // Cross-axis (X) reference.
            let widest = items
                .iter()
                .map(|i| i.bounds.2 - i.bounds.0)
                .fold(0.0_f64, f64::max);
            let cross_ref_x = match args.align {
                CrossAxisAlign::Start => origin_x,
                CrossAxisAlign::Center => origin_x + widest / 2.0,
                CrossAxisAlign::End => origin_x + widest,
            };

            let mut cursor = origin_y;
            items
                .iter()
                .map(|item| {
                    let w = item.bounds.2 - item.bounds.0;
                    let h = item.bounds.3 - item.bounds.1;
                    let ty = cursor;
                    let tx = match args.align {
                        CrossAxisAlign::Start => cross_ref_x,
                        CrossAxisAlign::Center => cross_ref_x - w / 2.0,
                        CrossAxisAlign::End => cross_ref_x - w,
                    };
                    cursor += h + gap;
                    (tx, ty)
                })
                .collect()
        }
    };

    // ── 3. Build UpdateNode commands ──────────────────────────────────────────
    let commands: Vec<Command> = items
        .iter()
        .zip(targets.iter())
        .filter_map(|(item, (tx, ty))| {
            let dx = tx - item.bounds.0;
            let dy = ty - item.bounds.1;
            if dx.abs() < 1e-9 && dy.abs() < 1e-9 {
                return None; // already in position
            }
            let old = item.node.clone();
            let mut new = old.clone();
            new.transform = new.transform.then(&Transform::translate(dx, dy));
            Some(Command::UpdateNode { old, new })
        })
        .collect();

    if commands.is_empty() {
        return ToolResult::text("All nodes are already in the target positions — nothing changed");
    }

    let moved = commands.len();
    {
        let mut doc = state.document.lock().await;
        let mut history = state.history.lock().await;
        history.execute_discrete(Command::Batch(commands), &mut doc);
    }

    ToolResult::text(format!(
        "layout_nodes: moved {} of {} node(s) using {:?} layout",
        moved, n, args.layout
    ))
    .with_data(serde_json::json!({ "moved": moved, "total": n }))
}
pub async fn flatten_group(state: &AppState, args: FlattenGroupArgs) -> ToolResult {
    tracing::debug!("tool: flatten_group");

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

    // Collect all group IDs that need flattening (recursive).
    fn collect_groups(doc: &photonic_core::Document, nid: NodeId, result: &mut Vec<NodeId>) {
        if let Some(node) = doc.nodes.get(&nid) {
            if let SceneNodeKind::Group(g) = &node.kind {
                // Depth-first: flatten children first.
                for &child_id in &g.children {
                    collect_groups(doc, child_id, result);
                }
                result.push(nid);
            }
        }
    }

    let mut groups_to_ungroup = Vec::new();
    for &nid in &node_ids {
        collect_groups(&doc, nid, &mut groups_to_ungroup);
    }

    if groups_to_ungroup.is_empty() {
        return ToolResult::error("No groups found to flatten");
    }

    // Ungroup from innermost to outermost (depth-first order).
    let mut ungrouped = 0usize;
    for group_id in &groups_to_ungroup {
        // Re-check because previous ungroupings may have changed the tree.
        let node = match doc.nodes.get(group_id) {
            Some(n) => n.clone(),
            None => continue,
        };
        if let SceneNodeKind::Group(g) = &node.kind {
            let children = g.children.clone();
            let layer_id = node.layer_id;

            // Find the group's index in its layer.
            let group_index = doc
                .layers
                .get(&layer_id)
                .and_then(|l| l.node_ids.iter().position(|id| id == group_id))
                .unwrap_or(0);

            history.execute_discrete(
                Command::UngroupNodes {
                    group: node,
                    layer_id,
                    group_index,
                    children,
                },
                &mut doc,
            );
            ungrouped += 1;
        }
    }

    ToolResult::text(format!("Flattened {ungrouped} group(s)"))
        .with_data(serde_json::json!({ "ungrouped": ungrouped }))
}
pub async fn center_on_canvas(state: &AppState, args: CenterOnCanvasArgs) -> ToolResult {
    tracing::debug!("tool: center_on_canvas");
    use kurbo::Shape;

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let canvas_cx = doc.width / 2.0;
    let canvas_cy = doc.height / 2.0;

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

    // Compute combined bbox.
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for nid in &node_ids {
        if let Some(node) = doc.nodes.get(nid) {
            if let SceneNodeKind::Path(pn) = &node.kind {
                let bb = pn.path_data.to_bez_path().bounding_box();
                let tx = node.transform.matrix[4];
                let ty = node.transform.matrix[5];
                min_x = min_x.min(bb.x0 + tx);
                min_y = min_y.min(bb.y0 + ty);
                max_x = max_x.max(bb.x1 + tx);
                max_y = max_y.max(bb.y1 + ty);
            } else {
                let tx = node.transform.matrix[4];
                let ty = node.transform.matrix[5];
                min_x = min_x.min(tx);
                min_y = min_y.min(ty);
                max_x = max_x.max(tx);
                max_y = max_y.max(ty);
            }
        }
    }

    if min_x >= max_x && min_y >= max_y {
        return ToolResult::error("No measurable artwork");
    }

    let art_cx = (min_x + max_x) / 2.0;
    let art_cy = (min_y + max_y) / 2.0;
    let dx = if args.horizontal {
        canvas_cx - art_cx
    } else {
        0.0
    };
    let dy = if args.vertical {
        canvas_cy - art_cy
    } else {
        0.0
    };

    let mut modified = 0usize;
    for nid in &node_ids {
        if let Some(node) = doc.nodes.get(nid) {
            let mut new_node = node.clone();
            new_node.transform.matrix[4] += dx;
            new_node.transform.matrix[5] += dy;
            history.execute_discrete(
                Command::UpdateNode {
                    old: node.clone(),
                    new: new_node,
                },
                &mut doc,
            );
            modified += 1;
        }
    }

    ToolResult::text(format!(
        "Centered {modified} node(s) on canvas (dx={dx:.1}, dy={dy:.1})"
    ))
    .with_data(serde_json::json!({ "modified": modified, "dx": dx, "dy": dy }))
}
pub async fn fit_to_canvas(state: &AppState, args: FitToCanvasArgs) -> ToolResult {
    tracing::debug!("tool: fit_to_canvas");
    use kurbo::Shape;

    let padding = args.padding.unwrap_or(10.0);

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let canvas_w = doc.width;
    let canvas_h = doc.height;

    // Gather target nodes.
    let target_ids: Vec<NodeId> = if args.node_ids.is_empty() {
        doc.nodes.keys().copied().collect()
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

    if target_ids.is_empty() {
        return ToolResult::error("No nodes to fit");
    }

    // Compute combined bounding box of all target paths.
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for nid in &target_ids {
        if let Some(node) = doc.nodes.get(nid) {
            if let SceneNodeKind::Path(pn) = &node.kind {
                let bez = pn.path_data.to_bez_path();
                let bb = bez.bounding_box();
                let tx = node.transform.matrix[4];
                let ty = node.transform.matrix[5];
                min_x = min_x.min(bb.x0 + tx);
                min_y = min_y.min(bb.y0 + ty);
                max_x = max_x.max(bb.x1 + tx);
                max_y = max_y.max(bb.y1 + ty);
            }
        }
    }

    if min_x >= max_x || min_y >= max_y {
        return ToolResult::error("No measurable artwork found");
    }

    let art_w = max_x - min_x;
    let art_h = max_y - min_y;
    let art_cx = (min_x + max_x) / 2.0;
    let art_cy = (min_y + max_y) / 2.0;

    let target_w = canvas_w - 2.0 * padding;
    let target_h = canvas_h - 2.0 * padding;
    if target_w <= 0.0 || target_h <= 0.0 {
        return ToolResult::error("Canvas too small for the specified padding");
    }

    let scale = (target_w / art_w).min(target_h / art_h).min(1.0); // Don't scale up
    let canvas_cx = canvas_w / 2.0;
    let canvas_cy = canvas_h / 2.0;

    // Apply uniform scale + translate to center.
    let mut modified = 0usize;
    for nid in &target_ids {
        if let Some(node) = doc.nodes.get(nid) {
            if let SceneNodeKind::Path(pn) = &node.kind {
                let bez = pn.path_data.to_bez_path();
                let mut new_bez = kurbo::BezPath::new();

                for el in bez.elements() {
                    let xform = |p: kurbo::Point| -> kurbo::Point {
                        let nx = (p.x + node.transform.matrix[4] - art_cx) * scale + canvas_cx;
                        let ny = (p.y + node.transform.matrix[5] - art_cy) * scale + canvas_cy;
                        kurbo::Point::new(nx, ny)
                    };
                    match *el {
                        kurbo::PathEl::MoveTo(p) => new_bez.move_to(xform(p)),
                        kurbo::PathEl::LineTo(p) => new_bez.line_to(xform(p)),
                        kurbo::PathEl::CurveTo(c1, c2, p) => {
                            new_bez.curve_to(xform(c1), xform(c2), xform(p))
                        }
                        kurbo::PathEl::QuadTo(c, p) => new_bez.quad_to(xform(c), xform(p)),
                        kurbo::PathEl::ClosePath => new_bez.close_path(),
                    }
                }

                let mut new_node = node.clone();
                if let SceneNodeKind::Path(ref mut np) = new_node.kind {
                    np.path_data = PathData::from_bez_path(&new_bez);
                }
                new_node.transform = Transform::default();
                history.execute_discrete(
                    Command::UpdateNode {
                        old: node.clone(),
                        new: new_node,
                    },
                    &mut doc,
                );
                modified += 1;
            }
        }
    }

    ToolResult::text(format!(
        "Fit {modified} node(s) to canvas (scale={scale:.2})"
    ))
    .with_data(serde_json::json!({ "modified": modified, "scale": scale }))
}
pub async fn scatter_copies(state: &AppState, args: ScatterCopiesArgs) -> ToolResult {
    tracing::debug!("tool: scatter_copies");

    let count = args.count.unwrap_or(20).max(1);
    if count > MAX_GENERATED_WORK {
        return ToolResult::error(format!(
            "scatter_copies may generate at most {MAX_GENERATED_WORK} copies"
        ));
    }
    let rot_range = args.rotation_range.unwrap_or(0.0).abs();
    let scale_range = args.scale_range.unwrap_or(0.0).abs();
    let seed = args.seed.unwrap_or(42).max(1);

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let src_nid = match uuid::Uuid::parse_str(&args.node_id) {
        Ok(id) => id,
        Err(_) => match doc.find_node_by_name(&args.node_id) {
            Some(n) => n.id,
            None => return ToolResult::error(format!("Node not found: {}", args.node_id)),
        },
    };
    let source = match doc.nodes.get(&src_nid) {
        Some(n) => n.clone(),
        None => return ToolResult::error("Source node not found"),
    };

    let layer_id = source.layer_id;
    let mut rng = seed;
    let mut created_ids = Vec::new();

    for i in 0..count {
        let rx = (xorshift64(&mut rng) * 0.5 + 0.5) * args.width + args.x;
        let ry = (xorshift64(&mut rng) * 0.5 + 0.5) * args.height + args.y;
        let rot = if rot_range > 0.0 {
            xorshift64(&mut rng) * rot_range
        } else {
            0.0
        };
        let rot_rad = rot.to_radians();
        let s = if scale_range > 0.0 {
            1.0 + xorshift64(&mut rng) * scale_range
        } else {
            1.0
        };

        let cos_r = rot_rad.cos();
        let sin_r = rot_rad.sin();

        let mut new_node = source.clone();
        new_node.id = uuid::Uuid::new_v4();
        new_node.name = format!("{} #{}", source.name, i + 1);
        new_node.transform = Transform {
            matrix: [s * cos_r, s * sin_r, -s * sin_r, s * cos_r, rx, ry],
        };

        let nid = new_node.id;
        created_ids.push(nid);
        history.execute_discrete(
            Command::AddNode {
                node: new_node,
                layer_id: Some(layer_id),
            },
            &mut doc,
        );
    }

    ToolResult::text(format!(
        "Scattered {} copies of '{}' in area ({},{}) {}×{}",
        count, source.name, args.x, args.y, args.width, args.height
    ))
    .with_data(serde_json::json!({ "count": count, "created_ids": created_ids }))
}
pub async fn flip_nodes(state: &AppState, args: FlipNodesArgs) -> ToolResult {
    tracing::debug!("tool: flip_nodes");
    use kurbo::Shape;

    let flip_h = args.axis == "horizontal";
    let flip_v = args.axis == "vertical";
    if !flip_h && !flip_v {
        return ToolResult::error("axis must be 'horizontal' or 'vertical'");
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

        match &mut new_node.kind {
            SceneNodeKind::Path(pn) => {
                let bez = pn.path_data.to_bez_path();
                let bbox = bez.bounding_box();
                let cx = bbox.x0 + bbox.width() / 2.0;
                let cy = bbox.y0 + bbox.height() / 2.0;

                let flip_point = |p: kurbo::Point| -> kurbo::Point {
                    kurbo::Point::new(
                        if flip_h { 2.0 * cx - p.x } else { p.x },
                        if flip_v { 2.0 * cy - p.y } else { p.y },
                    )
                };

                let mut new_bez = kurbo::BezPath::new();
                for el in bez.elements() {
                    match *el {
                        kurbo::PathEl::MoveTo(p) => new_bez.move_to(flip_point(p)),
                        kurbo::PathEl::LineTo(p) => new_bez.line_to(flip_point(p)),
                        kurbo::PathEl::CurveTo(c1, c2, p) => {
                            new_bez.curve_to(flip_point(c1), flip_point(c2), flip_point(p))
                        }
                        kurbo::PathEl::QuadTo(c, p) => {
                            new_bez.quad_to(flip_point(c), flip_point(p))
                        }
                        kurbo::PathEl::ClosePath => new_bez.close_path(),
                    }
                }
                pn.path_data = PathData::from_bez_path(&new_bez);
            }
            // raster: no path geometry — flip via transform scale like text/groups
            SceneNodeKind::Text(_) | SceneNodeKind::Group(_) | SceneNodeKind::Raster(_) => {
                // For text/groups, flip via transform scale.
                if flip_h {
                    new_node.transform.matrix[0] *= -1.0;
                    new_node.transform.matrix[2] *= -1.0;
                }
                if flip_v {
                    new_node.transform.matrix[1] *= -1.0;
                    new_node.transform.matrix[3] *= -1.0;
                }
            }
        }

        history.execute_discrete(
            Command::UpdateNode {
                old: node,
                new: new_node,
            },
            &mut doc,
        );
        modified += 1;
    }

    let axis_label = if flip_h { "horizontally" } else { "vertically" };
    ToolResult::text(format!("Flipped {modified} node(s) {axis_label}"))
        .with_data(serde_json::json!({ "modified": modified, "axis": args.axis }))
}
pub async fn transform_copies(state: &AppState, args: TransformCopiesArgs) -> ToolResult {
    tracing::debug!("tool: transform_copies");

    let copies = args.copies.unwrap_or(5).max(1);
    let tx = args.translate_x.unwrap_or(0.0);
    let ty = args.translate_y.unwrap_or(0.0);
    let rot_deg = args.rotate.unwrap_or(0.0);
    let scale_factor = args.scale.unwrap_or(1.0);
    let opacity_step = args.opacity_step.unwrap_or(1.0);

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let nid = match uuid::Uuid::parse_str(&args.node_id) {
        Ok(id) => id,
        Err(_) => match doc.find_node_by_name(&args.node_id) {
            Some(n) => n.id,
            None => return ToolResult::error(format!("Node not found: {}", args.node_id)),
        },
    };

    let source = match doc.nodes.get(&nid) {
        Some(n) => n.clone(),
        None => return ToolResult::error("Source node not found"),
    };

    let layer_id = source.layer_id;
    let mut created_ids = Vec::new();

    // Each copy accumulates transforms from the previous.
    let base_matrix = source.transform.matrix;

    for i in 1..=copies {
        let mut new_node = source.clone();
        new_node.id = uuid::Uuid::new_v4();
        new_node.name = format!("{} Copy {}", source.name, i);

        // Compute cumulative transform: translate, rotate, scale applied i times.
        let cumulative_tx = tx * i as f64;
        let cumulative_ty = ty * i as f64;
        let cumulative_rot = (rot_deg * i as f64).to_radians();
        let cumulative_scale = scale_factor.powi(i as i32);
        let cumulative_opacity = opacity_step.powi(i as i32);

        // Build the incremental transform matrix.
        let cos_r = cumulative_rot.cos();
        let sin_r = cumulative_rot.sin();
        let s = cumulative_scale;

        // Transform: scale * rotate, then translate
        // [s*cos  -s*sin  tx]
        // [s*sin   s*cos  ty]
        let inc_matrix = [
            s * cos_r,
            s * sin_r,
            -s * sin_r,
            s * cos_r,
            cumulative_tx,
            cumulative_ty,
        ];

        // Compose: inc_matrix * base_matrix
        let a = inc_matrix;
        let b = base_matrix;
        new_node.transform = Transform {
            matrix: [
                a[0] * b[0] + a[2] * b[1],
                a[1] * b[0] + a[3] * b[1],
                a[0] * b[2] + a[2] * b[3],
                a[1] * b[2] + a[3] * b[3],
                a[0] * b[4] + a[2] * b[5] + a[4],
                a[1] * b[4] + a[3] * b[5] + a[5],
            ],
        };

        new_node.opacity = (source.opacity * cumulative_opacity).clamp(0.0, 1.0);

        let copy_id = new_node.id;
        created_ids.push(copy_id);
        history.execute_discrete(
            Command::AddNode {
                node: new_node,
                layer_id: Some(layer_id),
            },
            &mut doc,
        );
    }

    ToolResult::text(format!(
        "Created {} copies of '{}' (translate=[{tx},{ty}], rotate={rot_deg}°, scale={scale_factor})",
        copies, source.name
    ))
    .with_data(serde_json::json!({
        "copies": copies,
        "created_ids": created_ids,
    }))
}
/// Divide a path node's bounding box into a rows×cols grid of rectangle nodes.
pub async fn split_into_grid(state: &AppState, args: SplitIntoGridArgs) -> ToolResult {
    if args.rows == 0 {
        return ToolResult::error("rows must be ≥ 1");
    }
    if args.cols == 0 {
        return ToolResult::error("cols must be ≥ 1");
    }
    let cell_count = match args.rows.checked_mul(args.cols) {
        Some(cell_count) => cell_count,
        None => return ToolResult::error("rows × cols overflow before grid allocation"),
    };
    if cell_count > MAX_GENERATED_WORK {
        return ToolResult::error(format!(
            "split_into_grid may generate at most {MAX_GENERATED_WORK} cells (rows × cols)"
        ));
    }

    let mut doc = state.document.lock().await;

    // Read source node.
    let source = match doc.nodes.get(&args.node_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("node {} not found", args.node_id)),
    };

    // Source must be a path.
    let path_node = match &source.kind {
        SceneNodeKind::Path(p) => p.clone(),
        _ => return ToolResult::error("split_into_grid requires a path node"),
    };

    // Get local bounding box.
    let local_bbox = match path_node.path_data.bounding_box() {
        Some(b) => b,
        None => return ToolResult::error("source path has no computable bounding box"),
    };

    // Apply the source node's transform to the four corners to get world-space bounds.
    let t = &source.transform;
    let corners = [
        t.apply(local_bbox.x0, local_bbox.y0),
        t.apply(local_bbox.x1, local_bbox.y0),
        t.apply(local_bbox.x0, local_bbox.y1),
        t.apply(local_bbox.x1, local_bbox.y1),
    ];
    let min_x = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
    let min_y = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
    let max_x = corners
        .iter()
        .map(|c| c.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_y = corners
        .iter()
        .map(|c| c.1)
        .fold(f64::NEG_INFINITY, f64::max);
    let total_w = max_x - min_x;
    let total_h = max_y - min_y;

    let gx = args.gutter_x.unwrap_or(0.0).max(0.0);
    let gy = args.gutter_y.unwrap_or(0.0).max(0.0);

    let cell_w = (total_w - gx * (args.cols as f64 - 1.0)) / args.cols as f64;
    let cell_h = (total_h - gy * (args.rows as f64 - 1.0)) / args.rows as f64;

    if cell_w <= 0.0 {
        return ToolResult::error(format!(
            "gutter_x ({gx}) is too large — cells would have non-positive width ({cell_w:.2})"
        ));
    }
    if cell_h <= 0.0 {
        return ToolResult::error(format!(
            "gutter_y ({gy}) is too large — cells would have non-positive height ({cell_h:.2})"
        ));
    }

    let target_layer = args.layer_id.unwrap_or(source.layer_id);
    let keep = args.keep_original.unwrap_or(false);
    let source_name = source.name.clone();

    let mut commands: Vec<Command> = Vec::with_capacity(cell_count + if keep { 0 } else { 1 });
    let mut created_ids: Vec<uuid::Uuid> = Vec::with_capacity(cell_count);

    for r in 0..args.rows {
        for c in 0..args.cols {
            let x = min_x + c as f64 * (cell_w + gx);
            let y = min_y + r as f64 * (cell_h + gy);

            let pd = PathData::rect(x, y, cell_w, cell_h);
            let mut cell_pn = PathNode::new(pd);
            cell_pn.fill = path_node.fill.clone();
            cell_pn.stroke = path_node.stroke.clone();

            let cell_name = format!("{} {},{}", source_name, r + 1, c + 1);
            let mut cell_node =
                SceneNode::new(&cell_name, target_layer, SceneNodeKind::Path(cell_pn));
            cell_node.opacity = source.opacity;
            cell_node.blend_mode = source.blend_mode;
            cell_node.tags = source.tags.clone();

            created_ids.push(cell_node.id);
            commands.push(Command::AddNode {
                node: cell_node,
                layer_id: Some(target_layer),
            });
        }
    }

    if !keep {
        commands.push(Command::RemoveNode {
            node_id: args.node_id,
        });
    }

    let batch = Command::Batch(commands);
    let mut history = state.history.lock().await;
    history.execute_discrete(batch, &mut doc);

    let count = created_ids.len();
    ToolResult::text(format!(
        "Split into {}×{} grid — created {} rectangle{} from \"{}\"{}",
        args.rows,
        args.cols,
        count,
        if count == 1 { "" } else { "s" },
        source_name,
        if keep {
            " (original kept)"
        } else {
            " (original removed)"
        },
    ))
    .with_data(serde_json::json!({
        "created": created_ids,
        "rows": args.rows,
        "cols": args.cols,
        "cell_width":  cell_w,
        "cell_height": cell_h,
    }))
}
/// Iteratively push nodes apart until none of their bounding boxes overlap.
pub async fn distribute_no_overlap(state: &AppState, args: DistributeNoOverlapArgs) -> ToolResult {
    use kurbo::Shape as _;
    tracing::debug!("tool: distribute_no_overlap");

    let padding = args.padding.unwrap_or(4.0_f64).max(0.0);
    let max_iter = args.max_iterations.unwrap_or(100).min(500);

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    // Resolve node IDs (from args or current selection).
    let ids: Vec<uuid::Uuid> = if args.node_ids.is_empty() {
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

    if ids.len() < 2 {
        return ToolResult::error("need at least 2 nodes to distribute");
    }

    // Cap at 100 nodes to keep O(n²) bounded.
    let ids: Vec<uuid::Uuid> = ids.into_iter().take(100).collect();
    let n = ids.len();

    // Snapshot current translation offsets (dx, dy) accumulated during simulation.
    let mut offsets: Vec<(f64, f64)> = vec![(0.0_f64, 0.0_f64); n];

    // Get node bounding boxes in local space (without transform — we apply transform separately).
    let mut local_bboxes: Vec<(f64, f64, f64, f64)> = ids
        .iter()
        .map(|id| -> (f64, f64, f64, f64) {
            if let Some(node) = doc.nodes.get(id) {
                if let SceneNodeKind::Path(pn) = &node.kind {
                    let bb = pn.path_data.to_bez_path().bounding_box();
                    return (bb.x0, bb.y0, bb.x1, bb.y1);
                }
            }
            (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64)
        })
        .collect();

    // Include node's existing translation in local_bboxes (world bbox = local_bbox + translate).
    let translates: Vec<(f64, f64)> = ids
        .iter()
        .map(|id| {
            if let Some(node) = doc.nodes.get(id) {
                (node.transform.matrix[4], node.transform.matrix[5])
            } else {
                (0.0, 0.0)
            }
        })
        .collect();

    // Make bboxes world-space.
    for i in 0..n {
        let (tx, ty) = translates[i];
        local_bboxes[i].0 += tx;
        local_bboxes[i].1 += ty;
        local_bboxes[i].2 += tx;
        local_bboxes[i].3 += ty;
    }

    let mut iterations_done = 0usize;

    for _ in 0..max_iter {
        let mut any_overlap = false;

        for i in 0..n {
            for j in (i + 1)..n {
                let (ax0, ay0, ax1, ay1) = (
                    local_bboxes[i].0 + offsets[i].0 - padding / 2.0,
                    local_bboxes[i].1 + offsets[i].1 - padding / 2.0,
                    local_bboxes[i].2 + offsets[i].0 + padding / 2.0,
                    local_bboxes[i].3 + offsets[i].1 + padding / 2.0,
                );
                let (bx0, by0, bx1, by1) = (
                    local_bboxes[j].0 + offsets[j].0 - padding / 2.0,
                    local_bboxes[j].1 + offsets[j].1 - padding / 2.0,
                    local_bboxes[j].2 + offsets[j].0 + padding / 2.0,
                    local_bboxes[j].3 + offsets[j].1 + padding / 2.0,
                );

                let overlap_x: f64 = (ax1.min(bx1) - ax0.max(bx0)).max(0.0);
                let overlap_y: f64 = (ay1.min(by1) - ay0.max(by0)).max(0.0);

                if overlap_x > 0.0 && overlap_y > 0.0 {
                    any_overlap = true;
                    // Push along the axis with smaller overlap.
                    let (push_x, push_y) = if overlap_x < overlap_y {
                        // Push horizontally.
                        let acx = (ax0 + ax1) / 2.0;
                        let bcx = (bx0 + bx1) / 2.0;
                        let dir = if acx <= bcx { -1.0 } else { 1.0 };
                        (dir * overlap_x / 2.0, 0.0)
                    } else {
                        // Push vertically.
                        let acy = (ay0 + ay1) / 2.0;
                        let bcy = (by0 + by1) / 2.0;
                        let dir = if acy <= bcy { -1.0 } else { 1.0 };
                        (0.0, dir * overlap_y / 2.0)
                    };
                    offsets[i].0 += push_x;
                    offsets[i].1 += push_y;
                    offsets[j].0 -= push_x;
                    offsets[j].1 -= push_y;
                }
            }
        }

        iterations_done += 1;
        if !any_overlap {
            break;
        }
    }

    // Apply offsets as UpdateNode commands.
    let mut commands = Vec::new();
    let mut moved = 0usize;
    for (i, id) in ids.iter().enumerate() {
        let (dx, dy): (f64, f64) = offsets[i];
        if dx.abs() > 0.01 || dy.abs() > 0.01 {
            if let Some(node) = doc.nodes.get(id).cloned() {
                let mut new_node = node.clone();
                new_node.transform.matrix[4] += dx;
                new_node.transform.matrix[5] += dy;
                commands.push(Command::UpdateNode {
                    old: node,
                    new: new_node,
                });
                moved += 1;
            }
        }
    }

    if commands.is_empty() {
        return ToolResult::text("No overlapping nodes found — nothing moved".to_string());
    }

    let batch = if commands.len() == 1 {
        commands.remove(0)
    } else {
        Command::Batch(commands)
    };
    history.execute_discrete(batch, &mut doc);

    ToolResult::text(format!(
        "Distributed {moved} nodes in {iterations_done} iterations"
    ))
    .with_data(serde_json::json!({
        "moved": moved,
        "iterations": iterations_done,
        "total_nodes": n,
    }))
}
pub async fn snap_to_pixel(state: &AppState, args: SnapToPixelArgs) -> ToolResult {
    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let mut doc = state.document.lock().await;
    let mut commands: Vec<Command> = Vec::new();
    let mut snapped = 0usize;

    for id in &args.node_ids {
        let node = match doc.nodes.get(id) {
            Some(n) => n.clone(),
            None => return ToolResult::error(format!("Node {} not found", id)),
        };
        let mut updated = node.clone();
        // Round translation components to nearest integer.
        updated.transform.matrix[4] = updated.transform.matrix[4].round();
        updated.transform.matrix[5] = updated.transform.matrix[5].round();
        if (node.transform.matrix[4] - updated.transform.matrix[4]).abs() > 1e-9
            || (node.transform.matrix[5] - updated.transform.matrix[5]).abs() > 1e-9
        {
            commands.push(Command::UpdateNode {
                old: node,
                new: updated,
            });
            snapped += 1;
        }
    }

    if commands.is_empty() {
        return ToolResult::text(format!(
            "{} node(s) already on integer coordinates — no changes made",
            args.node_ids.len()
        ));
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(Command::Batch(commands), &mut doc);

    ToolResult::text(format!(
        "Snapped {} of {} node(s) to pixel coordinates",
        snapped,
        args.node_ids.len()
    ))
    .with_data(serde_json::json!({ "snapped_count": snapped }))
}
pub async fn distribute_on_path(state: &AppState, args: DistributeOnPathArgs) -> ToolResult {
    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let doc = state.document.lock().await;

    // Resolve the guide path.
    let path_node = match doc.nodes.get(&args.path_node_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("path_node_id {} not found", args.path_node_id)),
    };
    let path_data = match &path_node.kind {
        SceneNodeKind::Path(p) => p.path_data.clone(),
        _ => return ToolResult::error("path_node_id must reference a path node"),
    };

    // Validate source nodes exist.
    for id in &args.node_ids {
        if !doc.nodes.contains_key(id) {
            return ToolResult::error(format!("node_id {} not found", id));
        }
    }

    let count = args.count.unwrap_or(args.node_ids.len()).max(1);
    let align = args.align_to_path.unwrap_or(false);
    let target_layer = args
        .layer_id
        .or(Some(path_node.layer_id))
        .or(doc.active_layer_id);

    let positions = path_data.sample_positions(count);
    if positions.is_empty() {
        return ToolResult::error("Path has no geometry to distribute along");
    }

    let mut commands: Vec<Command> = Vec::new();
    let mut new_ids: Vec<uuid::Uuid> = Vec::new();

    for (k, (px, py, angle_deg)) in positions.iter().enumerate() {
        // Cycle through source nodes.
        let src_id = args.node_ids[k % args.node_ids.len()];
        let src = doc.nodes[&src_id].clone();

        let mut new_node = src.clone();
        new_node.id = uuid::Uuid::new_v4();
        new_node.name = format!("{} {}", src.name, k + 1);

        // Position: offset to path sample point.
        new_node.transform.matrix[4] = px + src.transform.matrix[4];
        new_node.transform.matrix[5] = py + src.transform.matrix[5];

        // Align to path tangent if requested.
        if align {
            use std::f64::consts::PI;
            let rad = angle_deg * PI / 180.0;
            let (cos_r, sin_r) = (rad.cos(), rad.sin());
            // Build a pure rotation matrix and compose with existing transform.
            let m = &src.transform.matrix;
            // Apply rotation to [m0,m1,m2,m3] (linear part), keep new translation.
            new_node.transform.matrix[0] = m[0] * cos_r + m[2] * sin_r;
            new_node.transform.matrix[1] = m[1] * cos_r + m[3] * sin_r;
            new_node.transform.matrix[2] = -m[0] * sin_r + m[2] * cos_r;
            new_node.transform.matrix[3] = -m[1] * sin_r + m[3] * cos_r;
        }

        new_ids.push(new_node.id);
        commands.push(Command::AddNode {
            node: new_node,
            layer_id: target_layer,
        });
    }

    drop(doc);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(Command::Batch(commands), &mut doc);

    ToolResult::text(format!(
        "Distributed {} node(s) across {} position(s) along path '{}'",
        new_ids.len(),
        positions.len(),
        path_node.name
    ))
    .with_data(serde_json::json!({ "node_ids": new_ids, "count": positions.len() }))
}
/// Duplicate selected nodes and flip each copy across its own bounding-box
/// center, producing mirrored twins that can be repositioned independently.
pub async fn mirror_copy(state: &AppState, args: MirrorCopyArgs) -> ToolResult {
    tracing::debug!("tool: mirror_copy");
    use kurbo::Shape as _;

    let flip_h = args.axis.as_deref().unwrap_or("horizontal") != "vertical";
    // flip_h = true  → flip left/right (mirror across vertical axis)
    // flip_h = false → flip top/bottom (mirror across horizontal axis)

    // Collect source node IDs.
    let src_ids: Vec<NodeId> = {
        let doc = state.document.lock().await;
        if args.node_ids.is_empty() {
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
        }
    };

    if src_ids.is_empty() {
        return ToolResult::error("No nodes specified and nothing selected");
    }

    // Build clones using the existing subtree helper.
    let mut all_commands: Vec<Command> = Vec::new();
    let mut new_root_ids: Vec<uuid::Uuid> = Vec::new();

    for src_id in &src_ids {
        let (cloned_nodes, layer_id) = {
            let doc = state.document.lock().await;
            let layer = doc
                .nodes
                .get(src_id)
                .map(|n| n.layer_id)
                .unwrap_or_else(uuid::Uuid::nil);
            let nodes = clone_subtree(&doc, *src_id, layer, 0.0, 0.0);
            (nodes, layer)
        };

        if cloned_nodes.is_empty() {
            continue;
        }

        // Flip the root node's geometry.
        let mut modified = cloned_nodes;
        {
            let root = &mut modified[0];
            // Build a friendly name
            root.name = if root.name.is_empty() {
                "mirror".to_string()
            } else {
                format!("{} mirror", root.name)
            };

            match &mut root.kind {
                SceneNodeKind::Path(pn) => {
                    let bez = pn.path_data.to_bez_path();
                    let bbox = bez.bounding_box();
                    let cx = bbox.x0 + bbox.width() / 2.0;
                    let cy = bbox.y0 + bbox.height() / 2.0;

                    let flip_pt = |p: kurbo::Point| {
                        kurbo::Point::new(
                            if flip_h { 2.0 * cx - p.x } else { p.x },
                            if !flip_h { 2.0 * cy - p.y } else { p.y },
                        )
                    };

                    let mut new_bez = kurbo::BezPath::new();
                    for el in bez.elements() {
                        match *el {
                            kurbo::PathEl::MoveTo(p) => new_bez.move_to(flip_pt(p)),
                            kurbo::PathEl::LineTo(p) => new_bez.line_to(flip_pt(p)),
                            kurbo::PathEl::CurveTo(c1, c2, p) => {
                                new_bez.curve_to(flip_pt(c1), flip_pt(c2), flip_pt(p))
                            }
                            kurbo::PathEl::QuadTo(c, p) => new_bez.quad_to(flip_pt(c), flip_pt(p)),
                            kurbo::PathEl::ClosePath => new_bez.close_path(),
                        }
                    }
                    pn.path_data = PathData::from_bez_path(&new_bez);
                }
                // raster: no path geometry — mirror via transform like text/group
                SceneNodeKind::Text(_) | SceneNodeKind::Group(_) | SceneNodeKind::Raster(_) => {
                    if flip_h {
                        root.transform.matrix[0] *= -1.0;
                        root.transform.matrix[2] *= -1.0;
                    } else {
                        root.transform.matrix[1] *= -1.0;
                        root.transform.matrix[3] *= -1.0;
                    }
                }
            }

            new_root_ids.push(root.id);
        }

        for node in modified {
            all_commands.push(Command::AddNode {
                layer_id: Some(layer_id),
                node,
            });
        }
    }

    if all_commands.is_empty() {
        return ToolResult::error("No nodes found to mirror");
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let batch = if all_commands.len() == 1 {
        all_commands.remove(0)
    } else {
        Command::Batch(all_commands)
    };
    history.execute_discrete(batch, &mut doc);

    ToolResult::text(format!(
        "Created {} mirrored cop{} ({}). New node IDs: {}",
        new_root_ids.len(),
        if new_root_ids.len() == 1 { "y" } else { "ies" },
        if flip_h { "horizontally" } else { "vertically" },
        new_root_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
    .with_data(serde_json::json!({
        "node_ids": new_root_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
        "axis": if flip_h { "horizontal" } else { "vertical" },
    }))
}
/// Reverse the stacking order of children within each selected group node.
/// Useful to flip front-to-back ordering of blend results or any grouped artwork.
pub async fn reverse_node_order(state: &AppState, args: ReverseNodeOrderArgs) -> ToolResult {
    tracing::debug!("tool: reverse_node_order");

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

    let mut reversed = 0usize;
    let mut skipped = 0usize;
    let mut commands = Vec::new();

    for nid in &node_ids {
        let node = match doc.nodes.get(nid) {
            Some(n) => n.clone(),
            None => {
                skipped += 1;
                continue;
            }
        };
        match &node.kind {
            SceneNodeKind::Group(g) if g.children.len() > 1 => {
                let mut new_node = node.clone();
                if let SceneNodeKind::Group(ref mut ng) = new_node.kind {
                    ng.children.reverse();
                }
                commands.push(Command::UpdateNode {
                    old: node,
                    new: new_node,
                });
                reversed += 1;
            }
            SceneNodeKind::Group(_) => {
                skipped += 1;
            } // 0 or 1 children — no-op
            _ => {
                skipped += 1;
            }
        }
    }

    if commands.is_empty() {
        return ToolResult::error("No group nodes with 2+ children found in the specified IDs");
    }

    let batch = if commands.len() == 1 {
        commands.remove(0)
    } else {
        Command::Batch(commands)
    };
    history.execute_discrete(batch, &mut doc);

    ToolResult::text(format!(
        "Reversed child order in {} group node(s). Skipped: {}.",
        reversed, skipped
    ))
}
pub async fn apply_flex_layout(state: &AppState, args: ApplyFlexLayoutArgs) -> ToolResult {
    let mut doc = state.document.lock().await;

    // Resolve group node
    let uid = match uuid::Uuid::parse_str(&args.group_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.group_id).map(|n| n.id))
    {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.group_id)),
    };

    let group_node = match doc.nodes.get(&uid) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Node '{}' not found.", args.group_id)),
    };

    let child_ids = match &group_node.kind {
        SceneNodeKind::Group(g) => g.children.clone(),
        _ => return ToolResult::error("Target node is not a group."),
    };

    if child_ids.is_empty() {
        return ToolResult::text("Group has no children — nothing to layout.")
            .with_data(serde_json::json!({ "arranged": 0 }));
    }

    let direction = args.direction.as_deref().unwrap_or("row");
    let gap = args.gap.unwrap_or(8.0);
    let align = args.align.as_deref().unwrap_or("center");
    let padding = args.padding.unwrap_or(0.0);

    // Collect children with their bounding boxes
    struct ChildInfo {
        id: NodeId,
        tx: f64,
        ty: f64,
        w: f64,
        h: f64,
    }

    let mut children: Vec<ChildInfo> = Vec::new();
    for cid in &child_ids {
        let child = match doc.nodes.get(cid) {
            Some(n) => n.clone(),
            None => continue,
        };
        let (w, h) = match &child.kind {
            SceneNodeKind::Path(pn) => {
                if let Some(bb) = pn.path_data.bounding_box() {
                    (bb.width().abs().max(1.0), bb.height().abs().max(1.0))
                } else {
                    (60.0, 30.0)
                }
            }
            // raster: no path geometry — use default dimensions
            SceneNodeKind::Text(_) | SceneNodeKind::Group(_) | SceneNodeKind::Raster(_) => {
                (60.0, 30.0)
            }
        };
        let tx = child.transform.matrix[4];
        let ty = child.transform.matrix[5];
        children.push(ChildInfo {
            id: *cid,
            tx,
            ty,
            w,
            h,
        });
    }

    if children.is_empty() {
        return ToolResult::text("No accessible children found.")
            .with_data(serde_json::json!({ "arranged": 0 }));
    }

    // Sort by position along main axis
    match direction {
        "column" => {
            children.sort_by(|a, b| a.ty.partial_cmp(&b.ty).unwrap_or(std::cmp::Ordering::Equal))
        }
        _ => children.sort_by(|a, b| a.tx.partial_cmp(&b.tx).unwrap_or(std::cmp::Ordering::Equal)),
    }

    // Compute cross-axis extent for alignment
    let cross_max: f64 = match direction {
        "column" => children.iter().map(|c| c.w).fold(0.0_f64, f64::max),
        _ => children.iter().map(|c| c.h).fold(0.0_f64, f64::max),
    };

    let mut cursor = padding;
    let mut commands: Vec<Command> = Vec::new();

    for child in &children {
        let cross_size = match direction {
            "column" => child.w,
            _ => child.h,
        };

        let cross_offset = match align {
            "start" => padding,
            "end" => padding + cross_max - cross_size,
            _ => padding + (cross_max - cross_size) / 2.0, // center
        };

        let (new_tx, new_ty) = match direction {
            "column" => (cross_offset, cursor),
            _ => (cursor, cross_offset),
        };

        let main_size = match direction {
            "column" => child.h,
            _ => child.w,
        };
        cursor += main_size + gap;

        let old = doc.nodes.get(&child.id).unwrap().clone();
        let mut new_node = old.clone();
        new_node.transform.matrix[4] = new_tx;
        new_node.transform.matrix[5] = new_ty;
        commands.push(Command::UpdateNode { old, new: new_node });
    }

    let arranged = commands.len();
    let mut history = state.history.lock().await;
    history.execute_discrete(Command::Batch(commands), &mut doc);
    drop(history);

    ToolResult::text(format!(
        "Applied {} flex layout to {} children (gap={}, align={}, padding={}).",
        direction, arranged, gap, align, padding
    ))
    .with_data(serde_json::json!({
        "group_id": uid.to_string(),
        "direction": direction,
        "gap": gap,
        "align": align,
        "padding": padding,
        "arranged": arranged,
    }))
}
/// Stack all children of a group at the same anchor point (z-stack).
/// Every child is repositioned so that its specified alignment anchor
/// aligns with the union bounding box of all children.
pub async fn apply_stack_layout(state: &AppState, args: ApplyStackLayoutArgs) -> ToolResult {
    tracing::debug!("tool: apply_stack_layout group={}", args.group_id);
    let mut doc = state.document.lock().await;

    let uid = match uuid::Uuid::parse_str(&args.group_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.group_id).map(|n| n.id))
    {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.group_id)),
    };

    let group_node = match doc.nodes.get(&uid) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Node '{}' not found.", args.group_id)),
    };

    let child_ids = match &group_node.kind {
        SceneNodeKind::Group(g) => g.children.clone(),
        _ => return ToolResult::error("Target node is not a group."),
    };

    if child_ids.is_empty() {
        return ToolResult::text("Group has no children — nothing to stack.")
            .with_data(serde_json::json!({ "stacked": 0 }));
    }

    let align_h = args.align_h.as_deref().unwrap_or("center");
    let align_v = args.align_v.as_deref().unwrap_or("center");

    // Collect each child's current position and dimensions.
    let mut children: Vec<(NodeId, f64, f64, f64, f64)> = Vec::new(); // (id, tx, ty, w, h)
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;

    for cid in &child_ids {
        let child = match doc.nodes.get(cid) {
            Some(n) => n.clone(),
            None => continue,
        };
        let (w, h) = match &child.kind {
            SceneNodeKind::Path(pn) => {
                if let Some(bb) = pn.path_data.bounding_box() {
                    (bb.width().abs().max(1.0), bb.height().abs().max(1.0))
                } else {
                    (60.0, 30.0)
                }
            }
            // raster: no path geometry — use default dimensions
            SceneNodeKind::Text(_) | SceneNodeKind::Group(_) | SceneNodeKind::Raster(_) => {
                (60.0, 30.0)
            }
        };
        let tx = child.transform.matrix[4];
        let ty = child.transform.matrix[5];
        min_x = min_x.min(tx);
        min_y = min_y.min(ty);
        max_x = max_x.max(tx + w);
        max_y = max_y.max(ty + h);
        children.push((*cid, tx, ty, w, h));
    }

    if children.is_empty() {
        return ToolResult::text("No accessible children found.")
            .with_data(serde_json::json!({ "stacked": 0 }));
    }

    // Union bounding box of all children.
    let union_x = min_x;
    let union_y = min_y;
    let union_w = (max_x - min_x).max(1.0);
    let union_h = (max_y - min_y).max(1.0);

    let mut history = state.history.lock().await;
    let count = children.len();

    for (cid, _tx, _ty, w, h) in &children {
        let new_tx = match align_h {
            "left" => union_x,
            "right" => union_x + union_w - w,
            _ => union_x + (union_w - w) / 2.0, // center
        };
        let new_ty = match align_v {
            "top" => union_y,
            "bottom" => union_y + union_h - h,
            _ => union_y + (union_h - h) / 2.0, // center
        };

        let child = match doc.nodes.get(cid) {
            Some(n) => n.clone(),
            None => continue,
        };
        let mut new_child = child.clone();
        new_child.transform.matrix[4] = new_tx;
        new_child.transform.matrix[5] = new_ty;
        history.execute_discrete(
            Command::UpdateNode {
                old: child,
                new: new_child,
            },
            &mut doc,
        );
    }

    ToolResult::text(format!(
        "Stacked {} children in '{}' (align_h={}, align_v={}).",
        count, args.group_id, align_h, align_v
    ))
    .with_data(serde_json::json!({
        "group_id": uid.to_string(),
        "stacked": count,
        "align_h": align_h,
        "align_v": align_v,
    }))
}
pub async fn apply_grid_layout(state: &AppState, args: ApplyGridLayoutArgs) -> ToolResult {
    let mut doc = state.document.lock().await;

    let uid = match uuid::Uuid::parse_str(&args.group_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.group_id).map(|n| n.id))
    {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.group_id)),
    };

    let child_ids = match doc.nodes.get(&uid) {
        Some(n) => match &n.kind {
            SceneNodeKind::Group(g) => g.children.clone(),
            _ => return ToolResult::error("Target node is not a group."),
        },
        None => return ToolResult::error(format!("Node '{}' not found.", args.group_id)),
    };

    if child_ids.is_empty() {
        return ToolResult::text("Group has no children — nothing to layout.")
            .with_data(serde_json::json!({ "arranged": 0 }));
    }

    let cols = args.columns.unwrap_or(3).max(1);
    let gap_x = args.gap_x.unwrap_or(8.0);
    let gap_y = args.gap_y.unwrap_or(8.0);
    let padding = args.padding.unwrap_or(0.0);

    // Collect children with bounding sizes
    struct ChildInfo {
        id: NodeId,
        w: f64,
        h: f64,
    }
    let mut children: Vec<ChildInfo> = Vec::new();
    for cid in &child_ids {
        let child = match doc.nodes.get(cid) {
            Some(n) => n.clone(),
            None => continue,
        };
        let (w, h) = match &child.kind {
            SceneNodeKind::Path(pn) => {
                if let Some(bb) = pn.path_data.bounding_box() {
                    (bb.width().abs().max(1.0), bb.height().abs().max(1.0))
                } else {
                    (60.0, 30.0)
                }
            }
            _ => (60.0, 30.0),
        };
        children.push(ChildInfo { id: *cid, w, h });
    }

    // Compute column widths and row heights
    let n = children.len();
    let rows = (n + cols - 1) / cols;

    let col_width: f64 = children.iter().map(|c| c.w).fold(0.0_f64, f64::max);
    let row_height: f64 = children.iter().map(|c| c.h).fold(0.0_f64, f64::max);

    let mut commands: Vec<Command> = Vec::new();
    for (i, child) in children.iter().enumerate() {
        let col = i % cols;
        let row = i / cols;
        let new_tx = padding + col as f64 * (col_width + gap_x);
        let new_ty = padding + row as f64 * (row_height + gap_y);

        if let Some(old) = doc.nodes.get(&child.id) {
            let mut new_node = old.clone();
            new_node.transform.matrix[4] = new_tx;
            new_node.transform.matrix[5] = new_ty;
            commands.push(Command::UpdateNode {
                old: old.clone(),
                new: new_node,
            });
        }
    }

    let arranged = commands.len();
    let mut history = state.history.lock().await;
    history.execute_discrete(Command::Batch(commands), &mut doc);
    drop(history);

    ToolResult::text(format!(
        "Applied grid layout to {} children ({} cols × {} rows, gap={}×{}).",
        arranged, cols, rows, gap_x, gap_y
    ))
    .with_data(serde_json::json!({
        "group_id": uid.to_string(),
        "columns": cols,
        "rows": rows,
        "gap_x": gap_x,
        "gap_y": gap_y,
        "arranged": arranged,
    }))
}
/// Create N evenly-spaced rotational copies of a node around a center point.
pub async fn rotate_copies(state: &AppState, args: RotateCopiesArgs) -> ToolResult {
    tracing::debug!("tool: rotate_copies count={}", args.count);
    use photonic_core::transform::Transform;

    if args.count < 2 {
        return ToolResult::error("count must be at least 2.");
    }

    let mut doc = state.document.lock().await;

    let src_id = match uuid::Uuid::parse_str(&args.node_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.node_id).map(|n| n.id))
    {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    let src_node = match doc.nodes.get(&src_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    let layer_id = src_node.layer_id;

    // Determine rotation center
    let (cx, cy) = if let (Some(cx), Some(cy)) = (args.cx, args.cy) {
        (cx, cy)
    } else if let Some(lb) = src_node.local_bounds() {
        let (x0, y0) = src_node.transform.apply(lb.x0, lb.y0);
        let (x1, y1) = src_node.transform.apply(lb.x1, lb.y1);
        ((x0 + x1) / 2.0, (y0 + y1) / 2.0)
    } else {
        src_node.transform.apply(0.0, 0.0)
    };

    let angle_step = std::f64::consts::TAU / args.count as f64;
    let mut cmds: Vec<Command> = Vec::new();
    let mut copy_ids: Vec<NodeId> = vec![src_id];

    // Create count-1 copies
    for i in 1..args.count {
        let angle = angle_step * i as f64;
        let rot = Transform::rotate_around(angle, cx, cy);
        let mut copy = src_node.clone();
        copy.id = uuid::Uuid::new_v4();
        copy.name = format!("{} copy {}", src_node.name, i);
        // Compose: rot applied after existing transform
        copy.transform = src_node.transform.then(&rot);
        // Fix translation: rotate the world-space position
        let (orig_tx, orig_ty) = (src_node.transform.matrix[4], src_node.transform.matrix[5]);
        let (rot_tx, rot_ty) = rot.apply(orig_tx, orig_ty);
        copy.transform.matrix[4] = rot_tx;
        copy.transform.matrix[5] = rot_ty;
        copy_ids.push(copy.id);
        cmds.push(Command::AddNode {
            node: copy,
            layer_id: Some(layer_id),
        });
    }

    // Optionally wrap in a group
    if args.group {
        // First add all copies, then group them with the original
        let all_ids = copy_ids.clone();
        let mut history = state.history.lock().await;
        for cmd in cmds {
            history.execute_discrete(cmd, &mut doc);
        }
        // Group: create a group with all ids
        let group_node = photonic_core::node::SceneNode::new(
            format!("{} ×{}", src_node.name, args.count),
            layer_id,
            SceneNodeKind::Group(GroupNode {
                children: all_ids.clone(),
                ..Default::default()
            }),
        );
        let group_id = group_node.id;
        let cmd = Command::GroupNodes {
            group: group_node,
            children: all_ids,
            layer_id,
            insert_index: 0,
        };
        history.execute_discrete(cmd, &mut doc);
        ToolResult::text(format!(
            "Created {} rotational copies grouped as one node.",
            args.count - 1
        ))
        .with_data(serde_json::json!({ "group_id": group_id.to_string(), "count": args.count }))
    } else {
        let mut history = state.history.lock().await;
        let batch = Command::Batch(cmds);
        history.execute_discrete(batch, &mut doc);
        ToolResult::text(format!(
            "Created {} rotational copies of '{}'.",
            args.count - 1,
            src_node.name
        ))
        .with_data(serde_json::json!({
            "source_id": src_id.to_string(),
            "copy_ids": copy_ids.iter().skip(1).map(|id| id.to_string()).collect::<Vec<_>>(),
            "count": args.count,
            "center": [cx, cy],
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{AppState, McpServerConfig};
    use kurbo::Shape;
    use photonic_core::{
        color::Color,
        node::PathNode,
        style::{Fill, FillKind, Gradient, GradientStop, Stroke},
        AuditLog, Document,
    };
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::Mutex;

    fn test_state() -> AppState {
        let (tx, _rx) = std::sync::mpsc::channel();
        AppState {
            document: Arc::new(Mutex::new(Document::new("t", 200.0, 100.0))),
            history: Arc::new(Mutex::new(photonic_core::history::CommandHistory::new(100))),
            document_path: Arc::new(StdMutex::new(None)),
            capture_tx: Arc::new(StdMutex::new(tx)),
            config: McpServerConfig::default(),
            audit_log: Arc::new(StdMutex::new(AuditLog::new())),
            clipboard_ring: Arc::new(crate::handlers::clipboard::new_clipboard_ring()),
        }
    }

    fn gradient() -> Gradient {
        Gradient::linear(
            10.0,
            20.0,
            30.0,
            20.0,
            vec![
                GradientStop::new(0.0, Color::BLACK),
                GradientStop::new(1.0, Color::WHITE),
            ],
        )
    }

    const DANCER_CONTOUR: &str = "M714.6176303490295,407.12902770204585C719.0967149614303,407.12902770204585 718.2340759953883,407.60282833073194 725.4573601610513,400.88144525891346C726.144894070665,400.2416838047889 725.8049499944054,394.8349653218793 725.9927741786374,394.8349653218793C725.9927741786374,394.8349653218793 725.9927741786374,394.8349653218793 720.7035937499999,397.68125C720.7035937499999,397.68125 720.70359375,397.68124999999986 709.0400444299803,406.0971033737129C709.0400444299803,406.0971033737129 709.0400444299802,406.09710337371297 709.0400444299802,406.09710337371297Z";

    async fn seed_dancer_contour(state: &AppState) -> uuid::Uuid {
        let mut doc = state.document.lock().await;
        let layer_id = doc.active_layer_id.expect("default layer");
        let node = SceneNode::new(
            "dancer contour",
            layer_id,
            SceneNodeKind::Path(PathNode::new(
                PathData::from_svg(DANCER_CONTOUR).expect("real compact contour parses"),
            )),
        );
        let id = node.id;
        doc.add_node(node, Some(layer_id));
        id
    }

    async fn seed_gradient_path(state: &AppState) -> uuid::Uuid {
        let mut doc = state.document.lock().await;
        let layer_id = doc.active_layer_id.expect("default layer");
        let mut stroke = Stroke::solid(Color::BLACK, 1.0);
        stroke.paint = Some(FillKind::Gradient(gradient()));
        let node = SceneNode::new(
            "gradient path",
            layer_id,
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(10.0, 20.0, 20.0, 10.0))
                    .with_fill(Fill::gradient(gradient()))
                    .with_stroke(stroke),
            ),
        );
        let id = node.id;
        doc.nodes.insert(id, node);
        doc.layers
            .get_mut(&layer_id)
            .expect("default layer")
            .node_ids
            .push(id);
        id
    }

    fn paint_coords(node: &SceneNode) -> (Vec<f64>, Vec<f64>) {
        let SceneNodeKind::Path(path) = &node.kind else {
            panic!("expected path")
        };
        let FillKind::Gradient(fill) = &path.fill.kind else {
            panic!("expected gradient fill")
        };
        let Some(FillKind::Gradient(stroke)) = &path.stroke.paint else {
            panic!("expected gradient stroke")
        };
        (fill.coords.clone(), stroke.coords.clone())
    }

    #[tokio::test]
    async fn duplicate_nodes_offsets_fill_and_stroke_user_space_gradients() {
        let state = test_state();
        let source_id = seed_gradient_path(&state).await;

        let result = duplicate_nodes(
            &state,
            DuplicateNodesArgs {
                node_ids: vec![source_id],
                count: Some(1),
                offset: Some(TranslateArg { x: 40.0, y: 700.0 }),
                layer_id: None,
            },
        )
        .await;
        assert_ne!(result.is_error, Some(true));

        let doc = state.document.lock().await;
        let copy = doc
            .nodes
            .values()
            .find(|node| node.id != source_id)
            .expect("duplicated node");
        assert_eq!(copy.transform.matrix[4], 40.0);
        assert_eq!(copy.transform.matrix[5], 700.0);
        assert_eq!(
            paint_coords(copy),
            (
                vec![50.0, 720.0, 70.0, 720.0],
                vec![50.0, 720.0, 70.0, 720.0]
            )
        );
    }

    #[tokio::test]
    async fn apply_transform_translate_offsets_fill_and_stroke_user_space_gradients() {
        let state = test_state();
        let source_id = seed_gradient_path(&state).await;

        let result = apply_transform(
            &state,
            ApplyTransformArgs {
                node_ids: vec![source_id],
                operation: TransformOperation::Translate,
                translate: Some(TranslateArg { x: 40.0, y: 700.0 }),
                rotate: None,
                scale: None,
                matrix: None,
                shear: None,
            },
        )
        .await;
        assert_ne!(result.is_error, Some(true));

        let doc = state.document.lock().await;
        let node = &doc.nodes[&source_id];
        assert_eq!(node.transform.matrix[4], 40.0);
        assert_eq!(node.transform.matrix[5], 700.0);
        assert_eq!(
            paint_coords(node),
            (
                vec![50.0, 720.0, 70.0, 720.0],
                vec![50.0, 720.0, 70.0, 720.0]
            )
        );
    }

    #[tokio::test]
    async fn matrix_translation_preserves_dancer_geometry_and_translates_world_bounds() {
        let state = test_state();
        let id = seed_dancer_contour(&state).await;
        let (before_path, before_bounds, before_area, before_anchors) = {
            let doc = state.document.lock().await;
            let node = &doc.nodes[&id];
            let SceneNodeKind::Path(path) = &node.kind else {
                unreachable!()
            };
            let bez = path.path_data.to_bez_path();
            (
                path.path_data.clone(),
                node.local_bounds().unwrap(),
                bez.area().abs(),
                bez.elements()
                    .iter()
                    .filter(|element| !matches!(element, kurbo::PathEl::ClosePath))
                    .count(),
            )
        };
        let matrix = [1.0, 0.0, 0.0, 1.0, 56.73772898238582, 14.472548564763088];
        let result = apply_transform(
            &state,
            ApplyTransformArgs {
                node_ids: vec![id],
                operation: TransformOperation::Matrix,
                translate: None,
                rotate: None,
                scale: None,
                matrix: Some(matrix),
                shear: None,
            },
        )
        .await;
        assert_ne!(result.is_error, Some(true), "{:?}", result.content);

        let doc = state.document.lock().await;
        let node = &doc.nodes[&id];
        let SceneNodeKind::Path(path) = &node.kind else {
            unreachable!()
        };
        let after = path.path_data.to_bez_path();
        assert_eq!(path.path_data, before_path);
        assert_eq!(node.transform.matrix, matrix);
        assert_eq!(
            after
                .elements()
                .iter()
                .filter(|element| !matches!(element, kurbo::PathEl::ClosePath))
                .count(),
            before_anchors
        );
        assert!((after.area().abs() - before_area).abs() < 1e-9);
        let local = node.local_bounds().unwrap();
        assert_eq!(local, before_bounds);
        let world = node.transform.to_kurbo().transform_rect_bbox(local);
        assert!((world.x0 - (before_bounds.x0 + matrix[4])).abs() < 1e-9);
        assert!((world.y0 - (before_bounds.y0 + matrix[5])).abs() < 1e-9);
        assert!((world.x1 - (before_bounds.x1 + matrix[4])).abs() < 1e-9);
        assert!((world.y1 - (before_bounds.y1 + matrix[5])).abs() < 1e-9);
    }

    #[tokio::test]
    async fn transform_rejects_legacy_empty_path_without_mutating_it() {
        let state = test_state();
        let id = {
            let mut doc = state.document.lock().await;
            let layer_id = doc.active_layer_id.unwrap();
            let node = SceneNode::new(
                "empty legacy path",
                layer_id,
                SceneNodeKind::Path(PathNode::new(PathData::new())),
            );
            let id = node.id;
            doc.add_node(node, Some(layer_id));
            id
        };
        let result = apply_transform(
            &state,
            ApplyTransformArgs {
                node_ids: vec![id],
                operation: TransformOperation::Matrix,
                translate: None,
                rotate: None,
                scale: None,
                matrix: Some([1.0, 0.0, 0.0, 1.0, 10.0, 20.0]),
                shear: None,
            },
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(state.document.lock().await.nodes[&id]
            .transform
            .is_identity());
    }
}
