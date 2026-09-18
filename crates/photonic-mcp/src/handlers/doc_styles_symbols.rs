use crate::protocol::{
    ApplyGraphicStyleArgs, ApplyWidthProfileArgs, BreakLinkToSymbolArgs, DefineGraphicStyleArgs,
    DefineSymbolArgs, DefineWidthProfileArgs, DeleteGraphicStyleArgs, DeleteSymbolArgs,
    DeleteWidthProfileArgs, LoadSymbolLibraryArgs, PlaceSymbolArgs, SpraySymbolInstancesArgs,
    ToolResult,
};
use crate::server::AppState;

/// Define (or update) a named graphic style.
pub async fn define_graphic_style(state: &AppState, args: DefineGraphicStyleArgs) -> ToolResult {
    tracing::debug!("tool: define_graphic_style");
    use photonic_core::GraphicStyle;

    if args.name.trim().is_empty() {
        return ToolResult::error("Graphic style name must not be empty.");
    }

    let doc = state.document.lock().await;
    let (fill_json, stroke_json, opacity) = if let Some(ref nid) = args.node_id {
        // Capture from a node
        let node_id = uuid::Uuid::parse_str(nid)
            .ok()
            .or_else(|| doc.find_node_by_name(nid).map(|n| n.id));
        let node = node_id.and_then(|id| doc.nodes.get(&id)).cloned();
        drop(doc);
        match node {
            None => return ToolResult::error(format!("Node '{}' not found.", nid)),
            Some(n) => {
                use photonic_core::node::SceneNodeKind;
                let (fill, stroke) = match &n.kind {
                    SceneNodeKind::Path(pn) => (pn.fill.clone(), pn.stroke.clone()),
                    SceneNodeKind::Text(tn) => {
                        use photonic_core::style::Stroke;
                        (tn.fill.clone(), Stroke::none())
                    }
                    SceneNodeKind::Group(_) => {
                        use photonic_core::style::{Fill, Stroke};
                        (Fill::default(), Stroke::none())
                    }
                    // raster: no fill/stroke to capture
                    SceneNodeKind::Raster(_) => {
                        use photonic_core::style::{Fill, Stroke};
                        (Fill::default(), Stroke::none())
                    }
                };
                let fj = serde_json::to_string(&fill).unwrap_or_default();
                let sj = serde_json::to_string(&stroke).unwrap_or_default();
                (fj, sj, n.opacity)
            }
        }
    } else {
        drop(doc);
        // Build from explicit parameters
        use photonic_core::style::{Fill, Stroke};
        use photonic_core::Color;
        let fill = if let Some(ref hex) = args.fill_hex {
            Color::from_hex(hex).map(Fill::solid).unwrap_or_default()
        } else {
            Fill::default()
        };
        let stroke = if let (Some(ref hex), Some(w)) = (&args.stroke_hex, args.stroke_width) {
            Color::from_hex(hex)
                .map(|c| Stroke::solid(c, w))
                .unwrap_or_default()
        } else {
            Stroke::none()
        };
        let fj = serde_json::to_string(&fill).unwrap_or_default();
        let sj = serde_json::to_string(&stroke).unwrap_or_default();
        (fj, sj, args.opacity.unwrap_or(1.0))
    };

    let mut doc = state.document.lock().await;
    let name = args.name.trim().to_string();
    let style = GraphicStyle::new(&name, fill_json, stroke_json, opacity);
    if let Some(existing) = doc.graphic_styles.iter_mut().find(|s| s.name == name) {
        *existing = style;
        ToolResult::text(format!("Updated graphic style '{}'.", name))
    } else {
        doc.graphic_styles.push(style);
        ToolResult::text(format!("Defined graphic style '{}'.", name))
    }
}

/// List all named graphic styles in the document.
pub async fn list_graphic_styles(state: &AppState) -> ToolResult {
    tracing::debug!("tool: list_graphic_styles");
    let doc = state.document.lock().await;
    let styles: Vec<serde_json::Value> = doc
        .graphic_styles
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "opacity": s.opacity,
                "id": s.id.to_string(),
            })
        })
        .collect();
    ToolResult::text(format!("{} graphic style(s).", styles.len()))
        .with_data(serde_json::json!({ "styles": styles }))
}

