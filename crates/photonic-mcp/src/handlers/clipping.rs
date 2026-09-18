use crate::protocol::*;
use crate::server::AppState;
use kurbo;
use photonic_core::{
    history::Command,
    node::{NodeId, PathNode, SceneNode, SceneNodeKind},
    path::PathData,
};

/// Make a clipping mask from a group node.
/// The topmost child (last in `children`) becomes the clip path for all other children.
pub async fn make_clipping_mask(state: &AppState, args: MakeClippingMaskArgs) -> ToolResult {
    tracing::debug!("tool: make_clipping_mask");
    let mut doc = state.document.lock().await;

    // Resolve node ID
    let group_id = {
        let id = args.group_id.trim();
        if let Ok(uuid) = uuid::Uuid::parse_str(id) {
            uuid
        } else {
            match doc.nodes.values().find(|n| n.name == id) {
                Some(n) => n.id,
                None => return ToolResult::error(format!("Node '{}' not found.", args.group_id)),
            }
        }
    };

    let node = match doc.nodes.get(&group_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Node '{}' not found.", args.group_id)),
    };

    let group = match &node.kind {
        SceneNodeKind::Group(g) => g.clone(),
        _ => return ToolResult::error(format!("Node '{}' is not a group.", args.group_id)),
    };

    if group.children.len() < 2 {
        return ToolResult::error(
            "Group must have at least 2 children: one clip path and one or more masked objects.",
        );
    }

    // Topmost child (last in children list) is the clip path
    let clip_id = *group.children.last().unwrap();

    let mut new_node = node.clone();
    if let SceneNodeKind::Group(ref mut g) = new_node.kind {
        g.clip_node_id = Some(clip_id);
    }

    let clip_name = doc
        .nodes
        .get(&clip_id)
        .map(|n| n.name.clone())
        .unwrap_or_else(|| clip_id.to_string());
    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::UpdateNode {
            old: node,
            new: new_node,
        },
        &mut doc,
    );
    drop(history);

    ToolResult::text(format!(
        "Clipping mask created on group '{}' using '{}' as clip path.",
        args.group_id, clip_name
    ))
    .with_data(serde_json::json!({ "group_id": group_id, "clip_node_id": clip_id }))
}
/// Release the clipping mask from a group node, restoring all children as normal objects.
pub async fn release_clipping_mask(state: &AppState, args: ReleaseClippingMaskArgs) -> ToolResult {
    tracing::debug!("tool: release_clipping_mask");
    let mut doc = state.document.lock().await;

    let group_id = {
        let id = args.group_id.trim();
        if let Ok(uuid) = uuid::Uuid::parse_str(id) {
            uuid
        } else {
            match doc.nodes.values().find(|n| n.name == id) {
                Some(n) => n.id,
                None => return ToolResult::error(format!("Node '{}' not found.", args.group_id)),
            }
        }
    };

    let node = match doc.nodes.get(&group_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Node '{}' not found.", args.group_id)),
    };

    let had_mask = match &node.kind {
        SceneNodeKind::Group(g) => g.clip_node_id.is_some(),
        _ => return ToolResult::error(format!("Node '{}' is not a group.", args.group_id)),
    };

    if !had_mask {
        return ToolResult::error(format!(
            "Group '{}' does not have a clipping mask.",
            args.group_id
        ));
    }

    let mut new_node = node.clone();
    if let SceneNodeKind::Group(ref mut g) = new_node.kind {
        g.clip_node_id = None;
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::UpdateNode {
            old: node,
            new: new_node,
        },
        &mut doc,
    );
    drop(history);

    ToolResult::text(format!(
        "Released clipping mask from group '{}'.",
        args.group_id
    ))
    .with_data(serde_json::json!({ "group_id": group_id }))
}
/// Combine multiple path nodes into a single compound path.
/// Overlapping subpaths create holes via the even-odd fill rule.
/// The bottommost node (first in document order) keeps its position and donates
/// its fill, stroke, and transform; all other source nodes are removed.
pub async fn make_compound_path(state: &AppState, args: MakeCompoundPathArgs) -> ToolResult {
    if args.node_ids.len() < 2 {
        return ToolResult::error("make_compound_path requires at least 2 node_ids");
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    // Validate: all must be top-level path nodes.
    for &id in &args.node_ids {
        match doc.nodes.get(&id) {
            Some(n) => {
                if !matches!(n.kind, SceneNodeKind::Path(_)) {
                    return ToolResult::error(format!("Node {} is not a path node", id));
                }
            }
            None => return ToolResult::error(format!("Node {} not found", id)),
        }
    }

    // Determine document order among selected nodes (bottommost first).
    let mut ordered_ids: Vec<NodeId> = Vec::new();
    for node in doc.nodes_in_draw_order() {
        if args.node_ids.contains(&node.id) {
            ordered_ids.push(node.id);
        }
    }
    if ordered_ids.len() != args.node_ids.len() {
        return ToolResult::error(
            "One or more nodes not found in draw order (may be inside a group)",
        );
    }

    // The bottommost node is the base: its ID becomes the compound path ID.
    let base_id = ordered_ids[0];
    let base_node = doc.nodes[&base_id].clone();
    // Concatenate all BezPaths in the base node's local coordinate system. Use
    // kurbo's canonical affine composition instead of hand-rolling point and
    // inverse transforms; this preserves every command/control point and avoids
    // subtly mixing SVG and affine matrix conventions.
    let [ba, bb, bc, bd, _, _] = base_node.transform.matrix;
    let base_det = ba * bd - bb * bc;
    if !base_det.is_finite() || base_det.abs() < 1e-12 {
        return ToolResult::error("Cannot make a compound path with a singular base transform");
    }
    let world_to_base = base_node.transform.to_kurbo().inverse();
    let mut merged = kurbo::BezPath::new();
    for &id in &ordered_ids {
        let node = &doc.nodes[&id];
        let pn = match &node.kind {
            SceneNodeKind::Path(p) => p,
            _ => unreachable!(),
        };
        let mut bez = match pn.path_data.try_to_bez_path() {
            Ok(bez) if !bez.elements().is_empty() => bez,
            Ok(_) => return ToolResult::error(format!("Path node {} has no geometry", id)),
            Err(error) => {
                return ToolResult::error(format!(
                    "Path node {} has invalid geometry: {}",
                    id, error
                ))
            }
        };
        bez.apply_affine(world_to_base * node.transform.to_kurbo());
        for element in bez.elements() {
            merged.push(*element);
        }
    }

    // Round-trip now, before deleting any source nodes. PathData is the stored
    // representation used by bounds, inspection, rendering, and SVG export.
    let merged_path = PathData::from_bez_path(&merged);
    if merged_path
        .try_to_bez_path()
        .map_or(true, |path| path.elements().is_empty())
    {
        return ToolResult::error("Compound path assembly produced invalid or empty geometry");
    }

    let compound_name = args
        .name
        .unwrap_or_else(|| format!("{} (compound)", base_node.name));

    // Build the updated base node: merged path + is_compound flag + new name.
    let mut updated_node = base_node.clone();
    updated_node.name = compound_name.clone();
    if let SceneNodeKind::Path(ref mut p) = updated_node.kind {
        p.path_data = merged_path;
        p.is_compound = true;
    }

    // Batch: UpdateNode for base, RemoveNode for all other sources.
    let mut cmds = vec![Command::UpdateNode {
        old: base_node,
        new: updated_node,
    }];
    for &id in &ordered_ids[1..] {
        cmds.push(Command::RemoveNode { node_id: id });
    }

    history.execute_discrete(Command::Batch(cmds), &mut doc);
    history.schedule_mcp_checkpoint(format!("Make compound path '{}'", compound_name));

    ToolResult::text(format!(
        "Combined {} paths into compound path '{}' (id: {}).",
        ordered_ids.len(),
        compound_name,
        base_id
    ))
    .with_data(serde_json::json!({
        "node_id": base_id,
        "source_count": ordered_ids.len(),
    }))
}
/// Split a compound path back into individual path nodes, one per subpath.
pub async fn release_compound_path(state: &AppState, args: ReleaseCompoundPathArgs) -> ToolResult {
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let node = match doc.nodes.get(&args.node_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Node {} not found", args.node_id)),
    };

    let pn = match &node.kind {
        SceneNodeKind::Path(p) => p.clone(),
        _ => return ToolResult::error("Node is not a path node"),
    };

    // Split BezPath into individual subpaths (each beginning with MoveTo).
    let bez = pn.path_data.to_bez_path();
    let mut subpaths: Vec<kurbo::BezPath> = Vec::new();
    let mut current = kurbo::BezPath::new();

    for el in bez.elements() {
        if matches!(el, kurbo::PathEl::MoveTo(_)) && !current.elements().is_empty() {
            subpaths.push(current);
            current = kurbo::BezPath::new();
        }
        current.push(*el);
    }
    if !current.elements().is_empty() {
        subpaths.push(current);
    }

    if subpaths.len() < 2 {
        // Nothing to release — just clear the compound flag.
        let mut updated = node.clone();
        if let SceneNodeKind::Path(ref mut p) = updated.kind {
            p.is_compound = false;
        }
        history.execute_discrete(
            Command::UpdateNode {
                old: node,
                new: updated,
            },
            &mut doc,
        );
        return ToolResult::text(
            "Compound path had only one subpath; compound flag cleared.".to_string(),
        );
    }

    let layer_id = match doc.node_layer_and_index(&args.node_id) {
        Some((lid, _)) => lid,
        None => return ToolResult::error("Node has no layer position"),
    };

    let base_name = node.name.trim_end_matches(" (compound)").to_string();
    let mut new_ids: Vec<NodeId> = vec![args.node_id]; // first subpath reuses base node ID
    let mut cmds: Vec<Command> = Vec::new();

    // Update the compound node in-place to become subpath 0 (keeps layer position).
    let mut updated_base = node.clone();
    updated_base.name = format!("{} 1", base_name);
    if let SceneNodeKind::Path(ref mut p) = updated_base.kind {
        p.path_data = PathData::from_bez_path(&subpaths[0]);
        p.is_compound = false;
    }
    cmds.push(Command::UpdateNode {
        old: node.clone(),
        new: updated_base,
    });

    // Add one new node per remaining subpath.
    for (i, subpath_bez) in subpaths[1..].iter().enumerate() {
        let mut sub_pn = PathNode::new(PathData::from_bez_path(subpath_bez));
        sub_pn.fill = pn.fill.clone();
        sub_pn.stroke = pn.stroke.clone();
        sub_pn.is_compound = false;

        let sub_id = uuid::Uuid::new_v4();
        let sub_node = SceneNode::new(
            format!("{} {}", base_name, i + 2),
            layer_id,
            SceneNodeKind::Path(sub_pn),
        )
        .with_transform(node.transform);
        // Copy opacity/blend_mode manually since SceneNode::new doesn't expose them as builders.
        let mut sub_node = sub_node;
        sub_node.id = sub_id;
        sub_node.opacity = node.opacity;
        sub_node.visible = node.visible;
        sub_node.locked = node.locked;
        sub_node.blend_mode = node.blend_mode;

        new_ids.push(sub_id);
        cmds.push(Command::AddNode {
            node: sub_node,
            layer_id: Some(layer_id),
        });
    }

    history.execute_discrete(Command::Batch(cmds), &mut doc);
    history.schedule_mcp_checkpoint(format!("Release compound path '{}'", node.name));

    ToolResult::text(format!(
        "Released '{}' into {} individual path(s).",
        node.name,
        new_ids.len()
    ))
    .with_data(serde_json::json!({
        "node_ids": new_ids,
        "subpath_count": new_ids.len(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::clipboard::new_clipboard_ring;
    use crate::server::McpServerConfig;
    use kurbo::Shape;
    use photonic_core::{
        export::{export_svg, SvgExportOptions},
        transform::Transform,
        AuditLog, Document,
    };
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::Mutex;

    fn test_state(document: Document) -> AppState {
        let (tx, _rx) = std::sync::mpsc::channel();
        AppState {
            document: Arc::new(Mutex::new(document)),
            history: Arc::new(Mutex::new(photonic_core::CommandHistory::new(100))),
            document_path: Arc::new(StdMutex::new(None)),
            capture_tx: Arc::new(StdMutex::new(tx)),
            config: McpServerConfig::default(),
            path_policy: photonic_core::PathPolicy::test_default(),
            audit_log: Arc::new(StdMutex::new(AuditLog::new())),
            clipboard_ring: Arc::new(new_clipboard_ring()),
            video_engine: Arc::new(crate::handlers::video_jobs::VideoEngineHandle::new()),
            video_jobs: Arc::new(StdMutex::new(
                crate::handlers::video_jobs::JobRegistry::new(),
            )),
        }
    }

    fn transformed_rect(
        name: &str,
        layer_id: photonic_core::layer::LayerId,
        width: f64,
        height: f64,
        transform: Transform,
    ) -> SceneNode {
        let mut node = SceneNode::new(
            name,
            layer_id,
            SceneNodeKind::Path(PathNode::new(PathData::rect(0.0, 0.0, width, height))),
        );
        node.transform = transform;
        node
    }

    fn mesh_covers(mesh: &photonic_render::tessellator::Mesh, point: [f32; 2]) -> bool {
        fn side(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
            (p[0] - b[0]) * (a[1] - b[1]) - (a[0] - b[0]) * (p[1] - b[1])
        }
        mesh.indices.chunks_exact(3).any(|triangle| {
            let a = mesh.vertices[triangle[0] as usize];
            let b = mesh.vertices[triangle[1] as usize];
            let c = mesh.vertices[triangle[2] as usize];
            let s1 = side(point, a, b);
            let s2 = side(point, b, c);
            let s3 = side(point, c, a);
            (s1 >= 0.0 && s2 >= 0.0 && s3 >= 0.0) || (s1 <= 0.0 && s2 <= 0.0 && s3 <= 0.0)
        })
    }

    fn dancer_subpaths() -> Vec<kurbo::BezPath> {
        let source = PathData::from_svg(include_str!("fixtures/mbd_dancer.path").trim())
            .expect("checked-in dancer path fixture must parse")
            .to_bez_path();
        let mut subpaths = Vec::new();
        let mut current = kurbo::BezPath::new();
        for element in source.elements() {
            if matches!(element, kurbo::PathEl::MoveTo(_)) && !current.elements().is_empty() {
                subpaths.push(current);
                current = kurbo::BezPath::new();
            }
            current.push(*element);
        }
        if !current.elements().is_empty() {
            subpaths.push(current);
        }
        subpaths
    }

    #[tokio::test]
    async fn create_path_rejects_payloads_that_parse_to_empty_geometry() {
        let state = test_state(Document::new("invalid", 100.0, 100.0));
        let args: CreatePathArgs = serde_json::from_value(serde_json::json!({
            "path_data": "_",
            "name": "must not exist",
            "fill": { "type": "solid", "color": "#000000" },
            "stroke": { "enabled": false }
        }))
        .unwrap();
        let result = crate::handlers::nodes::create_path(&state, args).await;
        assert_eq!(result.is_error, Some(true), "{:?}", result.content);
        assert!(state.document.lock().await.nodes.is_empty());
    }

    #[tokio::test]
    async fn dancer_create_transform_compound_round_trip_preserves_geometry_and_holes() {
        let source_svg = include_str!("fixtures/mbd_dancer.path").trim();
        let source_path = PathData::from_svg(source_svg).expect("real exported path parses");
        let source_bez = source_path.to_bez_path();
        let source_anchor_count = source_bez
            .elements()
            .iter()
            .filter(|element| !matches!(element, kurbo::PathEl::ClosePath))
            .count();
        assert_eq!(source_anchor_count, 162);
        let source_bounds = source_bez.bounding_box();
        let source_area = source_bez.area().abs();
        let source_mesh = photonic_render::tessellator::tessellate_fill(&source_path, false, 0.1);
        assert!(!source_mesh.is_empty());

        let subpaths = dancer_subpaths();
        assert_eq!(subpaths.len(), 3);
        let state = test_state(Document::new("dancer round trip", 1296.0, 1296.0));
        let mut ids = Vec::new();
        for (index, subpath) in subpaths.iter().enumerate() {
            let path_data = PathData::from_bez_path(subpath);
            let args: CreatePathArgs = serde_json::from_value(serde_json::json!({
                "path_data": path_data.as_svg(),
                "name": format!("dancer contour {index}"),
                "fill": { "type": "solid", "color": "#000000" },
                "stroke": { "enabled": false }
            }))
            .unwrap();
            let result = crate::handlers::nodes::create_path(&state, args).await;
            assert_ne!(result.is_error, Some(true), "{:?}", result.content);
            let document = state.document.lock().await;
            let node = document
                .nodes
                .values()
                .find(|node| node.name == format!("dancer contour {index}"))
                .expect("created contour");
            let SceneNodeKind::Path(path) = &node.kind else {
                panic!("created contour must be a path")
            };
            assert!(path.path_data.has_drawable_geometry());
            ids.push(node.id);
        }

        let matrix = [1.0, 0.0, 0.0, 1.0, 56.73772898238582, 14.472548564763088];
        let before_paths: Vec<PathData> = {
            let document = state.document.lock().await;
            ids.iter()
                .map(|id| match &document.nodes[id].kind {
                    SceneNodeKind::Path(path) => path.path_data.clone(),
                    _ => unreachable!(),
                })
                .collect()
        };
        let result = crate::handlers::nodes::apply_transform(
            &state,
            ApplyTransformArgs {
                node_ids: ids.clone(),
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
        {
            let document = state.document.lock().await;
            for (id, before) in ids.iter().zip(&before_paths) {
                let node = &document.nodes[id];
                let SceneNodeKind::Path(path) = &node.kind else {
                    unreachable!()
                };
                assert_eq!(
                    &path.path_data, before,
                    "matrix transform must not bake or erase path data"
                );
                assert_eq!(node.transform.matrix, matrix);
                assert!(node.local_bounds().is_some());
            }
        }

        let result = make_compound_path(
            &state,
            MakeCompoundPathArgs {
                node_ids: ids.clone(),
                name: Some("Dancer rebuilt compound".into()),
            },
        )
        .await;
        assert_ne!(result.is_error, Some(true), "{:?}", result.content);

        let document = state.document.lock().await;
        assert_eq!(document.nodes.len(), 1);
        let compound = &document.nodes[&ids[0]];
        let SceneNodeKind::Path(compound_path) = &compound.kind else {
            panic!("compound result must be a path")
        };
        assert!(compound_path.is_compound);
        assert_eq!(compound.transform.matrix, matrix);
        let compound_bez = compound_path.path_data.to_bez_path();
        let compound_anchor_count = compound_bez
            .elements()
            .iter()
            .filter(|element| !matches!(element, kurbo::PathEl::ClosePath))
            .count();
        assert_eq!(compound_anchor_count, source_anchor_count);
        assert!((compound_bez.area().abs() - source_area).abs() < 1e-6);
        let compound_bounds = compound.local_bounds().expect("nonzero compound bounds");
        assert!((compound_bounds.x0 - source_bounds.x0).abs() < 1e-6);
        assert!((compound_bounds.y0 - source_bounds.y0).abs() < 1e-6);
        assert!((compound_bounds.x1 - source_bounds.x1).abs() < 1e-6);
        assert!((compound_bounds.y1 - source_bounds.y1).abs() < 1e-6);
        let world_bounds = compound
            .transform
            .to_kurbo()
            .transform_rect_bbox(compound_bounds);
        assert!((world_bounds.x0 - (source_bounds.x0 + matrix[4])).abs() < 1e-6);
        assert!((world_bounds.y0 - (source_bounds.y0 + matrix[5])).abs() < 1e-6);
        assert!((world_bounds.x1 - (source_bounds.x1 + matrix[4])).abs() < 1e-6);
        assert!((world_bounds.y1 - (source_bounds.y1 + matrix[5])).abs() < 1e-6);

        let compound_mesh =
            photonic_render::tessellator::tessellate_fill(&compound_path.path_data, true, 0.1);
        assert!(!compound_mesh.is_empty());
        // Compare raster-fill membership across the silhouette. This catches
        // criss-crossed subpaths and verifies even-odd output matches the
        // original nonzero-wound SVG rendering.
        for y_step in 0..64 {
            for x_step in 0..64 {
                let point = [
                    (source_bounds.x0 + source_bounds.width() * (x_step as f64 + 0.5) / 64.0)
                        as f32,
                    (source_bounds.y0 + source_bounds.height() * (y_step as f64 + 0.5) / 64.0)
                        as f32,
                ];
                assert_eq!(
                    mesh_covers(&source_mesh, point),
                    mesh_covers(&compound_mesh, point),
                    "rendering differs at {point:?}"
                );
            }
        }
        for hole in &subpaths[1..] {
            let bounds = hole.bounding_box();
            let center = [bounds.center().x as f32, bounds.center().y as f32];
            assert!(
                !mesh_covers(&compound_mesh, center),
                "hole must remain clear"
            );
        }

        let svg = export_svg(&document, &SvgExportOptions::default());
        assert!(svg.contains("fill-rule=\"evenodd\""));
        assert!(kurbo::BezPath::from_svg(compound_path.path_data.as_svg()).is_ok());
    }

    #[tokio::test]
    async fn transformed_contours_make_renderable_exportable_compound_with_holes() {
        let mut document = Document::new("compound regression", 160.0, 140.0);
        let layer_id = document.active_layer_id.expect("default layer");

        // All three contours have distinct, non-identity transforms. In world
        // space the outer contour is x=20..120/y=20..120 and the two holes are
        // x=40..60/y=45..65 and x=80..100/y=75..95.
        let outer = transformed_rect(
            "outer",
            layer_id,
            50.0,
            50.0,
            Transform::new(2.0, 0.0, 0.0, 2.0, 20.0, 20.0),
        );
        let hole_1 = transformed_rect(
            "hole 1",
            layer_id,
            10.0,
            10.0,
            Transform::new(2.0, 0.0, 0.0, 2.0, 40.0, 45.0),
        );
        let hole_2 = transformed_rect(
            "hole 2",
            layer_id,
            8.0,
            8.0,
            Transform::new(2.5, 0.0, 0.0, 2.5, 80.0, 75.0),
        );
        let ids = vec![outer.id, hole_1.id, hole_2.id];
        for node in [outer, hole_1, hole_2] {
            document.add_node(node, Some(layer_id));
        }
        let state = test_state(document);

        let result = make_compound_path(
            &state,
            MakeCompoundPathArgs {
                node_ids: ids.clone(),
                name: Some("dancer contours".into()),
            },
        )
        .await;
        assert_ne!(result.is_error, Some(true), "{:?}", result.content);

        let document = state.document.lock().await;
        assert_eq!(
            document.nodes.len(),
            1,
            "source contours should be replaced"
        );
        let compound = document.nodes.get(&ids[0]).expect("base node retained");
        let path = match &compound.kind {
            SceneNodeKind::Path(path) => path,
            _ => panic!("compound must remain a path"),
        };
        assert!(path.is_compound);
        let bez = path
            .path_data
            .try_to_bez_path()
            .expect("valid stored SVG path");
        let anchor_count = bez
            .elements()
            .iter()
            .filter(|element| !matches!(element, kurbo::PathEl::ClosePath))
            .count();
        assert_eq!(anchor_count, 12, "all three four-anchor contours survive");
        let bounds = compound.local_bounds().expect("nonempty local bounds");
        assert!(bounds.width() > 0.0 && bounds.height() > 0.0);
        assert!(bez.area().abs() > 0.0);

        // This is the exact even-odd tessellation consumed by the interactive
        // and headless raster renderers. Sample local points corresponding to a
        // solid part of the silhouette and each transparent cutout.
        let mesh = photonic_render::tessellator::tessellate_fill(&path.path_data, true, 0.1);
        assert!(!mesh.is_empty());
        assert!(mesh_covers(&mesh, [5.0, 5.0]));
        assert!(
            !mesh_covers(&mesh, [15.0, 17.5]),
            "first cutout must be clear"
        );
        assert!(
            !mesh_covers(&mesh, [35.0, 32.5]),
            "second cutout must be clear"
        );

        let svg = export_svg(&document, &SvgExportOptions::default());
        assert!(svg.contains("fill-rule=\"evenodd\""), "{svg}");
        let d = path.path_data.as_svg();
        assert_eq!(d.matches('M').count(), 3, "SVG must contain three subpaths");
        assert!(kurbo::BezPath::from_svg(d).is_ok(), "exported d must parse");
    }
}
