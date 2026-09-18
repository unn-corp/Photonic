use crate::handlers::shared::{ordering::*, paths::*};
use crate::protocol::*;
use crate::server::AppState;
use photonic_core::{
    history::Command,
    node::{PathNode, SceneNode, SceneNodeKind},
};

pub async fn boolean_operation(state: &AppState, args: BooleanOperationArgs) -> ToolResult {
    use photonic_core::ops::boolean::boolean_op;

    let mut doc = state.document.lock().await;

    // Clone both nodes
    let target_node = match doc.get_node(&args.target_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("target node {} not found", args.target_id)),
    };
    let tool_node = match doc.get_node(&args.tool_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("tool node {} not found", args.tool_id)),
    };

    // Both must be path nodes
    let (target_path_node, tool_path_node) = match (&target_node.kind, &tool_node.kind) {
        (SceneNodeKind::Path(tp), SceneNodeKind::Path(op)) => (tp.clone(), op.clone()),
        _ => return ToolResult::error("Both nodes must be path nodes"),
    };

    // Bake each node's transform into its path data
    let target_baked = apply_affine_to_path(
        &target_path_node.path_data,
        target_node.transform.to_kurbo(),
    );
    let tool_baked =
        apply_affine_to_path(&tool_path_node.path_data, tool_node.transform.to_kurbo());

    let result_path = match boolean_op(&target_baked, &tool_baked, args.operation) {
        Ok(p) => p,
        Err(e) => return ToolResult::error(format!("Boolean operation failed: {}", e)),
    };

    // Determine target's layer and z-position for result placement
    let (layer_id, target_index) = match doc.node_layer_and_index(&args.target_id) {
        Some(v) => v,
        None => return ToolResult::error("Could not determine target node position"),
    };
    let tool_index = doc
        .node_layer_and_index(&args.tool_id)
        .map(|(_, i)| i)
        .unwrap_or(0);

    // Build result node (inherits fill/stroke from target)
    use photonic_core::ops::boolean::BooleanOp;
    let op_name = match args.operation {
        BooleanOp::Union => "union",
        BooleanOp::Subtract => "subtract",
        BooleanOp::Intersect => "intersect",
        BooleanOp::Exclude => "exclude",
        BooleanOp::Divide => "divide",
    };
    let result_name = format!("{} {} {}", target_node.name, op_name, tool_node.name);
    let mut result_path_node = PathNode::new(result_path);
    result_path_node.fill = target_path_node.fill.clone();
    result_path_node.stroke = target_path_node.stroke.clone();

    let result_node = SceneNode::new(
        &result_name,
        layer_id,
        SceneNodeKind::Path(result_path_node),
    );
    let result_id = result_node.id;

    let original_len = doc
        .layers
        .get(&layer_id)
        .map(|l| l.node_ids.len())
        .unwrap_or(2);

    let cmd = if args.keep_originals {
        Command::AddNode {
            node: result_node,
            layer_id: Some(layer_id),
        }
    } else {
        // After removing tool and target, result appends at original_len - 2.
        // Then reorder result to target's original z-position.
        let tool_is_below = tool_index < target_index;
        let adjusted_target = if tool_is_below {
            target_index.saturating_sub(1)
        } else {
            target_index
        };
        let result_pos_after_add = original_len.saturating_sub(2);

        Command::Batch(vec![
            Command::RemoveNode {
                node_id: args.tool_id,
            },
            Command::RemoveNode {
                node_id: args.target_id,
            },
            Command::AddNode {
                node: result_node,
                layer_id: Some(layer_id),
            },
            Command::ReorderNode {
                layer_id,
                node_id: result_id,
                old_index: result_pos_after_add,
                new_index: adjusted_target,
            },
        ])
    };

    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!(
        "Boolean {} complete — result '{}' (id: {})",
        op_name, result_name, result_id
    ))
    .with_data(serde_json::json!({ "result_id": result_id }))
}
/// Clip all selected paths to the boundary of the frontmost selected node.
///
/// The frontmost node (highest z-order) acts as the crop mask: every other
/// selected path is replaced by `path ∩ frontmost_path`. The frontmost node
/// itself is removed. All transforms are baked into path coordinates before
/// the intersection so that results are correct regardless of node transform.
pub async fn pathfinder_crop(state: &AppState, args: PathfinderCropArgs) -> ToolResult {
    use photonic_core::ops::boolean::{boolean_op, BooleanOp};
    use photonic_core::ops::transform_ops::apply_affine_to_path;

    if args.node_ids.len() < 2 {
        return ToolResult::error("node_ids must contain at least 2 path node IDs");
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    // ── Verify all nodes exist and are paths ─────────────────────────────────
    for nid in &args.node_ids {
        let node = match doc.nodes.get(nid) {
            Some(n) => n,
            None => return ToolResult::error(format!("node {} not found", nid)),
        };
        if !matches!(node.kind, SceneNodeKind::Path(_)) {
            return ToolResult::error(format!("node {} is not a path node", nid));
        }
    }

    // ── Determine z-order and find frontmost ─────────────────────────────────
    let frontmost_id = {
        let mut best_id = args.node_ids[0];
        let mut best_key = node_z_key(&doc, &best_id);
        for nid in &args.node_ids[1..] {
            let key = node_z_key(&doc, nid);
            if key > best_key {
                best_key = key;
                best_id = *nid;
            }
        }
        best_id
    };

    // ── Bake frontmost path ───────────────────────────────────────────────────
    let front_node = doc.nodes[&frontmost_id].clone();
    let front_pn = match &front_node.kind {
        SceneNodeKind::Path(p) => p.clone(),
        _ => unreachable!(),
    };
    let front_path = apply_affine_to_path(&front_pn.path_data, front_node.transform.to_kurbo());

    // ── Build update commands for each back node ──────────────────────────────
    let mut commands: Vec<Command> = Vec::new();
    let mut cropped = 0usize;

    for nid in &args.node_ids {
        if *nid == frontmost_id {
            continue;
        }
        let node = doc.nodes[nid].clone();
        let pn = match &node.kind {
            SceneNodeKind::Path(p) => p.clone(),
            _ => unreachable!(),
        };
        let baked_path = apply_affine_to_path(&pn.path_data, node.transform.to_kurbo());

        let intersected = match boolean_op(&baked_path, &front_path, BooleanOp::Intersect) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult::error(format!("intersection failed for node {}: {}", nid, e))
            }
        };

        let mut new_node = node.clone();
        if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
            new_pn.path_data = intersected;
        }
        // Reset transform since path is now in world space.
        new_node.transform = photonic_core::transform::Transform::IDENTITY;
        commands.push(Command::UpdateNode {
            old: node,
            new: new_node,
        });
        cropped += 1;
    }

    // Remove the frontmost (crop mask) last so undo works cleanly.
    commands.push(Command::RemoveNode {
        node_id: frontmost_id,
    });

    history.execute_discrete(Command::Batch(commands), &mut doc);

    ToolResult::text(format!(
        "Cropped {} node(s) to the frontmost boundary.",
        cropped
    ))
    .with_data(serde_json::json!({
        "cropped":       cropped,
        "removed_id":    frontmost_id,
    }))
}
/// Divide two paths at every overlap edge into distinct colored face nodes.
/// Exactly two path node IDs must be provided. Up to three result nodes are
/// created; the originals are removed. Face colors are inherited from the
/// source shape that contained each face. Single undoable step.
pub async fn pathfinder_divide(state: &AppState, args: PathfinderDivideArgs) -> ToolResult {
    use photonic_core::ops::boolean::divide_paths;
    use photonic_core::ops::transform_ops::apply_affine_to_path;

    if args.node_ids.len() != 2 {
        return ToolResult::error("pathfinder_divide requires exactly 2 node IDs (back, front)");
    }

    let back_id = args.node_ids[0];
    let front_id = args.node_ids[1];

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let back_node = match doc.nodes.get(&back_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("node {} not found", back_id)),
    };
    let front_node = match doc.nodes.get(&front_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("node {} not found", front_id)),
    };

    let back_pn = match &back_node.kind {
        SceneNodeKind::Path(p) => p.clone(),
        _ => return ToolResult::error("back node is not a path"),
    };
    let front_pn = match &front_node.kind {
        SceneNodeKind::Path(p) => p.clone(),
        _ => return ToolResult::error("front node is not a path"),
    };

    // Bake transforms into path coordinates.
    let back_baked = apply_affine_to_path(&back_pn.path_data, back_node.transform.to_kurbo());
    let front_baked = apply_affine_to_path(&front_pn.path_data, front_node.transform.to_kurbo());

    let faces = match divide_paths(&back_baked, &front_baked) {
        Ok(faces) => faces,
        Err(e) => {
            return ToolResult::error(format!(
                "pathfinder_divide geometry stage failed for nodes {} and {}: {}",
                back_id, front_id, e
            ))
        }
    };
    if faces.is_empty() {
        return ToolResult::error("Divide produced no faces — shapes may not overlap");
    }

    let target_layer = args.layer_id.unwrap_or(back_node.layer_id);
    let source_pns = [&back_pn, &front_pn];
    let source_nodes = [&back_node, &front_node];

    let mut commands: Vec<Command> = Vec::new();
    commands.push(Command::RemoveNode { node_id: back_id });
    commands.push(Command::RemoveNode { node_id: front_id });

    let mut created_ids: Vec<uuid::Uuid> = Vec::new();
    for (i, (path_data, source_idx)) in faces.into_iter().enumerate() {
        let src_pn = source_pns[source_idx];
        let src_node = source_nodes[source_idx];
        let mut new_pn = src_pn.clone();
        new_pn.path_data = path_data;
        let mut new_node = SceneNode::new(
            format!("{} face {}", src_node.name, i + 1),
            target_layer,
            SceneNodeKind::Path(new_pn),
        );
        new_node.opacity = src_node.opacity;
        new_node.blend_mode = src_node.blend_mode;
        new_node.tags = src_node.tags.clone();
        let new_id = new_node.id;
        commands.push(Command::AddNode {
            node: new_node,
            layer_id: Some(target_layer),
        });
        created_ids.push(new_id);
    }

    history.execute_discrete(Command::Batch(commands), &mut doc);

    ToolResult::text(format!("Divided into {} face(s).", created_ids.len())).with_data(
        serde_json::json!({
            "face_count": created_ids.len(),
            "created_node_ids": created_ids,
        }),
    )
}
/// Trim all selected nodes of overlapping areas, then merge (union) any nodes
/// that share the same solid fill color into a single combined shape.
///
/// Process:
///  1. Sort nodes back-to-front by z-order.
///  2. Each node is trimmed: regions covered by nodes above it are subtracted.
///  3. Trimmed faces are grouped by solid fill color (RGBA, rounded to 2 dp).
///     Non-solid fills each form their own group.
///  4. Each group's paths are unioned into one shape.
///  5. The original nodes are replaced by the merged result nodes.
///  6. Strokes are disabled on all result nodes (Illustrator behaviour).
pub async fn pathfinder_merge(state: &AppState, args: PathfinderMergeArgs) -> ToolResult {
    use photonic_core::ops::boolean::{boolean_op, BooleanOp};
    use photonic_core::ops::transform_ops::apply_affine_to_path;
    use photonic_core::style::FillKind;

    if args.node_ids.len() < 2 {
        return ToolResult::error("node_ids must contain at least 2 path node IDs");
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    // Verify all nodes exist and are paths.
    for nid in &args.node_ids {
        let node = match doc.nodes.get(nid) {
            Some(n) => n,
            None => return ToolResult::error(format!("node {} not found", nid)),
        };
        if !matches!(node.kind, SceneNodeKind::Path(_)) {
            return ToolResult::error(format!("node {} is not a path node", nid));
        }
    }

    // Sort back-to-front by z-order.
    let mut sorted_ids = args.node_ids.clone();
    sorted_ids.sort_by_key(|nid| node_z_key(&doc, nid));

    let target_layer = args
        .layer_id
        .unwrap_or_else(|| doc.nodes[&sorted_ids[0]].layer_id);

    // Bake all paths.
    let baked: Vec<(uuid::Uuid, photonic_core::path::PathData)> = sorted_ids
        .iter()
        .map(|nid| {
            let node = &doc.nodes[nid];
            let pn = match &node.kind {
                SceneNodeKind::Path(p) => p,
                _ => unreachable!(),
            };
            (
                *nid,
                apply_affine_to_path(&pn.path_data, node.transform.to_kurbo()),
            )
        })
        .collect();

    // Trim each node: subtract all nodes above it.
    // trimmed_faces[i] = (nid, trimmed_path, fill_key_string, source_pn clone)
    let mut trimmed_faces: Vec<(uuid::Uuid, photonic_core::path::PathData, String)> = Vec::new();
    for i in 0..baked.len() {
        let (nid, ref path) = baked[i];
        let mut trimmed = path.clone();
        for j in (i + 1)..baked.len() {
            match boolean_op(&trimmed, &baked[j].1, BooleanOp::Subtract) {
                Ok(p) => trimmed = p,
                Err(e) => {
                    return ToolResult::error(format!("merge trim step failed at z {}: {}", j, e))
                }
            }
        }
        // Build a fill group key.
        let fill_key = match &doc.nodes[&nid].kind {
            SceneNodeKind::Path(pn) => match &pn.fill.kind {
                FillKind::Solid(c) => format!("solid:{:.2},{:.2},{:.2},{:.2}", c.r, c.g, c.b, c.a),
                _ => format!("other:{}", nid), // non-solid: unique group
            },
            _ => format!("other:{}", nid),
        };
        trimmed_faces.push((nid, trimmed, fill_key));
    }

    // Group by fill_key, preserving back-to-front order for first representative.
    let mut groups: Vec<(String, Vec<photonic_core::path::PathData>)> = Vec::new();
    let mut key_to_group_idx: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (_, trimmed_path, fill_key) in &trimmed_faces {
        if let Some(&idx) = key_to_group_idx.get(fill_key) {
            groups[idx].1.push(trimmed_path.clone());
        } else {
            let idx = groups.len();
            key_to_group_idx.insert(fill_key.clone(), idx);
            groups.push((fill_key.clone(), vec![trimmed_path.clone()]));
        }
    }

    // For each group, union all paths.
    // Representative node (first occurrence back-to-front) donates style.
    let mut commands: Vec<Command> = Vec::new();

    // Remove all originals first.
    for nid in &sorted_ids {
        commands.push(Command::RemoveNode { node_id: *nid });
    }

    let mut created_count = 0usize;
    for (fill_key, paths) in &groups {
        // Union all paths in the group.
        let mut merged = paths[0].clone();
        for path in &paths[1..] {
            match boolean_op(&merged, path, BooleanOp::Union) {
                Ok(p) => merged = p,
                Err(e) => return ToolResult::error(format!("merge union step failed: {}", e)),
            }
        }

        // Find the representative (first sorted_id with this fill_key).
        let rep_id = trimmed_faces
            .iter()
            .find(|(_, _, k)| k == fill_key)
            .map(|(nid, _, _)| *nid)
            .unwrap();
        let rep_node = doc.nodes[&rep_id].clone();
        let rep_pn = match &rep_node.kind {
            SceneNodeKind::Path(p) => p.clone(),
            _ => unreachable!(),
        };

        let mut new_pn = rep_pn.clone();
        new_pn.path_data = merged;
        new_pn.stroke.enabled = false;

        let group_name = if paths.len() > 1 {
            format!("{} merged", rep_node.name)
        } else {
            rep_node.name.clone()
        };
        let mut new_node = SceneNode::new(group_name, target_layer, SceneNodeKind::Path(new_pn));
        new_node.opacity = rep_node.opacity;
        new_node.blend_mode = rep_node.blend_mode;
        commands.push(Command::AddNode {
            node: new_node,
            layer_id: Some(target_layer),
        });
        created_count += 1;
    }

    history.execute_discrete(Command::Batch(commands), &mut doc);

    ToolResult::text(format!(
        "Merged {} node(s) into {} result shape(s); strokes disabled.",
        sorted_ids.len(),
        created_count
    ))
    .with_data(serde_json::json!({
        "source_count":  sorted_ids.len(),
        "result_count":  created_count,
    }))
}
/// Subtract all back nodes from the frontmost node's path.
///
/// The frontmost node (highest z-order) has the union of all other selected
/// nodes subtracted from its path in sequence. The back nodes are removed.
/// The frontmost node's fill/stroke style is preserved unchanged.
pub async fn pathfinder_minus_back(state: &AppState, args: PathfinderMinusBackArgs) -> ToolResult {
    use photonic_core::ops::boolean::{boolean_op, BooleanOp};
    use photonic_core::ops::transform_ops::apply_affine_to_path;

    if args.node_ids.len() < 2 {
        return ToolResult::error("node_ids must contain at least 2 path node IDs");
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    // ── Verify all nodes exist and are paths ─────────────────────────────────
    for nid in &args.node_ids {
        let node = match doc.nodes.get(nid) {
            Some(n) => n,
            None => return ToolResult::error(format!("node {} not found", nid)),
        };
        if !matches!(node.kind, SceneNodeKind::Path(_)) {
            return ToolResult::error(format!("node {} is not a path node", nid));
        }
    }

    // ── Determine frontmost (highest z-order) ────────────────────────────────
    let frontmost_id = {
        let mut best_id = args.node_ids[0];
        let mut best_key = node_z_key(&doc, &best_id);
        for nid in &args.node_ids[1..] {
            let key = node_z_key(&doc, nid);
            if key > best_key {
                best_key = key;
                best_id = *nid;
            }
        }
        best_id
    };

    // ── Bake frontmost path and subtract each back node ───────────────────────
    let front_node = doc.nodes[&frontmost_id].clone();
    let front_pn = match &front_node.kind {
        SceneNodeKind::Path(p) => p.clone(),
        _ => unreachable!(),
    };
    let mut result_path =
        apply_affine_to_path(&front_pn.path_data, front_node.transform.to_kurbo());

    for nid in &args.node_ids {
        if *nid == frontmost_id {
            continue;
        }
        let node = doc.nodes[nid].clone();
        let pn = match &node.kind {
            SceneNodeKind::Path(p) => p.clone(),
            _ => unreachable!(),
        };
        let baked = apply_affine_to_path(&pn.path_data, node.transform.to_kurbo());
        result_path = match boolean_op(&result_path, &baked, BooleanOp::Subtract) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult::error(format!("subtraction failed for node {}: {}", nid, e))
            }
        };
    }

    // ── Build commands: update front node, remove back nodes ─────────────────
    let mut commands: Vec<Command> = Vec::new();

    let mut new_front = front_node.clone();
    if let SceneNodeKind::Path(ref mut new_pn) = new_front.kind {
        new_pn.path_data = result_path;
    }
    new_front.transform = photonic_core::transform::Transform::IDENTITY;
    commands.push(Command::UpdateNode {
        old: front_node,
        new: new_front,
    });

    let back_count = args.node_ids.len() - 1;
    for nid in &args.node_ids {
        if *nid != frontmost_id {
            commands.push(Command::RemoveNode { node_id: *nid });
        }
    }

    history.execute_discrete(Command::Batch(commands), &mut doc);

    ToolResult::text(format!(
        "Subtracted {} back node(s) from frontmost; back nodes removed.",
        back_count
    ))
    .with_data(serde_json::json!({
        "result_node_id": frontmost_id,
        "removed_count":  back_count,
    }))
}
/// Subtract the frontmost node's path from every back node.
///
/// The frontmost node (highest z-order) punches a hole in each back node;
/// each back node is updated with `back_path - front_path`. The frontmost
/// node is then removed. Each back node's fill/stroke is preserved.
pub async fn pathfinder_minus_front(
    state: &AppState,
    args: PathfinderMinusFrontArgs,
) -> ToolResult {
    use photonic_core::ops::boolean::{boolean_op, BooleanOp};
    use photonic_core::ops::transform_ops::apply_affine_to_path;

    if args.node_ids.len() < 2 {
        return ToolResult::error("node_ids must contain at least 2 path node IDs");
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    // ── Verify all nodes exist and are paths ─────────────────────────────────
    for nid in &args.node_ids {
        let node = match doc.nodes.get(nid) {
            Some(n) => n,
            None => return ToolResult::error(format!("node {} not found", nid)),
        };
        if !matches!(node.kind, SceneNodeKind::Path(_)) {
            return ToolResult::error(format!("node {} is not a path node", nid));
        }
    }

    // ── Determine frontmost (highest z-order) ────────────────────────────────
    let frontmost_id = {
        let mut best_id = args.node_ids[0];
        let mut best_key = node_z_key(&doc, &best_id);
        for nid in &args.node_ids[1..] {
            let key = node_z_key(&doc, nid);
            if key > best_key {
                best_key = key;
                best_id = *nid;
            }
        }
        best_id
    };

    // ── Bake the frontmost path (the cutter) ─────────────────────────────────
    let front_node = doc.nodes[&frontmost_id].clone();
    let front_pn = match &front_node.kind {
        SceneNodeKind::Path(p) => p.clone(),
        _ => unreachable!(),
    };
    let front_path = apply_affine_to_path(&front_pn.path_data, front_node.transform.to_kurbo());

    // ── Subtract front from each back node ───────────────────────────────────
    let mut commands: Vec<Command> = Vec::new();
    let mut updated = 0usize;

    for nid in &args.node_ids {
        if *nid == frontmost_id {
            continue;
        }
        let node = doc.nodes[nid].clone();
        let pn = match &node.kind {
            SceneNodeKind::Path(p) => p.clone(),
            _ => unreachable!(),
        };
        let baked = apply_affine_to_path(&pn.path_data, node.transform.to_kurbo());
        let result = match boolean_op(&baked, &front_path, BooleanOp::Subtract) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult::error(format!("subtraction failed for node {}: {}", nid, e))
            }
        };
        let mut new_node = node.clone();
        if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
            new_pn.path_data = result;
        }
        new_node.transform = photonic_core::transform::Transform::IDENTITY;
        commands.push(Command::UpdateNode {
            old: node,
            new: new_node,
        });
        updated += 1;
    }

    // Remove the frontmost (cutter) last.
    commands.push(Command::RemoveNode {
        node_id: frontmost_id,
    });

    history.execute_discrete(Command::Batch(commands), &mut doc);

    ToolResult::text(format!(
        "Subtracted frontmost from {} back node(s); frontmost removed.",
        updated
    ))
    .with_data(serde_json::json!({
        "updated_count": updated,
        "removed_id":    frontmost_id,
    }))
}
/// Convert each selected path from filled to stroked outline.
///
/// For each node: the solid fill color is moved to the stroke; the fill is set
/// to none. If the fill is a gradient, the stroke defaults to black. Existing
/// stroke width is preserved (or defaults to 1.0 if no stroke was set). The
/// path data is unchanged. Single undoable step.
pub async fn pathfinder_outline(state: &AppState, args: PathfinderOutlineArgs) -> ToolResult {
    use photonic_core::style::{Fill, FillKind};

    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let mut commands: Vec<Command> = Vec::new();
    let mut updated = 0usize;

    for nid in &args.node_ids {
        let node = match doc.nodes.get(nid) {
            Some(n) => n.clone(),
            None => return ToolResult::error(format!("node {} not found", nid)),
        };
        let pn = match &node.kind {
            SceneNodeKind::Path(p) => p.clone(),
            _ => continue, // silently skip non-path nodes
        };

        // Determine stroke color from fill.
        let stroke_color = match &pn.fill.kind {
            FillKind::Solid(c) => *c,
            FillKind::Gradient(g) => g
                .stops
                .first()
                .map(|s| s.color)
                .unwrap_or(photonic_core::color::Color::BLACK),
            FillKind::FluidGradient(fg) => fg
                .points
                .first()
                .map(|p| p.color)
                .unwrap_or(photonic_core::color::Color::BLACK),
            FillKind::MeshGradient(_) => photonic_core::color::Color::BLACK,
            FillKind::Pattern(_) => photonic_core::color::Color::BLACK,
            FillKind::None => photonic_core::color::Color::BLACK,
        };

        let stroke_width = if pn.stroke.enabled {
            pn.stroke.width
        } else {
            1.0
        };

        let mut new_node = node.clone();
        if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
            new_pn.fill = Fill::none();
            new_pn.stroke.color = stroke_color;
            new_pn.stroke.width = stroke_width;
            new_pn.stroke.enabled = true;
        }
        commands.push(Command::UpdateNode {
            old: node,
            new: new_node,
        });
        updated += 1;
    }

    if commands.is_empty() {
        return ToolResult::text("No path nodes found in node_ids; nothing changed.".to_string());
    }

    history.execute_discrete(Command::Batch(commands), &mut doc);

    ToolResult::text(format!(
        "Outlined {} node(s); fills removed, strokes set.",
        updated
    ))
    .with_data(serde_json::json!({
        "outlined_count": updated,
    }))
}
/// Remove hidden portions of each node by subtracting all paths above it.
///
/// Nodes are processed back-to-front. Each node's path is replaced by
/// `its_path - union(all_paths_above)`. Strokes are disabled on every result
/// node; fills are preserved. No nodes are removed. Single undoable step.
pub async fn pathfinder_trim(state: &AppState, args: PathfinderTrimArgs) -> ToolResult {
    use photonic_core::ops::boolean::{boolean_op, BooleanOp};
    use photonic_core::ops::transform_ops::apply_affine_to_path;

    if args.node_ids.len() < 2 {
        return ToolResult::error("node_ids must contain at least 2 path node IDs");
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    // ── Verify all nodes exist and are paths ─────────────────────────────────
    for nid in &args.node_ids {
        let node = match doc.nodes.get(nid) {
            Some(n) => n,
            None => return ToolResult::error(format!("node {} not found", nid)),
        };
        if !matches!(node.kind, SceneNodeKind::Path(_)) {
            return ToolResult::error(format!("node {} is not a path node", nid));
        }
    }

    // ── Sort nodes back-to-front by z-order ──────────────────────────────────
    let mut sorted_ids = args.node_ids.clone();
    sorted_ids.sort_by_key(|nid| node_z_key(&doc, nid));
    // sorted_ids[0] = backmost, sorted_ids[last] = frontmost

    // ── Bake all paths up front ───────────────────────────────────────────────
    let baked_paths: Vec<(uuid::Uuid, photonic_core::path::PathData)> = sorted_ids
        .iter()
        .map(|nid| {
            let node = &doc.nodes[nid];
            let pn = match &node.kind {
                SceneNodeKind::Path(p) => p,
                _ => unreachable!(),
            };
            (
                *nid,
                apply_affine_to_path(&pn.path_data, node.transform.to_kurbo()),
            )
        })
        .collect();

    // ── For each node (back to front), subtract all nodes above it ────────────
    let mut commands: Vec<Command> = Vec::new();

    for i in 0..sorted_ids.len() {
        let nid = sorted_ids[i];
        let mut trimmed = baked_paths[i].1.clone();

        // Subtract every node above this one (higher index = higher z).
        for j in (i + 1)..sorted_ids.len() {
            trimmed = match boolean_op(&trimmed, &baked_paths[j].1, BooleanOp::Subtract) {
                Ok(p) => p,
                Err(e) => {
                    return ToolResult::error(format!(
                        "trim subtraction failed at step {}: {}",
                        j, e
                    ))
                }
            };
        }

        let node = doc.nodes[&nid].clone();
        let mut new_node = node.clone();
        if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
            new_pn.path_data = trimmed;
            new_pn.stroke.enabled = false; // Trim removes strokes (Illustrator behaviour)
        }
        new_node.transform = photonic_core::transform::Transform::IDENTITY;
        commands.push(Command::UpdateNode {
            old: node,
            new: new_node,
        });
    }

    history.execute_discrete(Command::Batch(commands), &mut doc);

    ToolResult::text(format!(
        "Trimmed {} node(s); hidden areas removed, strokes disabled.",
        sorted_ids.len()
    ))
    .with_data(serde_json::json!({
        "trimmed_count": sorted_ids.len(),
    }))
}
/// Use a path node as a cutting edge to divide all nodes below it in z-order.
/// Each overlapping node beneath the cutter is split into two face nodes:
/// the region inside the cutter and the region outside. The cutter is removed.
/// Non-overlapping nodes below are unchanged. Single undoable step.
pub async fn divide_objects_below(state: &AppState, args: DivideObjectsBelowArgs) -> ToolResult {
    use photonic_core::ops::boolean::{boolean_op, divide_paths, BooleanOp};
    use photonic_core::ops::transform_ops::apply_affine_to_path;

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let cutter_node = match doc.nodes.get(&args.node_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("node {} not found", args.node_id)),
    };
    let cutter_pn = match &cutter_node.kind {
        SceneNodeKind::Path(p) => p.clone(),
        _ => return ToolResult::error("cutter node must be a path"),
    };
    let cutter_baked = apply_affine_to_path(&cutter_pn.path_data, cutter_node.transform.to_kurbo());

    // Find all path nodes below the cutter in the same layer.
    let (cutter_layer_id, cutter_z) = match doc.node_layer_and_index(&args.node_id) {
        Some(x) => x,
        None => return ToolResult::error("could not determine cutter z-order"),
    };
    let layer = match doc.layers.get(&cutter_layer_id) {
        Some(l) => l.clone(),
        None => return ToolResult::error("cutter layer not found"),
    };

    let below_ids: Vec<uuid::Uuid> = layer.node_ids[..cutter_z].to_vec();

    let mut commands: Vec<Command> = Vec::new();
    let mut split_count = 0usize;

    for target_id in &below_ids {
        let target_node = match doc.nodes.get(target_id) {
            Some(n) => n.clone(),
            None => continue,
        };
        let target_pn = match &target_node.kind {
            SceneNodeKind::Path(p) => p.clone(),
            _ => continue, // skip non-path nodes
        };
        let target_baked =
            apply_affine_to_path(&target_pn.path_data, target_node.transform.to_kurbo());

        // Skip if no overlap.
        let overlap = match boolean_op(&target_baked, &cutter_baked, BooleanOp::Intersect) {
            Ok(overlap) => overlap,
            Err(e) => {
                return ToolResult::error(format!(
                    "divide_objects_below overlap-check stage failed for node {}: {}",
                    target_id, e
                ))
            }
        };
        if overlap.is_empty() {
            continue;
        }

        let faces = match divide_paths(&target_baked, &cutter_baked) {
            Ok(faces) => faces,
            Err(e) => {
                return ToolResult::error(format!(
                    "divide_objects_below divide stage failed for node {}: {}",
                    target_id, e
                ))
            }
        };
        commands.push(Command::RemoveNode {
            node_id: *target_id,
        });
        for (i, (path_data, _source_idx)) in faces.into_iter().enumerate() {
            let mut new_pn = target_pn.clone();
            new_pn.path_data = path_data;
            let mut new_node = SceneNode::new(
                format!("{} face {}", target_node.name, i + 1),
                cutter_layer_id,
                SceneNodeKind::Path(new_pn),
            );
            new_node.opacity = target_node.opacity;
            new_node.blend_mode = target_node.blend_mode;
            new_node.tags = target_node.tags.clone();
            commands.push(Command::AddNode {
                node: new_node,
                layer_id: Some(cutter_layer_id),
            });
        }
        split_count += 1;
    }

    // Remove the cutter.
    commands.push(Command::RemoveNode {
        node_id: args.node_id,
    });

    if commands.len() == 1 {
        // Only the cutter removal — nothing actually overlapped.
        history.execute_discrete(Command::Batch(commands), &mut doc);
        return ToolResult::text(
            "No overlapping objects found below the cutter; cutter removed.".to_string(),
        );
    }

    history.execute_discrete(Command::Batch(commands), &mut doc);
    ToolResult::text(format!(
        "Divided {} object(s) below; cutter removed.",
        split_count
    ))
    .with_data(serde_json::json!({ "split_count": split_count }))
}