/// Apply a named graphic style to one or more nodes.
pub async fn apply_graphic_style(state: &AppState, args: ApplyGraphicStyleArgs) -> ToolResult {
    tracing::debug!("tool: apply_graphic_style");
    use photonic_core::history::Command;
    use photonic_core::node::SceneNodeKind;
    use photonic_core::style::{Fill, Stroke};

    // Read style definition first (drop lock before re-acquiring with history)
    let style_data = {
        let doc = state.document.lock().await;
        doc.graphic_styles
            .iter()
            .find(|s| s.name == args.name)
            .cloned()
    };
    let style = match style_data {
        None => return ToolResult::error(format!("No graphic style named '{}'.", args.name)),
        Some(s) => s,
    };

    let fill: Fill = serde_json::from_str(&style.fill_json).unwrap_or_default();
    let stroke: Stroke = serde_json::from_str(&style.stroke_json).unwrap_or_default();
    let opacity = style.opacity;

    let doc = state.document.lock().await;
    let mut commands: Vec<Command> = Vec::new();

    for id_str in &args.node_ids {
        let node_id = uuid::Uuid::parse_str(id_str)
            .ok()
            .or_else(|| doc.find_node_by_name(id_str).map(|n| n.id));
        if let Some(nid) = node_id {
            if let Some(node) = doc.nodes.get(&nid).cloned() {
                let mut new_node = node.clone();
                new_node.opacity = opacity;
                match &mut new_node.kind {
                    SceneNodeKind::Path(pn) => {
                        pn.fill = fill.clone();
                        pn.stroke = stroke.clone();
                    }
                    SceneNodeKind::Text(tn) => {
                        tn.fill = fill.clone();
                    }
                    SceneNodeKind::Group(_) => {}
                    // raster: no fill/stroke to apply
                    SceneNodeKind::Raster(_) => {}
                }
                commands.push(Command::UpdateNode {
                    old: node,
                    new: new_node,
                });
            }
        }
    }

    if commands.is_empty() {
        return ToolResult::text("No matching nodes found.");
    }

    let count = commands.len();
    drop(doc);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(Command::Batch(commands), &mut doc);
    drop(history);

    ToolResult::text(format!(
        "Applied graphic style '{}' to {} node(s).",
        args.name, count
    ))
}

/// Delete a named graphic style.
pub async fn delete_graphic_style(state: &AppState, args: DeleteGraphicStyleArgs) -> ToolResult {
    tracing::debug!("tool: delete_graphic_style");
    let mut doc = state.document.lock().await;
    let before = doc.graphic_styles.len();
    doc.graphic_styles.retain(|s| s.name != args.name);
    if doc.graphic_styles.len() < before {
        ToolResult::text(format!("Deleted graphic style '{}'.", args.name))
    } else {
        ToolResult::error(format!("No graphic style named '{}' found.", args.name))
    }
}

/// Define (or overwrite) a named variable-width stroke profile.
pub async fn define_width_profile(state: &AppState, args: DefineWidthProfileArgs) -> ToolResult {
    tracing::debug!("tool: define_width_profile");
    use photonic_core::WidthProfile;

    if args.name.trim().is_empty() {
        return ToolResult::error("Width profile name must not be empty.");
    }
    if args.widths.len() < 2 {
        return ToolResult::error("Width profile must have at least 2 width values.");
    }
    if args.widths.iter().any(|&w| w < 0.0) {
        return ToolResult::error("All width values must be non-negative.");
    }

    let name = args.name.trim().to_string();
    let profile = WidthProfile::new(&name, args.widths);
    let mut doc = state.document.lock().await;

    if let Some(existing) = doc.width_profiles.iter_mut().find(|p| p.name == name) {
        *existing = profile;
        ToolResult::text(format!("Updated width profile '{}'.", name))
    } else {
        doc.width_profiles.push(profile);
        ToolResult::text(format!("Defined width profile '{}'.", name))
    }
}

