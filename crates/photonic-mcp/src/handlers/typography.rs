use crate::protocol::*;
use crate::server::AppState;
use photonic_core::{
    history::Command,
    node::{FontStyle, NodeId, SceneNode, SceneNodeKind, TextNode},
    transform::Transform,
};

pub async fn create_text(state: &AppState, args: CreateTextArgs) -> ToolResult {
    use photonic_core::node::TextAlign;
    tracing::debug!(
        "tool: create_text {:?}",
        &args.content[..args.content.len().min(40)]
    );

    let mut text_node = TextNode::new(&args.content);
    if let Some(ff) = args.font_family {
        text_node.font_family = ff;
    }
    if let Some(fs) = args.font_size {
        text_node.font_size = fs;
    }
    if let Some(fw) = args.font_weight {
        text_node.font_weight = fw;
    }
    if let Some(ref a) = args.align {
        text_node.align = match a.as_str() {
            "center" => TextAlign::Center,
            "right" => TextAlign::Right,
            _ => TextAlign::Left,
        };
    }
    if let Some(lh) = args.line_height {
        text_node.line_height = lh;
    }
    if let Some(ls) = args.letter_spacing {
        text_node.letter_spacing = ls;
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

    let name = args.name.unwrap_or_else(|| "Text".to_string());
    let mut node = SceneNode::new(&name, uuid::Uuid::nil(), SceneNodeKind::Text(text_node));
    node.transform = Transform::translate(args.x, args.y);
    if !args.tags.is_empty() {
        node.tags = args.tags;
    }

    let mut doc = state.document.lock().await;
    let node_id = node.id;
    let cmd = Command::AddNode {
        node,
        layer_id: args.layer_id,
    };
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!("Created text '{}' (id: {})", name, node_id))
        .with_data(serde_json::json!({ "node_id": node_id }))
}
/// Save (or update) a named character style in the document.
pub async fn create_character_style(
    state: &AppState,
    args: CreateCharacterStyleArgs,
) -> ToolResult {
    tracing::debug!("tool: create_character_style");
    use photonic_core::{style::FillKind, CharacterStyle};

    if args.name.trim().is_empty() {
        return ToolResult::error("Style name must not be empty");
    }

    let mut doc = state.document.lock().await;

    // Optionally capture attributes from a source text node.
    let mut style = CharacterStyle {
        name: args.name.trim().to_string(),
        font_family: args.font_family.clone(),
        font_size: args.font_size,
        font_weight: args.font_weight,
        fill_hex: args.fill_hex.clone(),
        letter_spacing: args.letter_spacing,
        line_height: args.line_height,
    };

    if let Some(src_id_str) = &args.source_node_id {
        let src_id = uuid::Uuid::parse_str(src_id_str).ok().or_else(|| {
            doc.nodes
                .values()
                .find(|n| n.name == *src_id_str)
                .map(|n| n.id)
        });
        if let Some(sid) = src_id {
            if let Some(node) = doc.nodes.get(&sid) {
                if let photonic_core::SceneNodeKind::Text(t) = &node.kind {
                    // Capture from node; explicit args override.
                    if style.font_family.is_none() {
                        style.font_family = Some(t.font_family.clone());
                    }
                    if style.font_size.is_none() {
                        style.font_size = Some(t.font_size);
                    }
                    if style.font_weight.is_none() {
                        style.font_weight = Some(t.font_weight);
                    }
                    if style.letter_spacing.is_none() {
                        style.letter_spacing = Some(t.letter_spacing);
                    }
                    if style.line_height.is_none() {
                        style.line_height = Some(t.line_height);
                    }
                    if style.fill_hex.is_none() && t.fill.enabled {
                        if let FillKind::Solid(c) = &t.fill.kind {
                            style.fill_hex = Some(c.to_hex());
                        }
                    }
                }
            }
        }
    }

    // Replace existing or append.
    let action = if let Some(existing) = doc
        .character_styles
        .iter_mut()
        .find(|s| s.name == style.name)
    {
        *existing = style.clone();
        "Updated"
    } else {
        doc.character_styles.push(style.clone());
        "Created"
    };

    ToolResult::text(format!("{action} character style '{}'.", style.name)).with_data(
        serde_json::json!({
            "name": style.name,
            "font_family": style.font_family,
            "font_size": style.font_size,
            "font_weight": style.font_weight,
            "fill_hex": style.fill_hex,
            "letter_spacing": style.letter_spacing,
            "line_height": style.line_height,
        }),
    )
}
/// Delete a named character style from the document.
pub async fn delete_character_style(
    state: &AppState,
    args: DeleteCharacterStyleArgs,
) -> ToolResult {
    tracing::debug!("tool: delete_character_style");
    let mut doc = state.document.lock().await;
    let before = doc.character_styles.len();
    doc.character_styles.retain(|s| s.name != args.name);
    if doc.character_styles.len() < before {
        ToolResult::text(format!("Deleted character style '{}'.", args.name))
    } else {
        ToolResult::error(format!("No character style named '{}' found.", args.name))
    }
}
/// Save (or update) a named paragraph style.
pub async fn create_paragraph_style(
    state: &AppState,
    args: CreateParagraphStyleArgs,
) -> ToolResult {
    tracing::debug!("tool: create_paragraph_style");
    use photonic_core::ParagraphStyle;

    if args.name.trim().is_empty() {
        return ToolResult::error("Style name must not be empty");
    }

    let mut doc = state.document.lock().await;

    let mut style = ParagraphStyle {
        name: args.name.trim().to_string(),
        align: args.align.clone(),
        line_height: args.line_height,
        letter_spacing: args.letter_spacing,
        font_size: args.font_size,
        font_family: args.font_family.clone(),
    };

    // Optionally capture from a source text node.
    if let Some(src_str) = &args.source_node_id {
        let src_id = uuid::Uuid::parse_str(src_str).ok().or_else(|| {
            doc.nodes
                .values()
                .find(|n| n.name == *src_str)
                .map(|n| n.id)
        });
        if let Some(sid) = src_id {
            if let Some(node) = doc.nodes.get(&sid) {
                if let photonic_core::SceneNodeKind::Text(t) = &node.kind {
                    use photonic_core::node::TextAlign;
                    if style.align.is_none() {
                        style.align = Some(match t.align {
                            TextAlign::Left => "left".to_string(),
                            TextAlign::Center => "center".to_string(),
                            TextAlign::Right => "right".to_string(),
                        });
                    }
                    if style.line_height.is_none() {
                        style.line_height = Some(t.line_height);
                    }
                    if style.letter_spacing.is_none() {
                        style.letter_spacing = Some(t.letter_spacing);
                    }
                    if style.font_size.is_none() {
                        style.font_size = Some(t.font_size);
                    }
                    if style.font_family.is_none() {
                        style.font_family = Some(t.font_family.clone());
                    }
                }
            }
        }
    }

    let action = if let Some(existing) = doc
        .paragraph_styles
        .iter_mut()
        .find(|s| s.name == style.name)
    {
        *existing = style.clone();
        "Updated"
    } else {
        doc.paragraph_styles.push(style.clone());
        "Created"
    };

    ToolResult::text(format!("{action} paragraph style '{}'.", style.name)).with_data(
        serde_json::json!({
            "name": style.name,
            "align": style.align,
            "line_height": style.line_height,
            "letter_spacing": style.letter_spacing,
            "font_size": style.font_size,
            "font_family": style.font_family,
        }),
    )
}
/// Delete a named paragraph style from the document.
pub async fn delete_paragraph_style(
    state: &AppState,
    args: DeleteParagraphStyleArgs,
) -> ToolResult {
    tracing::debug!("tool: delete_paragraph_style");
    let mut doc = state.document.lock().await;
    let before = doc.paragraph_styles.len();
    doc.paragraph_styles.retain(|s| s.name != args.name);
    if doc.paragraph_styles.len() < before {
        ToolResult::text(format!("Deleted paragraph style '{}'.", args.name))
    } else {
        ToolResult::error(format!("No paragraph style named '{}' found.", args.name))
    }
}
/// Apply a named character style to one or more text nodes.
pub async fn apply_character_style(state: &AppState, args: ApplyCharacterStyleArgs) -> ToolResult {
    tracing::debug!("tool: apply_character_style");
    use photonic_core::color::Color;
    use photonic_core::history::Command;
    use photonic_core::style::Fill;

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    // Look up style.
    let style = match doc
        .character_styles
        .iter()
        .find(|s| s.name == args.style_name)
        .cloned()
    {
        Some(s) => s,
        None => {
            return ToolResult::error(format!("Character style '{}' not found.", args.style_name))
        }
    };

    // Resolve target nodes.
    let target_ids: Vec<photonic_core::NodeId> = if args.node_ids.is_empty() {
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

    if target_ids.is_empty() {
        return ToolResult::error("No target nodes specified and no active selection.");
    }

    let mut applied = 0usize;
    let mut commands = Vec::new();

    for nid in &target_ids {
        if let Some(node) = doc.nodes.get(nid).cloned() {
            if let photonic_core::SceneNodeKind::Text(_) = &node.kind {
                let mut new_node = node.clone();
                if let photonic_core::SceneNodeKind::Text(ref mut t) = new_node.kind {
                    if let Some(ff) = &style.font_family {
                        t.font_family = ff.clone();
                    }
                    if let Some(fs) = style.font_size {
                        t.font_size = fs;
                    }
                    if let Some(fw) = style.font_weight {
                        t.font_weight = fw;
                    }
                    if let Some(ls) = style.letter_spacing {
                        t.letter_spacing = ls;
                    }
                    if let Some(lh) = style.line_height {
                        t.line_height = lh;
                    }
                    if let Some(hex) = &style.fill_hex {
                        if let Some(color) = Color::from_hex(hex) {
                            t.fill = Fill::solid(color);
                        }
                    }
                }
                commands.push(Command::UpdateNode {
                    old: node,
                    new: new_node,
                });
                applied += 1;
            }
        }
    }

    if commands.is_empty() {
        return ToolResult::error("No text nodes found in the target set.");
    }

    let batch = if commands.len() == 1 {
        commands.remove(0)
    } else {
        Command::Batch(commands)
    };
    history.execute_discrete(batch, &mut doc);

    ToolResult::text(format!(
        "Applied character style '{}' to {applied} text node(s).",
        style.name
    ))
    .with_data(serde_json::json!({
        "style_name": style.name,
        "nodes_updated": applied,
    }))
}
/// Apply a named paragraph style to one or more text nodes.
pub async fn apply_paragraph_style(state: &AppState, args: ApplyParagraphStyleArgs) -> ToolResult {
    tracing::debug!("tool: apply_paragraph_style");
    use photonic_core::history::Command;
    use photonic_core::node::TextAlign;

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let style = match doc
        .paragraph_styles
        .iter()
        .find(|s| s.name == args.style_name)
        .cloned()
    {
        Some(s) => s,
        None => {
            return ToolResult::error(format!("Paragraph style '{}' not found.", args.style_name))
        }
    };

    let target_ids: Vec<photonic_core::NodeId> = if args.node_ids.is_empty() {
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

    if target_ids.is_empty() {
        return ToolResult::error("No target nodes specified and no active selection.");
    }

    let mut applied = 0usize;
    let mut commands = Vec::new();

    for nid in &target_ids {
        if let Some(node) = doc.nodes.get(nid).cloned() {
            if let photonic_core::SceneNodeKind::Text(_) = &node.kind {
                let mut new_node = node.clone();
                if let photonic_core::SceneNodeKind::Text(ref mut t) = new_node.kind {
                    if let Some(align_str) = &style.align {
                        t.align = match align_str.as_str() {
                            "center" => TextAlign::Center,
                            "right" => TextAlign::Right,
                            _ => TextAlign::Left,
                        };
                    }
                    if let Some(lh) = style.line_height {
                        t.line_height = lh;
                    }
                    if let Some(ls) = style.letter_spacing {
                        t.letter_spacing = ls;
                    }
                    if let Some(fs) = style.font_size {
                        t.font_size = fs;
                    }
                    if let Some(ff) = &style.font_family {
                        t.font_family = ff.clone();
                    }
                }
                commands.push(Command::UpdateNode {
                    old: node,
                    new: new_node,
                });
                applied += 1;
            }
        }
    }

    if commands.is_empty() {
        return ToolResult::error("No text nodes found in the target set.");
    }

    let batch = if commands.len() == 1 {
        commands.remove(0)
    } else {
        Command::Batch(commands)
    };
    history.execute_discrete(batch, &mut doc);

    ToolResult::text(format!(
        "Applied paragraph style '{}' to {applied} text node(s).",
        style.name
    ))
    .with_data(serde_json::json!({
        "style_name": style.name,
        "nodes_updated": applied,
    }))
}
/// List all character styles saved in the document.
pub async fn list_character_styles(state: &AppState) -> ToolResult {
    tracing::debug!("tool: list_character_styles");
    let doc = state.document.lock().await;
    if doc.character_styles.is_empty() {
        return ToolResult::text("No character styles defined.");
    }
    let styles: Vec<_> = doc
        .character_styles
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "font_family": s.font_family,
                "font_size": s.font_size,
                "font_weight": s.font_weight,
                "fill_hex": s.fill_hex,
                "letter_spacing": s.letter_spacing,
                "line_height": s.line_height,
            })
        })
        .collect();
    ToolResult::text(format!("{} character style(s).", styles.len()))
        .with_data(serde_json::json!({ "character_styles": styles }))
}
/// List all paragraph styles saved in the document.
pub async fn list_paragraph_styles(state: &AppState) -> ToolResult {
    tracing::debug!("tool: list_paragraph_styles");
    let doc = state.document.lock().await;
    if doc.paragraph_styles.is_empty() {
        return ToolResult::text("No paragraph styles defined.");
    }
    let styles: Vec<_> = doc
        .paragraph_styles
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "align": s.align,
                "line_height": s.line_height,
                "letter_spacing": s.letter_spacing,
                "font_size": s.font_size,
                "font_family": s.font_family,
            })
        })
        .collect();
    ToolResult::text(format!("{} paragraph style(s).", styles.len()))
        .with_data(serde_json::json!({ "paragraph_styles": styles }))
}
/// Set advanced node-level character metrics: baseline shift and super/subscript.
pub async fn set_character_metrics(state: &AppState, args: SetCharacterMetricsArgs) -> ToolResult {
    use photonic_core::node::ScriptPosition;
    tracing::debug!("tool: set_character_metrics");

    // Validate script_position up front so a bad value fails before mutating.
    let parsed_script = match args.script_position.as_deref() {
        Some(s) => match ScriptPosition::from_str_opt(s) {
            Some(sp) => Some(sp),
            None => {
                return ToolResult::error(format!(
                    "Unknown script_position '{}'. Valid values: normal, superscript, subscript.",
                    s
                ))
            }
        },
        None => None,
    };

    if args.baseline_shift.is_none() && parsed_script.is_none() {
        return ToolResult::error(
            "Nothing to change: provide baseline_shift and/or script_position.".to_string(),
        );
    }

    let mut doc = state.document.lock().await;

    let node_id = uuid::Uuid::parse_str(&args.node_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.node_id).map(|n| n.id));
    let node_id = match node_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    let node = match doc.nodes.get(&node_id) {
        Some(n) if matches!(n.kind, SceneNodeKind::Text(_)) => n.clone(),
        Some(_) => {
            return ToolResult::error(format!("Node '{}' is not a text node.", args.node_id))
        }
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    let mut new_node = node.clone();
    let (baseline_shift, script_position) = if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
        if let Some(bs) = args.baseline_shift {
            tn.baseline_shift = bs;
        }
        if let Some(sp) = parsed_script {
            tn.script_position = sp;
        }
        (tn.baseline_shift, tn.script_position)
    } else {
        (0.0, ScriptPosition::Normal)
    };

    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::UpdateNode {
            old: node,
            new: new_node,
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Character metrics on '{}' set: baseline_shift={}, script_position={}.",
        args.node_id,
        baseline_shift,
        script_position.as_str()
    ))
    .with_data(serde_json::json!({
        "node_id": node_id.to_string(),
        "baseline_shift": baseline_shift,
        "script_position": script_position.as_str(),
    }))
}
/// Set the font style (normal / italic / oblique) on a text node.
pub async fn set_font_style(state: &AppState, args: SetFontStyleArgs) -> ToolResult {
    tracing::debug!("tool: set_font_style");
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
    if !matches!(node.kind, SceneNodeKind::Text(_)) {
        return ToolResult::error(format!("Node '{}' is not a text node.", args.node_id));
    }
    let font_style = match args.style.to_lowercase().as_str() {
        "italic" => FontStyle::Italic,
        "oblique" => FontStyle::Oblique,
        _ => FontStyle::Normal,
    };
    let mut new_node = node.clone();
    if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
        tn.font_style = font_style;
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
        "Set font style to '{}' on node '{}'.",
        args.style, args.node_id
    ))
    .with_data(serde_json::json!({ "node_id": node_id, "font_style": args.style }))
}
/// Set the font weight (100–900) on a text node.
pub async fn set_font_weight(state: &AppState, args: SetFontWeightArgs) -> ToolResult {
    tracing::debug!("tool: set_font_weight");
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
    if !matches!(node.kind, SceneNodeKind::Text(_)) {
        return ToolResult::error(format!("Node '{}' is not a text node.", args.node_id));
    }
    let weight = args.weight.clamp(100, 900);
    let mut new_node = node.clone();
    if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
        tn.font_weight = weight;
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
        "Set font weight to {} on node '{}'.",
        weight, args.node_id
    ))
    .with_data(serde_json::json!({ "node_id": node_id, "font_weight": weight }))
}
/// Set the text decoration (underline, line-through, overline, or none) on a text node.
pub async fn set_text_decoration(state: &AppState, args: SetTextDecorationArgs) -> ToolResult {
    tracing::debug!("tool: set_text_decoration");

    let decoration = match args.decoration.to_lowercase().as_str() {
        "" | "none" => String::new(),
        "underline" | "line-through" | "overline" => args.decoration.to_lowercase(),
        other => {
            return ToolResult::error(format!(
                "Unknown decoration '{}'. Valid values: none, underline, line-through, overline.",
                other
            ))
        }
    };

    let mut doc = state.document.lock().await;

    let node_id = uuid::Uuid::parse_str(&args.node_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.node_id).map(|n| n.id));
    let node_id = match node_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    let node = match doc.nodes.get(&node_id) {
        Some(n) if matches!(n.kind, SceneNodeKind::Text(_)) => n.clone(),
        Some(_) => {
            return ToolResult::error(format!("Node '{}' is not a text node.", args.node_id))
        }
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    let mut new_node = node.clone();
    if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
        tn.text_decoration = decoration.clone();
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::UpdateNode {
            old: node,
            new: new_node,
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Text decoration on '{}' set to '{}'.",
        args.node_id,
        if decoration.is_empty() {
            "none"
        } else {
            &decoration
        }
    ))
    .with_data(serde_json::json!({ "node_id": node_id.to_string(), "decoration": decoration }))
}
/// Set the text layout direction of a text node (horizontal or vertical).
pub async fn set_text_direction(state: &AppState, args: SetTextDirectionArgs) -> ToolResult {
    tracing::debug!("tool: set_text_direction");
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

    if !matches!(node.kind, SceneNodeKind::Text(_)) {
        return ToolResult::error(format!("Node '{}' is not a text node.", args.node_id));
    }

    let mut new_node = node.clone();
    if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
        tn.vertical = args.vertical;
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

    let dir = if args.vertical {
        "vertical"
    } else {
        "horizontal"
    };
    ToolResult::text(format!(
        "Text node '{}' set to {} layout.",
        args.node_id, dir
    ))
    .with_data(serde_json::json!({ "node_id": node_id, "vertical": args.vertical }))
}
/// Flow a text node inside a closed path area (Area Type).
pub async fn set_text_area(state: &AppState, args: SetTextAreaArgs) -> ToolResult {
    tracing::debug!("tool: set_text_area");
    let mut doc = state.document.lock().await;

    let resolve = |id: &str| -> Option<NodeId> {
        uuid::Uuid::parse_str(id)
            .ok()
            .or_else(|| doc.find_node_by_name(id).map(|n| n.id))
    };

    let text_id = match resolve(&args.text_node_id) {
        Some(id) => id,
        None => return ToolResult::error(format!("Text node '{}' not found.", args.text_node_id)),
    };
    let area_id = match resolve(&args.area_path_id) {
        Some(id) => id,
        None => return ToolResult::error(format!("Area path '{}' not found.", args.area_path_id)),
    };
    if text_id == area_id {
        return ToolResult::error("Text node and area path must be different nodes.");
    }

    let text_node = match doc.nodes.get(&text_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Text node '{}' not found.", args.text_node_id)),
    };
    if !matches!(text_node.kind, SceneNodeKind::Text(_)) {
        return ToolResult::error(format!("Node '{}' is not a text node.", args.text_node_id));
    }

    match doc.nodes.get(&area_id) {
        Some(n) if matches!(n.kind, SceneNodeKind::Path(_)) => {}
        Some(_) => {
            return ToolResult::error(format!("Node '{}' is not a path node.", args.area_path_id))
        }
        None => return ToolResult::error(format!("Area path '{}' not found.", args.area_path_id)),
    }

    let mut new_node = text_node.clone();
    if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
        tn.area_path_id = Some(area_id);
    }

    let area_name = doc
        .nodes
        .get(&area_id)
        .map(|n| n.name.clone())
        .unwrap_or_else(|| area_id.to_string());
    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::UpdateNode {
            old: text_node,
            new: new_node,
        },
        &mut doc,
    );
    drop(history);

    ToolResult::text(format!(
        "Text node '{}' now flows inside area '{}'.",
        args.text_node_id, area_name
    ))
    .with_data(serde_json::json!({ "text_node_id": text_id, "area_path_id": area_id }))
}
/// Place text along a path spine (Type on a Path).
pub async fn set_text_path(state: &AppState, args: SetTextPathArgs) -> ToolResult {
    tracing::debug!("tool: set_text_path");
    let mut doc = state.document.lock().await;

    let resolve = |id: &str| -> Option<NodeId> {
        uuid::Uuid::parse_str(id)
            .ok()
            .or_else(|| doc.find_node_by_name(id).map(|n| n.id))
    };

    let text_id = match resolve(&args.text_node_id) {
        Some(id) => id,
        None => return ToolResult::error(format!("Text node '{}' not found.", args.text_node_id)),
    };
    let path_id = match resolve(&args.path_node_id) {
        Some(id) => id,
        None => return ToolResult::error(format!("Path node '{}' not found.", args.path_node_id)),
    };

    let text_node = match doc.nodes.get(&text_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Text node '{}' not found.", args.text_node_id)),
    };

    // Verify target is actually a text node.
    if !matches!(text_node.kind, SceneNodeKind::Text(_)) {
        return ToolResult::error(format!("Node '{}' is not a text node.", args.text_node_id));
    }

    if text_id == path_id {
        return ToolResult::error("Text node and path node must be different nodes.");
    }

    // Verify spine is a path node.
    match doc.nodes.get(&path_id) {
        Some(n) if matches!(n.kind, SceneNodeKind::Path(_)) => {}
        Some(_) => {
            return ToolResult::error(format!("Node '{}' is not a path node.", args.path_node_id))
        }
        None => return ToolResult::error(format!("Path node '{}' not found.", args.path_node_id)),
    }

    let mut new_node = text_node.clone();
    if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
        tn.path_spine_id = Some(path_id);
        tn.path_offset = args.offset;
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::UpdateNode {
            old: text_node,
            new: new_node,
        },
        &mut doc,
    );
    drop(history);

    ToolResult::text(format!(
        "Text node '{}' now follows path '{}'  (offset: {:.1}).",
        args.text_node_id, args.path_node_id, args.offset
    ))
    .with_data(serde_json::json!({
        "text_node_id": text_id,
        "path_node_id": path_id,
        "offset": args.offset,
    }))
}
/// Remove the area boundary from a text node (revert to normal point text).
pub async fn clear_text_area(state: &AppState, args: ClearTextAreaArgs) -> ToolResult {
    tracing::debug!("tool: clear_text_area");
    let mut doc = state.document.lock().await;

    let text_id = uuid::Uuid::parse_str(&args.text_node_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.text_node_id).map(|n| n.id));
    let text_id = match text_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Text node '{}' not found.", args.text_node_id)),
    };

    let text_node = match doc.nodes.get(&text_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Text node '{}' not found.", args.text_node_id)),
    };

    if !matches!(text_node.kind, SceneNodeKind::Text(_)) {
        return ToolResult::error(format!("Node '{}' is not a text node.", args.text_node_id));
    }

    let had_area = matches!(&text_node.kind, SceneNodeKind::Text(tn) if tn.area_path_id.is_some());
    if !had_area {
        return ToolResult::error(format!(
            "Text node '{}' does not have an area path.",
            args.text_node_id
        ));
    }

    let mut new_node = text_node.clone();
    if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
        tn.area_path_id = None;
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::UpdateNode {
            old: text_node,
            new: new_node,
        },
        &mut doc,
    );
    drop(history);

    ToolResult::text(format!(
        "Removed area boundary from text node '{}'.",
        args.text_node_id
    ))
    .with_data(serde_json::json!({ "text_node_id": text_id }))
}
/// Remove the path spine from a text node (revert to normal positioned text).
pub async fn clear_text_path(state: &AppState, args: ClearTextPathArgs) -> ToolResult {
    tracing::debug!("tool: clear_text_path");
    let mut doc = state.document.lock().await;

    let text_id = uuid::Uuid::parse_str(&args.text_node_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.text_node_id).map(|n| n.id));
    let text_id = match text_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Text node '{}' not found.", args.text_node_id)),
    };

    let text_node = match doc.nodes.get(&text_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Text node '{}' not found.", args.text_node_id)),
    };

    if !matches!(text_node.kind, SceneNodeKind::Text(_)) {
        return ToolResult::error(format!("Node '{}' is not a text node.", args.text_node_id));
    }

    let had_spine =
        matches!(&text_node.kind, SceneNodeKind::Text(tn) if tn.path_spine_id.is_some());
    if !had_spine {
        return ToolResult::error(format!(
            "Text node '{}' is not on a path.",
            args.text_node_id
        ));
    }

    let mut new_node = text_node.clone();
    if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
        tn.path_spine_id = None;
        tn.path_offset = 0.0;
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::UpdateNode {
            old: text_node,
            new: new_node,
        },
        &mut doc,
    );
    drop(history);

    ToolResult::text(format!(
        "Removed path spine from text node '{}'.",
        args.text_node_id
    ))
    .with_data(serde_json::json!({ "text_node_id": text_id }))
}
/// Set paragraph-level text options: spacing before/after paragraphs and first-line indent.
pub async fn set_paragraph_options(state: &AppState, args: SetParagraphOptionsArgs) -> ToolResult {
    tracing::debug!("tool: set_paragraph_options");
    let mut doc = state.document.lock().await;

    let node_id = uuid::Uuid::parse_str(&args.node_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.node_id).map(|n| n.id));
    let node_id = match node_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    let node = match doc.nodes.get(&node_id) {
        Some(n) if matches!(n.kind, SceneNodeKind::Text(_)) => n.clone(),
        Some(_) => {
            return ToolResult::error(format!("Node '{}' is not a text node.", args.node_id))
        }
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    let mut new_node = node.clone();
    if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
        if let Some(v) = args.spacing_before {
            tn.paragraph_spacing_before = v;
        }
        if let Some(v) = args.spacing_after {
            tn.paragraph_spacing_after = v;
        }
        if let Some(v) = args.indent {
            tn.text_indent = v;
        }
    }

    let (sb, sa, ti) = match &new_node.kind {
        SceneNodeKind::Text(tn) => (
            tn.paragraph_spacing_before,
            tn.paragraph_spacing_after,
            tn.text_indent,
        ),
        _ => (0.0, 0.0, 0.0),
    };

    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::UpdateNode {
            old: node,
            new: new_node,
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Paragraph options on '{}': spacing_before={:.1}, spacing_after={:.1}, indent={:.1}.",
        args.node_id, sb, sa, ti
    ))
    .with_data(serde_json::json!({
        "node_id": node_id.to_string(),
        "spacing_before": sb, "spacing_after": sa, "indent": ti
    }))
}
/// Set explicit tab stop positions on a text node.
pub async fn set_tab_stops(state: &AppState, args: SetTabStopsArgs) -> ToolResult {
    tracing::debug!("tool: set_tab_stops");
    let mut doc = state.document.lock().await;

    let node_id = uuid::Uuid::parse_str(&args.node_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.node_id).map(|n| n.id));
    let node_id = match node_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    let node = match doc.nodes.get(&node_id) {
        Some(n) if matches!(n.kind, SceneNodeKind::Text(_)) => n.clone(),
        Some(_) => {
            return ToolResult::error(format!("Node '{}' is not a text node.", args.node_id))
        }
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    if args.stops.is_empty() {
        return ToolResult::error(
            "stops must contain at least one position. Use clear_tab_stops to reset to defaults.",
        );
    }

    let mut sorted_stops = args.stops.clone();
    sorted_stops.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut new_node = node.clone();
    if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
        tn.tab_stops = sorted_stops.clone();
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::UpdateNode {
            old: node,
            new: new_node,
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Set {} tab stop(s) on '{}': {:?}",
        sorted_stops.len(),
        args.node_id,
        sorted_stops
    ))
    .with_data(serde_json::json!({
        "node_id": node_id.to_string(),
        "tab_stops": sorted_stops,
    }))
}
/// Clear custom tab stops on a text node, restoring default tab spacing.
pub async fn clear_tab_stops(state: &AppState, args: ClearTabStopsArgs) -> ToolResult {
    tracing::debug!("tool: clear_tab_stops");
    let mut doc = state.document.lock().await;

    let node_id = uuid::Uuid::parse_str(&args.node_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.node_id).map(|n| n.id));
    let node_id = match node_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    let node = match doc.nodes.get(&node_id) {
        Some(n) if matches!(n.kind, SceneNodeKind::Text(_)) => n.clone(),
        Some(_) => {
            return ToolResult::error(format!("Node '{}' is not a text node.", args.node_id))
        }
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    let mut new_node = node.clone();
    if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
        tn.tab_stops.clear();
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::UpdateNode {
            old: node,
            new: new_node,
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Cleared tab stops on '{}'. Default tab spacing restored.",
        args.node_id
    ))
    .with_data(serde_json::json!({ "node_id": node_id.to_string(), "tab_stops": [] }))
}
/// Set (or add/remove) OpenType feature tags on a text node.
pub async fn set_opentype_features(state: &AppState, args: SetOpenTypeFeaturesArgs) -> ToolResult {
    tracing::debug!("tool: set_opentype_features");
    let mut doc = state.document.lock().await;

    let node_id = uuid::Uuid::parse_str(&args.node_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.node_id).map(|n| n.id));
    let node_id = match node_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    let node = match doc.nodes.get(&node_id) {
        Some(n) if matches!(n.kind, SceneNodeKind::Text(_)) => n.clone(),
        Some(_) => {
            return ToolResult::error(format!("Node '{}' is not a text node.", args.node_id))
        }
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    let mut new_node = node.clone();
    if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
        let mode = if args.mode.is_empty() {
            "set"
        } else {
            args.mode.as_str()
        };
        match mode {
            "add" => {
                for f in &args.features {
                    if !tn.opentype_features.contains(f) {
                        tn.opentype_features.push(f.clone());
                    }
                }
            }
            "remove" => {
                tn.opentype_features.retain(|f| !args.features.contains(f));
            }
            _ => {
                // "set" is default
                tn.opentype_features = args.features.clone();
            }
        }
    }

    let features_after = match &new_node.kind {
        SceneNodeKind::Text(tn) => tn.opentype_features.clone(),
        _ => vec![],
    };

    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::UpdateNode {
            old: node,
            new: new_node,
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "OpenType features on '{}' updated ({} active).",
        args.node_id,
        features_after.len()
    ))
    .with_data(serde_json::json!({ "node_id": node_id.to_string(), "features": features_after }))
}
/// Return the active OpenType feature tags on a text node.
pub async fn get_opentype_features(state: &AppState, args: GetOpenTypeFeaturesArgs) -> ToolResult {
    tracing::debug!("tool: get_opentype_features");
    let doc = state.document.lock().await;

    let node_id = uuid::Uuid::parse_str(&args.node_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.node_id).map(|n| n.id));
    let node_id = match node_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    match doc.nodes.get(&node_id) {
        Some(n) => match &n.kind {
            SceneNodeKind::Text(tn) => {
                ToolResult::text(format!(
                    "Node '{}' has {} OpenType feature(s): {}",
                    args.node_id, tn.opentype_features.len(),
                    if tn.opentype_features.is_empty() { "(none — using font defaults)".to_string() }
                    else { tn.opentype_features.join(", ") }
                ))
                .with_data(serde_json::json!({ "node_id": node_id.to_string(), "features": tn.opentype_features }))
            }
            _ => ToolResult::error(format!("Node '{}' is not a text node.", args.node_id)),
        },
        None => ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    }
}
/// Bind a text node to a document variable so apply_variables replaces its content.
pub async fn bind_text_variable(state: &AppState, args: BindTextVariableArgs) -> ToolResult {
    tracing::debug!("tool: bind_text_variable");
    let mut doc = state.document.lock().await;

    let node_id = uuid::Uuid::parse_str(&args.node_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.node_id).map(|n| n.id));
    let node_id = match node_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    // Verify the variable exists.
    if !doc.variables.iter().any(|v| v.name == args.variable_name) {
        return ToolResult::error(format!(
            "Variable '{}' not found. Use define_variable first.",
            args.variable_name
        ));
    }

    let node = match doc.nodes.get(&node_id) {
        Some(n) => n.clone(),
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };
    if !matches!(node.kind, SceneNodeKind::Text(_)) {
        return ToolResult::error(format!("Node '{}' is not a text node.", args.node_id));
    }

    let mut new_node = node.clone();
    if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
        tn.variable_binding = Some(args.variable_name.clone());
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
        "Text node '{}' bound to variable '{}'.",
        args.node_id, args.variable_name
    ))
    .with_data(serde_json::json!({ "node_id": node_id, "variable_name": args.variable_name }))
}
/// Remove the variable binding from a text node.
pub async fn unbind_text_variable(state: &AppState, args: UnbindTextVariableArgs) -> ToolResult {
    tracing::debug!("tool: unbind_text_variable");
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

    if !matches!(node.kind, SceneNodeKind::Text(_)) {
        return ToolResult::error(format!("Node '{}' is not a text node.", args.node_id));
    }

    let had_binding =
        matches!(&node.kind, SceneNodeKind::Text(tn) if tn.variable_binding.is_some());
    if !had_binding {
        return ToolResult::error(format!(
            "Text node '{}' does not have a variable binding.",
            args.node_id
        ));
    }

    let mut new_node = node.clone();
    if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
        tn.variable_binding = None;
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
        "Removed variable binding from text node '{}'.",
        args.node_id
    ))
    .with_data(serde_json::json!({ "node_id": node_id }))
}
/// Link two text nodes as a threaded text chain (overflow from `from` flows into `to`).
pub async fn link_text_frames(state: &AppState, args: LinkTextFramesArgs) -> ToolResult {
    tracing::debug!("tool: link_text_frames");
    let mut doc = state.document.lock().await;

    let from_id = uuid::Uuid::parse_str(&args.from_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.from_id).map(|n| n.id));
    let from_id = match from_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.from_id)),
    };

    let to_id = uuid::Uuid::parse_str(&args.to_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.to_id).map(|n| n.id));
    let to_id = match to_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.to_id)),
    };

    if from_id == to_id {
        return ToolResult::error("A text frame cannot be linked to itself.");
    }

    // Validate both are text nodes.
    let from_node = match doc.nodes.get(&from_id) {
        Some(n) if matches!(n.kind, SceneNodeKind::Text(_)) => n.clone(),
        Some(_) => {
            return ToolResult::error(format!("Node '{}' is not a text node.", args.from_id))
        }
        None => return ToolResult::error(format!("Node '{}' not found.", args.from_id)),
    };
    let to_node = match doc.nodes.get(&to_id) {
        Some(n) if matches!(n.kind, SceneNodeKind::Text(_)) => n.clone(),
        Some(_) => return ToolResult::error(format!("Node '{}' is not a text node.", args.to_id)),
        None => return ToolResult::error(format!("Node '{}' not found.", args.to_id)),
    };

    let mut new_from = from_node.clone();
    let mut new_to = to_node.clone();
    if let SceneNodeKind::Text(ref mut tn) = new_from.kind {
        tn.next_frame = Some(to_id);
    }
    if let SceneNodeKind::Text(ref mut tn) = new_to.kind {
        tn.prev_frame = Some(from_id);
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::Batch(vec![
            Command::UpdateNode {
                old: from_node,
                new: new_from,
            },
            Command::UpdateNode {
                old: to_node,
                new: new_to,
            },
        ]),
        &mut doc,
    );
    drop(history);

    ToolResult::text(format!(
        "Linked text frames: '{}' → '{}'.",
        args.from_id, args.to_id
    ))
    .with_data(serde_json::json!({ "from_id": from_id.to_string(), "to_id": to_id.to_string() }))
}
/// Unlink a text node from its thread chain, updating adjacent frame links.
pub async fn unlink_text_frames(state: &AppState, args: UnlinkTextFramesArgs) -> ToolResult {
    tracing::debug!("tool: unlink_text_frames");
    let mut doc = state.document.lock().await;

    let node_id = uuid::Uuid::parse_str(&args.node_id)
        .ok()
        .or_else(|| doc.find_node_by_name(&args.node_id).map(|n| n.id));
    let node_id = match node_id {
        Some(id) => id,
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    let node = match doc.nodes.get(&node_id) {
        Some(n) if matches!(n.kind, SceneNodeKind::Text(_)) => n.clone(),
        Some(_) => {
            return ToolResult::error(format!("Node '{}' is not a text node.", args.node_id))
        }
        None => return ToolResult::error(format!("Node '{}' not found.", args.node_id)),
    };

    let (prev_id, next_id) = match &node.kind {
        SceneNodeKind::Text(tn) => (tn.prev_frame, tn.next_frame),
        _ => (None, None),
    };

    if prev_id.is_none() && next_id.is_none() {
        return ToolResult::error(format!(
            "Node '{}' is not part of a text thread.",
            args.node_id
        ));
    }

    let mut commands: Vec<Command> = Vec::new();

    // Update this node.
    let mut new_node = node.clone();
    if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
        tn.next_frame = None;
        tn.prev_frame = None;
    }
    commands.push(Command::UpdateNode {
        old: node,
        new: new_node,
    });

    // Clear next_frame link from prev node.
    if let Some(pid) = prev_id {
        if let Some(prev) = doc.nodes.get(&pid).cloned() {
            let mut new_prev = prev.clone();
            if let SceneNodeKind::Text(ref mut tn) = new_prev.kind {
                tn.next_frame = None;
            }
            commands.push(Command::UpdateNode {
                old: prev,
                new: new_prev,
            });
        }
    }

    // Clear prev_frame link from next node.
    if let Some(nid) = next_id {
        if let Some(next) = doc.nodes.get(&nid).cloned() {
            let mut new_next = next.clone();
            if let SceneNodeKind::Text(ref mut tn) = new_next.kind {
                tn.prev_frame = None;
            }
            commands.push(Command::UpdateNode {
                old: next,
                new: new_next,
            });
        }
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(Command::Batch(commands), &mut doc);
    drop(history);

    ToolResult::text(format!("Unlinked text frame '{}'.", args.node_id))
        .with_data(serde_json::json!({ "node_id": node_id.to_string() }))
}
