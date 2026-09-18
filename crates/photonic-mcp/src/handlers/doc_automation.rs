use crate::protocol::{
    BranchCreateArgs, BranchDeleteArgs, BranchSwitchArgs, CheckGrammarArgs, DefineActionArgs,
    DefineGrammarRuleArgs, DeleteActionArgs, DeleteGrammarRuleArgs, DeleteWorkspaceArgs,
    LoadWorkspaceArgs, RegisterEventTriggerArgs, RemoveEventTriggerArgs, SaveWorkspaceArgs,
    ToolResult,
};
use crate::server::AppState;
use photonic_core::Command;
use serde_json::json;

/// Define (or update) a named document grammar rule.
pub async fn define_grammar_rule(state: &AppState, args: DefineGrammarRuleArgs) -> ToolResult {
    tracing::debug!("tool: define_grammar_rule");
    if args.name.trim().is_empty() {
        return ToolResult::error("Rule name must not be empty.");
    }
    let valid_types = [
        "palette_includes",
        "max_colors",
        "min_text_size",
        "required_layer",
        "max_node_count",
    ];
    if !valid_types.contains(&args.rule_type.as_str()) {
        return ToolResult::error(format!(
            "Unknown rule_type '{}'. Valid types: {}",
            args.rule_type,
            valid_types.join(", ")
        ));
    }
    let params_json = args.params.to_string();
    let mut doc = state.document.lock().await;
    // Overwrite if name already exists
    let existing_idx = doc.grammar_rules.iter().position(|r| r.name == args.name);
    let rule = photonic_core::GrammarRule::new(&args.name, &args.rule_type, &params_json);
    let name = rule.name.clone();
    let rule_type = rule.rule_type.clone();
    if let Some(idx) = existing_idx {
        doc.grammar_rules[idx] = rule;
    } else {
        doc.grammar_rules.push(rule);
    }
    ToolResult::text(format!(
        "Grammar rule '{}' (type: {}) defined.",
        name, rule_type
    ))
    .with_data(json!({ "name": name, "rule_type": rule_type }))
}

/// List all grammar rules defined in the document.
pub async fn list_grammar_rules(state: &AppState) -> ToolResult {
    tracing::debug!("tool: list_grammar_rules");
    let doc = state.document.lock().await;
    let rules: Vec<serde_json::Value> = doc
        .grammar_rules
        .iter()
        .map(|r| json!({ "name": r.name, "rule_type": r.rule_type, "params": r.params_json }))
        .collect();
    ToolResult::text(format!("{} grammar rule(s).", rules.len()))
        .with_data(json!({ "rules": rules }))
}

/// Delete a named grammar rule.
pub async fn delete_grammar_rule(state: &AppState, args: DeleteGrammarRuleArgs) -> ToolResult {
    tracing::debug!("tool: delete_grammar_rule");
    let mut doc = state.document.lock().await;
    let before = doc.grammar_rules.len();
    doc.grammar_rules.retain(|r| r.name != args.name);
    if doc.grammar_rules.len() == before {
        return ToolResult::error(format!("No grammar rule named '{}'.", args.name));
    }
    ToolResult::text(format!("Grammar rule '{}' deleted.", args.name))
        .with_data(json!({ "name": args.name }))
}