/// List all named variable-width stroke profiles in the document.
pub async fn list_width_profiles(state: &AppState) -> ToolResult {
    tracing::debug!("tool: list_width_profiles");
    let doc = state.document.lock().await;
    let profiles: Vec<serde_json::Value> = doc
        .width_profiles
        .iter()
        .map(|p| {
            serde_json::json!({
                "name": p.name,
                "widths": p.widths,
                "average_width": p.average_width(),
                "id": p.id.to_string(),
            })
        })
        .collect();
    ToolResult::text(format!("{} width profile(s).", profiles.len()))
        .with_data(serde_json::json!({ "profiles": profiles }))
}

/// Apply a named width profile to path nodes (sets stroke width to the profile average).
pub async fn apply_width_profile(state: &AppState, args: ApplyWidthProfileArgs) -> ToolResult {
    tracing::debug!("tool: apply_width_profile");
    use photonic_core::history::Command;
    use photonic_core::node::SceneNodeKind;

    // Read profile (drop lock before re-acquiring)
    let (profile_id, avg_width) = {
        let doc = state.document.lock().await;
        match doc.width_profiles.iter().find(|p| p.name == args.name) {
            None => return ToolResult::error(format!("No width profile named '{}'.", args.name)),
            Some(p) => (p.id, p.average_width()),
        }
    };

    let doc = state.document.lock().await;
    let mut commands: Vec<Command> = Vec::new();

    for id_str in &args.node_ids {
        let node_id = uuid::Uuid::parse_str(id_str)
            .ok()
            .or_else(|| doc.find_node_by_name(id_str).map(|n| n.id));
        if let Some(nid) = node_id {
            if let Some(node) = doc.nodes.get(&nid).cloned() {
                if let SceneNodeKind::Path(ref pn) = node.kind {
                    let mut new_node = node.clone();
                    if let SceneNodeKind::Path(ref mut pn2) = new_node.kind {
                        // Legacy uniform fallback + the profile link that drives
                        // true variable-width rendering.
                        pn2.stroke.width = avg_width;
                        pn2.stroke.width_profile_id = Some(profile_id);
                    }
                    let _ = pn; // suppress warning
                    commands.push(Command::UpdateNode {
                        old: node,
                        new: new_node,
                    });
                }
            }
        }
    }

    if commands.is_empty() {
        return ToolResult::text("No matching path nodes found.");
    }

    let count = commands.len();
    drop(doc);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(Command::Batch(commands), &mut doc);
    drop(history);

    ToolResult::text(format!(
        "Applied width profile '{}' (avg {:.1}px) to {} path node(s).",
        args.name, avg_width, count
    ))
}

/// Delete a named width profile.
pub async fn delete_width_profile(state: &AppState, args: DeleteWidthProfileArgs) -> ToolResult {
    tracing::debug!("tool: delete_width_profile");
    let mut doc = state.document.lock().await;
    let before = doc.width_profiles.len();
    doc.width_profiles.retain(|p| p.name != args.name);
    if doc.width_profiles.len() < before {
        ToolResult::text(format!("Deleted width profile '{}'.", args.name))
    } else {
        ToolResult::error(format!("No width profile named '{}' found.", args.name))
    }
}

/// Designate a node as a named symbol master.
pub async fn define_symbol(state: &AppState, args: DefineSymbolArgs) -> ToolResult {
    tracing::debug!("tool: define_symbol");
    use photonic_core::Symbol;

    if args.name.trim().is_empty() {
        return ToolResult::error("Symbol name must not be empty.");
    }
    let name = args.name.trim().to_string();
    let mut doc = state.document.lock().await;

    let node_id = uuid::Uuid::parse_str(&args.node_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.node_id).map(|n| n.id));
    let node_id = match node_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    if !doc.nodes.contains_key(&node_id) {
        return ToolResult::error(format!("Node '{}' not found.", args.node_id));
    }

    // Upsert the symbol.
    let action = if let Some(existing) = doc.symbols.iter_mut().find(|s| s.name == name) {
        existing.master_node_id = node_id;
        "Updated"
    } else {
        doc.symbols.push(Symbol::new(&name, node_id));
        "Defined"
    };

    ToolResult::text(format!(
        "{action} symbol '{name}' (master: {}).",
        args.node_id
    ))
    .with_data(serde_json::json!({ "symbol_name": name, "master_node_id": node_id }))
}

