use crate::handlers::nodes::{color_distance, node_world_aabb, point_in_polygon, solid_fill_color};
use crate::protocol::*;
use crate::server::AppState;
use kurbo;
use photonic_core::{
    history::Command,
    node::{NodeId, SceneNode, SceneNodeKind},
};

pub async fn find_nodes(state: &AppState, args: FindNodesArgs) -> ToolResult {
    tracing::debug!("tool: find_nodes");
    let doc = state.document.lock().await;

    let limit = args.limit.unwrap_or(200).max(1);
    let visible_only = args.visible_only.unwrap_or(false);
    let include_details = args.include_details.unwrap_or(false);
    let name_lower = args.name_contains.as_deref().map(|s| s.to_lowercase());

    let region_rect: Option<kurbo::Rect> = args
        .in_region
        .as_ref()
        .map(|r| kurbo::Rect::new(r.x, r.y, r.x + r.width, r.y + r.height));

    let mut matched: Vec<serde_json::Value> = Vec::new();
    let mut truncated = false;

    'outer: for node in doc.nodes.values() {
        if visible_only && !node.visible {
            continue;
        }

        if let Some(lid) = args.layer_id {
            if node.layer_id != lid {
                continue;
            }
        }

        if let Some(ref nt) = args.node_type {
            let kind_str = match &node.kind {
                SceneNodeKind::Path(_) => "path",
                SceneNodeKind::Group(_) => "group",
                SceneNodeKind::Text(_) => "text",
                SceneNodeKind::Raster(_) => "raster",
            };
            if kind_str != nt.as_str() {
                continue;
            }
        }

        if let Some(ref required) = args.tags {
            if !required.iter().all(|t| node.tags.contains(t)) {
                continue;
            }
        }

        if let Some(ref any) = args.tags_any {
            if !any.is_empty() && !any.iter().any(|t| node.tags.contains(t)) {
                continue;
            }
        }

        if let Some(ref needle) = name_lower {
            if !node.name.to_lowercase().contains(needle.as_str()) {
                continue;
            }
        }

        // Spatial filter: groups/text have no local_bounds → always pass.
        if let Some(filter_rect) = region_rect {
            if let Some(lb) = node.local_bounds() {
                let t = &node.transform;
                let corners = [
                    t.apply(lb.x0, lb.y0),
                    t.apply(lb.x1, lb.y0),
                    t.apply(lb.x0, lb.y1),
                    t.apply(lb.x1, lb.y1),
                ];
                let wx0 = corners
                    .iter()
                    .map(|(x, _)| *x)
                    .fold(f64::INFINITY, f64::min);
                let wy0 = corners
                    .iter()
                    .map(|(_, y)| *y)
                    .fold(f64::INFINITY, f64::min);
                let wx1 = corners
                    .iter()
                    .map(|(x, _)| *x)
                    .fold(f64::NEG_INFINITY, f64::max);
                let wy1 = corners
                    .iter()
                    .map(|(_, y)| *y)
                    .fold(f64::NEG_INFINITY, f64::max);
                let no_overlap = wx1 < filter_rect.x0
                    || wx0 > filter_rect.x1
                    || wy1 < filter_rect.y0
                    || wy0 > filter_rect.y1;
                if no_overlap {
                    continue;
                }
            }
        }

        let entry = if include_details {
            serde_json::to_value(node).unwrap_or_default()
        } else {
            let kind_str = match &node.kind {
                SceneNodeKind::Path(_) => "path",
                SceneNodeKind::Group(_) => "group",
                SceneNodeKind::Text(_) => "text",
                SceneNodeKind::Raster(_) => "raster",
            };
            serde_json::json!({
                "id":       node.id,
                "name":     node.name,
                "type":     kind_str,
                "tags":     node.tags,
                "layer_id": node.layer_id,
                "visible":  node.visible,
            })
        };
        matched.push(entry);

        if matched.len() >= limit {
            truncated = true;
            break 'outer;
        }
    }

    let count = matched.len();
    ToolResult::text(format!(
        "Found {} node(s){}",
        count,
        if truncated {
            " (results truncated)"
        } else {
            ""
        }
    ))
    .with_data(serde_json::json!({
        "nodes":     matched,
        "count":     count,
        "truncated": truncated,
    }))
}
/// Find nodes by fill or stroke color and replace those colors — plus
/// optionally node-level opacity — in a single undoable batch.
///
/// This is the "Find & Replace" for color. It eliminates the common
/// AI-agent pattern of: get_document_state → iterate nodes → call
/// update_node for each match (N round-trips, N undo steps).  A single
/// `find_replace_style` call handles the entire document in one step.
///
/// It is equally useful for humans doing brand refreshes: swap every
/// instance of a brand color across the whole file without touching
/// anything else.
///
/// Gradient support: matching checks solid fills *and* individual stop /
/// control-point colors inside linear, radial, fluid, and mesh gradients.
/// Only the matching colors within each gradient are replaced; unmatched
/// stops are left untouched.
///
/// `dry_run: true` returns a preview of what would change without mutating.
pub async fn find_replace_style(state: &AppState, args: FindReplaceStyleArgs) -> ToolResult {
    use photonic_core::color::Color;
    use photonic_core::style::FillKind;

    // ── 1. Parse search colors ────────────────────────────────────────────────
    let find_fill: Option<Color> = match &args.fill_color {
        Some(hex) => match Color::from_hex(hex) {
            Some(c) => Some(c),
            None => return ToolResult::error(format!("Invalid fill_color: '{}'", hex)),
        },
        None => None,
    };

    let find_stroke: Option<Color> = match &args.stroke_color {
        Some(hex) => match Color::from_hex(hex) {
            Some(c) => Some(c),
            None => return ToolResult::error(format!("Invalid stroke_color: '{}'", hex)),
        },
        None => None,
    };

    if find_fill.is_none()
        && find_stroke.is_none()
        && args.stroke_width.is_none()
        && args.font_family.is_none()
    {
        return ToolResult::error(
            "At least one search criterion must be specified: fill_color, stroke_color, stroke_width, or font_family",
        );
    }

    if args.new_fill_color.is_none()
        && args.new_stroke_color.is_none()
        && args.new_opacity.is_none()
        && args.new_stroke_width.is_none()
        && args.new_font_family.is_none()
    {
        return ToolResult::error(
            "At least one replacement must be specified: new_fill_color, new_stroke_color, new_opacity, new_stroke_width, or new_font_family",
        );
    }

    // ── 2. Parse replacement colors ───────────────────────────────────────────
    let new_fill: Option<Color> = match &args.new_fill_color {
        Some(hex) => match Color::from_hex(hex) {
            Some(c) => Some(c),
            None => return ToolResult::error(format!("Invalid new_fill_color: '{}'", hex)),
        },
        None => None,
    };

    let new_stroke: Option<Color> = match &args.new_stroke_color {
        Some(hex) => match Color::from_hex(hex) {
            Some(c) => Some(c),
            None => return ToolResult::error(format!("Invalid new_stroke_color: '{}'", hex)),
        },
        None => None,
    };

    let tolerance = args.color_tolerance.unwrap_or(0.0).clamp(0.0, 1.0);

    // Width tolerance: fractional — tolerance=0.1 means ±10% of the target value.
    // When tolerance=0.0 we use a tiny epsilon to handle f64 round-trips cleanly.
    let width_tolerance_abs = |target: f64| -> f64 {
        if tolerance == 0.0 {
            1e-9
        } else {
            target * (tolerance as f64)
        }
    };

    // ── 3. Color distance helper (normalized to [0, 1]) ───────────────────────
    // Euclidean distance in linear RGB divided by √3 (the maximum possible distance).
    let color_near = |a: Color, b: Color| -> bool {
        let dr = a.r - b.r;
        let dg = a.g - b.g;
        let db = a.b - b.b;
        let dist = ((dr * dr + dg * dg + db * db) / 3.0_f32).sqrt();
        dist <= tolerance
    };

    // ── 4. Collect candidate nodes ────────────────────────────────────────────
    let candidates: Vec<SceneNode> = {
        let doc = state.document.lock().await;
        match &args.node_ids {
            Some(ids) => ids
                .iter()
                .filter_map(|id| doc.nodes.get(id).cloned())
                .collect(),
            None => doc
                .nodes
                .values()
                .filter(|n| args.layer_id.is_none_or(|lid| n.layer_id == lid))
                .cloned()
                .collect(),
        }
    };

    // ── 5. Match and build replacements ──────────────────────────────────────
    let mut commands: Vec<Command> = Vec::new();
    let mut changed: Vec<serde_json::Value> = Vec::new();

    for node in &candidates {
        let mut new_node = node.clone();
        let mut changes: Vec<String> = Vec::new();
        let mut fill_matched = false;
        let mut stroke_matched = false;
        let mut width_matched = false;
        let mut font_matched = false;

        match &mut new_node.kind {
            SceneNodeKind::Path(path) => {
                // Fill search
                if let Some(target) = find_fill {
                    match &mut path.fill.kind {
                        FillKind::Solid(c) if color_near(*c, target) => {
                            fill_matched = true;
                            if let Some(nc) = new_fill {
                                changes.push(format!("fill: {} → {}", c.to_hex(), nc.to_hex()));
                                *c = nc;
                            }
                        }
                        FillKind::Gradient(g) => {
                            for stop in &mut g.stops {
                                if color_near(stop.color, target) {
                                    fill_matched = true;
                                    if let Some(nc) = new_fill {
                                        changes.push(format!(
                                            "gradient stop @{:.0}%: {} → {}",
                                            stop.offset * 100.0,
                                            stop.color.to_hex(),
                                            nc.to_hex()
                                        ));
                                        stop.color = nc;
                                    }
                                }
                            }
                        }
                        FillKind::FluidGradient(fg) => {
                            for pt in &mut fg.points {
                                if color_near(pt.color, target) {
                                    fill_matched = true;
                                    if let Some(nc) = new_fill {
                                        changes.push(format!(
                                            "fluid point ({:.0},{:.0}): {} → {}",
                                            pt.x,
                                            pt.y,
                                            pt.color.to_hex(),
                                            nc.to_hex()
                                        ));
                                        pt.color = nc;
                                    }
                                }
                            }
                        }
                        FillKind::MeshGradient(mg) => {
                            for (i, c) in mg.cell_colors.iter_mut().enumerate() {
                                if color_near(*c, target) {
                                    fill_matched = true;
                                    if let Some(nc) = new_fill {
                                        changes.push(format!(
                                            "mesh cell {}: {} → {}",
                                            i,
                                            c.to_hex(),
                                            nc.to_hex()
                                        ));
                                        *c = nc;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }

                // Stroke color search
                if let Some(target) = find_stroke {
                    if path.stroke.enabled && color_near(path.stroke.color, target) {
                        stroke_matched = true;
                        if let Some(nc) = new_stroke {
                            changes.push(format!(
                                "stroke: {} → {}",
                                path.stroke.color.to_hex(),
                                nc.to_hex()
                            ));
                            path.stroke.color = nc;
                        }
                    }
                }

                // Stroke width search
                if let Some(target_w) = args.stroke_width {
                    if path.stroke.enabled
                        && (path.stroke.width - target_w).abs() <= width_tolerance_abs(target_w)
                    {
                        width_matched = true;
                        if let Some(nw) = args.new_stroke_width {
                            changes.push(format!("stroke-width: {} → {}", path.stroke.width, nw));
                            path.stroke.width = nw;
                        }
                    }
                }
            }

            SceneNodeKind::Text(text) => {
                // Text nodes carry their own fill and stroke
                if let Some(target) = find_fill {
                    if let FillKind::Solid(c) = &mut text.fill.kind {
                        if color_near(*c, target) {
                            fill_matched = true;
                            if let Some(nc) = new_fill {
                                changes.push(format!("fill: {} → {}", c.to_hex(), nc.to_hex()));
                                *c = nc;
                            }
                        }
                    }
                }
                if let Some(target) = find_stroke {
                    if text.stroke.enabled && color_near(text.stroke.color, target) {
                        stroke_matched = true;
                        if let Some(nc) = new_stroke {
                            changes.push(format!(
                                "stroke: {} → {}",
                                text.stroke.color.to_hex(),
                                nc.to_hex()
                            ));
                            text.stroke.color = nc;
                        }
                    }
                }

                // Stroke width search on text
                if let Some(target_w) = args.stroke_width {
                    if text.stroke.enabled
                        && (text.stroke.width - target_w).abs() <= width_tolerance_abs(target_w)
                    {
                        width_matched = true;
                        if let Some(nw) = args.new_stroke_width {
                            changes.push(format!("stroke-width: {} → {}", text.stroke.width, nw));
                            text.stroke.width = nw;
                        }
                    }
                }

                // Font family search (text nodes only)
                if let Some(ref target_ff) = args.font_family {
                    if text.font_family.to_lowercase() == target_ff.to_lowercase() {
                        font_matched = true;
                        if let Some(ref nff) = args.new_font_family {
                            changes.push(format!("font-family: {} → {}", text.font_family, nff));
                            text.font_family = nff.clone();
                        }
                    }
                }
            }

            SceneNodeKind::Group(_) => {
                // Groups carry no direct fill/stroke — skip style matching.
            }
            // raster: no vector fill/stroke/font to match
            SceneNodeKind::Raster(_) => {}
        }

        // Node-level opacity override applied to any matched node.
        let any_matched = fill_matched || stroke_matched || width_matched || font_matched;
        if any_matched {
            if let Some(new_op) = args.new_opacity {
                let new_op = new_op.clamp(0.0, 1.0);
                if (new_node.opacity - new_op).abs() > 1e-4 {
                    changes.push(format!("opacity: {:.2} → {:.2}", node.opacity, new_op));
                    new_node.opacity = new_op;
                }
            }
        }

        if !changes.is_empty() {
            changed.push(serde_json::json!({
                "node_id": node.id,
                "name": node.name,
                "changes": changes,
            }));
            if !args.dry_run {
                commands.push(Command::UpdateNode {
                    old: node.clone(),
                    new: new_node,
                });
            }
        }
    }

    // ── 6. Dry-run: report without mutating ───────────────────────────────────
    if args.dry_run {
        let msg = if changed.is_empty() {
            "dry_run: no nodes match the search criteria".to_string()
        } else {
            format!("dry_run: {} node(s) would be updated", changed.len())
        };
        return ToolResult::text(msg).with_data(serde_json::json!({ "matches": changed }));
    }

    // ── 7. Execute batch (single undo step) ───────────────────────────────────
    if commands.is_empty() {
        return ToolResult::text("No nodes matched the search criteria — nothing changed")
            .with_data(serde_json::json!({ "changed": [] }));
    }

    let count = commands.len();
    {
        let mut doc = state.document.lock().await;
        let mut history = state.history.lock().await;
        history.execute_discrete(Command::Batch(commands), &mut doc);
    }

    ToolResult::text(format!("Updated {} node(s)", count))
        .with_data(serde_json::json!({ "changed": changed }))
}
/// Search and replace text content across text nodes.
pub async fn find_replace_text(state: &AppState, args: FindReplaceTextArgs) -> ToolResult {
    // 1. Build the regex pattern
    let pattern = if args.regex {
        args.find.clone()
    } else {
        regex::escape(&args.find)
    };
    let pattern = if args.case_sensitive {
        pattern
    } else {
        format!("(?i){}", pattern)
    };
    let re = match regex::Regex::new(&pattern) {
        Ok(r) => r,
        Err(e) => return ToolResult::error(format!("Invalid regex: {}", e)),
    };

    // 2. Collect candidate text nodes
    let candidates: Vec<SceneNode> = {
        let doc = state.document.lock().await;
        match &args.node_ids {
            Some(ids) => ids
                .iter()
                .filter_map(|id| doc.nodes.get(id).cloned())
                .collect(),
            None => doc
                .nodes
                .values()
                .filter(|n| matches!(n.kind, SceneNodeKind::Text(_)))
                .cloned()
                .collect(),
        }
    };

    if candidates.is_empty() {
        return ToolResult::text("No text nodes found.")
            .with_data(serde_json::json!({ "changed": [] }));
    }

    // 3. Apply replacements
    let mut commands: Vec<Command> = Vec::new();
    let mut changed: Vec<serde_json::Value> = Vec::new();

    for node in &candidates {
        if let SceneNodeKind::Text(tn) = &node.kind {
            let new_content = re
                .replace_all(&tn.content, args.replace.as_str())
                .into_owned();
            if new_content != tn.content {
                changed.push(serde_json::json!({
                    "id":          node.id,
                    "name":        node.name,
                    "old_content": tn.content,
                    "new_content": new_content,
                }));
                if !args.dry_run {
                    let mut new_node = node.clone();
                    if let SceneNodeKind::Text(ref mut new_tn) = new_node.kind {
                        new_tn.content = new_content;
                    }
                    commands.push(Command::UpdateNode {
                        old: node.clone(),
                        new: new_node,
                    });
                }
            }
        }
    }

    if changed.is_empty() {
        return ToolResult::text("No text nodes matched the search pattern.")
            .with_data(serde_json::json!({ "changed": [] }));
    }

    if args.dry_run {
        return ToolResult::text(format!(
            "dry_run: {} text node(s) would be updated.",
            changed.len()
        ))
        .with_data(serde_json::json!({ "changed": changed }));
    }

    // 4. Execute as a single undo-able batch
    let count = commands.len();
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(Command::Batch(commands), &mut doc);
    history.schedule_mcp_checkpoint(format!("Find/replace text ({} nodes)", count));

    ToolResult::text(format!("Updated {} text node(s).", count))
        .with_data(serde_json::json!({ "changed": changed }))
}
pub async fn set_selection(state: &AppState, args: SetSelectionArgs) -> ToolResult {
    tracing::debug!("tool: set_selection");

    let mut doc = state.document.lock().await;

    if !args.additive {
        doc.selection.clear();
    }

    let mut added = 0usize;
    for id_str in &args.node_ids {
        let nid = uuid::Uuid::parse_str(id_str)
            .ok()
            .or_else(|| doc.find_node_by_name(id_str).map(|n| n.id));
        if let Some(id) = nid {
            if doc.nodes.contains_key(&id) {
                doc.selection.add(id);
                added += 1;
            }
        }
    }

    let total = doc.selection.node_ids.len();
    ToolResult::text(format!("Selection: {added} added, {total} total"))
        .with_data(serde_json::json!({ "added": added, "total": total }))
}
pub async fn get_selection(state: &AppState) -> ToolResult {
    tracing::debug!("tool: get_selection");

    let doc = state.document.lock().await;
    let ids: Vec<NodeId> = doc.selection.node_ids.iter().copied().collect();
    let count = ids.len();

    let nodes_info: Vec<serde_json::Value> = ids
        .iter()
        .filter_map(|nid| {
            doc.nodes.get(nid).map(|n| {
                let kind = match &n.kind {
                    SceneNodeKind::Path(_) => "path",
                    SceneNodeKind::Text(_) => "text",
                    SceneNodeKind::Group(_) => "group",
                    SceneNodeKind::Raster(_) => "raster",
                };
                serde_json::json!({
                    "id": nid,
                    "name": n.name,
                    "kind": kind,
                    "visible": n.visible,
                    "locked": n.locked,
                })
            })
        })
        .collect();

    if count == 0 {
        ToolResult::text("Nothing selected")
            .with_data(serde_json::json!({ "count": 0, "nodes": [] }))
    } else {
        ToolResult::text(format!("{count} node(s) selected"))
            .with_data(serde_json::json!({ "count": count, "nodes": nodes_info }))
    }
}
pub async fn select_all(state: &AppState, args: SelectAllArgs) -> ToolResult {
    tracing::debug!("tool: select_all");

    let mut doc = state.document.lock().await;

    let layer_filter = args.layer_id.and_then(|s| {
        uuid::Uuid::parse_str(&s)
            .ok()
            .or_else(|| doc.layers.values().find(|l| l.name == s).map(|l| l.id))
    });

    doc.selection.clear();
    let mut count = 0usize;

    let nids: Vec<NodeId> = doc.nodes.keys().copied().collect();
    for nid in nids {
        if let Some(lid) = layer_filter {
            if let Some(node) = doc.nodes.get(&nid) {
                if node.layer_id != lid {
                    continue;
                }
            }
        }
        doc.selection.add(nid);
        count += 1;
    }

    ToolResult::text(format!("Selected {count} node(s)"))
        .with_data(serde_json::json!({ "selected": count }))
}
pub async fn deselect_all(state: &AppState, _args: DeselectAllArgs) -> ToolResult {
    tracing::debug!("tool: deselect_all");

    let mut doc = state.document.lock().await;
    let prev_count = doc.selection.node_ids.len();
    doc.selection.clear();

    ToolResult::text(format!("Deselected {prev_count} node(s)"))
        .with_data(serde_json::json!({ "deselected": prev_count }))
}
/// Select all document nodes that share a specific attribute with the reference
/// node. Updates the document's active selection and returns the matching IDs.
pub async fn select_same(state: &AppState, args: SelectSameArgs) -> ToolResult {
    let tolerance_f64 = args.tolerance.unwrap_or(0.01);
    let tolerance = tolerance_f64 as f32;
    let include_self = args.include_self.unwrap_or(true);

    let mut doc = state.document.lock().await;

    let ref_node = match doc.nodes.get(&args.node_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("reference node {} not found", args.node_id)),
    };

    let mut matched: Vec<uuid::Uuid> = Vec::new();

    for (nid, node) in &doc.nodes {
        let is_self = *nid == args.node_id;
        if is_self && !include_self {
            continue;
        }

        let matches = match args.attribute {
            SelectSameAttribute::FillColor => {
                let ref_color = solid_fill_color(&ref_node);
                let cand_color = solid_fill_color(node);
                match (ref_color, cand_color) {
                    (Some(rc), Some(cc)) => color_distance(rc, cc) <= tolerance,
                    (None, None) => true, // both have no solid fill
                    _ => false,
                }
            }
            SelectSameAttribute::StrokeColor => {
                if let (SceneNodeKind::Path(rp), SceneNodeKind::Path(cp)) =
                    (&ref_node.kind, &node.kind)
                {
                    match (rp.stroke.enabled, cp.stroke.enabled) {
                        (true, true) => {
                            color_distance(rp.stroke.color, cp.stroke.color) <= tolerance
                        }
                        (false, false) => true,
                        _ => false,
                    }
                } else {
                    false
                }
            }
            SelectSameAttribute::StrokeWeight => {
                if let (SceneNodeKind::Path(rp), SceneNodeKind::Path(cp)) =
                    (&ref_node.kind, &node.kind)
                {
                    (rp.stroke.width - cp.stroke.width).abs() <= tolerance as f64
                } else {
                    false
                }
            }
            SelectSameAttribute::Opacity => (ref_node.opacity - node.opacity).abs() <= tolerance,
            SelectSameAttribute::BlendMode => ref_node.blend_mode == node.blend_mode,
            SelectSameAttribute::ObjectType => {
                std::mem::discriminant(&ref_node.kind) == std::mem::discriminant(&node.kind)
            }
        };

        if matches {
            matched.push(*nid);
        }
    }

    // Update the document selection.
    doc.selection.clear();
    for nid in &matched {
        doc.selection.add(*nid);
    }

    let attr_label = match args.attribute {
        SelectSameAttribute::FillColor => "fill color",
        SelectSameAttribute::StrokeColor => "stroke color",
        SelectSameAttribute::StrokeWeight => "stroke weight",
        SelectSameAttribute::Opacity => "opacity",
        SelectSameAttribute::BlendMode => "blend mode",
        SelectSameAttribute::ObjectType => "object type",
    };
    let attr_key = match args.attribute {
        SelectSameAttribute::FillColor => "fill_color",
        SelectSameAttribute::StrokeColor => "stroke_color",
        SelectSameAttribute::StrokeWeight => "stroke_weight",
        SelectSameAttribute::Opacity => "opacity",
        SelectSameAttribute::BlendMode => "blend_mode",
        SelectSameAttribute::ObjectType => "object_type",
    };
    let count = matched.len();
    ToolResult::text(format!(
        "Selected {} node(s) with matching {}.",
        count, attr_label
    ))
    .with_data(serde_json::json!({
        "node_ids": matched,
        "count":    count,
        "attribute": attr_key,
    }))
}
/// Find the topmost visible node at (canvas_x, canvas_y) and select all nodes
/// that share the specified attribute with it.
pub async fn magic_wand_select(state: &AppState, args: MagicWandSelectArgs) -> ToolResult {
    let tolerance_f64 = args.tolerance.unwrap_or(0.01);
    let tolerance = tolerance_f64 as f32;
    let (cx, cy) = (args.canvas_x, args.canvas_y);

    let mut doc = state.document.lock().await;

    // ── 1. Hit-test: topmost visible unlocked node whose world AABB contains the point ─
    // Nodes are iterated front-to-back (reversed draw order) to pick the topmost.
    let ref_node_id: Option<photonic_core::node::NodeId> = {
        let ordered: Vec<_> = doc.nodes_in_draw_order().into_iter().rev().collect();
        let mut found = None;
        for node in ordered {
            if !node.visible || node.locked {
                continue;
            }
            let (bx0, by0, bx1, by1) = match node_world_aabb(node) {
                Some(b) => b,
                None => continue,
            };
            if cx >= bx0 && cx <= bx1 && cy >= by0 && cy <= by1 {
                found = Some(node.id);
                break;
            }
        }
        found
    };

    let ref_id = match ref_node_id {
        Some(id) => id,
        None => return ToolResult::error("No node found at the specified canvas coordinates"),
    };

    let ref_node = doc.nodes.get(&ref_id).cloned().unwrap();

    // ── 2. Select all nodes matching the reference attribute ─────────────────
    let mut matched: Vec<photonic_core::node::NodeId> = Vec::new();
    for (nid, node) in &doc.nodes {
        let matches = match args.attribute {
            SelectSameAttribute::FillColor => {
                let ref_color = solid_fill_color(&ref_node);
                let cand_color = solid_fill_color(node);
                match (ref_color, cand_color) {
                    (Some(rc), Some(cc)) => color_distance(rc, cc) <= tolerance,
                    (None, None) => true,
                    _ => false,
                }
            }
            SelectSameAttribute::StrokeColor => {
                if let (SceneNodeKind::Path(rp), SceneNodeKind::Path(cp)) =
                    (&ref_node.kind, &node.kind)
                {
                    match (rp.stroke.enabled, cp.stroke.enabled) {
                        (true, true) => {
                            color_distance(rp.stroke.color, cp.stroke.color) <= tolerance
                        }
                        (false, false) => true,
                        _ => false,
                    }
                } else {
                    false
                }
            }
            SelectSameAttribute::StrokeWeight => {
                if let (SceneNodeKind::Path(rp), SceneNodeKind::Path(cp)) =
                    (&ref_node.kind, &node.kind)
                {
                    (rp.stroke.width - cp.stroke.width).abs() <= tolerance as f64
                } else {
                    false
                }
            }
            SelectSameAttribute::Opacity => (ref_node.opacity - node.opacity).abs() <= tolerance,
            SelectSameAttribute::BlendMode => ref_node.blend_mode == node.blend_mode,
            SelectSameAttribute::ObjectType => {
                std::mem::discriminant(&ref_node.kind) == std::mem::discriminant(&node.kind)
            }
        };
        if matches {
            matched.push(*nid);
        }
    }

    doc.selection.clear();
    for nid in &matched {
        doc.selection.add(*nid);
    }

    let attr_label = match args.attribute {
        SelectSameAttribute::FillColor => "fill color",
        SelectSameAttribute::StrokeColor => "stroke color",
        SelectSameAttribute::StrokeWeight => "stroke weight",
        SelectSameAttribute::Opacity => "opacity",
        SelectSameAttribute::BlendMode => "blend mode",
        SelectSameAttribute::ObjectType => "object type",
    };
    let count = matched.len();
    ToolResult::text(format!(
        "Clicked node: {}. Selected {} node(s) with matching {}.",
        ref_node.name, count, attr_label
    ))
    .with_data(serde_json::json!({
        "clicked_node_id": ref_id,
        "node_ids": matched,
        "count": count,
        "attribute": attr_label,
    }))
}
/// Select nodes whose bounding-box centroid (or any corner) lies inside the
/// given canvas-space polygon.
pub async fn lasso_select(state: &AppState, args: LassoSelectArgs) -> ToolResult {
    if args.points.len() < 3 {
        return ToolResult::error(
            "lasso_select requires at least 3 points to form a closed polygon",
        );
    }

    let mut doc = state.document.lock().await;

    let poly: Vec<[f64; 2]> = args.points.clone();
    let mut selected_ids: Vec<photonic_core::node::NodeId> = Vec::new();

    for node in doc.nodes_in_draw_order() {
        if !node.visible {
            continue;
        }
        let (wx0, wy0, wx1, wy1) = match node_world_aabb(node) {
            Some(b) => b,
            None => continue,
        };

        let inside = if args.centroid_mode {
            // Check if the AABB centroid is inside the polygon.
            let cx = (wx0 + wx1) / 2.0;
            let cy = (wy0 + wy1) / 2.0;
            point_in_polygon(cx, cy, &poly)
        } else {
            // Check if any AABB corner is inside the polygon.
            let corners = [(wx0, wy0), (wx1, wy0), (wx0, wy1), (wx1, wy1)];
            corners.iter().any(|&(x, y)| point_in_polygon(x, y, &poly))
        };

        if inside {
            selected_ids.push(node.id);
        }
    }

    if !args.additive {
        doc.selection.clear();
    }
    for nid in &selected_ids {
        doc.selection.add(*nid);
    }

    let count = selected_ids.len();
    ToolResult::text(format!("Lasso selected {} node(s).", count)).with_data(serde_json::json!({
        "node_ids": selected_ids,
        "count": count,
    }))
}
/// Select all nodes whose kind matches the specified filter.
pub async fn select_by_kind(state: &AppState, args: SelectByKindArgs) -> ToolResult {
    let mut doc = state.document.lock().await;

    let active_layer = doc.active_layer_id;

    let matching: Vec<NodeId> = doc
        .nodes
        .iter()
        .filter(|(_, node)| match &args.kind {
            ObjectKindFilter::Path => matches!(node.kind, SceneNodeKind::Path(_)),
            ObjectKindFilter::Text => matches!(node.kind, SceneNodeKind::Text(_)),
            ObjectKindFilter::Group => matches!(node.kind, SceneNodeKind::Group(_)),
            ObjectKindFilter::SameLayer => active_layer
                .map(|lid| node.layer_id == lid)
                .unwrap_or(false),
        })
        .map(|(id, _)| *id)
        .collect();

    if !args.additive {
        doc.selection.clear();
    }
    let count = matching.len();
    for nid in &matching {
        doc.selection.add(*nid);
    }

    ToolResult::text(format!(
        "Selected {} {} node(s)",
        count,
        format!("{:?}", args.kind).to_lowercase()
    ))
    .with_data(serde_json::json!({
        "selected_count": count,
        "node_ids": matching,
    }))
}
/// Replace the selection with the direct children of the specified group node.
pub async fn select_inside_group(state: &AppState, args: SelectInsideGroupArgs) -> ToolResult {
    let mut doc = state.document.lock().await;
    let group_id = args.group_id;

    let children = match doc.nodes.get(&group_id) {
        Some(node) => {
            if let SceneNodeKind::Group(g) = &node.kind {
                g.children.clone()
            } else {
                return ToolResult::error(format!(
                    "Node {} is not a group (kind: {:?})",
                    group_id,
                    std::mem::discriminant(&node.kind)
                ));
            }
        }
        None => return ToolResult::error(format!("No node found with id {}", group_id)),
    };

    if children.is_empty() {
        return ToolResult::text(format!("Group {} has no children", group_id));
    }

    if !args.additive {
        doc.selection.clear();
    }
    for cid in &children {
        doc.selection.add(*cid);
    }

    ToolResult::text(format!(
        "Selected {} child node(s) inside group {}",
        children.len(),
        group_id
    ))
    .with_data(serde_json::json!({
        "group_id": group_id,
        "selected_count": children.len(),
        "selected_ids": children,
    }))
}
/// Select all nodes in the document whose visual attributes match those of the
/// reference node(s). Implements Illustrator's "Select > Same > …" and
/// "Global Edit" behaviour.
pub async fn select_similar(state: &AppState, args: SelectSimilarArgs) -> ToolResult {
    tracing::debug!("tool: select_similar");
    use photonic_core::style::FillKind;

    let mut doc = state.document.lock().await;

    // Resolve reference IDs.
    let ref_ids: Vec<NodeId> = if args.node_ids.is_empty() {
        doc.selection.node_ids.iter().copied().collect()
    } else {
        args.node_ids
            .iter()
            .filter_map(|s| {
                uuid::Uuid::parse_str(s)
                    .ok()
                    .or_else(|| doc.nodes.values().find(|n| n.name == *s).map(|n| n.id))
            })
            .collect()
    };

    if ref_ids.is_empty() {
        return ToolResult::error("No reference nodes — pass node_ids or make a selection first");
    }

    let match_by = args.match_by.as_deref().unwrap_or("fill_color");
    let tol = args.tolerance.unwrap_or(5) as i32;

    // Color tolerance as f32 fraction (tol is 0-255 scale → convert to 0-1 scale).
    let tol_f = tol as f32 / 255.0;

    // Collect attributes from reference nodes.
    let mut ref_fill_colors: Vec<[f32; 3]> = Vec::new();
    let mut ref_stroke_colors: Vec<[f32; 3]> = Vec::new();
    let mut ref_stroke_widths: Vec<f64> = Vec::new();
    let mut ref_opacities: Vec<f32> = Vec::new();
    let mut ref_kinds: Vec<&'static str> = Vec::new();

    for rid in &ref_ids {
        if let Some(node) = doc.nodes.get(rid) {
            ref_opacities.push(node.opacity);
            match &node.kind {
                SceneNodeKind::Path(p) => {
                    ref_kinds.push("path");
                    if p.fill.enabled {
                        if let FillKind::Solid(c) = &p.fill.kind {
                            ref_fill_colors.push([c.r, c.g, c.b]);
                        }
                    }
                    if p.stroke.enabled {
                        ref_stroke_colors.push([
                            p.stroke.color.r,
                            p.stroke.color.g,
                            p.stroke.color.b,
                        ]);
                        ref_stroke_widths.push(p.stroke.width);
                    }
                }
                SceneNodeKind::Text(t) => {
                    ref_kinds.push("text");
                    if t.fill.enabled {
                        if let FillKind::Solid(c) = &t.fill.kind {
                            ref_fill_colors.push([c.r, c.g, c.b]);
                        }
                    }
                    if t.stroke.enabled {
                        ref_stroke_colors.push([
                            t.stroke.color.r,
                            t.stroke.color.g,
                            t.stroke.color.b,
                        ]);
                        ref_stroke_widths.push(t.stroke.width);
                    }
                }
                SceneNodeKind::Group(_) => {
                    ref_kinds.push("group");
                }
                // raster: no vector fill/stroke attributes to collect
                SceneNodeKind::Raster(_) => {
                    ref_kinds.push("raster");
                }
            }
        }
    }

    // Helper closures.
    let color_matches = |a: [f32; 3], ref_colors: &[[f32; 3]]| -> bool {
        ref_colors.iter().any(|rc| {
            (a[0] - rc[0]).abs() <= tol_f
                && (a[1] - rc[1]).abs() <= tol_f
                && (a[2] - rc[2]).abs() <= tol_f
        })
    };

    let criteria: Vec<&str> = match_by.split(',').map(|s| s.trim()).collect();

    // Collect all matching node IDs.
    let all_ids: Vec<NodeId> = doc.nodes.keys().copied().collect();
    let mut matched: Vec<NodeId> = Vec::new();

    for nid in &all_ids {
        if ref_ids.contains(nid) {
            continue;
        } // skip the reference itself
        let node = match doc.nodes.get(nid) {
            Some(n) => n,
            None => continue,
        };

        let mut node_matches = true;
        for criterion in &criteria {
            let ok = match *criterion {
                "fill_color" => match &node.kind {
                    SceneNodeKind::Path(p) => {
                        if p.fill.enabled {
                            if let FillKind::Solid(c) = &p.fill.kind {
                                color_matches([c.r, c.g, c.b], &ref_fill_colors)
                            } else {
                                false
                            }
                        } else {
                            ref_fill_colors.is_empty()
                        }
                    }
                    SceneNodeKind::Text(t) => {
                        if t.fill.enabled {
                            if let FillKind::Solid(c) = &t.fill.kind {
                                color_matches([c.r, c.g, c.b], &ref_fill_colors)
                            } else {
                                false
                            }
                        } else {
                            ref_fill_colors.is_empty()
                        }
                    }
                    SceneNodeKind::Group(_) => false,
                    // raster: no vector fill/stroke
                    SceneNodeKind::Raster(_) => false,
                },
                "stroke_color" => match &node.kind {
                    SceneNodeKind::Path(p) => {
                        if p.stroke.enabled {
                            color_matches(
                                [p.stroke.color.r, p.stroke.color.g, p.stroke.color.b],
                                &ref_stroke_colors,
                            )
                        } else {
                            false
                        }
                    }
                    SceneNodeKind::Text(t) => {
                        if t.stroke.enabled {
                            color_matches(
                                [t.stroke.color.r, t.stroke.color.g, t.stroke.color.b],
                                &ref_stroke_colors,
                            )
                        } else {
                            false
                        }
                    }
                    SceneNodeKind::Group(_) => false,
                    // raster: no vector fill/stroke
                    SceneNodeKind::Raster(_) => false,
                },
                "stroke_width" => match &node.kind {
                    SceneNodeKind::Path(p) => {
                        if p.stroke.enabled {
                            ref_stroke_widths
                                .iter()
                                .any(|&rw| (p.stroke.width - rw).abs() < 0.01)
                        } else {
                            false
                        }
                    }
                    SceneNodeKind::Text(t) => {
                        if t.stroke.enabled {
                            ref_stroke_widths
                                .iter()
                                .any(|&rw| (t.stroke.width - rw).abs() < 0.01)
                        } else {
                            false
                        }
                    }
                    SceneNodeKind::Group(_) => false,
                    // raster: no vector fill/stroke
                    SceneNodeKind::Raster(_) => false,
                },
                "kind" => {
                    let k = match &node.kind {
                        SceneNodeKind::Path(_) => "path",
                        SceneNodeKind::Text(_) => "text",
                        SceneNodeKind::Group(_) => "group",
                        SceneNodeKind::Raster(_) => "raster",
                    };
                    ref_kinds.contains(&k)
                }
                "opacity" => ref_opacities
                    .iter()
                    .any(|&ro| (node.opacity - ro).abs() < 0.01_f32),
                "tags" => {
                    // Match if any ref node shares at least one tag with this node.
                    let node_tags: std::collections::HashSet<_> = node.tags.iter().collect();
                    ref_ids.iter().any(|rid| {
                        if let Some(rn) = doc.nodes.get(rid) {
                            rn.tags.iter().any(|t| node_tags.contains(t))
                        } else {
                            false
                        }
                    })
                }
                _ => true, // unknown criterion — ignore
            };
            if !ok {
                node_matches = false;
                break;
            }
        }

        if node_matches {
            matched.push(*nid);
        }
    }

    // Apply selection.
    if args.additive {
        for nid in &matched {
            doc.selection.node_ids.insert(*nid);
        }
        for nid in &ref_ids {
            doc.selection.node_ids.insert(*nid);
        }
    } else {
        doc.selection.node_ids.clear();
        for nid in matched.iter().chain(ref_ids.iter()) {
            doc.selection.node_ids.insert(*nid);
        }
    }

    let total = doc.selection.node_ids.len();

    ToolResult::text(format!(
        "Selected {total} node(s) matching {match_by} (tolerance={tol})"
    ))
    .with_data(serde_json::json!({
        "matched_count": matched.len(),
        "total_selected": total,
        "match_by": match_by,
        "tolerance": tol,
        "node_ids": doc.selection.node_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
    }))
}
