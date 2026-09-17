use crate::handlers::shared::spatial::world_aabb;
use crate::protocol::{
    AddConstructionLineArgs, AddDimensionArgs, ApplyDocumentTemplateArgs, DiffCheckpointsArgs,
    FitToMarginsArgs, GetCanvasOverviewArgs, GetDocumentStateArgs, JumpToHistoryArgs,
    ListHistoryArgs, RemoveDimensionArgs, ResizeCanvasArgs, RestoreCheckpointArgs,
    SetArtboardMarginsArgs, SetDocumentBleedArgs, SetDocumentColorModeArgs, SetDocumentDpiArgs,
    ToolResult, UndoRedoArgs,
};
use crate::server::AppState;
use photonic_core::node::SceneNodeKind;
use photonic_core::style::FillKind;
use serde_json::json;
use std::collections::BTreeSet;
use uuid::Uuid;

pub async fn get_document_state(state: &AppState, args: GetDocumentStateArgs) -> ToolResult {
    tracing::debug!("tool: get_document_state");
    let doc = state.document.lock().await;

    let layers: Vec<_> = doc
        .layer_order
        .iter()
        .filter(|id| {
            args.layer_id
                .map(|filter_id| filter_id == **id)
                .unwrap_or(true)
        })
        .filter_map(|id| doc.layers.get(id))
        .map(|layer| {
            let nodes: Vec<_> = layer
                .node_ids
                .iter()
                .enumerate()
                .filter_map(|(z_index, nid)| doc.nodes.get(nid).map(|n| (z_index, nid, n)))
                .map(|(z_index, _nid, node)| {
                    if args.summary_only {
                        // Compact: only id, name, kind type, z_index
                        let kind_type = match &node.kind {
                            SceneNodeKind::Path(_) => "path",
                            SceneNodeKind::Group(_) => "group",
                            SceneNodeKind::Text(_) => "text",
                            SceneNodeKind::Raster(_) => "raster",
                        };
                        return json!({
                            "id": node.id,
                            "name": node.name,
                            "kind": kind_type,
                            "z_index": z_index,
                        });
                    }

                    let mut v = serde_json::to_value(node).unwrap_or_default();
                    // Strip verbose path data unless requested
                    if !args.include_path_data {
                        if let Some(kind) = v.get_mut("kind") {
                            if let Some(path_data) = kind.get_mut("path_data") {
                                *path_data = json!("<omitted>");
                            }
                        }
                    }
                    if let Some(obj) = v.as_object_mut() {
                        // layer_id is redundant — it's already the enclosing layer
                        obj.remove("layer_id");
                        // Inject z_index so Claude can reason about stacking order
                        obj.insert("z_index".to_string(), json!(z_index));
                        // For groups, also surface the children array at the top level
                        if let SceneNodeKind::Group(g) = &node.kind {
                            obj.insert("children".to_string(), json!(g.children));
                        }
                    }
                    v
                })
                .collect();

            json!({
                "id": layer.id,
                "name": layer.name,
                "visible": layer.visible,
                "locked": layer.locked,
                "opacity": layer.opacity,
                "node_count": nodes.len(),
                "nodes": nodes,
            })
        })
        .collect();

    let state_value = json!({
        "id": doc.id,
        "name": doc.name,
        "width": doc.width,
        "height": doc.height,
        "node_count": doc.node_count(),
        "layer_count": doc.layers.len(),
        "active_layer_id": doc.active_layer_id,
        "selection": doc.selection.ids().collect::<Vec<_>>(),
        "layers": layers,
    });

    ToolResult::text(format!(
        "Document '{}' — {} node(s) across {} layer(s)",
        doc.name,
        doc.node_count(),
        doc.layers.len()
    ))
    .with_data(state_value)
}

pub async fn get_document_info(state: &AppState) -> ToolResult {
    tracing::debug!("tool: get_document_info");
    let doc = state.document.lock().await;

    // Count nodes by kind
    let mut path_count = 0usize;
    let mut text_count = 0usize;
    let mut group_count = 0usize;
    let mut font_names: BTreeSet<String> = BTreeSet::new();
    let mut fill_hex: BTreeSet<String> = BTreeSet::new();

    for node in doc.nodes.values() {
        match &node.kind {
            SceneNodeKind::Path(p) => {
                path_count += 1;
                if p.fill.enabled {
                    if let FillKind::Solid(c) = &p.fill.kind {
                        fill_hex.insert(c.to_hex());
                    }
                }
            }
            SceneNodeKind::Text(t) => {
                text_count += 1;
                if !t.font_family.is_empty() {
                    font_names.insert(t.font_family.clone());
                }
                if t.fill.enabled {
                    if let FillKind::Solid(c) = &t.fill.kind {
                        fill_hex.insert(c.to_hex());
                    }
                }
            }
            SceneNodeKind::Group(_) => {
                group_count += 1;
            }
            // raster: no vector geometry / fill / font to tally
            SceneNodeKind::Raster(_) => {}
        }
    }

    let layer_summaries: Vec<serde_json::Value> = doc
        .layer_order
        .iter()
        .filter_map(|id| doc.layers.get(id))
        .map(|l| {
            json!({
                "id": l.id,
                "name": l.name,
                "visible": l.visible,
                "locked": l.locked,
                "is_template": l.is_template,
                "node_count": l.node_ids.len(),
            })
        })
        .collect();

    let total = path_count + text_count + group_count;

    let artboard_summaries: Vec<serde_json::Value> = doc
        .artboards
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "name": a.name,
                "x": a.x,
                "y": a.y,
                "width": a.width,
                "height": a.height,
                "active": Some(a.id) == doc.active_artboard,
            })
        })
        .collect();

    ToolResult::text(format!(
        "Document '{}': {}×{} canvas, {} node(s) in {} layer(s) — {} path(s), {} text(s), {} group(s); {} artboard(s)",
        doc.name, doc.width as u32, doc.height as u32,
        total, layer_summaries.len(),
        path_count, text_count, group_count,
        artboard_summaries.len(),
    ))
    .with_data(json!({
        "name": doc.name,
        "canvas": { "width": doc.width, "height": doc.height },
        "layer_count": layer_summaries.len(),
        "layers": layer_summaries,
        "artboard_count": artboard_summaries.len(),
        "artboards": artboard_summaries,
        "active_artboard": doc.active_artboard,
        // Print-production properties (honored on PDF/raster export).
        "dpi": doc.dpi,
        "bleed_mm": doc.bleed_mm,
        "slug_mm": doc.slug_mm,
        "color_mode": match doc.color_mode {
            photonic_core::document::ColorMode::Rgb => "rgb",
            photonic_core::document::ColorMode::Cmyk => "cmyk",
        },
        "nodes": {
            "total": total,
            "path": path_count,
            "text": text_count,
            "group": group_count,
        },
        "font_names": font_names.iter().take(20).collect::<Vec<_>>(),
        "fill_colors": fill_hex.iter().take(20).collect::<Vec<_>>(),
    }))
}