/// Check the document against its grammar rules and return pass/fail per rule.
pub async fn check_grammar(state: &AppState, args: CheckGrammarArgs) -> ToolResult {
    tracing::debug!("tool: check_grammar");
    let doc = state.document.lock().await;

    if doc.grammar_rules.is_empty() {
        return ToolResult::text("No grammar rules defined.").with_data(json!({ "results": [] }));
    }

    let rules: Vec<_> = if args.rule_names.is_empty() {
        doc.grammar_rules.iter().collect()
    } else {
        doc.grammar_rules
            .iter()
            .filter(|r| args.rule_names.contains(&r.name))
            .collect()
    };

    // Pre-collect document metrics once
    use photonic_core::node::SceneNodeKind;
    use photonic_core::style::FillKind;

    let mut unique_colors: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut min_text_size: f64 = f64::MAX;
    let mut total_nodes = 0usize;

    for node in doc.nodes_in_draw_order() {
        if !node.visible {
            continue;
        }
        total_nodes += 1;
        match &node.kind {
            SceneNodeKind::Path(pn) => {
                if let FillKind::Solid(c) = &pn.fill.kind {
                    unique_colors.insert(format!("{:.3},{:.3},{:.3}", c.r, c.g, c.b));
                }
            }
            SceneNodeKind::Text(tn) => {
                if let FillKind::Solid(c) = &tn.fill.kind {
                    unique_colors.insert(format!("{:.3},{:.3},{:.3}", c.r, c.g, c.b));
                }
                if tn.font_size < min_text_size {
                    min_text_size = tn.font_size;
                }
            }
            SceneNodeKind::Group(_) => {}
            // raster: no fill color / text size to sample
            SceneNodeKind::Raster(_) => {}
        }
    }
    let layer_names: Vec<String> = doc
        .layer_order
        .iter()
        .filter_map(|id| doc.layers.get(id))
        .map(|l| l.name.clone())
        .collect();

    let mut results: Vec<serde_json::Value> = Vec::new();

    for rule in rules {
        let params: serde_json::Value = serde_json::from_str(&rule.params_json)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        let (passed, message) = match rule.rule_type.as_str() {
            "palette_includes" => {
                let hex = params["color_hex"].as_str().unwrap_or("").to_lowercase();
                // Parse hex to approximate r,g,b and compare against collected colors
                let target = parse_hex_to_rgb(&hex);
                let found = if let Some((tr, tg, tb)) = target {
                    unique_colors.iter().any(|c| {
                        let parts: Vec<f32> = c.split(',').filter_map(|x| x.parse().ok()).collect();
                        if parts.len() == 3 {
                            ((parts[0] - tr).abs() < 0.02)
                                && ((parts[1] - tg).abs() < 0.02)
                                && ((parts[2] - tb).abs() < 0.02)
                        } else {
                            false
                        }
                    })
                } else {
                    false
                };
                if found {
                    (true, format!("Color {} is present in the document.", hex))
                } else {
                    (
                        false,
                        format!("Color {} was not found in any visible fill.", hex),
                    )
                }
            }
            "max_colors" => {
                let limit = params["count"].as_u64().unwrap_or(10) as usize;
                if unique_colors.len() <= limit {
                    (
                        true,
                        format!(
                            "{} unique color(s) — within limit of {}.",
                            unique_colors.len(),
                            limit
                        ),
                    )
                } else {
                    (
                        false,
                        format!(
                            "{} unique color(s) exceed limit of {}.",
                            unique_colors.len(),
                            limit
                        ),
                    )
                }
            }
            "min_text_size" => {
                let min_px = params["px"].as_f64().unwrap_or(12.0);
                if min_text_size == f64::MAX {
                    (
                        true,
                        "No text nodes — constraint vacuously satisfied.".to_string(),
                    )
                } else if min_text_size >= min_px {
                    (
                        true,
                        format!(
                            "Smallest text is {:.0}px — meets minimum of {:.0}px.",
                            min_text_size, min_px
                        ),
                    )
                } else {
                    (
                        false,
                        format!(
                            "Text as small as {:.0}px found — minimum is {:.0}px.",
                            min_text_size, min_px
                        ),
                    )
                }
            }
            "required_layer" => {
                let target_name = params["name"].as_str().unwrap_or("");
                let prefix = params["prefix"].as_str().unwrap_or("");
                let found = if !target_name.is_empty() {
                    layer_names.iter().any(|n| n == target_name)
                } else if !prefix.is_empty() {
                    layer_names.iter().any(|n| n.starts_with(prefix))
                } else {
                    false
                };
                if found {
                    (true, "Required layer is present.".to_string())
                } else {
                    let desc = if !target_name.is_empty() {
                        format!("'{}'", target_name)
                    } else {
                        format!("with prefix '{}'", prefix)
                    };
                    (
                        false,
                        format!(
                            "Required layer {} not found. Layers: {}.",
                            desc,
                            layer_names.join(", ")
                        ),
                    )
                }
            }
            "max_node_count" => {
                let limit = params["count"].as_u64().unwrap_or(500) as usize;
                if total_nodes <= limit {
                    (
                        true,
                        format!("{} node(s) — within limit of {}.", total_nodes, limit),
                    )
                } else {
                    (
                        false,
                        format!("{} node(s) exceed limit of {}.", total_nodes, limit),
                    )
                }
            }
            _ => (false, format!("Unknown rule type '{}'.", rule.rule_type)),
        };

        results.push(json!({
            "rule": rule.name,
            "rule_type": rule.rule_type,
            "passed": passed,
            "message": message,
        }));
    }

    let pass_count = results
        .iter()
        .filter(|r| r["passed"].as_bool().unwrap_or(false))
        .count();
    let fail_count = results.len() - pass_count;
    let summary = if fail_count == 0 {
        format!("All {} rule(s) passed.", results.len())
    } else {
        format!(
            "{}/{} rule(s) passed, {} failed.",
            pass_count,
            results.len(),
            fail_count
        )
    };

    ToolResult::text(summary).with_data(json!({
        "pass_count": pass_count,
        "fail_count": fail_count,
        "results": results,
    }))
}