/// List all symbols defined in the document.
pub async fn list_symbols(state: &AppState) -> ToolResult {
    tracing::debug!("tool: list_symbols");
    let doc = state.document.lock().await;
    if doc.symbols.is_empty() {
        return ToolResult::text("No symbols defined.")
            .with_data(serde_json::json!({ "symbols": [] }));
    }
    let syms: Vec<_> = doc.symbols.iter().map(|s| serde_json::json!({
        "name": s.name,
        "id": s.id,
        "master_node_id": s.master_node_id,
        "master_name": doc.nodes.get(&s.master_node_id).map(|n| n.name.clone()).unwrap_or_default(),
    })).collect();
    ToolResult::text(format!("{} symbol(s).", syms.len()))
        .with_data(serde_json::json!({ "symbols": syms }))
}

/// Place an instance of a named symbol at the given position.
pub async fn place_symbol(state: &AppState, args: PlaceSymbolArgs) -> ToolResult {
    tracing::debug!("tool: place_symbol");
    use photonic_core::history::Command;
    use photonic_core::transform::Transform;

    let mut doc = state.document.lock().await;

    let symbol = match doc.symbols.iter().find(|s| s.name == args.symbol_name) {
        Some(s) => s.clone(),
        None => return ToolResult::error(format!("Symbol '{}' not found.", args.symbol_name)),
    };

    let master = match doc.nodes.get(&symbol.master_node_id) {
        Some(n) => n.clone(),
        None => {
            return ToolResult::error("Symbol master node is missing from document.".to_string())
        }
    };

    // Clone the master to create an instance.
    let layer_id = match doc
        .active_layer_id
        .or_else(|| doc.layer_order.first().copied())
    {
        Some(id) => id,
        None => return ToolResult::error("No layer available."),
    };
    let instance_name = format!("{} (instance)", symbol.name);
    let mut instance = master.clone();
    instance.id = uuid::Uuid::new_v4();
    instance.name = instance_name;
    instance.layer_id = layer_id;
    instance.transform = Transform::translate(args.x, args.y);
    instance.symbol_ref = Some(symbol.id);

    let instance_id = instance.id;
    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::AddNode {
            node: instance,
            layer_id: Some(layer_id),
        },
        &mut doc,
    );
    drop(history);

    ToolResult::text(format!(
        "Placed instance of '{}' at ({:.1}, {:.1}).",
        args.symbol_name, args.x, args.y
    ))
    .with_data(serde_json::json!({ "instance_id": instance_id, "symbol_name": args.symbol_name }))
}

/// Break the link between an instance node and its symbol master.
pub async fn break_link_to_symbol(state: &AppState, args: BreakLinkToSymbolArgs) -> ToolResult {
    tracing::debug!("tool: break_link_to_symbol");
    use photonic_core::history::Command;

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
    // Bake the current master geometry/style (+ overrides) into the instance so
    // breaking the link preserves what's rendered rather than reverting to the
    // frozen copy captured at placement time.
    new_node.kind = doc.resolve_render_node(&node).kind.clone();
    new_node.symbol_ref = None;
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
    drop(history);

    ToolResult::text(format!("Broke symbol link on node '{}'.", args.node_id))
        .with_data(serde_json::json!({ "node_id": node_id }))
}

/// Delete a named symbol from the registry (instances become unlinked standalone nodes).
pub async fn delete_symbol(state: &AppState, args: DeleteSymbolArgs) -> ToolResult {
    tracing::debug!("tool: delete_symbol");
    let mut doc = state.document.lock().await;
    let before = doc.symbols.len();
    doc.symbols.retain(|s| s.name != args.name);
    if doc.symbols.len() < before {
        ToolResult::text(format!(
            "Deleted symbol '{}'. Existing instances remain as standalone nodes.",
            args.name
        ))
    } else {
        ToolResult::error(format!("No symbol named '{}' found.", args.name))
    }
}