pub async fn undo(state: &AppState, args: UndoRedoArgs) -> (ToolResult, bool) {
    tracing::debug!("tool: undo");
    let steps = args.steps.unwrap_or(1);
    // Acquire both locks once so the render thread is only blocked for one
    // short critical section rather than for N separate lock acquisitions.
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let mut count = 0;
    for _ in 0..steps {
        if history.undo(&mut doc) {
            count += 1;
        } else {
            break;
        }
    }
    let moved = count > 0;
    let result = if moved {
        ToolResult::text(format!("Undid {} step(s)", count))
    } else {
        ToolResult::text("Nothing to undo")
    };
    (result, moved)
}

pub async fn redo(state: &AppState, args: UndoRedoArgs) -> (ToolResult, bool) {
    tracing::debug!("tool: redo");
    let steps = args.steps.unwrap_or(1);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let mut count = 0;
    for _ in 0..steps {
        if history.redo(&mut doc) {
            count += 1;
        } else {
            break;
        }
    }
    let moved = count > 0;
    let result = if moved {
        ToolResult::text(format!("Redid {} step(s)", count))
    } else {
        ToolResult::text("Nothing to redo")
    };
    (result, moved)
}

pub async fn resize_canvas(state: &AppState, args: ResizeCanvasArgs) -> ToolResult {
    tracing::debug!("tool: resize_canvas");
    use photonic_core::history::Command;

    if args.width <= 0.0 || args.height <= 0.0 {
        return ToolResult::error("Width and height must be positive");
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let old_w = doc.width;
    let old_h = doc.height;

    history.execute_discrete(
        Command::ResizeCanvas {
            old_width: old_w,
            old_height: old_h,
            new_width: args.width,
            new_height: args.height,
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Resized canvas: {old_w}×{old_h} → {}×{}",
        args.width, args.height
    ))
    .with_data(serde_json::json!({
        "old_width": old_w, "old_height": old_h,
        "new_width": args.width, "new_height": args.height,
    }))
}

/// List all saved checkpoints.
pub async fn list_checkpoints(state: &AppState) -> ToolResult {
    let infos = state.history.lock().await.list_checkpoints();
    let list: Vec<_> = infos
        .iter()
        .map(|c| json!({ "id": c.id.to_string(), "name": c.name, "created_at": c.created_at }))
        .collect();
    ToolResult::text(format!("{} checkpoint(s)", list.len()))
        .with_data(json!({ "checkpoints": list }))
}

/// Restore the document to a saved checkpoint, clearing undo/redo history.
pub async fn restore_checkpoint(state: &AppState, args: RestoreCheckpointArgs) -> ToolResult {
    let id = match uuid::Uuid::parse_str(&args.checkpoint_id) {
        Ok(id) => id,
        Err(_) => return ToolResult::error("Invalid checkpoint ID"),
    };
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    match history.restore_checkpoint(id) {
        Some(snapshot) => {
            *doc = snapshot;
            ToolResult::text(format!("Restored to checkpoint '{}'", args.checkpoint_id))
        }
        None => ToolResult::error(format!("Checkpoint '{}' not found", args.checkpoint_id)),
    }
}

/// Compare two checkpoint snapshots and return a structured diff of
/// added/removed/modified nodes and layers.
pub async fn diff_checkpoints(state: &AppState, args: DiffCheckpointsArgs) -> ToolResult {
    tracing::debug!("tool: diff_checkpoints");

    let from_uuid = match uuid::Uuid::parse_str(&args.from_id) {
        Ok(id) => id,
        Err(_) => return ToolResult::error(format!("Invalid from_id: '{}'", args.from_id)),
    };
    let to_uuid = match uuid::Uuid::parse_str(&args.to_id) {
        Ok(id) => id,
        Err(_) => return ToolResult::error(format!("Invalid to_id: '{}'", args.to_id)),
    };

    let history = state.history.lock().await;

    let from_info = history
        .list_checkpoints()
        .into_iter()
        .find(|c| c.id == from_uuid);
    let to_info = history
        .list_checkpoints()
        .into_iter()
        .find(|c| c.id == to_uuid);

    let from_doc = match history.get_checkpoint_snapshot(from_uuid) {
        Some(d) => d,
        None => return ToolResult::error(format!("Checkpoint '{}' not found", args.from_id)),
    };
    let to_doc = match history.get_checkpoint_snapshot(to_uuid) {
        Some(d) => d,
        None => return ToolResult::error(format!("Checkpoint '{}' not found", args.to_id)),
    };

    // Drop the history lock before doing the (potentially heavy) diff.
    drop(history);

    // ── Node diff ────────────────────────────────────────────────────────────
    let mut added_nodes = Vec::new();
    let mut removed_nodes = Vec::new();
    let mut modified_nodes = Vec::new();

    for (id, node) in &to_doc.nodes {
        let kind_str = match &node.kind {
            SceneNodeKind::Path(_) => "path",
            SceneNodeKind::Group(_) => "group",
            SceneNodeKind::Text(_) => "text",
            SceneNodeKind::Raster(_) => "raster",
        };
        if !from_doc.nodes.contains_key(id) {
            added_nodes.push(json!({ "id": id.to_string(), "name": node.name, "kind": kind_str }));
        } else if let Some(old) = from_doc.nodes.get(id) {
            let from_val = serde_json::to_value(old).unwrap_or_default();
            let to_val = serde_json::to_value(node).unwrap_or_default();
            if from_val != to_val {
                let changed: Vec<String> =
                    if let (Some(fo), Some(to)) = (from_val.as_object(), to_val.as_object()) {
                        fo.keys()
                            .filter(|k| fo.get(*k) != to.get(*k))
                            .cloned()
                            .collect()
                    } else {
                        vec![]
                    };
                modified_nodes.push(json!({
                    "id": id.to_string(),
                    "name": node.name,
                    "kind": kind_str,
                    "changed_fields": changed,
                }));
            }
        }
    }
    for (id, node) in &from_doc.nodes {
        if !to_doc.nodes.contains_key(id) {
            let kind_str = match &node.kind {
                SceneNodeKind::Path(_) => "path",
                SceneNodeKind::Group(_) => "group",
                SceneNodeKind::Text(_) => "text",
                SceneNodeKind::Raster(_) => "raster",
            };
            removed_nodes
                .push(json!({ "id": id.to_string(), "name": node.name, "kind": kind_str }));
        }
    }

    // ── Layer diff ────────────────────────────────────────────────────────────
    let mut added_layers = Vec::new();
    let mut removed_layers = Vec::new();
    let mut modified_layers = Vec::new();

    for (id, layer) in &to_doc.layers {
        if !from_doc.layers.contains_key(id) {
            added_layers.push(json!({ "id": id.to_string(), "name": layer.name }));
        } else if let Some(old) = from_doc.layers.get(id) {
            let from_val = serde_json::to_value(old).unwrap_or_default();
            let to_val = serde_json::to_value(layer).unwrap_or_default();
            if from_val != to_val {
                let changed: Vec<String> =
                    if let (Some(fo), Some(to)) = (from_val.as_object(), to_val.as_object()) {
                        fo.keys()
                            .filter(|k| fo.get(*k) != to.get(*k))
                            .cloned()
                            .collect()
                    } else {
                        vec![]
                    };
                modified_layers.push(json!({
                    "id": id.to_string(),
                    "name": layer.name,
                    "changed_fields": changed,
                }));
            }
        }
    }
    for (id, layer) in &from_doc.layers {
        if !to_doc.layers.contains_key(id) {
            removed_layers.push(json!({ "id": id.to_string(), "name": layer.name }));
        }
    }

    let total_changes = added_nodes.len()
        + removed_nodes.len()
        + modified_nodes.len()
        + added_layers.len()
        + removed_layers.len()
        + modified_layers.len();

    let from_name = from_info
        .as_ref()
        .map(|c| c.name.as_str())
        .unwrap_or("<unknown>");
    let to_name = to_info
        .as_ref()
        .map(|c| c.name.as_str())
        .unwrap_or("<unknown>");

    ToolResult::text(format!(
        "Diff '{}' → '{}': {} node change(s) ({} added, {} removed, {} modified), {} layer change(s)",
        from_name, to_name,
        added_nodes.len() + removed_nodes.len() + modified_nodes.len(),
        added_nodes.len(), removed_nodes.len(), modified_nodes.len(),
        added_layers.len() + removed_layers.len() + modified_layers.len(),
    ))
    .with_data(json!({
        "from_checkpoint": {
            "id": args.from_id,
            "name": from_info.as_ref().map(|c| c.name.clone()).unwrap_or_default(),
            "created_at": from_info.as_ref().map(|c| c.created_at).unwrap_or(0),
        },
        "to_checkpoint": {
            "id": args.to_id,
            "name": to_info.as_ref().map(|c| c.name.clone()).unwrap_or_default(),
            "created_at": to_info.as_ref().map(|c| c.created_at).unwrap_or(0),
        },
        "total_changes": total_changes,
        "nodes": {
            "added":    added_nodes,
            "removed":  removed_nodes,
            "modified": modified_nodes,
        },
        "layers": {
            "added":    added_layers,
            "removed":  removed_layers,
            "modified": modified_layers,
        },
    }))
}

// ─── export profiles ─────────────────────────────────────────────────────────

/// Return the current document as a reusable template: canvas size, layers,
/// guides, and export profiles are preserved; all node content is stripped.
pub async fn get_document_template(state: &AppState) -> ToolResult {
    tracing::debug!("tool: get_document_template");
    let doc = state.document.lock().await;

    // Clone and strip all node content so the template carries structure only.
    let mut template = doc.clone();
    template.nodes.clear();
    template.selection = Default::default();
    for layer in template.layers.values_mut() {
        layer.node_ids.clear();
    }

    match template.to_json() {
        Ok(json_str) => {
            let bytes = json_str.len();
            ToolResult::text(format!(
                "Document template captured — {} layer(s), {} guide(s), {} export profile(s) ({bytes} bytes)",
                template.layers.len(),
                template.guides.len(),
                template.export_profiles.len(),
            ))
            .with_data(serde_json::json!({
                "template_json": json_str,
                "layer_count": template.layers.len(),
                "guide_count": template.guides.len(),
                "export_profile_count": template.export_profiles.len(),
                "canvas": { "width": template.width, "height": template.height },
            }))
        }
        Err(e) => ToolResult::error(format!("Failed to serialize template: {e}")),
    }
}

/// Apply a template (from `get_document_template`) to the current document.
/// Canvas size, guides, and export profiles from the template are merged in;
/// existing nodes are preserved. New layers from the template are added only
/// if no layer with the same name already exists.
pub async fn apply_document_template(
    state: &AppState,
    args: ApplyDocumentTemplateArgs,
) -> ToolResult {
    tracing::debug!("tool: apply_document_template");

    let template = match photonic_core::document::Document::from_json(&args.template_json) {
        Ok(t) => t,
        Err(e) => return ToolResult::error(format!("Invalid template JSON: {e}")),
    };

    let mut doc = state.document.lock().await;

    // 1. Canvas size.
    if template.width > 0.0
        && template.height > 0.0
        && (template.width != doc.width || template.height != doc.height)
    {
        doc.width = template.width;
        doc.height = template.height;
    }

    // 2. Guides — deduplicate standard guides by axis+position and angled lines
    // by angle+origin. Their remaining metadata is preserved when copied.
    let mut guides_added = 0usize;
    for tg in &template.guides {
        let already = doc
            .guides
            .iter()
            .any(|g| match (g.angle_degrees, tg.angle_degrees) {
                (None, None) => {
                    g.orientation == tg.orientation && (g.position - tg.position).abs() < 0.5
                }
                (Some(angle), Some(template_angle)) => {
                    (angle - template_angle).abs() < 0.5
                        && (g.position_x - tg.position_x).abs() < 0.5
                        && (g.position_y - tg.position_y).abs() < 0.5
                }
                _ => false,
            });
        if !already {
            let mut guide = tg.clone();
            guide.id = Uuid::new_v4();
            doc.guides.push(guide);
            guides_added += 1;
        }
    }

    // 3. Export profiles — replace same-name or append.
    let mut profiles_added = 0usize;
    let mut profiles_updated = 0usize;
    for tp in &template.export_profiles {
        if let Some(existing) = doc.export_profiles.iter_mut().find(|p| p.name == tp.name) {
            *existing = tp.clone();
            profiles_updated += 1;
        } else {
            doc.export_profiles.push(tp.clone());
            profiles_added += 1;
        }
    }

    // 4. Layers — add template layers whose name doesn't exist in current doc.
    let mut layers_added = 0usize;
    for tlid in &template.layer_order {
        if let Some(tlayer) = template.layers.get(tlid) {
            let name_exists = doc.layers.values().any(|l| l.name == tlayer.name);
            if !name_exists {
                let mut new_layer = tlayer.clone();
                new_layer.node_ids.clear(); // template layers have no nodes
                doc.add_layer(new_layer);
                layers_added += 1;
            }
        }
    }

    ToolResult::text(format!(
        "Template applied — {} layer(s) added, {} guide(s) added, {} export profile(s) added/updated",
        layers_added, guides_added, profiles_added + profiles_updated,
    ))
    .with_data(serde_json::json!({
        "layers_added": layers_added,
        "guides_added": guides_added,
        "export_profiles_added": profiles_added,
        "export_profiles_updated": profiles_updated,
        "canvas": { "width": doc.width, "height": doc.height },
    }))
}

// ─── Color Swatches ───────────────────────────────────────────────────────────

/// Return a compact spatial overview of all visible nodes: bounding boxes and fill colors.
/// Useful for AI agents to understand document layout without loading the full document state.
pub async fn get_canvas_overview(state: &AppState, args: GetCanvasOverviewArgs) -> ToolResult {
    tracing::debug!("tool: get_canvas_overview");
    let doc = state.document.lock().await;

    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;

    let mut node_entries: Vec<serde_json::Value> = Vec::new();

    for node in doc.nodes_in_draw_order() {
        if !node.visible && !args.include_hidden {
            continue;
        }
        // World-space position origin
        let (wx, wy) = node.transform.apply(0.0, 0.0);

        // World-space bounds using all four transformed local corners.
        let (bx, by, bw, bh) = world_aabb(node)
            .map(|[x, y, width, height]| (x, y, width.max(1.0), height.max(1.0)))
            .unwrap_or((wx, wy, 1.0, 1.0));

        // Expand canvas bounds
        if bx < min_x {
            min_x = bx;
        }
        if by < min_y {
            min_y = by;
        }
        if bx + bw > max_x {
            max_x = bx + bw;
        }
        if by + bh > max_y {
            max_y = by + bh;
        }

        // Extract fill color as hex
        let fill_hex = match &node.kind {
            SceneNodeKind::Path(pn) => match &pn.fill.kind {
                FillKind::Solid(c) => format!(
                    "#{:02X}{:02X}{:02X}",
                    (c.r * 255.0) as u8,
                    (c.g * 255.0) as u8,
                    (c.b * 255.0) as u8
                ),
                FillKind::Gradient(_) => "#gradient".to_string(),
                FillKind::FluidGradient(_) => "#fluid".to_string(),
                FillKind::MeshGradient(_) => "#mesh".to_string(),
                FillKind::Pattern(_) => "#pattern".to_string(),
                FillKind::None => "#none".to_string(),
            },
            SceneNodeKind::Text(tn) => match &tn.fill.kind {
                FillKind::Solid(c) => format!(
                    "#{:02X}{:02X}{:02X}",
                    (c.r * 255.0) as u8,
                    (c.g * 255.0) as u8,
                    (c.b * 255.0) as u8
                ),
                _ => "#000000".to_string(),
            },
            SceneNodeKind::Group(_) => "#group".to_string(),
            // raster: no vector fill
            SceneNodeKind::Raster(_) => "#raster".to_string(),
        };

        let layer_name = doc
            .layers
            .get(&node.layer_id)
            .map(|l| l.name.as_str())
            .unwrap_or("?");

        node_entries.push(json!({
            "id": node.id,
            "name": node.name,
            "layer": layer_name,
            "visible": node.visible,
            "kind": match &node.kind {
                SceneNodeKind::Path(_) => "path",
                SceneNodeKind::Text(_) => "text",
                SceneNodeKind::Group(_) => "group",
                SceneNodeKind::Raster(_) => "raster",
            },
            "bounds": { "x": bx, "y": by, "w": bw, "h": bh },
            "fill_hex": fill_hex,
        }));
    }

    // If no nodes, use defaults
    if min_x == f64::MAX {
        min_x = 0.0;
        min_y = 0.0;
        max_x = 800.0;
        max_y = 600.0;
    }

    ToolResult::text(format!(
        "{} node(s) in canvas overview.",
        node_entries.len()
    ))
    .with_data(json!({
        "node_count": node_entries.len(),
        "canvas_bounds": {
            "x": min_x, "y": min_y,
            "w": (max_x - min_x).max(1.0),
            "h": (max_y - min_y).max(1.0)
        },
        "nodes": node_entries,
    }))
}

/// Add an angled construction line (infinite guide) through a point at any angle.
pub async fn add_construction_line(state: &AppState, args: AddConstructionLineArgs) -> ToolResult {
    tracing::debug!("tool: add_construction_line");
    use photonic_core::document::{Guide, GuideOrientation};

    let color = if let Some(hex) = &args.color {
        let h = hex.trim_start_matches('#');
        if h.len() >= 6 {
            let r = u8::from_str_radix(&h[0..2], 16).unwrap_or(255) as f32 / 255.0;
            let g = u8::from_str_radix(&h[2..4], 16).unwrap_or(128) as f32 / 255.0;
            let b = u8::from_str_radix(&h[4..6], 16).unwrap_or(0) as f32 / 255.0;
            let a = if h.len() >= 8 {
                u8::from_str_radix(&h[6..8], 16).unwrap_or(255) as f32 / 255.0
            } else {
                0.85
            };
            Some([r, g, b, a])
        } else {
            Some([1.0, 0.5, 0.0, 0.85])
        } // orange default
    } else {
        Some([1.0, 0.5, 0.0, 0.85])
    };

    let mut guide = Guide::new(GuideOrientation::Horizontal, 0.0);
    guide.color = color;
    guide.angle_degrees = Some(args.angle_degrees);
    guide.position_x = args.x;
    guide.position_y = args.y;

    let id = guide.id;
    let mut doc = state.document.lock().await;
    doc.guides.push(guide);

    ToolResult::text(format!(
        "Added construction line at ({:.1}, {:.1}) angle={:.1}°.",
        args.x, args.y, args.angle_degrees
    ))
    .with_data(json!({ "id": id.to_string(), "x": args.x, "y": args.y, "angle_degrees": args.angle_degrees }))
}

/// Set the document bleed and/or slug margins for print production.
pub async fn set_document_bleed(state: &AppState, args: SetDocumentBleedArgs) -> ToolResult {
    tracing::debug!("tool: set_document_bleed");
    let mut doc = state.document.lock().await;

    if let Some(b) = args.bleed_mm {
        if b < 0.0 {
            return ToolResult::error("bleed_mm must be >= 0.");
        }
        doc.bleed_mm = b;
    }
    if let Some(s) = args.slug_mm {
        if s < 0.0 {
            return ToolResult::error("slug_mm must be >= 0.");
        }
        doc.slug_mm = s;
    }

    ToolResult::text(format!(
        "Document print settings: bleed={:.3} mm, slug={:.3} mm.",
        doc.bleed_mm, doc.slug_mm
    ))
    .with_data(json!({ "bleed_mm": doc.bleed_mm, "slug_mm": doc.slug_mm }))
}

/// Return the current document bleed and slug values.
pub async fn get_document_bleed(state: &AppState) -> ToolResult {
    tracing::debug!("tool: get_document_bleed");
    let doc = state.document.lock().await;
    ToolResult::text(format!(
        "Bleed: {:.3} mm, Slug: {:.3} mm.",
        doc.bleed_mm, doc.slug_mm
    ))
    .with_data(json!({ "bleed_mm": doc.bleed_mm, "slug_mm": doc.slug_mm }))
}

/// Set the document color mode (rgb or cmyk).
pub async fn set_document_color_mode(
    state: &AppState,
    args: SetDocumentColorModeArgs,
) -> ToolResult {
    tracing::debug!("tool: set_document_color_mode");
    let mode_str = match args.mode.as_deref() {
        Some(m) => m,
        None => return ToolResult::error("mode is required"),
    };
    let color_mode = match mode_str {
        "rgb" => photonic_core::document::ColorMode::Rgb,
        "cmyk" => photonic_core::document::ColorMode::Cmyk,
        other => return ToolResult::error(format!("mode must be 'rgb' or 'cmyk', got '{other}'")),
    };
    let mut doc = state.document.lock().await;
    doc.color_mode = color_mode;
    let mode_label = match doc.color_mode {
        photonic_core::document::ColorMode::Rgb => "rgb",
        photonic_core::document::ColorMode::Cmyk => "cmyk",
    };
    ToolResult::text(format!("Document color mode set to '{mode_label}'."))
        .with_data(json!({ "color_mode": mode_label }))
}

/// Return the current document color mode.
pub async fn get_document_color_mode(state: &AppState) -> ToolResult {
    tracing::debug!("tool: get_document_color_mode");
    let doc = state.document.lock().await;
    let mode_label = match doc.color_mode {
        photonic_core::document::ColorMode::Rgb => "rgb",
        photonic_core::document::ColorMode::Cmyk => "cmyk",
    };
    ToolResult::text(format!("Document color mode: '{mode_label}'."))
        .with_data(json!({ "color_mode": mode_label }))
}

/// Set the document resolution (DPI). Controls the physical size the document's
/// pixel dimensions map to on export: physical size = px / dpi × 72 pt. Presets
/// set this (e.g. 300 for print); the default is 72 (px ≡ pt).
pub async fn set_document_dpi(state: &AppState, args: SetDocumentDpiArgs) -> ToolResult {
    tracing::debug!("tool: set_document_dpi");
    let dpi = match args.dpi {
        Some(d) if d > 0.0 && d.is_finite() => d,
        Some(d) => return ToolResult::error(format!("dpi must be a positive number, got {d}")),
        None => return ToolResult::error("dpi is required"),
    };
    let mut doc = state.document.lock().await;
    doc.dpi = dpi;
    // Physical page size at this DPI (px → pt via ×72/dpi), reported for clarity.
    let w_pt = doc.width * 72.0 / dpi;
    let h_pt = doc.height * 72.0 / dpi;
    ToolResult::text(format!(
        "Document DPI set to {dpi} — {}×{} px is {:.2}×{:.2} pt ({:.3}×{:.3} in) on export",
        doc.width as u32,
        doc.height as u32,
        w_pt,
        h_pt,
        w_pt / 72.0,
        h_pt / 72.0,
    ))
    .with_data(json!({
        "dpi": dpi,
        "page_pt": { "width": w_pt, "height": h_pt },
        "page_in": { "width": w_pt / 72.0, "height": h_pt / 72.0 },
    }))
}

/// Return the current document DPI plus the physical page size it implies.
pub async fn get_document_dpi(state: &AppState) -> ToolResult {
    tracing::debug!("tool: get_document_dpi");
    let doc = state.document.lock().await;
    let w_pt = doc.width * 72.0 / doc.dpi;
    let h_pt = doc.height * 72.0 / doc.dpi;
    ToolResult::text(format!(
        "Document DPI: {} — {}×{} px is {:.2}×{:.2} pt ({:.3}×{:.3} in) on export",
        doc.dpi,
        doc.width as u32,
        doc.height as u32,
        w_pt,
        h_pt,
        w_pt / 72.0,
        h_pt / 72.0,
    ))
    .with_data(json!({
        "dpi": doc.dpi,
        "page_pt": { "width": w_pt, "height": h_pt },
        "page_in": { "width": w_pt / 72.0, "height": h_pt / 72.0 },
    }))
}

/// Set the artboard safe-area margins (top/right/bottom/left in document units).
pub async fn set_artboard_margins(state: &AppState, args: SetArtboardMarginsArgs) -> ToolResult {
    tracing::debug!("tool: set_artboard_margins");
    let mut doc = state.document.lock().await;

    if let Some(v) = args.top {
        if v < 0.0 {
            return ToolResult::error("top margin must be >= 0");
        }
        doc.margin_top = v;
    }
    if let Some(v) = args.right {
        if v < 0.0 {
            return ToolResult::error("right margin must be >= 0");
        }
        doc.margin_right = v;
    }
    if let Some(v) = args.bottom {
        if v < 0.0 {
            return ToolResult::error("bottom margin must be >= 0");
        }
        doc.margin_bottom = v;
    }
    if let Some(v) = args.left {
        if v < 0.0 {
            return ToolResult::error("left margin must be >= 0");
        }
        doc.margin_left = v;
    }

    ToolResult::text(format!(
        "Artboard margins set — top: {:.1}, right: {:.1}, bottom: {:.1}, left: {:.1}.",
        doc.margin_top, doc.margin_right, doc.margin_bottom, doc.margin_left
    ))
    .with_data(json!({
        "top": doc.margin_top, "right": doc.margin_right,
        "bottom": doc.margin_bottom, "left": doc.margin_left
    }))
}

/// Return the current artboard safe-area margin values.
pub async fn get_artboard_margins(state: &AppState) -> ToolResult {
    tracing::debug!("tool: get_artboard_margins");
    let doc = state.document.lock().await;
    ToolResult::text(format!(
        "Artboard margins — top: {:.1}, right: {:.1}, bottom: {:.1}, left: {:.1}.",
        doc.margin_top, doc.margin_right, doc.margin_bottom, doc.margin_left
    ))
    .with_data(json!({
        "top": doc.margin_top, "right": doc.margin_right,
        "bottom": doc.margin_bottom, "left": doc.margin_left
    }))
}

/// Return the most recent edit history entries from the undo stack.
pub async fn list_history(state: &AppState, args: ListHistoryArgs) -> ToolResult {
    tracing::debug!("tool: list_history");
    let limit = args.limit.unwrap_or(20).min(200);
    let history = state.history.lock().await;
    let entries = history.history_entries(limit);
    let total = history.undo_depth();
    drop(history);

    let items: Vec<serde_json::Value> = entries
        .iter()
        .map(|(step, desc)| json!({ "step": step, "description": desc }))
        .collect();

    let summary = if items.is_empty() {
        "No edit history — document hasn't been modified yet.".to_string()
    } else {
        format!("Last {} of {} total edit(s):", items.len(), total)
    };

    ToolResult::text(summary)
        .with_data(json!({ "total": total, "returned": items.len(), "entries": items }))
}

/// Jump to a specific position in the undo/redo history.
/// index=0 is the empty-document state; index=undo_depth() is the current state.
pub async fn jump_to_history(state: &AppState, args: JumpToHistoryArgs) -> ToolResult {
    tracing::debug!("tool: jump_to_history index={}", args.index);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let current = history.undo_depth();
    let max_index = current + history.redo_depth();
    let target = args.index.min(max_index);

    if target == current {
        return ToolResult::text(format!("Already at history index {} (no change).", current))
            .with_data(serde_json::json!({ "index": current, "total": max_index, "moved": 0 }));
    }

    let mut moved: isize = 0;
    if target < current {
        // Undo (current - target) times
        let steps = current - target;
        for _ in 0..steps {
            if !history.undo(&mut doc) {
                break;
            }
            moved -= 1;
        }
    } else {
        // Redo (target - current) times
        let steps = target - current;
        for _ in 0..steps {
            if !history.redo(&mut doc) {
                break;
            }
            moved += 1;
        }
    }

    let new_depth = history.undo_depth();
    ToolResult::text(format!(
        "Jumped from index {} to {} ({:+} step(s)).",
        current, new_depth, moved
    ))
    .with_data(serde_json::json!({
        "from": current,
        "to": new_depth,
        "moved": moved,
        "total": max_index,
    }))
}

/// Scale and position nodes to fill the artboard safe area (artboard minus margins).
pub async fn fit_to_margins(state: &AppState, args: FitToMarginsArgs) -> ToolResult {
    use photonic_core::history::Command;
    tracing::debug!("tool: fit_to_margins");

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    // Compute the safe area
    let safe_x = doc.margin_left + args.padding;
    let safe_y = doc.margin_top + args.padding;
    let safe_w = doc.width - doc.margin_left - doc.margin_right - args.padding * 2.0;
    let safe_h = doc.height - doc.margin_top - doc.margin_bottom - args.padding * 2.0;

    if safe_w <= 0.0 || safe_h <= 0.0 {
        return ToolResult::error("Margins + padding exceed artboard size; safe area is empty.");
    }

    // Collect target node IDs
    let target_ids: Vec<photonic_core::node::NodeId> = if args.node_ids.is_empty() {
        doc.nodes.keys().copied().collect()
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

    if target_ids.is_empty() {
        return ToolResult::error("No target nodes found.");
    }

    // Compute the union bounding box of all targets
    let mut union_x0 = f64::MAX;
    let mut union_y0 = f64::MAX;
    let mut union_x1 = f64::MIN;
    let mut union_y1 = f64::MIN;
    let mut valid_ids: Vec<photonic_core::node::NodeId> = Vec::new();

    for nid in &target_ids {
        if let Some(node) = doc.nodes.get(nid) {
            if let Some([x, y, width, height]) = world_aabb(node) {
                union_x0 = union_x0.min(x);
                union_y0 = union_y0.min(y);
                union_x1 = union_x1.max(x + width);
                union_y1 = union_y1.max(y + height);
                valid_ids.push(*nid);
            }
        }
    }

    if valid_ids.is_empty() || union_x0 >= union_x1 || union_y0 >= union_y1 {
        return ToolResult::error("No nodes with valid bounds found.");
    }

    let content_w = union_x1 - union_x0;
    let content_h = union_y1 - union_y0;

    // Compute scale factor
    let scale = if args.uniform {
        (safe_w / content_w).min(safe_h / content_h)
    } else {
        1.0 // non-uniform handled per-axis below
    };

    let scale_x = if args.uniform {
        scale
    } else {
        safe_w / content_w
    };
    let scale_y = if args.uniform {
        scale
    } else {
        safe_h / content_h
    };

    // Center the scaled content in the safe area
    let target_cx = safe_x + safe_w / 2.0;
    let target_cy = safe_y + safe_h / 2.0;

    let content_cx = (union_x0 + union_x1) / 2.0;
    let content_cy = (union_y0 + union_y1) / 2.0;

    let mut cmds: Vec<Command> = Vec::new();
    for nid in &valid_ids {
        if let Some(node) = doc.nodes.get(nid) {
            let tx = node.transform.matrix[4];
            let ty = node.transform.matrix[5];
            // New position: shift from content center to target center, apply scale
            let new_tx = target_cx + (tx - content_cx) * scale_x;
            let new_ty = target_cy + (ty - content_cy) * scale_y;
            let mut new_node = node.clone();
            new_node.transform.matrix[4] = new_tx;
            new_node.transform.matrix[5] = new_ty;
            // Scale every linear term so rotation and shear are preserved.
            new_node.transform.matrix[0] *= scale_x;
            new_node.transform.matrix[2] *= scale_x;
            new_node.transform.matrix[1] *= scale_y;
            new_node.transform.matrix[3] *= scale_y;
            cmds.push(Command::UpdateNode {
                old: node.clone(),
                new: new_node,
            });
        }
    }

    if cmds.is_empty() {
        return ToolResult::error("No changes to apply.");
    }

    let moved = cmds.len();
    history.execute_discrete(Command::Batch(cmds), &mut doc);

    ToolResult::text(format!(
        "Fitted {} node(s) to safe area ({:.1}×{:.1}) with scale ×{:.3}.",
        moved, safe_w, safe_h, scale_x
    ))
    .with_data(serde_json::json!({
        "nodes_fitted": moved,
        "safe_area": { "x": safe_x, "y": safe_y, "w": safe_w, "h": safe_h },
        "scale_x": (scale_x * 1000.0).round() / 1000.0,
        "scale_y": (scale_y * 1000.0).round() / 1000.0,
    }))
}

// ─── Dimension Annotations ────────────────────────────────────────────────────

fn node_center(doc: &photonic_core::Document, id_str: &str) -> Option<(f64, f64)> {
    let uid = uuid::Uuid::parse_str(id_str)
        .ok()
        .or_else(|| doc.find_node_by_name(id_str).map(|n| n.id))?;
    let node = doc.nodes.get(&uid)?;
    if let Some(lb) = node.local_bounds() {
        let (x0, y0) = node.transform.apply(lb.x0, lb.y0);
        let (x1, y1) = node.transform.apply(lb.x1, lb.y1);
        Some(((x0 + x1) / 2.0, (y0 + y1) / 2.0))
    } else {
        let (wx, wy) = node.transform.apply(0.0, 0.0);
        Some((wx, wy))
    }
}

/// Add a dimension annotation showing the distance between two nodes.
pub async fn add_dimension(state: &AppState, args: AddDimensionArgs) -> ToolResult {
    tracing::debug!("tool: add_dimension");

    let mut doc = state.document.lock().await;

    let (from_x, from_y) = match node_center(&doc, &args.from_node_id) {
        Some(c) => c,
        None => return ToolResult::error(format!("Node '{}' not found.", args.from_node_id)),
    };
    let (to_x, to_y) = match node_center(&doc, &args.to_node_id) {
        Some(c) => c,
        None => return ToolResult::error(format!("Node '{}' not found.", args.to_node_id)),
    };

    let from_uid = uuid::Uuid::parse_str(&args.from_node_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.from_node_id).map(|n| n.id))
        .unwrap();
    let to_uid = uuid::Uuid::parse_str(&args.to_node_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.to_node_id).map(|n| n.id))
        .unwrap();

    let axis = args.axis.unwrap_or_else(|| "diagonal".to_string());
    let label_offset = args.label_offset.unwrap_or(20.0);

    let dim = photonic_core::DimensionAnnotation::new(
        from_uid,
        to_uid,
        axis.clone(),
        label_offset,
        from_x,
        from_y,
        to_x,
        to_y,
    );
    let distance = dim.distance();
    let dim_id = dim.id;
    doc.dimensions.push(dim);

    ToolResult::text(format!(
        "Added {} dimension: {:.1} units between nodes.",
        axis, distance
    ))
    .with_data(serde_json::json!({
        "id": dim_id.to_string(),
        "axis": axis,
        "distance": (distance * 10.0).round() / 10.0,
        "from": [from_x, from_y],
        "to": [to_x, to_y],
    }))
}

