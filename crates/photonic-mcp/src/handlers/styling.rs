use crate::handlers::nodes::{
    lerp_point, path_center_x, path_center_y, solid_fill_of, style_prop_enabled,
};
use crate::handlers::shared::{random::*, styling::*};
use crate::protocol::*;
use crate::server::AppState;
use kurbo;
use photonic_core::{
    history::Command,
    layer::BlendMode,
    node::{NodeId, PathNode, SceneNode, SceneNodeKind},
    path::PathData,
    transform::Transform,
};

/// #202: apply one paint to many nodes in a single undoable call, each re-fit to
/// its own bounding box (bbox-relative gradients). Reuses the `fill` paint shape.
pub async fn set_paint(state: &AppState, args: SetPaintArgs) -> ToolResult {
    use photonic_core::history::Command;

    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty.");
    }
    let target = args
        .target
        .as_deref()
        .unwrap_or("fill")
        .to_ascii_lowercase();
    if target != "fill" && target != "stroke" {
        return ToolResult::error(format!(
            "Invalid target '{target}' (expected \"fill\" or \"stroke\")."
        ));
    }

    let mut doc = state.document.lock().await;
    let mut commands = Vec::new();
    let mut applied = 0usize;
    let mut skipped: Vec<String> = Vec::new();

    for id_str in &args.node_ids {
        let nid = uuid::Uuid::parse_str(id_str)
            .ok()
            .or_else(|| doc.find_node_by_name(id_str).map(|n| n.id));
        let Some(nid) = nid else {
            skipped.push(id_str.clone());
            continue;
        };
        let Some(node) = doc.nodes.get(&nid) else {
            skipped.push(id_str.clone());
            continue;
        };

        // World-space bounding box (x, y, w, h) for bbox-relative resolution.
        let bbox = node
            .local_bounds()
            .map(|lb| {
                let (x0, y0) = node.transform.apply(lb.x0, lb.y0);
                let (x1, y1) = node.transform.apply(lb.x1, lb.y1);
                (
                    x0.min(x1),
                    y0.min(y1),
                    (x1 - x0).abs().max(1e-6),
                    (y1 - y0).abs().max(1e-6),
                )
            })
            .unwrap_or((0.0, 0.0, 1.0, 1.0));

        let fill = match args.paint.resolved_for_bbox(bbox).to_fill() {
            Ok(f) => f,
            Err(e) => return ToolResult::error(e),
        };

        let mut new_node = node.clone();
        let ok = match &mut new_node.kind {
            SceneNodeKind::Path(pn) => {
                if target == "fill" {
                    pn.fill = fill;
                } else {
                    apply_stroke_paint(&mut pn.stroke, &fill);
                }
                true
            }
            SceneNodeKind::Text(tn) => {
                if target == "fill" {
                    tn.fill = fill;
                } else {
                    apply_stroke_paint(&mut tn.stroke, &fill);
                }
                true
            }
            _ => false,
        };
        if ok {
            commands.push(Command::UpdateNode {
                old: node.clone(),
                new: new_node,
            });
            applied += 1;
        } else {
            skipped.push(id_str.clone());
        }
    }

    if commands.is_empty() {
        return ToolResult::error(format!(
            "No paintable nodes found ({} skipped).",
            skipped.len()
        ));
    }

    let mut history = state.history.lock().await;
    for cmd in commands {
        history.execute_discrete(cmd, &mut doc);
    }
    drop(history);

    ToolResult::text(format!(
        "Applied {target} paint to {applied} node(s){}.",
        if skipped.is_empty() {
            String::new()
        } else {
            format!(" ({} skipped)", skipped.len())
        }
    ))
    .with_data(serde_json::json!({
        "applied_count": applied,
        "target": target,
        "skipped": skipped,
    }))
}
/// Copy the visual style of one node onto many targets in a single undoable step.
///
/// Copyable properties: fill, stroke (path nodes only), opacity, blend_mode (all node types).
/// Pass `properties` to copy a subset; omit it to copy all four.
pub async fn style_transfer(state: &AppState, args: StyleTransferArgs) -> ToolResult {
    tracing::debug!("tool: style_transfer (targets={})", args.target_ids.len());

    if args.target_ids.is_empty() {
        return ToolResult::error("target_ids must contain at least one node ID");
    }

    // ── Read phase ─────────────────────────────────────────────────────────
    let (source_node, target_nodes) = {
        let doc = state.document.lock().await;
        let source = match doc.get_node(&args.source_id).cloned() {
            Some(n) => n,
            None => return ToolResult::error(format!("Source node {} not found", args.source_id)),
        };
        let targets: Vec<SceneNode> = args
            .target_ids
            .iter()
            .filter_map(|id| doc.get_node(id).cloned())
            .collect();
        (source, targets)
    };

    if target_nodes.is_empty() {
        return ToolResult::error("None of the target_ids were found in the document");
    }

    // ── Prepare phase ──────────────────────────────────────────────────────
    let copy_fill = style_prop_enabled(&args.properties, "fill");
    let copy_stroke = style_prop_enabled(&args.properties, "stroke");
    let copy_opacity = style_prop_enabled(&args.properties, "opacity");
    let copy_blend_mode = style_prop_enabled(&args.properties, "blend_mode");

    // Extract source path-level style once (only meaningful if source is a Path).
    let src_fill = if copy_fill {
        if let SceneNodeKind::Path(ref p) = source_node.kind {
            Some(p.fill.clone())
        } else {
            None
        }
    } else {
        None
    };
    let src_stroke = if copy_stroke {
        if let SceneNodeKind::Path(ref p) = source_node.kind {
            Some(p.stroke.clone())
        } else {
            None
        }
    } else {
        None
    };

    let mut commands: Vec<Command> = Vec::with_capacity(target_nodes.len());

    for old_node in target_nodes {
        let mut new_node = old_node.clone();

        if copy_opacity {
            new_node.opacity = source_node.opacity;
        }
        if copy_blend_mode {
            // Blend modes other than Normal are not yet rendered; always apply Normal.
            new_node.blend_mode = BlendMode::Normal;
        }
        if let SceneNodeKind::Path(ref mut tp) = new_node.kind {
            if let Some(ref fill) = src_fill {
                tp.fill = fill.clone();
            }
            if let Some(ref stroke) = src_stroke {
                tp.stroke = stroke.clone();
            }
        }

        commands.push(Command::UpdateNode {
            old: old_node,
            new: new_node,
        });
    }

    let updated = commands.len();

    // ── Write phase ────────────────────────────────────────────────────────
    let cmd = Command::Batch(commands);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!(
        "Style transferred from '{}' to {} node(s)",
        source_node.name, updated
    ))
    .with_data(serde_json::json!({
        "source_id": args.source_id,
        "updated":   updated,
    }))
}
pub async fn blend_objects(state: &AppState, args: BlendObjectsArgs) -> ToolResult {
    tracing::debug!("tool: blend_objects");
    use photonic_core::color::Color;
    use photonic_core::style::{Fill, FillKind};

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    // Resolve both nodes.
    let resolve = |id_str: &str| -> Option<NodeId> {
        uuid::Uuid::parse_str(id_str)
            .ok()
            .or_else(|| doc.find_node_by_name(id_str).map(|n| n.id))
    };
    let nid_a = match resolve(&args.node_id_a) {
        Some(id) => id,
        None => return ToolResult::error(format!("Node A not found: {}", args.node_id_a)),
    };
    let nid_b = match resolve(&args.node_id_b) {
        Some(id) => id,
        None => return ToolResult::error(format!("Node B not found: {}", args.node_id_b)),
    };

    let node_a = doc.nodes.get(&nid_a).cloned();
    let node_b = doc.nodes.get(&nid_b).cloned();

    let (node_a, node_b) = match (node_a, node_b) {
        (Some(a), Some(b)) => (a, b),
        _ => return ToolResult::error("One or both nodes not found"),
    };

    let (pn_a, pn_b) = match (&node_a.kind, &node_b.kind) {
        (SceneNodeKind::Path(a), SceneNodeKind::Path(b)) => (a, b),
        _ => return ToolResult::error("Both nodes must be paths"),
    };

    let bez_a = pn_a.path_data.to_bez_path();
    let bez_b = pn_b.path_data.to_bez_path();

    if bez_a.elements().len() != bez_b.elements().len() {
        return ToolResult::error(format!(
            "Path element counts differ ({} vs {}). Both paths must have the same number of elements for blending. Use add_anchor_points to equalize.",
            bez_a.elements().len(), bez_b.elements().len()
        ));
    }

    // Extract solid fill colors for interpolation.
    let color_a = solid_fill_of(&pn_a.fill);
    let color_b = solid_fill_of(&pn_b.fill);

    // Get translation components for position interpolation.
    let tx_a = (node_a.transform.matrix[4], node_a.transform.matrix[5]);
    let tx_b = (node_b.transform.matrix[4], node_b.transform.matrix[5]);

    // ── Compute steps based on chosen mode ──────────────────────────────────
    let steps = if let Some(sp) = args.spacing {
        // Specified Distance: steps = ceil(center_distance / spacing)
        if sp <= 0.0 {
            return ToolResult::error("spacing must be positive");
        }
        let dx = tx_b.0 - tx_a.0;
        let dy = tx_b.1 - tx_a.1;
        let dist = (dx * dx + dy * dy).sqrt();
        ((dist / sp).ceil() as usize).saturating_sub(1).max(1)
    } else if args.smooth_color {
        // Smooth Color: auto-compute steps so color changes by ≤ 1/255 per step.
        if let (Some(ca), Some(cb)) = (&color_a, &color_b) {
            let dr = ((cb.r - ca.r).abs() * 255.0) as f64;
            let dg = ((cb.g - ca.g).abs() * 255.0) as f64;
            let db = ((cb.b - ca.b).abs() * 255.0) as f64;
            let max_delta = dr.max(dg).max(db);
            (max_delta.ceil() as usize).max(1)
        } else {
            // No solid fill to measure; fall back to default
            args.steps.unwrap_or(5).max(1)
        }
    } else {
        args.steps.unwrap_or(5).max(1)
    };

    let layer_id = node_a.layer_id;
    let mut created_ids = Vec::new();

    for i in 1..=steps {
        let t = i as f64 / (steps + 1) as f64;

        // Interpolate path geometry.
        let mut interp_bez = kurbo::BezPath::new();
        for (ea, eb) in bez_a.elements().iter().zip(bez_b.elements().iter()) {
            match (*ea, *eb) {
                (kurbo::PathEl::MoveTo(a), kurbo::PathEl::MoveTo(b)) => {
                    interp_bez.move_to(lerp_point(a, b, t));
                }
                (kurbo::PathEl::LineTo(a), kurbo::PathEl::LineTo(b)) => {
                    interp_bez.line_to(lerp_point(a, b, t));
                }
                (kurbo::PathEl::CurveTo(a1, a2, a3), kurbo::PathEl::CurveTo(b1, b2, b3)) => {
                    interp_bez.curve_to(
                        lerp_point(a1, b1, t),
                        lerp_point(a2, b2, t),
                        lerp_point(a3, b3, t),
                    );
                }
                (kurbo::PathEl::QuadTo(a1, a2), kurbo::PathEl::QuadTo(b1, b2)) => {
                    interp_bez.quad_to(lerp_point(a1, b1, t), lerp_point(a2, b2, t));
                }
                (kurbo::PathEl::ClosePath, kurbo::PathEl::ClosePath) => {
                    interp_bez.close_path();
                }
                _ => {
                    // Mismatched element types — fall back to element from A.
                    interp_bez.push(*ea);
                }
            }
        }

        let mut new_pn = pn_a.clone();
        new_pn.path_data = PathData::from_bez_path(&interp_bez);

        // Interpolate fill color.
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

        // Interpolate opacity.
        let opacity = node_a.opacity + (node_b.opacity - node_a.opacity) * t as f32;

        let name = format!("Blend {}/{}", i, steps);
        let mut node = SceneNode::new(&name, layer_id, SceneNodeKind::Path(new_pn));
        node.opacity = opacity;

        // Interpolate transform (translation only for simplicity).
        let interp_tx = (
            tx_a.0 + (tx_b.0 - tx_a.0) * t,
            tx_a.1 + (tx_b.1 - tx_a.1) * t,
        );
        node.transform = Transform::translate(interp_tx.0, interp_tx.1);

        let nid = node.id;
        created_ids.push(nid);
        history.execute_discrete(
            Command::AddNode {
                node,
                layer_id: Some(layer_id),
            },
            &mut doc,
        );
    }

    let mode = if args.spacing.is_some() {
        "spacing"
    } else if args.smooth_color {
        "smooth_color"
    } else {
        "steps"
    };
    ToolResult::text(format!(
        "Created {} blend steps between '{}' and '{}' (mode: {})",
        steps, node_a.name, node_b.name, mode
    ))
    .with_data(serde_json::json!({
        "steps": steps,
        "mode": mode,
        "created_ids": created_ids,
    }))
}
pub async fn sample_color_at(state: &AppState, args: SampleColorAtArgs) -> ToolResult {
    tracing::debug!("tool: sample_color_at");
    use kurbo::Shape;
    use photonic_core::style::FillKind;

    let doc = state.document.lock().await;
    let pt = kurbo::Point::new(args.x, args.y);

    // Find the topmost visible node whose bounding box contains the point.
    // We iterate layers top-to-bottom, nodes top-to-bottom.
    for lid in doc.layer_order.iter().rev() {
        let layer = match doc.layers.get(lid) {
            Some(l) if l.visible => l,
            _ => continue,
        };
        for nid in layer.node_ids.iter().rev() {
            let node = match doc.nodes.get(nid) {
                Some(n) if n.visible => n,
                _ => continue,
            };
            // Map the canvas point into the node's local space so moved/scaled/
            // rotated nodes hit-test correctly.
            let local = node.transform.to_kurbo().inverse() * pt;
            match &node.kind {
                SceneNodeKind::Path(pn) => {
                    let bez = pn.path_data.to_bez_path();
                    if bez.winding(local) != 0 {
                        let fill_hex = match &pn.fill.kind {
                            FillKind::Solid(c) => Some(c.to_hex()),
                            _ => None,
                        };
                        let stroke_hex = if pn.stroke.enabled {
                            Some(pn.stroke.color.to_hex())
                        } else {
                            None
                        };

                        return ToolResult::text(format!(
                            "Sampled '{}': fill={}, stroke={}",
                            node.name,
                            fill_hex.as_deref().unwrap_or("none"),
                            stroke_hex.as_deref().unwrap_or("none"),
                        ))
                        .with_data(serde_json::json!({
                            "node_id": nid,
                            "node_name": node.name,
                            "fill_color": fill_hex,
                            "stroke_color": stroke_hex,
                            "opacity": node.opacity,
                        }));
                    }
                }
                SceneNodeKind::Raster(rn)
                    if !rn.is_adjustment_layer()
                        && local.x >= 0.0
                        && local.y >= 0.0
                        && local.x < rn.image.width as f64
                        && local.y < rn.image.height as f64 =>
                {
                    let rgba = rn.image.pixel(local.x as u32, local.y as u32);
                    let cov = rn
                        .mask
                        .as_ref()
                        .map(|m| m.coverage(local.x as u32, local.y as u32))
                        .unwrap_or(1.0);
                    // Skip transparent/masked pixels so sampling falls through.
                    if (rgba[3] as f32 / 255.0) * cov * node.opacity > 0.0 {
                        let hex = format!("#{:02X}{:02X}{:02X}", rgba[0], rgba[1], rgba[2]);
                        return ToolResult::text(format!(
                            "Sampled '{}': color={} (raster pixel)",
                            node.name, hex
                        ))
                        .with_data(serde_json::json!({
                            "node_id": nid,
                            "node_name": node.name,
                            "fill_color": hex,
                            "stroke_color": null,
                            "opacity": node.opacity,
                        }));
                    }
                }
                _ => {}
            }
        }
    }

    ToolResult::text(format!("No node at ({}, {})", args.x, args.y))
        .with_data(serde_json::json!({ "node_id": null, "fill_color": null }))
}
pub async fn remove_fill(state: &AppState, args: RemoveStyleArgs) -> ToolResult {
    tracing::debug!("tool: remove_fill");

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

    let mut modified = 0usize;
    for nid in &node_ids {
        let node = match doc.nodes.get(nid) {
            Some(n) => n.clone(),
            None => continue,
        };
        let mut new_node = node.clone();
        match &mut new_node.kind {
            SceneNodeKind::Path(pn) => {
                pn.fill = photonic_core::style::Fill::none();
            }
            SceneNodeKind::Text(tn) => {
                tn.fill = photonic_core::style::Fill::none();
            }
            _ => continue,
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

    ToolResult::text(format!("Removed fill from {modified} node(s)"))
        .with_data(serde_json::json!({ "modified": modified }))
}
pub async fn remove_stroke(state: &AppState, args: RemoveStyleArgs) -> ToolResult {
    tracing::debug!("tool: remove_stroke");

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

    let mut modified = 0usize;
    for nid in &node_ids {
        let node = match doc.nodes.get(nid) {
            Some(n) => n.clone(),
            None => continue,
        };
        let mut new_node = node.clone();
        match &mut new_node.kind {
            SceneNodeKind::Path(pn) => {
                pn.stroke = photonic_core::style::Stroke::none();
            }
            SceneNodeKind::Text(tn) => {
                tn.stroke = photonic_core::style::Stroke::none();
            }
            _ => continue,
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

    ToolResult::text(format!("Removed stroke from {modified} node(s)"))
        .with_data(serde_json::json!({ "modified": modified }))
}
pub async fn set_blend_mode(state: &AppState, args: SetBlendModeArgs) -> ToolResult {
    tracing::debug!("tool: set_blend_mode");
    use photonic_core::layer::BlendMode;

    let mode = match args.blend_mode.as_str() {
        "normal" => BlendMode::Normal,
        "multiply" => BlendMode::Multiply,
        "screen" => BlendMode::Screen,
        "overlay" => BlendMode::Overlay,
        "darken" => BlendMode::Darken,
        "lighten" => BlendMode::Lighten,
        "color_dodge" => BlendMode::ColorDodge,
        "color_burn" => BlendMode::ColorBurn,
        "hard_light" => BlendMode::HardLight,
        "soft_light" => BlendMode::SoftLight,
        "difference" => BlendMode::Difference,
        "exclusion" => BlendMode::Exclusion,
        "hue" => BlendMode::Hue,
        "saturation" => BlendMode::Saturation,
        "color" => BlendMode::Color,
        "luminosity" => BlendMode::Luminosity,
        "linear_dodge" => BlendMode::LinearDodge,
        "linear_burn" => BlendMode::LinearBurn,
        "subtract" => BlendMode::Subtract,
        "divide" => BlendMode::Divide,
        "vivid_light" => BlendMode::VividLight,
        "linear_light" => BlendMode::LinearLight,
        "pin_light" => BlendMode::PinLight,
        "hard_mix" => BlendMode::HardMix,
        "darker_color" => BlendMode::DarkerColor,
        "lighter_color" => BlendMode::LighterColor,
        other => return ToolResult::error(format!("Unknown blend mode: '{other}'")),
    };

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
        new_node.blend_mode = mode;
        history.execute_discrete(
            Command::UpdateNode {
                old: node,
                new: new_node,
            },
            &mut doc,
        );
        modified += 1;
    }

    ToolResult::text(format!(
        "Set blend mode to '{}' on {modified} node(s)",
        args.blend_mode
    ))
    .with_data(serde_json::json!({ "modified": modified, "blend_mode": args.blend_mode }))
}
pub async fn set_opacity(state: &AppState, args: SetOpacityArgs) -> ToolResult {
    tracing::debug!("tool: set_opacity");

    let opacity = args.opacity.clamp(0.0, 1.0);

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
        new_node.opacity = opacity;
        history.execute_discrete(
            Command::UpdateNode {
                old: node,
                new: new_node,
            },
            &mut doc,
        );
        modified += 1;
    }

    ToolResult::text(format!("Set opacity to {opacity} on {modified} node(s)"))
        .with_data(serde_json::json!({ "modified": modified, "opacity": opacity }))
}
pub async fn randomize_colors(state: &AppState, args: RandomizeColorsArgs) -> ToolResult {
    tracing::debug!("tool: randomize_colors");
    use photonic_core::color::Color;
    use photonic_core::style::{Fill, FillKind};

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

    // Parse palette or generate random colors.
    let palette: Vec<Color> = if args.palette.is_empty() {
        let mut rng = args.seed.unwrap_or(42).max(1);
        (0..10)
            .map(|_| {
                let r = (xorshift64(&mut rng) * 0.5 + 0.5) as f32;
                let g = (xorshift64(&mut rng) * 0.5 + 0.5) as f32;
                let b = (xorshift64(&mut rng) * 0.5 + 0.5) as f32;
                Color::new(r, g, b, 1.0)
            })
            .collect()
    } else {
        args.palette
            .iter()
            .filter_map(|h| Color::from_hex(h))
            .collect()
    };

    if palette.is_empty() {
        return ToolResult::error("No valid colors in palette");
    }

    let mut rng = args.seed.unwrap_or(42).max(1);
    let mut modified = 0usize;

    for nid in &node_ids {
        let node = match doc.nodes.get(nid) {
            Some(n) => n.clone(),
            None => continue,
        };

        let mut new_node = node.clone();
        let mut pick = || -> Color {
            let idx = ((xorshift64(&mut rng) * 0.5 + 0.5) * palette.len() as f64) as usize
                % palette.len();
            palette[idx]
        };

        match &mut new_node.kind {
            SceneNodeKind::Path(pn) => {
                if args.fill {
                    pn.fill = Fill {
                        kind: FillKind::Solid(pick()),
                        ..Default::default()
                    };
                }
                if args.stroke && pn.stroke.enabled {
                    pn.stroke.color = pick();
                }
            }
            SceneNodeKind::Text(tn) => {
                if args.fill {
                    tn.fill = Fill {
                        kind: FillKind::Solid(pick()),
                        ..Default::default()
                    };
                }
            }
            _ => continue,
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

    ToolResult::text(format!(
        "Randomized colors on {modified} node(s) from {} palette colors",
        palette.len()
    ))
    .with_data(serde_json::json!({ "modified": modified }))
}
pub async fn swap_fill_stroke(state: &AppState, args: SwapFillStrokeArgs) -> ToolResult {
    tracing::debug!("tool: swap_fill_stroke");
    use photonic_core::style::{Fill, FillKind, Stroke};

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
                // Extract fill color → new stroke, stroke color → new fill.
                let old_fill_color = match &pn.fill.kind {
                    FillKind::Solid(c) => Some(*c),
                    _ => None,
                };
                let old_stroke_color = pn.stroke.color;
                let old_stroke_width = pn.stroke.width;
                let old_stroke_enabled = pn.stroke.enabled;

                // Set fill from old stroke.
                if old_stroke_enabled {
                    pn.fill = Fill {
                        kind: FillKind::Solid(old_stroke_color),
                        ..Default::default()
                    };
                } else {
                    pn.fill = Fill::none();
                }

                // Set stroke from old fill.
                if let Some(fc) = old_fill_color {
                    pn.stroke = Stroke {
                        color: fc,
                        width: if old_stroke_width > 0.0 {
                            old_stroke_width
                        } else {
                            1.0
                        },
                        enabled: true,
                        ..Default::default()
                    };
                } else {
                    pn.stroke = Stroke::none();
                }
            }
            SceneNodeKind::Text(tn) => {
                let old_fill_color = match &tn.fill.kind {
                    FillKind::Solid(c) => Some(*c),
                    _ => None,
                };
                let old_stroke_color = tn.stroke.color;
                let old_stroke_enabled = tn.stroke.enabled;

                if old_stroke_enabled {
                    tn.fill = Fill {
                        kind: FillKind::Solid(old_stroke_color),
                        ..Default::default()
                    };
                } else {
                    tn.fill = Fill::none();
                }
                if let Some(fc) = old_fill_color {
                    tn.stroke = Stroke {
                        color: fc,
                        width: 1.0,
                        enabled: true,
                        ..Default::default()
                    };
                } else {
                    tn.stroke = Stroke::none();
                }
            }
            _ => continue,
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

    ToolResult::text(format!("Swapped fill and stroke on {modified} node(s)"))
        .with_data(serde_json::json!({ "modified": modified }))
}
pub async fn hatch_fill(state: &AppState, args: HatchFillArgs) -> ToolResult {
    tracing::debug!("tool: hatch_fill");
    use kurbo::Shape;
    use photonic_core::color::Color;
    use photonic_core::style::{Fill, Stroke};

    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let spacing = args.spacing.unwrap_or(5.0).max(0.5);
    let angle_deg = args.angle.unwrap_or(45.0);
    let stroke_w = args.stroke_width.unwrap_or(1.0);

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let mut created = 0usize;
    let mut skipped = 0usize;

    let angles: Vec<f64> = {
        let mut a = vec![angle_deg];
        if let Some(ca) = args.cross_angle {
            a.push(ca);
        }
        a
    };

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
        let bbox = bez.bounding_box();
        let bw = bbox.width();
        let bh = bbox.height();
        if bw < 1e-9 || bh < 1e-9 {
            skipped += 1;
            continue;
        }

        let hatch_color = if let Some(ref hex) = args.color {
            Color::from_hex(hex).unwrap_or(Color::BLACK)
        } else {
            match &pn.fill.kind {
                photonic_core::style::FillKind::Solid(c) => *c,
                _ => Color::BLACK,
            }
        };

        let layer_id = node.layer_id;
        let cx = bbox.x0 + bw / 2.0;
        let cy = bbox.y0 + bh / 2.0;
        let diag = (bw * bw + bh * bh).sqrt();

        let mut hatch_path = kurbo::BezPath::new();

        for angle in &angles {
            let rad = angle.to_radians();
            let cos_a = rad.cos();
            let sin_a = rad.sin();

            // Direction perpendicular to hatch lines.
            let perp_x = -sin_a;
            let perp_y = cos_a;

            let n_lines = (diag / spacing) as i32 + 1;

            for i in -n_lines..=n_lines {
                let offset = i as f64 * spacing;
                // Line center point offset perpendicular to the hatch direction.
                let lx = cx + perp_x * offset;
                let ly = cy + perp_y * offset;

                // Line endpoints extending in the hatch direction.
                let p0 = kurbo::Point::new(lx - cos_a * diag, ly - sin_a * diag);
                let p1 = kurbo::Point::new(lx + cos_a * diag, ly + sin_a * diag);

                // Sample points along the line and find segments inside the path.
                let samples = 100;
                let mut inside = false;
                let mut seg_start = p0;

                for s in 0..=samples {
                    let t = s as f64 / samples as f64;
                    let pt = kurbo::Point::new(p0.x + (p1.x - p0.x) * t, p0.y + (p1.y - p0.y) * t);
                    let is_inside = bez.winding(pt) != 0;

                    if is_inside && !inside {
                        seg_start = pt;
                        inside = true;
                    } else if !is_inside && inside {
                        hatch_path.move_to(seg_start);
                        hatch_path.line_to(pt);
                        inside = false;
                    }
                }
                if inside {
                    hatch_path.move_to(seg_start);
                    hatch_path.line_to(p1);
                }
            }
        }

        if hatch_path.elements().is_empty() {
            skipped += 1;
            continue;
        }

        let mut hatch_pn = PathNode::new(PathData::from_bez_path(&hatch_path));
        hatch_pn.fill = Fill::none();
        hatch_pn.stroke = Stroke {
            color: hatch_color,
            width: stroke_w,
            ..Default::default()
        };

        let hatch_node = SceneNode::new(
            format!("{} Hatch", node.name),
            layer_id,
            SceneNodeKind::Path(hatch_pn),
        );
        history.execute_discrete(
            Command::AddNode {
                node: hatch_node,
                layer_id: Some(layer_id),
            },
            &mut doc,
        );
        created += 1;
    }

    if created == 0 {
        return ToolResult::error("No valid path nodes found for hatch fill");
    }

    ToolResult::text(format!(
        "Created hatch fill for {} node(s) (spacing={spacing}, angle={angle_deg}°){}",
        created,
        if skipped > 0 {
            format!(" — {skipped} skipped")
        } else {
            String::new()
        },
    ))
    .with_data(serde_json::json!({ "created": created, "skipped": skipped }))
}
pub async fn stipple_fill(state: &AppState, args: StippleFillArgs) -> ToolResult {
    tracing::debug!("tool: stipple_fill");
    use kurbo::Shape;
    use photonic_core::color::Color;
    use photonic_core::style::{Fill, FillKind, Stroke};

    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let count = args.count.unwrap_or(200).max(1);
    let dot_r = args.dot_radius.unwrap_or(1.5);
    let seed = args.seed.unwrap_or(42).max(1);

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let mut created_groups = 0usize;
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
        let bbox = bez.bounding_box();
        let bw = bbox.width();
        let bh = bbox.height();
        if bw < 1e-9 || bh < 1e-9 {
            skipped += 1;
            continue;
        }

        // Determine dot color.
        let dot_color = if let Some(ref hex) = args.color {
            Color::from_hex(hex).unwrap_or(Color::BLACK)
        } else {
            match &pn.fill.kind {
                FillKind::Solid(c) => *c,
                _ => Color::BLACK,
            }
        };

        let layer_id = node.layer_id;

        // Generate dots using rejection sampling.
        let mut rng = seed;
        let mut dot_path = kurbo::BezPath::new();
        let mut placed = 0usize;
        let max_attempts = count * 20; // prevent infinite loop on very small shapes

        for _ in 0..max_attempts {
            if placed >= count {
                break;
            }
            let rx = xorshift64(&mut rng) * 0.5 + 0.5; // [0, 1]
            let ry = xorshift64(&mut rng) * 0.5 + 0.5;
            let px = bbox.x0 + rx * bw;
            let py = bbox.y0 + ry * bh;
            let pt = kurbo::Point::new(px, py);

            // Test if point is inside the path.
            if bez.winding(pt) != 0 {
                // Add a small circle at this point.
                let circle = kurbo::Circle::new(pt, dot_r);
                for el in circle.to_path(0.1).elements() {
                    dot_path.push(*el);
                }
                placed += 1;
            }
        }

        if placed == 0 {
            skipped += 1;
            continue;
        }

        let mut dot_pn = PathNode::new(PathData::from_bez_path(&dot_path));
        dot_pn.fill = Fill {
            kind: FillKind::Solid(dot_color),
            ..Default::default()
        };
        dot_pn.stroke = Stroke::none();

        let dot_node = SceneNode::new(
            format!("{} Stipple", node.name),
            layer_id,
            SceneNodeKind::Path(dot_pn),
        );
        history.execute_discrete(
            Command::AddNode {
                node: dot_node,
                layer_id: Some(layer_id),
            },
            &mut doc,
        );
        created_groups += 1;
    }

    if created_groups == 0 {
        return ToolResult::error("No valid path nodes found for stipple fill");
    }

    ToolResult::text(format!(
        "Created stipple fill for {} node(s) ({count} dots each){}",
        created_groups,
        if skipped > 0 {
            format!(" — {skipped} skipped")
        } else {
            String::new()
        },
    ))
    .with_data(serde_json::json!({ "created": created_groups, "skipped": skipped }))
}
pub async fn add_drop_shadow(state: &AppState, args: AddDropShadowArgs) -> ToolResult {
    tracing::debug!("tool: add_drop_shadow");
    use photonic_core::color::Color;
    use photonic_core::style::{Fill, FillKind};

    if args.node_ids.is_empty() {
        return ToolResult::error("node_ids must not be empty");
    }

    let ox = args.offset_x.unwrap_or(5.0);
    let oy = args.offset_y.unwrap_or(5.0);
    let shadow_opacity = args.opacity.unwrap_or(0.4);
    let shadow_color = args.color.as_deref().unwrap_or("#000000");
    let sc = Color::from_hex(shadow_color).unwrap_or(Color::new(0.0, 0.0, 0.0, 1.0));

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let mut created = 0usize;
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

        // Create shadow: duplicate node, offset, recolor, place below original.
        let mut shadow = node.clone();
        shadow.id = uuid::Uuid::new_v4();
        shadow.name = format!("{} Shadow", node.name);
        shadow.opacity = shadow_opacity;

        // Apply offset to transform.
        shadow.transform.matrix[4] += ox;
        shadow.transform.matrix[5] += oy;

        // Recolor: set fill to shadow color for paths, set text fill for text.
        match &mut shadow.kind {
            SceneNodeKind::Path(pn) => {
                pn.fill = Fill {
                    kind: FillKind::Solid(sc),
                    ..Default::default()
                };
                pn.stroke = photonic_core::style::Stroke::none();
            }
            SceneNodeKind::Text(tn) => {
                tn.fill = Fill {
                    kind: FillKind::Solid(sc),
                    ..Default::default()
                };
                tn.stroke = photonic_core::style::Stroke::none();
            }
            SceneNodeKind::Group(_) => {
                // For groups, just offset and set opacity — child colors preserved as silhouette.
            }
            // raster: no vector fill to recolor — offset + opacity only
            SceneNodeKind::Raster(_) => {}
        }

        history.execute_discrete(
            Command::AddNode {
                node: shadow,
                layer_id: Some(node.layer_id),
            },
            &mut doc,
        );
        created += 1;
    }

    if created == 0 {
        return ToolResult::error("No valid nodes found");
    }

    ToolResult::text(format!(
        "Added drop shadow to {} node(s) (offset=[{ox},{oy}], opacity={shadow_opacity}){}",
        created,
        if skipped > 0 {
            format!(" — {skipped} skipped")
        } else {
            String::new()
        },
    ))
    .with_data(serde_json::json!({ "created": created, "skipped": skipped }))
}
pub async fn invert_colors(state: &AppState, args: InvertColorsArgs) -> ToolResult {
    use photonic_core::style::FillKind;

    // 1. Collect candidate path nodes
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
                .filter(|n| matches!(n.kind, SceneNodeKind::Path(_)))
                .cloned()
                .collect(),
        }
    };

    if candidates.is_empty() {
        return ToolResult::text("No path nodes found to invert.");
    }

    // 2. Build UpdateNode commands
    let mut commands: Vec<Command> = Vec::new();
    let mut count = 0usize;

    for node in &candidates {
        let mut new_node = node.clone();
        let mut modified = false;

        if let SceneNodeKind::Path(path) = &mut new_node.kind {
            match &mut path.fill.kind {
                FillKind::Solid(c) => *c = c.invert(),
                FillKind::Gradient(g) => {
                    for stop in &mut g.stops {
                        stop.color = stop.color.invert();
                    }
                }
                FillKind::FluidGradient(fg) => {
                    for pt in &mut fg.points {
                        pt.color = pt.color.invert();
                    }
                }
                FillKind::MeshGradient(mg) => {
                    for c in &mut mg.cell_colors {
                        *c = c.invert();
                    }
                }
                FillKind::Pattern(p) => {
                    p.tile.map_rgb(|[r, g, b]| [1.0 - r, 1.0 - g, 1.0 - b]);
                }
                FillKind::None => {}
            }
            if path.stroke.enabled {
                path.stroke.color = path.stroke.color.invert();
            }
            modified = true;
        }

        if modified {
            commands.push(Command::UpdateNode {
                old: node.clone(),
                new: new_node,
            });
            count += 1;
        }
    }

    if count == 0 {
        return ToolResult::text("Selected nodes contain no path nodes.");
    }

    // 3. Execute as a single undo-able batch
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(Command::Batch(commands), &mut doc);
    history.schedule_mcp_checkpoint(format!("Invert colors ({} nodes)", count));

    ToolResult::text(format!("Inverted colors on {} node(s).", count))
}
/// Shift RGB(A) channel values across selected artwork.
/// Each channel delta is added to the existing value and clamped to [0, 1].
pub async fn adjust_colors(state: &AppState, args: AdjustColorsArgs) -> ToolResult {
    use photonic_core::style::FillKind;

    let dr = args.delta_r;
    let dg = args.delta_g;
    let db = args.delta_b;
    let da = args.delta_a;

    if dr == 0.0 && dg == 0.0 && db == 0.0 && da == 0.0 {
        return ToolResult::text("No channel deltas specified; nothing to adjust.");
    }

    let shift_color = |c: photonic_core::Color| -> photonic_core::Color {
        photonic_core::Color {
            r: (c.r + dr).clamp(0.0, 1.0),
            g: (c.g + dg).clamp(0.0, 1.0),
            b: (c.b + db).clamp(0.0, 1.0),
            a: (c.a + da).clamp(0.0, 1.0),
        }
    };

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
                .filter(|n| matches!(n.kind, SceneNodeKind::Path(_)))
                .cloned()
                .collect(),
        }
    };

    if candidates.is_empty() {
        return ToolResult::text("No path nodes found to adjust.");
    }

    let mut commands: Vec<Command> = Vec::new();
    let mut count = 0usize;

    for node in &candidates {
        let mut new_node = node.clone();
        if let SceneNodeKind::Path(path) = &mut new_node.kind {
            match &mut path.fill.kind {
                FillKind::Solid(c) => *c = shift_color(*c),
                FillKind::Gradient(g) => {
                    for stop in &mut g.stops {
                        stop.color = shift_color(stop.color);
                    }
                }
                FillKind::FluidGradient(fg) => {
                    for pt in &mut fg.points {
                        pt.color = shift_color(pt.color);
                    }
                }
                FillKind::MeshGradient(mg) => {
                    for c in &mut mg.cell_colors {
                        *c = shift_color(*c);
                    }
                }
                FillKind::Pattern(p) => {
                    p.tile.map_pixels(|[r, g, b, a]| {
                        let c = shift_color(photonic_core::Color {
                            r: r as f32 / 255.0,
                            g: g as f32 / 255.0,
                            b: b as f32 / 255.0,
                            a: a as f32 / 255.0,
                        });
                        [
                            (c.r * 255.0).round().clamp(0.0, 255.0) as u8,
                            (c.g * 255.0).round().clamp(0.0, 255.0) as u8,
                            (c.b * 255.0).round().clamp(0.0, 255.0) as u8,
                            (c.a * 255.0).round().clamp(0.0, 255.0) as u8,
                        ]
                    });
                }
                FillKind::None => {}
            }
            if path.stroke.enabled {
                path.stroke.color = shift_color(path.stroke.color);
            }
            commands.push(Command::UpdateNode {
                old: node.clone(),
                new: new_node,
            });
            count += 1;
        }
    }

    if count == 0 {
        return ToolResult::text("Selected nodes contain no path nodes.");
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(Command::Batch(commands), &mut doc);
    history.schedule_mcp_checkpoint(format!("Adjust colors ({} nodes)", count));

    ToolResult::text(format!("Adjusted colors on {} node(s).", count)).with_data(
        serde_json::json!({
            "modified_count": count,
            "delta_r": dr, "delta_g": dg, "delta_b": db, "delta_a": da,
        }),
    )
}
pub async fn convert_to_grayscale(state: &AppState, args: ConvertToGrayscaleArgs) -> ToolResult {
    use photonic_core::style::FillKind;

    // 1. Collect candidate path nodes
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
                .filter(|n| matches!(n.kind, SceneNodeKind::Path(_)))
                .cloned()
                .collect(),
        }
    };

    if candidates.is_empty() {
        return ToolResult::text("No path nodes found to convert.");
    }

    // 2. Build UpdateNode commands
    let mut commands: Vec<Command> = Vec::new();
    let mut count = 0usize;

    for node in &candidates {
        let mut new_node = node.clone();
        let mut modified = false;

        if let SceneNodeKind::Path(path) = &mut new_node.kind {
            match &mut path.fill.kind {
                FillKind::Solid(c) => *c = c.to_grayscale(),
                FillKind::Gradient(g) => {
                    for stop in &mut g.stops {
                        stop.color = stop.color.to_grayscale();
                    }
                }
                FillKind::FluidGradient(fg) => {
                    for pt in &mut fg.points {
                        pt.color = pt.color.to_grayscale();
                    }
                }
                FillKind::MeshGradient(mg) => {
                    for c in &mut mg.cell_colors {
                        *c = c.to_grayscale();
                    }
                }
                FillKind::Pattern(p) => {
                    p.tile.map_rgb(|rgb| {
                        let l = photonic_core::raster::image::luma(rgb);
                        [l, l, l]
                    });
                }
                FillKind::None => {}
            }
            if path.stroke.enabled {
                path.stroke.color = path.stroke.color.to_grayscale();
            }
            modified = true;
        }

        if modified {
            commands.push(Command::UpdateNode {
                old: node.clone(),
                new: new_node,
            });
            count += 1;
        }
    }

    if count == 0 {
        return ToolResult::text("Selected nodes contain no path nodes.");
    }

    // 3. Execute as a single undo-able batch
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(Command::Batch(commands), &mut doc);
    history.schedule_mcp_checkpoint(format!("Convert to grayscale ({} nodes)", count));

    ToolResult::text(format!("Converted {} node(s) to grayscale.", count))
}
/// Distribute fill colors linearly across a set of path nodes.
/// The first and last nodes keep their solid fill colors; intermediate nodes
/// receive interpolated colors at evenly spaced positions along the range.
pub async fn blend_colors(state: &AppState, args: BlendColorsArgs) -> ToolResult {
    use photonic_core::style::FillKind;
    use photonic_core::Color;

    if args.node_ids.len() < 2 {
        return ToolResult::error("blend_colors requires at least 2 node_ids");
    }

    // 1. Collect nodes and validate they are all path nodes, then optionally sort.
    let nodes: Vec<SceneNode> = {
        let doc = state.document.lock().await;

        let mut out: Vec<SceneNode> = Vec::new();
        for &id in &args.node_ids {
            match doc.nodes.get(&id) {
                Some(n) => out.push(n.clone()),
                None => return ToolResult::error(format!("Node {} not found", id)),
            }
        }

        for n in &out {
            if !matches!(n.kind, SceneNodeKind::Path(_)) {
                return ToolResult::error(format!("Node '{}' is not a path node", n.name));
            }
        }

        // Sort by the requested direction.
        if let Some(dir) = &args.direction {
            match dir.as_str() {
                "horizontal" => {
                    out.sort_by(|a, b| {
                        let ax = path_center_x(a);
                        let bx = path_center_x(b);
                        ax.partial_cmp(&bx).unwrap_or(std::cmp::Ordering::Equal)
                    });
                }
                "vertical" => {
                    out.sort_by(|a, b| {
                        let ay = path_center_y(a);
                        let by_ = path_center_y(b);
                        ay.partial_cmp(&by_).unwrap_or(std::cmp::Ordering::Equal)
                    });
                }
                "depth" => {
                    // Build a global z-index from layer order.
                    let mut z_index: std::collections::HashMap<photonic_core::NodeId, usize> =
                        std::collections::HashMap::new();
                    let mut z = 0usize;
                    for layer_id in &doc.layer_order {
                        if let Some(layer) = doc.layers.get(layer_id) {
                            for &nid in &layer.node_ids {
                                z_index.insert(nid, z);
                                z += 1;
                            }
                        }
                    }
                    out.sort_by_key(|n| z_index.get(&n.id).copied().unwrap_or(0));
                }
                other => {
                    return ToolResult::error(format!(
                        "Unknown direction '{}'; use 'horizontal', 'vertical', or 'depth'",
                        other
                    ));
                }
            }
        }

        out
    };

    // 2. Extract solid fill colors from the first and last nodes.
    let start_color = match &nodes[0].kind {
        SceneNodeKind::Path(p) => match &p.fill.kind {
            FillKind::Solid(c) => *c,
            _ => return ToolResult::error("First node must have a solid fill for blending"),
        },
        _ => unreachable!(),
    };
    let end_color = match &nodes[nodes.len() - 1].kind {
        SceneNodeKind::Path(p) => match &p.fill.kind {
            FillKind::Solid(c) => *c,
            _ => return ToolResult::error("Last node must have a solid fill for blending"),
        },
        _ => unreachable!(),
    };

    // 3. Build UpdateNode commands for intermediate nodes only.
    let n = nodes.len();
    let mut commands: Vec<Command> = Vec::new();

    for (i, node) in nodes.iter().enumerate() {
        if i == 0 || i == n - 1 {
            continue; // endpoints keep their own colors
        }
        let t = i as f32 / (n - 1) as f32;
        let blended = Color {
            r: start_color.r + t * (end_color.r - start_color.r),
            g: start_color.g + t * (end_color.g - start_color.g),
            b: start_color.b + t * (end_color.b - start_color.b),
            a: start_color.a + t * (end_color.a - start_color.a),
        };
        let mut new_node = node.clone();
        if let SceneNodeKind::Path(ref mut p) = new_node.kind {
            p.fill.kind = FillKind::Solid(blended);
        }
        commands.push(Command::UpdateNode {
            old: node.clone(),
            new: new_node,
        });
    }

    if commands.is_empty() {
        return ToolResult::text(
            "No intermediate nodes to update (need at least 3 nodes to interpolate).",
        );
    }

    let updated = commands.len();
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(Command::Batch(commands), &mut doc);
    history.schedule_mcp_checkpoint(format!("Blend colors ({} nodes)", n));

    ToolResult::text(format!(
        "Blended colors across {} nodes ({} intermediate node(s) updated).",
        n, updated
    ))
    .with_data(serde_json::json!({
        "start_color": start_color.to_hex(),
        "end_color":   end_color.to_hex(),
        "node_count":  n,
        "updated_count": updated,
    }))
}
pub async fn recolor_artwork(state: &AppState, args: RecolorArtworkArgs) -> ToolResult {
    use photonic_core::color::Color;
    use photonic_core::style::FillKind;

    if args.palette.is_empty() {
        return ToolResult::error("palette must contain at least one color");
    }

    // Parse palette.
    let mut palette: Vec<[f32; 4]> = Vec::with_capacity(args.palette.len());
    for hex in &args.palette {
        match Color::from_hex(hex) {
            Some(c) => palette.push([c.r, c.g, c.b, c.a]),
            None => return ToolResult::error(format!("Invalid palette color: '{}'", hex)),
        }
    }

    let mut doc = state.document.lock().await;

    // Determine which nodes to process.
    let ids: Vec<NodeId> = if args.node_ids.is_empty() {
        doc.nodes.keys().cloned().collect()
    } else {
        for id in &args.node_ids {
            if !doc.nodes.contains_key(id) {
                return ToolResult::error(format!("Node {} not found", id));
            }
        }
        args.node_ids.clone()
    };

    // Helper: Euclidean RGB distance.
    fn color_dist(a: [f32; 4], b: [f32; 4]) -> f32 {
        let dr = a[0] - b[0];
        let dg = a[1] - b[1];
        let db = a[2] - b[2];
        dr * dr + dg * dg + db * db
    }
    fn nearest(c: [f32; 4], palette: &[[f32; 4]]) -> [f32; 4] {
        *palette
            .iter()
            .min_by(|a, b| {
                color_dist(c, **a)
                    .partial_cmp(&color_dist(c, **b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap()
    }

    let mut commands: Vec<Command> = Vec::new();
    let mut recolored = 0usize;

    for id in &ids {
        let node = match doc.nodes.get(id) {
            Some(n) => n.clone(),
            None => continue,
        };
        let pn = match &node.kind {
            SceneNodeKind::Path(p) => p.clone(),
            _ => continue,
        };
        if !pn.fill.enabled {
            continue;
        }
        let orig = match &pn.fill.kind {
            FillKind::Solid(c) => [c.r, c.g, c.b, c.a],
            _ => continue, // Only remap solid fills.
        };
        let target = nearest(orig, &palette);
        if (orig[0] - target[0]).abs() < 1e-6
            && (orig[1] - target[1]).abs() < 1e-6
            && (orig[2] - target[2]).abs() < 1e-6
        {
            continue; // Already that color.
        }
        let mut new_node = node.clone();
        if let SceneNodeKind::Path(ref mut p) = new_node.kind {
            p.fill.kind = FillKind::Solid(Color {
                r: target[0],
                g: target[1],
                b: target[2],
                a: target[3],
            });
        }
        commands.push(Command::UpdateNode {
            old: node,
            new: new_node,
        });
        recolored += 1;
    }

    if commands.is_empty() {
        return ToolResult::text(
            "No fills were remapped — all colors already in palette or no solid fills found",
        );
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(Command::Batch(commands), &mut doc);

    ToolResult::text(format!(
        "Recolored {} node(s) to nearest palette colors",
        recolored
    ))
    .with_data(serde_json::json!({ "recolored_count": recolored }))
}
pub async fn get_recent_colors(state: &AppState, _args: GetRecentColorsArgs) -> ToolResult {
    let doc = state.document.lock().await;
    let colors: Vec<serde_json::Value> = doc
        .recent_colors
        .iter()
        .map(|c| serde_json::json!({ "r": c.r, "g": c.g, "b": c.b, "a": c.a }))
        .collect();
    ToolResult::text(format!("{} recent color(s)", colors.len())).with_data(serde_json::json!({
        "count": colors.len(),
        "colors": colors,
    }))
}
/// Assign a path node as the blend spine for a group node.
pub async fn set_blend_spine(state: &AppState, args: SetBlendSpineArgs) -> ToolResult {
    tracing::debug!("tool: set_blend_spine");
    let mut doc = state.document.lock().await;

    let group_id = uuid::Uuid::parse_str(&args.group_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.group_id).map(|n| n.id));
    let group_id = match group_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Group '{}' not found.", args.group_id)),
    };

    let path_id = uuid::Uuid::parse_str(&args.path_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.path_id).map(|n| n.id));
    let path_id = match path_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Path '{}' not found.", args.path_id)),
    };

    let group_node = match doc.nodes.get(&group_id) {
        Some(n) if matches!(n.kind, SceneNodeKind::Group(_)) => n.clone(),
        Some(_) => return ToolResult::error(format!("Node '{}' is not a group.", args.group_id)),
        None => return ToolResult::error(format!("Group '{}' not found.", args.group_id)),
    };

    // Validate path node exists and is a path
    match doc.nodes.get(&path_id) {
        Some(n) if matches!(n.kind, SceneNodeKind::Path(_)) => {}
        Some(_) => {
            return ToolResult::error(format!("Node '{}' is not a path node.", args.path_id))
        }
        None => return ToolResult::error(format!("Path '{}' not found.", args.path_id)),
    }

    let mut new_group = group_node.clone();
    if let SceneNodeKind::Group(ref mut gn) = new_group.kind {
        gn.blend_spine_id = Some(path_id);
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::UpdateNode {
            old: group_node,
            new: new_group,
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Blend spine of group '{}' set to path '{}'.",
        args.group_id, args.path_id
    ))
    .with_data(serde_json::json!({
        "group_id": group_id.to_string(),
        "path_id": path_id.to_string()
    }))
}
/// Clear the blend spine assignment from a group node.
pub async fn clear_blend_spine(state: &AppState, args: ClearBlendSpineArgs) -> ToolResult {
    tracing::debug!("tool: clear_blend_spine");
    let mut doc = state.document.lock().await;

    let group_id = uuid::Uuid::parse_str(&args.group_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.group_id).map(|n| n.id));
    let group_id = match group_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Group '{}' not found.", args.group_id)),
    };

    let group_node = match doc.nodes.get(&group_id) {
        Some(n) if matches!(n.kind, SceneNodeKind::Group(_)) => n.clone(),
        Some(_) => return ToolResult::error(format!("Node '{}' is not a group.", args.group_id)),
        None => return ToolResult::error(format!("Group '{}' not found.", args.group_id)),
    };

    if let SceneNodeKind::Group(ref gn) = group_node.kind {
        if gn.blend_spine_id.is_none() {
            return ToolResult::text(format!(
                "Group '{}' has no blend spine assigned.",
                args.group_id
            ));
        }
    }

    let mut new_group = group_node.clone();
    if let SceneNodeKind::Group(ref mut gn) = new_group.kind {
        gn.blend_spine_id = None;
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::UpdateNode {
            old: group_node,
            new: new_group,
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Blend spine cleared from group '{}'.",
        args.group_id
    ))
    .with_data(serde_json::json!({ "group_id": group_id.to_string() }))
}
/// Reverse the direction of the blend spine path in a group node.
pub async fn reverse_blend_spine(state: &AppState, args: ReverseBlendSpineArgs) -> ToolResult {
    tracing::debug!("tool: reverse_blend_spine");
    let mut doc = state.document.lock().await;

    let group_id = uuid::Uuid::parse_str(&args.group_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.group_id).map(|n| n.id));
    let group_id = match group_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Group '{}' not found.", args.group_id)),
    };

    // Resolve the spine ID from the group
    let spine_id = match doc.nodes.get(&group_id) {
        Some(n) => match &n.kind {
            SceneNodeKind::Group(gn) => match gn.blend_spine_id {
                Some(sid) => sid,
                None => {
                    return ToolResult::error(format!(
                        "Group '{}' has no blend spine assigned.",
                        args.group_id
                    ))
                }
            },
            _ => return ToolResult::error(format!("Node '{}' is not a group.", args.group_id)),
        },
        None => return ToolResult::error(format!("Group '{}' not found.", args.group_id)),
    };

    let spine_node = match doc.nodes.get(&spine_id) {
        Some(n) if matches!(n.kind, SceneNodeKind::Path(_)) => n.clone(),
        Some(_) => return ToolResult::error("Blend spine node is not a path."),
        None => return ToolResult::error("Blend spine node not found in document."),
    };

    let mut new_spine = spine_node.clone();
    if let SceneNodeKind::Path(ref mut pn) = new_spine.kind {
        pn.path_data = pn.path_data.reverse();
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::UpdateNode {
            old: spine_node,
            new: new_spine,
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Blend spine of group '{}' reversed.",
        args.group_id
    ))
    .with_data(serde_json::json!({
        "group_id": group_id.to_string(),
        "spine_id": spine_id.to_string()
    }))
}
/// Expand a blend group into individual discrete objects at the parent layer.
/// Semantically equivalent to Illustrator's Object > Blend > Expand.
pub async fn expand_blend(state: &AppState, args: ExpandBlendArgs) -> ToolResult {
    tracing::debug!("tool: expand_blend");
    let mut doc = state.document.lock().await;

    let group_id_str = args.group_id.clone();
    let group_id = uuid::Uuid::parse_str(&group_id_str)
        .ok()
        .or_else(|| doc.find_node_by_name(&group_id_str).map(|n| n.id));
    let group_id = match group_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Group '{}' not found.", group_id_str)),
    };

    let group_node = match doc.nodes.get(&group_id) {
        Some(n) if matches!(n.kind, SceneNodeKind::Group(_)) => n.clone(),
        Some(_) => return ToolResult::error(format!("Node '{}' is not a group.", group_id_str)),
        None => return ToolResult::error(format!("Group '{}' not found.", group_id_str)),
    };

    let children = match &group_node.kind {
        SceneNodeKind::Group(g) => g.children.clone(),
        _ => unreachable!(),
    };

    let child_count = children.len();

    let (layer_id, group_index) = match doc.node_layer_and_index(&group_id) {
        Some(v) => v,
        None => return ToolResult::error("Blend group has no layer position."),
    };

    let cmd = Command::UngroupNodes {
        group: group_node,
        layer_id,
        group_index,
        children,
    };
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!(
        "Expanded blend group '{}' into {} individual object(s).",
        group_id_str, child_count
    ))
    .with_data(serde_json::json!({
        "group_id": group_id.to_string(),
        "child_count": child_count
    }))
}
/// Set per-instance fill and/or stroke color overrides on a symbol instance node.
pub async fn set_symbol_override(state: &AppState, args: SetSymbolOverrideArgs) -> ToolResult {
    tracing::debug!("tool: set_symbol_override");
    let mut doc = state.document.lock().await;

    let node_id = uuid::Uuid::parse_str(&args.node_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.node_id).map(|n| n.id));
    let node_id = match node_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    let node = match doc.nodes.get(&node_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    if node.symbol_ref.is_none() {
        return ToolResult::error(format!("Node '{}' is not a symbol instance.", args.node_id));
    }

    let mut new_node = node.clone();
    if let Some(hex) = args.fill_hex {
        new_node.symbol_fill_override = Some(hex);
    }
    if let Some(hex) = args.stroke_hex {
        new_node.symbol_stroke_override = Some(hex);
    }

    let fill_out = new_node.symbol_fill_override.clone();
    let stroke_out = new_node.symbol_stroke_override.clone();

    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::UpdateNode {
            old: node,
            new: new_node,
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Symbol overrides set on '{}': fill={:?}, stroke={:?}.",
        args.node_id, fill_out, stroke_out
    ))
    .with_data(serde_json::json!({
        "node_id": node_id.to_string(),
        "fill_override": fill_out,
        "stroke_override": stroke_out
    }))
}
/// Clear all per-instance color overrides on a symbol instance node.
pub async fn clear_symbol_overrides(
    state: &AppState,
    args: ClearSymbolOverridesArgs,
) -> ToolResult {
    tracing::debug!("tool: clear_symbol_overrides");
    let mut doc = state.document.lock().await;

    let node_id = uuid::Uuid::parse_str(&args.node_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.node_id).map(|n| n.id));
    let node_id = match node_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    let node = match doc.nodes.get(&node_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    if node.symbol_ref.is_none() {
        return ToolResult::error(format!("Node '{}' is not a symbol instance.", args.node_id));
    }

    if node.symbol_fill_override.is_none() && node.symbol_stroke_override.is_none() {
        return ToolResult::text(format!("Node '{}' has no symbol overrides.", args.node_id));
    }

    let mut new_node = node.clone();
    new_node.symbol_fill_override = None;
    new_node.symbol_stroke_override = None;

    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::UpdateNode {
            old: node,
            new: new_node,
        },
        &mut doc,
    );

    ToolResult::text(format!("Symbol overrides cleared on '{}'.", args.node_id))
        .with_data(serde_json::json!({ "node_id": node_id.to_string() }))
}
pub async fn copy_appearance(state: &AppState, args: CopyAppearanceArgs) -> ToolResult {
    if args.target_ids.is_empty() {
        return ToolResult::text("No target nodes specified.");
    }
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    // Resolve source node
    let src_id = {
        let id_res = uuid::Uuid::parse_str(&args.source_id);
        if let Ok(uuid) = id_res {
            if doc.nodes.contains_key(&uuid) {
                uuid
            } else {
                return ToolResult::text(format!("Source node '{}' not found.", args.source_id));
            }
        } else {
            match doc.nodes.values().find(|n| n.name == args.source_id) {
                Some(n) => n.id,
                None => {
                    return ToolResult::text(format!("Source node '{}' not found.", args.source_id))
                }
            }
        }
    };

    let (src_fill, src_stroke, src_opacity) = {
        let src = &doc.nodes[&src_id];
        let fill = if let SceneNodeKind::Path(ref p) = src.kind {
            Some(p.fill.clone())
        } else {
            None
        };
        let stroke = if let SceneNodeKind::Path(ref p) = src.kind {
            Some(p.stroke.clone())
        } else {
            None
        };
        (fill, stroke, src.opacity)
    };

    let mut cmds: Vec<Command> = Vec::new();
    let mut updated = 0usize;

    for tid_str in &args.target_ids {
        let tid = if let Ok(uuid) = uuid::Uuid::parse_str(tid_str) {
            if doc.nodes.contains_key(&uuid) {
                uuid
            } else {
                continue;
            }
        } else {
            match doc.nodes.values().find(|n| n.name == *tid_str) {
                Some(n) => n.id,
                None => continue,
            }
        };

        if tid == src_id {
            continue;
        }
        let mut new_node = doc.nodes[&tid].clone();
        let old_node = new_node.clone();

        if args.copy_opacity {
            new_node.opacity = src_opacity;
        }
        if let SceneNodeKind::Path(ref mut p) = new_node.kind {
            if args.copy_fill {
                if let Some(ref f) = src_fill {
                    p.fill = f.clone();
                }
            }
            if args.copy_stroke {
                if let Some(ref s) = src_stroke {
                    p.stroke = s.clone();
                }
            }
        }
        cmds.push(Command::UpdateNode {
            old: old_node,
            new: new_node,
        });
        updated += 1;
    }

    if cmds.is_empty() {
        return ToolResult::text("No valid target nodes found.");
    }

    let batch = if cmds.len() == 1 {
        cmds.remove(0)
    } else {
        Command::Batch(cmds)
    };
    history.execute_discrete(batch, &mut doc);
    ToolResult::text(format!(
        "Copied appearance from '{}' to {} node(s).",
        args.source_id, updated
    ))
    .with_data(serde_json::json!({ "updated": updated }))
}