/// Spray multiple instances of a named symbol scattered around a center point.
/// Uses the golden-angle spiral distribution for even, natural-looking scatter.
pub async fn spray_symbol_instances(
    state: &AppState,
    args: SpraySymbolInstancesArgs,
) -> ToolResult {
    tracing::debug!(
        "tool: spray_symbol_instances name={} count={}",
        args.symbol_name,
        args.count
    );
    use photonic_core::history::Command;
    use photonic_core::transform::Transform;

    let count = args.count.clamp(1, 200);
    let spread = if args.spread <= 0.0 {
        100.0
    } else {
        args.spread
    };

    let mut doc = state.document.lock().await;

    let symbol = match doc.symbols.iter().find(|s| s.name == args.symbol_name) {
        Some(s) => s.clone(),
        None => return ToolResult::error(format!("Symbol '{}' not found.", args.symbol_name)),
    };

    let master = match doc.nodes.get(&symbol.master_node_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error("Symbol master node is missing from document."),
    };

    let layer_id = match doc
        .active_layer_id
        .or_else(|| doc.layer_order.first().copied())
    {
        Some(id) => id,
        None => return ToolResult::error("No layer available."),
    };

    // Golden-angle spiral: even distribution of N points within a disk.
    const GOLDEN_ANGLE: f64 = std::f64::consts::TAU * (1.0 - 1.0 / 1.618_033_988_749_895);
    let mut instance_ids = Vec::with_capacity(count);
    let mut history = state.history.lock().await;

    for i in 0..count {
        let r = spread * ((i as f64 + 0.5) / count as f64).sqrt();
        let theta = i as f64 * GOLDEN_ANGLE;
        let ix = args.x + r * theta.cos();
        let iy = args.y + r * theta.sin();

        let instance_name = format!("{} (instance {})", symbol.name, i + 1);
        let mut instance = master.clone();
        instance.id = uuid::Uuid::new_v4();
        instance.name = instance_name;
        instance.layer_id = layer_id;
        instance.transform = Transform::translate(ix, iy);
        instance.symbol_ref = Some(symbol.id);
        instance_ids.push(instance.id);
        history.execute_discrete(
            Command::AddNode {
                node: instance,
                layer_id: Some(layer_id),
            },
            &mut doc,
        );
    }

    let ids: Vec<String> = instance_ids.iter().map(|id| id.to_string()).collect();
    ToolResult::text(format!(
        "Sprayed {} instance(s) of '{}' around ({:.1}, {:.1}) with spread={:.1}.",
        count, args.symbol_name, args.x, args.y, spread
    ))
    .with_data(serde_json::json!({
        "symbol_name": args.symbol_name,
        "count": count,
        "instance_ids": ids
    }))
}

