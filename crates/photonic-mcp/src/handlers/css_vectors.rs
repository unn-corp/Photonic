use crate::protocol::{CreateVectorsFromCssArgs, ToolResult};
use crate::server::AppState;
use photonic_core::{
    css_vectors::{compile_css_vectors, CssOrigin, CssVectorNode, CssViewport, CONTRACT_VERSION},
    history::Command,
    node::{GroupNode, PathNode, SceneNode, SceneNodeKind},
};
use uuid::Uuid;

/// Transport adapter for the reusable core compiler.  It deliberately builds
/// the complete subtree before taking document/history locks so parser or
/// lowering errors cannot leave partial artwork behind.
pub async fn create_vectors_from_css(
    state: &AppState,
    args: CreateVectorsFromCssArgs,
) -> ToolResult {
    let (canvas_w, canvas_h, active_layer) = {
        let doc = state.document.lock().await;
        (doc.width, doc.height, doc.active_layer_id)
    };
    let viewport = args
        .viewport
        .as_ref()
        .map(|v| CssViewport {
            width: v.width,
            height: v.height,
        })
        .unwrap_or(CssViewport {
            width: canvas_w,
            height: canvas_h,
        });
    let origin = args
        .origin
        .as_ref()
        .map(|p| CssOrigin { x: p.x, y: p.y })
        .unwrap_or(CssOrigin { x: 0.0, y: 0.0 });
    let plan = match compile_css_vectors(
        &args.css,
        args.selector.as_deref(),
        origin,
        viewport,
        args.strict,
    ) {
        Ok(plan) => plan,
        Err(diagnostics) => return ToolResult::error("CSS conversion rejected").with_data(
            serde_json::json!({ "diagnostics": diagnostics, "contract_version": CONTRACT_VERSION }),
        ),
    };
    let layer_id = args.layer_id.or(active_layer);
    let Some(layer_id) = layer_id else {
        return ToolResult::error("Document has no active layer");
    };
    {
        let doc = state.document.lock().await;
        let Some(layer) = doc.layers.get(&layer_id) else {
            return ToolResult::error("layer_id does not exist");
        };
        if layer.locked {
            return ToolResult::error("destination layer is locked");
        }
    }
    let mut nodes = Vec::new();
    let mut roots = Vec::new();
    let fingerprint_provenance = format!(
        "css-vector-v{} fingerprint={}",
        CONTRACT_VERSION, plan.fingerprint
    );
    for (i, root) in plan.roots.iter().enumerate() {
        let id = lower(root, layer_id, &mut nodes);
        // A caller-supplied root name applies only to a single selected root.
        if i == 0 {
            if let Some(name) = &args.group_name {
                if let Some(n) = nodes.iter_mut().find(|n: &&mut SceneNode| n.id == id) {
                    n.name = name.clone();
                }
            }
        }
        if let Some(n) = nodes.iter_mut().find(|n: &&mut SceneNode| n.id == id) {
            // Keep the source available for inspection without retaining a
            // full copy on every root when a stylesheet produces many roots.
            let provenance = if i == 0 {
                format!("{fingerprint_provenance} css={}", args.css)
            } else {
                fingerprint_provenance.clone()
            };
            n.prompt_history.push(provenance);
        }
        roots.push(id);
    }
    let root_node_ids = if args.dry_run {
        Vec::new()
    } else {
        roots.clone()
    };
    let created: Vec<Uuid> = if args.dry_run {
        Vec::new()
    } else {
        nodes.iter().map(|n| n.id).collect()
    };
    let (groups, paths, segments) = counts(&nodes);
    let data = serde_json::json!({
        "root_node_ids": root_node_ids, "created_node_ids": created,
        "bounds": {"x":plan.bounds.0,"y":plan.bounds.1,"width":plan.bounds.2,"height":plan.bounds.3},
        "node_counts": {"groups":groups,"paths":paths,"segments":segments},
        "resolved_viewport": {"width":viewport.width,"height":viewport.height},
        "diagnostics": plan.diagnostics, "source_fingerprint": plan.fingerprint,
        "contract_version": CONTRACT_VERSION, "dry_run": args.dry_run,
    });
    if args.dry_run {
        return ToolResult::text("CSS vector conversion plan").with_data(data);
    }
    // AddSubtree is self-contained and is one history edge: undo/redo removes
    // or restores every group/path as a single atomic operation.
    let cmd = Command::AddSubtree {
        layer_id,
        roots,
        nodes,
    };
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);
    ToolResult::text("Created editable vectors from CSS").with_data(data)
}

fn lower(input: &CssVectorNode, layer_id: Uuid, output: &mut Vec<SceneNode>) -> Uuid {
    match input {
        CssVectorNode::Path(p) => {
            let mut path = PathNode::new(p.path.clone());
            path.fill = p.fill.clone();
            path.stroke = p.stroke.clone();
            let mut node = SceneNode::new(&p.name, layer_id, SceneNodeKind::Path(path));
            node.opacity = p.opacity;
            // Persist source inspection information without introducing a new
            // opaque scene-graph field; prompts are intentionally non-printing.
            node.tags.push("css-vector-v1".into());
            let id = node.id;
            output.push(node);
            id
        }
        CssVectorNode::Group(g) => {
            let mut group = GroupNode::new();
            for child in &g.children {
                group.children.push(lower(child, layer_id, output));
            }
            let mut node = SceneNode::new(&g.name, layer_id, SceneNodeKind::Group(group));
            node.opacity = g.opacity;
            node.tags.push("css-vector-v1".into());
            node.prompt_history
                .push(format!("css-vector-v1 selector={}", g.provenance));
            let id = node.id;
            output.push(node);
            id
        }
    }
}