/// Parse a CSS hex color string to (r, g, b) in [0,1] range.
fn parse_hex_to_rgb(hex: &str) -> Option<(f32, f32, f32)> {
    let hex = hex.trim_start_matches('#');
    if hex.len() == 6 {
        let r = u8::from_str_radix(&hex[0..2], 16).ok()? as f32 / 255.0;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()? as f32 / 255.0;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()? as f32 / 255.0;
        Some((r, g, b))
    } else if hex.len() == 3 {
        let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()? as f32 / 255.0;
        let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()? as f32 / 255.0;
        let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()? as f32 / 255.0;
        Some((r, g, b))
    } else {
        None
    }
}

/// Define (or overwrite) a named action set — a replayable sequence of MCP tool calls.
pub async fn define_action(state: &AppState, args: DefineActionArgs) -> ToolResult {
    tracing::debug!("tool: define_action");
    if args.name.trim().is_empty() {
        return ToolResult::error("Action name must not be empty.");
    }
    if args.steps.is_empty() {
        return ToolResult::error("Action must have at least one step.");
    }
    let name = args.name.trim().to_string();
    let steps_json = serde_json::to_string(&args.steps).unwrap_or_default();
    let action_set = photonic_core::ActionSet::new(&name, &steps_json);

    let mut doc = state.document.lock().await;
    if let Some(idx) = doc.action_sets.iter().position(|a| a.name == name) {
        doc.action_sets[idx] = action_set;
        ToolResult::text(format!(
            "Action '{}' updated ({} step(s)).",
            name,
            args.steps.len()
        ))
        .with_data(json!({ "name": name, "step_count": args.steps.len() }))
    } else {
        doc.action_sets.push(action_set);
        ToolResult::text(format!(
            "Action '{}' defined ({} step(s)).",
            name,
            args.steps.len()
        ))
        .with_data(json!({ "name": name, "step_count": args.steps.len() }))
    }
}

/// List all named action sets.
pub async fn list_actions(state: &AppState) -> ToolResult {
    tracing::debug!("tool: list_actions");
    let doc = state.document.lock().await;
    let actions: Vec<serde_json::Value> = doc
        .action_sets
        .iter()
        .map(|a| {
            let step_count = serde_json::from_str::<serde_json::Value>(&a.steps_json)
                .ok()
                .and_then(|v| v.as_array().map(|arr| arr.len()))
                .unwrap_or(0);
            json!({ "name": a.name, "step_count": step_count })
        })
        .collect();
    ToolResult::text(format!("{} action(s).", actions.len()))
        .with_data(json!({ "actions": actions }))
}

/// Delete a named action set.
pub async fn delete_action(state: &AppState, args: DeleteActionArgs) -> ToolResult {
    tracing::debug!("tool: delete_action");
    let mut doc = state.document.lock().await;
    let before = doc.action_sets.len();
    doc.action_sets.retain(|a| a.name != args.name);
    if doc.action_sets.len() == before {
        ToolResult::error(format!("No action named '{}'.", args.name))
    } else {
        ToolResult::text(format!("Action '{}' deleted.", args.name))
            .with_data(json!({ "name": args.name }))
    }
}