/// Built-in symbol library definitions: (name, svg_path_d)
fn builtin_symbols(library: &str) -> Option<Vec<(&'static str, &'static str)>> {
    match library {
        "arrows" => Some(vec![
            (
                "arrow-right",
                "M10,45 L70,45 L70,30 L90,50 L70,70 L70,55 L10,55 Z",
            ),
            (
                "arrow-left",
                "M90,45 L30,45 L30,30 L10,50 L30,70 L30,55 L90,55 Z",
            ),
            (
                "arrow-up",
                "M45,90 L45,30 L30,30 L50,10 L70,30 L55,30 L55,90 Z",
            ),
            (
                "arrow-down",
                "M45,10 L45,70 L30,70 L50,90 L70,70 L55,70 L55,10 Z",
            ),
            (
                "double-arrow-h",
                "M10,50 L25,35 L25,43 L75,43 L75,35 L90,50 L75,65 L75,57 L25,57 L25,65 Z",
            ),
            (
                "arrow-ne",
                "M20,80 L70,30 L45,30 L45,20 L80,20 L80,55 L70,55 L70,30",
            ),
        ]),
        "shapes" => Some(vec![
            ("diamond", "M50,5 L95,50 L50,95 L5,50 Z"),
            ("hexagon", "M50,5 L91,27 L91,73 L50,95 L9,73 L9,27 Z"),
            ("pentagon", "M50,5 L95,34 L79,88 L21,88 L5,34 Z"),
            (
                "star-5pt",
                "M50,5 L61,35 L95,35 L68,57 L79,91 L50,70 L21,91 L32,57 L5,35 L39,35 Z",
            ),
            (
                "cross",
                "M35,5 L65,5 L65,35 L95,35 L95,65 L65,65 L65,95 L35,95 L35,65 L5,65 L5,35 L35,35 Z",
            ),
            ("checkmark", "M10,50 L35,75 L90,20"),
        ]),
        "ui" => Some(vec![
            (
                "checkbox-empty",
                "M10,10 L90,10 L90,90 L10,90 Z M15,15 L85,15 L85,85 L15,85 Z",
            ),
            (
                "checkbox-checked",
                "M10,10 L90,10 L90,90 L10,90 Z M20,50 L40,70 L80,25",
            ),
            (
                "radio-empty",
                "M50,5 A45,45 0 1 1 49.9,5 Z M50,15 A35,35 0 1 1 49.9,15 Z",
            ),
            ("close-x", "M15,15 L85,85 M85,15 L15,85"),
            ("menu-lines", "M10,25 L90,25 M10,50 L90,50 M10,75 L90,75"),
            ("plus-icon", "M50,10 L50,90 M10,50 L90,50"),
        ]),
        _ => None,
    }
}

/// Load a built-in symbol library, adding all symbols to the document.
pub async fn load_symbol_library(state: &AppState, args: LoadSymbolLibraryArgs) -> ToolResult {
    tracing::debug!("tool: load_symbol_library lib={}", args.library_name);
    use photonic_core::history::Command;
    use photonic_core::node::{PathNode, SceneNode};
    use photonic_core::path::PathData;
    use photonic_core::style::Stroke;
    use photonic_core::transform::Transform;
    use photonic_core::Symbol;

    let library = args.library_name.trim().to_lowercase();
    let entries = match builtin_symbols(&library) {
        Some(e) => e,
        None => {
            return ToolResult::error(format!(
                "Unknown library '{}'. Available: arrows, shapes, ui.",
                args.library_name
            ))
        }
    };

    let mut doc = state.document.lock().await;
    let layer_id = doc
        .active_layer_id
        .or_else(|| doc.layer_order.first().copied())
        .unwrap_or(uuid::Uuid::nil());

    let mut history = state.history.lock().await;
    let mut added = Vec::new();
    let mut skipped = Vec::new();

    // Off-canvas position so master nodes don't clutter the canvas.
    const OFF_X: f64 = -9999.0;

    for (i, (name, path_d)) in entries.iter().enumerate() {
        let sym_name = format!("{}/{}", library, name);

        // Skip if already defined.
        if doc.symbols.iter().any(|s| s.name == sym_name) {
            skipped.push(sym_name);
            continue;
        }

        let path_data = match PathData::from_svg(path_d) {
            Ok(pd) => pd,
            Err(_) => continue, // Skip malformed definitions (shouldn't happen)
        };

        // Build a black fill / no stroke path node for the master.
        let mut path_node = PathNode::new(path_data);
        path_node.stroke = Stroke::none();

        let mut master = SceneNode::new(
            sym_name.clone(),
            layer_id,
            photonic_core::node::SceneNodeKind::Path(path_node),
        );
        // Place master off-canvas, staggered so nodes don't overlap.
        master.transform = Transform::translate(OFF_X + i as f64 * 150.0, -9999.0);
        master.visible = false;

        let master_id = master.id;
        history.execute_discrete(
            Command::AddNode {
                node: master,
                layer_id: Some(layer_id),
            },
            &mut doc,
        );
        doc.symbols.push(Symbol::new(&sym_name, master_id));
        added.push(sym_name);
    }

    ToolResult::text(format!(
        "Loaded '{}' library: {} symbol(s) added, {} already present.",
        library,
        added.len(),
        skipped.len()
    ))
    .with_data(serde_json::json!({
        "library": library,
        "added": added,
        "skipped": skipped,
    }))
}