fn counts(nodes: &[SceneNode]) -> (usize, usize, usize) {
    let groups = nodes
        .iter()
        .filter(|n| matches!(n.kind, SceneNodeKind::Group(_)))
        .count();
    let paths: Vec<_> = nodes
        .iter()
        .filter_map(|n| {
            if let SceneNodeKind::Path(p) = &n.kind {
                Some(p)
            } else {
                None
            }
        })
        .collect();
    // Segment count is stable and intentionally approximate only at this
    // boundary: it counts exported path elements rather than renderer tessels.
    let segments = paths
        .iter()
        .map(|p| p.path_data.as_svg().matches(char::is_alphabetic).count())
        .sum();
    (groups, paths.len(), segments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::McpServerConfig;
    use photonic_core::{css_vectors::MAX_ELEMENTS, history::CommandHistory, AuditLog, Document};
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::Mutex;

    fn test_state() -> AppState {
        let (tx, _rx) = std::sync::mpsc::channel();
        AppState {
            document: Arc::new(Mutex::new(Document::new("t", 512.0, 512.0))),
            history: Arc::new(Mutex::new(CommandHistory::new(100))),
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

    fn args(dry_run: bool) -> CreateVectorsFromCssArgs {
        CreateVectorsFromCssArgs {
            css: ".badge { width: 240px; height: 96px; background: #6941c6; border: 3px solid #fff; border-radius: 24px; }".into(),
            selector: Some(".badge".into()),
            origin: None,
            viewport: None,
            layer_id: None,
            group_name: None,
            strict: true,
            dry_run,
        }
    }

    fn plan_json(result: &ToolResult) -> serde_json::Value {
        let crate::protocol::ContentItem::Text { text } = &result.content[1] else {
            panic!("missing JSON plan")
        };
        serde_json::from_str(text).unwrap()
    }

    #[tokio::test]
    async fn dry_run_is_repeatable_without_node_ids_or_mutation() {
        let state = test_state();
        let before_document = serde_json::to_value(&*state.document.lock().await).unwrap();
        let before_history = state.history.lock().await.undo_depth();

        let first = create_vectors_from_css(&state, args(true)).await;
        let second = create_vectors_from_css(&state, args(true)).await;
        assert_ne!(first.is_error, Some(true), "{first:?}");
        assert_ne!(second.is_error, Some(true), "{second:?}");

        let first_plan = plan_json(&first);
        let second_plan = plan_json(&second);
        assert_eq!(first_plan, second_plan);
        assert!(first_plan["root_node_ids"].as_array().unwrap().is_empty());
        assert!(first_plan["created_node_ids"]
            .as_array()
            .unwrap()
            .is_empty());

        assert_eq!(
            before_document,
            serde_json::to_value(&*state.document.lock().await).unwrap()
        );
        assert_eq!(before_history, state.history.lock().await.undo_depth());
    }

    #[tokio::test]
    async fn non_dry_run_reports_ids_inserted_into_document() {
        let state = test_state();
        let result = create_vectors_from_css(&state, args(false)).await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
        let plan = plan_json(&result);
        let created = plan["created_node_ids"].as_array().unwrap();
        let roots = plan["root_node_ids"].as_array().unwrap();
        assert!(!created.is_empty());
        assert!(!roots.is_empty());

        let doc = state.document.lock().await;
        assert_eq!(created.len(), doc.nodes.len());
        for id in created.iter().chain(roots) {
            let id = Uuid::parse_str(id.as_str().unwrap()).unwrap();
            assert!(doc.nodes.contains_key(&id));
        }
        assert_eq!(state.history.lock().await.undo_depth(), 1);
    }

    #[tokio::test]
    async fn multi_root_provenance_keeps_full_css_source_once() {
        let state = test_state();
        let css: String = (0..=MAX_ELEMENTS)
            .map(|i| format!(".item-{i} {{ width: 1px; height: 1px; }}"))
            .collect();
        let result = create_vectors_from_css(
            &state,
            CreateVectorsFromCssArgs {
                css: css.clone(),
                selector: None,
                origin: None,
                viewport: None,
                layer_id: None,
                group_name: None,
                strict: false,
                dry_run: false,
            },
        )
        .await;
        assert_ne!(result.is_error, Some(true), "{result:?}");

        let plan = plan_json(&result);
        let root_ids = plan["root_node_ids"].as_array().unwrap();
        assert_eq!(root_ids.len(), MAX_ELEMENTS);
        let doc = state.document.lock().await;
        let full_source_marker = format!("css={css}");
        let root_nodes: Vec<_> = root_ids
            .iter()
            .map(|id| Uuid::parse_str(id.as_str().unwrap()).unwrap())
            .filter_map(|id| doc.nodes.get(&id))
            .collect();
        assert_eq!(root_nodes.len(), MAX_ELEMENTS);
        let fingerprint_marker = format!("css-vector-v{} fingerprint=", CONTRACT_VERSION);
        let fingerprint_entries = root_nodes
            .iter()
            .flat_map(|node| node.prompt_history.iter())
            .filter(|entry| entry.starts_with(&fingerprint_marker))
            .count();
        assert_eq!(fingerprint_entries, MAX_ELEMENTS);
        let full_source_entries = root_nodes
            .iter()
            .flat_map(|node| node.prompt_history.iter())
            .filter(|entry| entry.contains(&full_source_marker))
            .count();
        assert_eq!(full_source_entries, 1);
    }
}