/// Save current document state as a named branch.
pub async fn branch_create(state: &AppState, args: BranchCreateArgs) -> ToolResult {
    tracing::debug!("tool: branch_create");
    // A named branch is a label on the current history commit (non-destructive).
    let mut history = state.history.lock().await;
    history.branch_create(args.name.clone());
    ToolResult::text(format!("Named the current state '{}'.", args.name))
        .with_data(json!({ "name": args.name }))
}

/// List all named states (labeled commits).
pub async fn branch_list(state: &AppState) -> ToolResult {
    tracing::debug!("tool: branch_list");
    let history = state.history.lock().await;
    let names = history.branch_list();
    ToolResult::text(format!("{} named state(s).", names.len()))
        .with_data(json!({ "branches": names }))
}

/// Switch to a named state — a non-destructive jump to that commit (the whole
/// edit tree is preserved and the jump is reversible).
pub async fn branch_switch(state: &AppState, args: BranchSwitchArgs) -> ToolResult {
    tracing::debug!("tool: branch_switch");
    let mut history = state.history.lock().await;
    let mut doc = state.document.lock().await;
    if history.branch_switch(&args.name, &mut doc) {
        ToolResult::text(format!("Jumped to named state '{}'.", args.name))
            .with_data(json!({ "name": args.name }))
    } else {
        ToolResult::error(format!("No state named '{}' found.", args.name))
    }
}

/// Delete a named state (removes the label; the commit itself stays in the tree).
pub async fn branch_delete(state: &AppState, args: BranchDeleteArgs) -> ToolResult {
    tracing::debug!("tool: branch_delete");
    let mut history = state.history.lock().await;
    if history.branch_delete(&args.name) {
        ToolResult::text(format!("Removed name '{}'.", args.name))
    } else {
        ToolResult::error(format!("No state named '{}' found.", args.name))
    }
}

/// Register a script event trigger — maps a document event to a named action.
pub async fn register_event_trigger(
    state: &AppState,
    args: RegisterEventTriggerArgs,
) -> ToolResult {
    tracing::debug!("tool: register_event_trigger");

    const VALID_EVENTS: &[&str] = &[
        "on_open",
        "on_save",
        "on_node_create",
        "on_selection_change",
    ];
    if !VALID_EVENTS.contains(&args.event.as_str()) {
        return ToolResult::error(format!(
            "Unknown event '{}'. Valid events: {}",
            args.event,
            VALID_EVENTS.join(", ")
        ));
    }

    let mut doc = state.document.lock().await;

    // Verify the action exists.
    if !doc.action_sets.iter().any(|a| a.name == args.action_name) {
        return ToolResult::error(format!(
            "No action named '{}' found. Define it first with `define_action`.",
            args.action_name
        ));
    }

    // Avoid duplicate registrations.
    let already = doc
        .event_triggers
        .iter()
        .any(|t| t.event == args.event && t.action_name == args.action_name);
    if already {
        return ToolResult::text(format!(
            "Trigger '{}' → '{}' is already registered.",
            args.event, args.action_name
        ));
    }

    doc.event_triggers.push(photonic_core::EventTrigger {
        event: args.event.clone(),
        action_name: args.action_name.clone(),
    });

    ToolResult::text(format!(
        "Registered trigger: {} → {}.",
        args.event, args.action_name
    ))
    .with_data(json!({ "event": args.event, "action_name": args.action_name }))
}

/// List all registered script event triggers.
pub async fn list_event_triggers(state: &AppState) -> ToolResult {
    tracing::debug!("tool: list_event_triggers");
    let doc = state.document.lock().await;
    let entries: Vec<serde_json::Value> = doc
        .event_triggers
        .iter()
        .map(|t| json!({ "event": t.event, "action_name": t.action_name }))
        .collect();
    ToolResult::text(format!("{} event trigger(s) registered.", entries.len()))
        .with_data(json!({ "count": entries.len(), "triggers": entries }))
}