/// List all dimension annotations in the document.
pub async fn list_dimensions(state: &AppState) -> ToolResult {
    tracing::debug!("tool: list_dimensions");
    let doc = state.document.lock().await;

    let items: Vec<serde_json::Value> = doc
        .dimensions
        .iter()
        .map(|d| {
            serde_json::json!({
                "id": d.id.to_string(),
                "from_node": d.from_node.to_string(),
                "to_node": d.to_node.to_string(),
                "axis": d.axis,
                "distance": (d.distance() * 10.0).round() / 10.0,
                "label_offset": d.label_offset,
                "from": [d.from_x, d.from_y],
                "to": [d.to_x, d.to_y],
            })
        })
        .collect();

    let count = items.len();
    ToolResult::text(format!("{} dimension annotation(s).", count))
        .with_data(serde_json::json!({ "dimensions": items, "count": count }))
}

/// Remove a dimension annotation by ID.
pub async fn remove_dimension(state: &AppState, args: RemoveDimensionArgs) -> ToolResult {
    tracing::debug!("tool: remove_dimension id={}", args.id);
    let id = match uuid::Uuid::parse_str(&args.id) {
        Ok(id) => id,
        Err(_) => return ToolResult::error(format!("Invalid dimension ID: '{}'", args.id)),
    };
    let mut doc = state.document.lock().await;
    let before = doc.dimensions.len();
    doc.dimensions.retain(|d| d.id != id);
    let removed = before - doc.dimensions.len();
    if removed == 0 {
        ToolResult::error(format!("Dimension '{}' not found.", args.id))
    } else {
        ToolResult::text(format!("Removed dimension '{}'.", args.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::doc_analysis::{analyze_composition, detect_rhythms, measure_distances};
    use crate::protocol::{
        AnalyzeCompositionArgs, ContentItem, DetectRhythmsArgs, FitToMarginsArgs,
        MeasureDistancesArgs,
    };
    use crate::server::McpServerConfig;
    use photonic_core::{
        node::{PathNode, SceneNode, SceneNodeKind},
        path::PathData,
        transform::Transform,
        AuditLog, Document,
    };
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::Mutex;

    fn test_state(width: f64, height: f64) -> AppState {
        let (tx, _rx) = std::sync::mpsc::channel();
        AppState {
            document: Arc::new(Mutex::new(Document::new("spatial test", width, height))),
            history: Arc::new(Mutex::new(photonic_core::history::CommandHistory::new(100))),
            document_path: Arc::new(StdMutex::new(None)),
            capture_tx: Arc::new(StdMutex::new(tx)),
            config: McpServerConfig::default(),
            audit_log: Arc::new(StdMutex::new(AuditLog::new())),
            clipboard_ring: Arc::new(crate::handlers::clipboard::new_clipboard_ring()),
        }
    }

    async fn add_rect(state: &AppState, name: &str, transform: Transform) -> uuid::Uuid {
        let mut doc = state.document.lock().await;
        let layer_id = doc.active_layer_id.expect("default layer");
        let node = SceneNode::new(
            name,
            layer_id,
            SceneNodeKind::Path(PathNode::new(PathData::rect(0.0, 0.0, 100.0, 50.0))),
        )
        .with_transform(transform);
        doc.add_node(node, Some(layer_id))
    }

    fn rotated_rect(tx: f64, ty: f64) -> Transform {
        let angle = std::f64::consts::FRAC_PI_4;
        Transform::new(angle.cos(), angle.sin(), -angle.sin(), angle.cos(), tx, ty)
    }

    fn json_data(result: &ToolResult) -> serde_json::Value {
        let Some(ContentItem::Text { text }) = result.content.last() else {
            panic!("tool result did not contain JSON text: {result:?}");
        };
        serde_json::from_str(text).expect("tool result JSON")
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-8, "{actual} != {expected}");
    }

    #[tokio::test]
    async fn overview_and_composition_use_full_affine_bounds() {
        let state = test_state(200.0, 200.0);
        add_rect(&state, "rotated", rotated_rect(40.0, 30.0)).await;

        let overview = get_canvas_overview(&state, GetCanvasOverviewArgs::default()).await;
        assert!(overview.is_error.is_none(), "{overview:?}");
        let overview_data = json_data(&overview);
        let bounds = &overview_data["nodes"][0]["bounds"];
        assert_close(bounds["x"].as_f64().unwrap(), 40.0 - 25.0 * 2.0_f64.sqrt());
        assert_close(bounds["y"].as_f64().unwrap(), 30.0);
        assert_close(bounds["w"].as_f64().unwrap(), 75.0 * 2.0_f64.sqrt());
        assert_close(bounds["h"].as_f64().unwrap(), 75.0 * 2.0_f64.sqrt());

        let analysis = analyze_composition(&state, AnalyzeCompositionArgs::default()).await;
        assert!(analysis.is_error.is_none(), "{analysis:?}");
        let analysis_data = json_data(&analysis);
        assert_close(analysis_data["canvas_coverage_pct"].as_f64().unwrap(), 28.1);
    }

    #[tokio::test]
    async fn rhythms_and_distances_use_full_affine_bounds() {
        let state = test_state(600.0, 300.0);
        let first = add_rect(&state, "first", rotated_rect(0.0, 0.0)).await;
        let second = add_rect(&state, "second", rotated_rect(200.0, 0.0)).await;
        add_rect(&state, "third", rotated_rect(400.0, 0.0)).await;

        let rhythms = detect_rhythms(
            &state,
            DetectRhythmsArgs {
                node_ids: Vec::new(),
                min_count: Some(3),
            },
        )
        .await;
        assert!(rhythms.is_error.is_none(), "{rhythms:?}");
        let rhythms_data = json_data(&rhythms);
        let uniform_width = rhythms_data["patterns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|pattern| pattern["type"] == "uniform_width")
            .expect("uniform-width rhythm");
        assert_close(uniform_width["width_px"].as_f64().unwrap(), 106.1);

        let distances = measure_distances(
            &state,
            MeasureDistancesArgs {
                node_ids: vec![first.to_string(), second.to_string()],
            },
        )
        .await;
        assert!(distances.is_error.is_none(), "{distances:?}");
        let distances_data = json_data(&distances);
        assert_close(
            distances_data["measurements"][0]["horizontal_gap_px"]
                .as_f64()
                .unwrap(),
            ((200.0 - 75.0 * 2.0_f64.sqrt()) * 10.0).round() / 10.0,
        );
    }

    #[tokio::test]
    async fn fit_to_margins_keeps_rotated_aabb_inside_safe_area() {
        let state = test_state(200.0, 300.0);
        let id = add_rect(&state, "rotated", rotated_rect(0.0, 0.0)).await;
        {
            let mut doc = state.document.lock().await;
            doc.margin_left = 50.0;
            doc.margin_right = 50.0;
            doc.margin_top = 50.0;
            doc.margin_bottom = 50.0;
        }

        let result = fit_to_margins(
            &state,
            FitToMarginsArgs {
                node_ids: vec![id.to_string()],
                uniform: true,
                padding: 0.0,
            },
        )
        .await;
        assert!(result.is_error.is_none(), "{result:?}");

        let doc = state.document.lock().await;
        let bounds = world_aabb(doc.nodes.get(&id).unwrap()).unwrap();
        for (actual, expected) in bounds.into_iter().zip([50.0, 100.0, 100.0, 100.0]) {
            assert_close(actual, expected);
        }
    }
}