/// Remove one or all event triggers for a given event.
pub async fn remove_event_trigger(state: &AppState, args: RemoveEventTriggerArgs) -> ToolResult {
    tracing::debug!("tool: remove_event_trigger");
    let mut doc = state.document.lock().await;
    let before = doc.event_triggers.len();
    if let Some(ref aname) = args.action_name {
        doc.event_triggers
            .retain(|t| !(t.event == args.event && &t.action_name == aname));
    } else {
        doc.event_triggers.retain(|t| t.event != args.event);
    }
    let removed = before - doc.event_triggers.len();
    if removed == 0 {
        ToolResult::error(format!(
            "No matching triggers found for event '{}'.",
            args.event
        ))
    } else {
        ToolResult::text(format!(
            "Removed {} trigger(s) for event '{}'.",
            removed, args.event
        ))
        .with_data(json!({ "removed": removed }))
    }
}

/// Save the current properties-panel search query as a named workspace preset.
pub async fn save_workspace(state: &AppState, args: SaveWorkspaceArgs) -> ToolResult {
    tracing::debug!("tool: save_workspace name={}", args.name);
    if args.name.is_empty() {
        return ToolResult::error("Workspace name must not be empty.");
    }
    let mut doc = state.document.lock().await;
    // Persisted document state, so it rides the history like the GUI route
    // (SPEC: every document mutation, without exception, is undoable).
    // `execute_discrete` because a save is a discrete act, never a drag.
    let old = doc.workspaces.clone();
    let mut new = old.clone();
    if let Some(ws) = new.iter_mut().find(|w| w.name == args.name) {
        ws.search_query = args.search_query.clone();
    } else {
        new.push(photonic_core::Workspace {
            name: args.name.clone(),
            search_query: args.search_query.clone(),
        });
    }
    if new != old {
        let mut history = state.history.lock().await;
        history.execute_discrete(Command::SetWorkspaces { old, new }, &mut doc);
    }
    ToolResult::text(format!(
        "Workspace '{}' saved (query: {:?}).",
        args.name, args.search_query
    ))
    .with_data(serde_json::json!({ "name": args.name, "search_query": args.search_query }))
}

/// Load a named workspace — returns the search query to apply.
pub async fn load_workspace(state: &AppState, args: LoadWorkspaceArgs) -> ToolResult {
    tracing::debug!("tool: load_workspace name={}", args.name);
    let doc = state.document.lock().await;
    match doc.workspaces.iter().find(|w| w.name == args.name) {
        Some(ws) => {
            let q = ws.search_query.clone();
            ToolResult::text(format!(
                "Workspace '{}' loaded. Apply search_query: {:?}.",
                args.name, q
            ))
            .with_data(serde_json::json!({ "name": args.name, "search_query": q }))
        }
        None => ToolResult::error(format!("Workspace '{}' not found.", args.name)),
    }
}

/// List all saved workspace presets.
pub async fn list_workspaces(state: &AppState) -> ToolResult {
    tracing::debug!("tool: list_workspaces");
    let doc = state.document.lock().await;
    let items: Vec<serde_json::Value> = doc
        .workspaces
        .iter()
        .map(|w| serde_json::json!({ "name": w.name, "search_query": w.search_query }))
        .collect();
    ToolResult::text(format!("{} workspace(s) defined.", items.len()))
        .with_data(serde_json::json!({ "workspaces": items }))
}

/// Delete a named workspace preset.
pub async fn delete_workspace(state: &AppState, args: DeleteWorkspaceArgs) -> ToolResult {
    tracing::debug!("tool: delete_workspace name={}", args.name);
    let mut doc = state.document.lock().await;
    let old = doc.workspaces.clone();
    let new: Vec<_> = old
        .iter()
        .filter(|w| w.name != args.name)
        .cloned()
        .collect();
    if new.len() < old.len() {
        // Undoable, like the GUI route — see `save_workspace`.
        let mut history = state.history.lock().await;
        history.execute_discrete(Command::SetWorkspaces { old, new }, &mut doc);
        ToolResult::text(format!("Workspace '{}' deleted.", args.name))
            .with_data(serde_json::json!({ "name": args.name }))
    } else {
        ToolResult::error(format!("Workspace '{}' not found.", args.name))
    }
}
