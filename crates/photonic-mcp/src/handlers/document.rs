use crate::protocol::{ArchiveProjectArgs, PlayActionArgs, SaveDocumentArgs, ToolResult};
use crate::server::AppState;
use serde_json::json;
use std::path::PathBuf;

pub use crate::handlers::doc_analysis::{analyze_composition, detect_rhythms, measure_distances};
pub use crate::handlers::doc_automation::{
    branch_create, branch_delete, branch_list, branch_switch, check_grammar, define_action,
    define_grammar_rule, delete_action, delete_grammar_rule, delete_workspace, list_actions,
    list_event_triggers, list_grammar_rules, list_workspaces, load_workspace,
    register_event_trigger, remove_event_trigger, save_workspace,
};
pub use crate::handlers::doc_data::{
    apply_variables, define_variable, delete_variable, list_constraints, list_variables,
    remove_constraint, set_constraint, set_variable_value,
};
pub use crate::handlers::doc_export::{
    add_export_profile, delete_layer, duplicate_layer, export_artboards, export_design_tokens,
    export_icon_set, export_pdf, export_raster, export_selection_as_svg, export_svg,
    import_design_tokens, list_export_profiles, preview_selection, remove_export_profile,
    reorder_layers, run_export_profile, set_active_layer,
};
pub use crate::handlers::doc_state::{
    add_construction_line, add_dimension, apply_document_template, diff_checkpoints,
    fit_to_margins, get_artboard_margins, get_canvas_overview, get_document_bleed,
    get_document_color_mode, get_document_dpi, get_document_info, get_document_state,
    get_document_template, jump_to_history, list_checkpoints, list_dimensions, list_history, redo,
    remove_dimension, resize_canvas, restore_checkpoint, set_artboard_margins, set_document_bleed,
    set_document_color_mode, set_document_dpi, undo,
};
pub use crate::handlers::doc_styles_symbols::{
    apply_graphic_style, apply_width_profile, break_link_to_symbol, define_graphic_style,
    define_symbol, define_width_profile, delete_graphic_style, delete_symbol, delete_width_profile,
    list_graphic_styles, list_symbols, list_width_profiles, load_symbol_library, place_symbol,
    spray_symbol_instances,
};
pub use crate::handlers::doc_swatches::{
    add_color_swatch, apply_color_swatch, apply_gradient_swatch, apply_pattern_fill,
    apply_spot_color, define_pattern, define_spot_color, delete_color_swatch,
    delete_gradient_swatch, delete_pattern, delete_spot_color, list_color_swatches,
    list_gradient_swatches, list_patterns, list_spot_colors, load_swatch_library,
    save_gradient_swatch, update_color_swatch,
};

/// Save the current document and persistent history in the same native
/// `.photon` container used by the GUI's File → Save command.
pub async fn save_document(state: &AppState, args: SaveDocumentArgs) -> ToolResult {
    tracing::debug!("tool: save_document");
    let _save_gate = state.document_saves.gate.lock().await;
    let requested_path = match args.path {
        Some(path) if !path.trim().is_empty() => PathBuf::from(path),
        Some(_) => return ToolResult::error("path must not be empty"),
        None => match state.document_path.lock() {
            Ok(path) => match path.clone() {
                Some(path) => path,
                None => {
                    return ToolResult::error(
                        "This document has no current path; call save_document with a path.",
                    )
                }
            },
            Err(_) => return ToolResult::error("Could not read the current document path"),
        },
    };
    let path = if requested_path.is_absolute() {
        requested_path
    } else {
        match std::env::current_dir() {
            Ok(dir) => dir.join(requested_path),
            Err(error) => {
                return ToolResult::error(format!("Could not resolve save path: {error}"))
            }
        }
    };
    let path = match crate::path_guard::check_path(state, &path, photonic_core::PathAccess::Write) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if let Some(parent) = path.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            return ToolResult::error(format!("Could not create save directory: {error}"));
        }
    }
    let (json, bytes, receipt) = {
        let doc = state.document.lock().await;
        let mut history = state.history.lock().await;
        history.enforce_size();
        match photonic_core::save_photon(&doc, Some(&history.snapshot_state())) {
            Ok(json) => {
                let bytes = json.len();
                (
                    json,
                    bytes,
                    crate::server::DocumentSaveReceipt {
                        document: doc.id,
                        node: history.current_node(),
                        path: path.clone(),
                    },
                )
            }
            Err(error) => {
                return ToolResult::error(format!("Could not serialize document: {error}"))
            }
        }
    };
    if let Err(error) = photonic_core::write_atomic_file(&path, json.as_bytes()) {
        return ToolResult::error(format!("Could not save document: {error}"));
    }
    {
        let doc = state.document.lock().await;
        if doc.id == receipt.document {
            if let Ok(mut current_path) = state.document_path.lock() {
                *current_path = Some(path.clone());
            }
        }
        state.document_saves.record(receipt);
    }
    ToolResult::text(format!("Saved {}", path.display()))
        .with_data(json!({ "path": path, "bytes": bytes }))
}

/// Write a separate portable project folder without changing the open document
/// or its history. Large media copies run off the async runtime.
pub async fn archive_project(state: &AppState, args: ArchiveProjectArgs) -> ToolResult {
    if args.path.trim().is_empty() {
        return ToolResult::error("path must not be empty");
    }
    let requested = PathBuf::from(args.path);
    let requested = if requested.is_absolute() {
        requested
    } else {
        match std::env::current_dir() {
            Ok(dir) => dir.join(requested),
            Err(error) => return ToolResult::error(format!("Could not resolve path: {error}")),
        }
    };
    let destination =
        match crate::path_guard::check_path(state, &requested, photonic_core::PathAccess::Write) {
            Ok(path) => path,
            Err(error) => return error,
        };
    let source = match state.document_path.lock() {
        Ok(path) => path.clone(),
        Err(_) => return ToolResult::error("Could not read the current document path"),
    };
    let document = state.document.lock().await.clone();
    if let Some(project) = document.timeline.as_ref() {
        for asset in project.media.assets.values() {
            let photonic_core::timeline::AssetSource::File { path, rel_path } = &asset.source
            else {
                continue;
            };
            let local = rel_path.as_ref().and_then(|relative| {
                source
                    .as_ref()
                    .and_then(|project_file| project_file.parent())
                    .map(|dir| dir.join(relative))
            });
            let selected = local
                .as_deref()
                .filter(|candidate| candidate.is_file())
                .unwrap_or(path);
            if let Err(error) =
                crate::path_guard::check_path(state, selected, photonic_core::PathAccess::Read)
            {
                return error;
            }
        }
    }
    match tokio::task::spawn_blocking(move || {
        photonic_video::project::archive::archive_project(
            &document,
            source.as_deref(),
            &destination,
        )
    })
    .await
    {
        Ok(Ok(path)) => ToolResult::text(format!("Archived {}", path.display()))
            .with_data(json!({ "path": path })),
        Ok(Err(error)) => ToolResult::error(format!("Archive failed: {error}")),
        Err(error) => ToolResult::error(format!("Archive worker failed: {error}")),
    }
}

/// Inspect project file dependencies without changing the document or history.
/// Full-byte hashes are checked for pinned LUTs and captured reference stills.
pub async fn get_project_dependencies(state: &AppState) -> ToolResult {
    let source = match state.document_path.lock() {
        Ok(path) => path.clone(),
        Err(_) => return ToolResult::error("Could not read the current document path"),
    };
    let document = state.document.lock().await.clone();
    if let Some(project) = document.timeline.as_ref() {
        for asset in project.media.assets.values() {
            let photonic_core::timeline::AssetSource::File { path, rel_path } = &asset.source
            else {
                continue;
            };
            let local = rel_path.as_ref().and_then(|relative| {
                source
                    .as_ref()
                    .and_then(|project_file| project_file.parent())
                    .map(|dir| dir.join(relative))
            });
            let selected = local
                .as_deref()
                .filter(|candidate| candidate.is_file())
                .unwrap_or(path);
            if selected.is_file() {
                if let Err(error) =
                    crate::path_guard::check_path(state, selected, photonic_core::PathAccess::Read)
                {
                    return error;
                }
            }
        }
    }
    match tokio::task::spawn_blocking(move || {
        photonic_video::project::dependencies::inspect_dependencies(&document, source.as_deref())
    })
    .await
    {
        Ok(report) => ToolResult::text(if report.ready {
            "Project dependencies are available"
        } else {
            "Project has missing or changed dependencies"
        })
        .with_data(json!(report)),
        Err(error) => ToolResult::error(format!("Dependency inspection failed: {error}")),
    }
}

// ─── Variable Width Profiles ─────────────────────────────────────────────────

// ─── Patterns ──────────────────────────────────────────────────────────────────

// ─── Document Variables ───────────────────────────────────────────────────────

// ─── Symbols ──────────────────────────────────────────────────────────────────

// ─── Actions ─────────────────────────────────────────────────────────────────

/// Play a named action set, with optional node ID substitutions.
pub fn play_action(
    state: &AppState,
    args: PlayActionArgs,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
    Box::pin(play_action_inner(state, args))
}

async fn play_action_inner(state: &AppState, args: PlayActionArgs) -> ToolResult {
    tracing::debug!("tool: play_action '{}'", args.name);
    use crate::protocol::ActionStep;

    // Read the steps without holding the lock during dispatch
    let steps: Vec<ActionStep> = {
        let doc = state.document.lock().await;
        let action = doc.action_sets.iter().find(|a| a.name == args.name);
        match action {
            None => return ToolResult::error(format!("No action named '{}'.", args.name)),
            Some(a) => match serde_json::from_str::<Vec<ActionStep>>(&a.steps_json) {
                Ok(s) => s,
                Err(e) => return ToolResult::error(format!("Malformed action steps: {}", e)),
            },
        }
    }; // doc lock released here

    let mut completed = 0usize;
    let mut last_error: Option<String> = None;

    for step in &steps {
        // Apply node ID substitutions to args JSON
        let mut args_value = step.args.clone();
        if !args.substitutions.is_empty() {
            let mut args_str = args_value.to_string();
            for (from, to) in &args.substitutions {
                args_str = args_str.replace(from.as_str(), to.as_str());
            }
            args_value = serde_json::from_str(&args_str).unwrap_or(step.args.clone());
        }

        // Guard against recursive action playback
        if step.tool == "play_action" {
            last_error = Some(format!(
                "Step {}: play_action cannot be nested.",
                completed + 1
            ));
            break;
        }
        match crate::server::dispatch_tool_inner(state, &step.tool, args_value).await {
            Ok(output) if output.result.is_error != Some(true) => {
                completed += 1;
            }
            Ok(output) => {
                let msg = output
                    .result
                    .content
                    .first()
                    .and_then(|c| {
                        if let crate::protocol::ContentItem::Text { text } = c {
                            Some(text.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| "Unknown error".to_string());
                last_error = Some(format!("Step {} ({}): {}", completed + 1, step.tool, msg));
                break;
            }
            Err(e) => {
                last_error = Some(format!("Step {} ({}): {}", completed + 1, step.tool, e));
                break;
            }
        }
    }

    if let Some(err) = last_error {
        ToolResult::error(format!(
            "Action '{}' failed at step {}/{}: {}",
            args.name,
            completed + 1,
            steps.len(),
            err
        ))
    } else {
        ToolResult::text(format!(
            "Action '{}' completed ({}/{} steps).",
            args.name,
            completed,
            steps.len()
        ))
        .with_data(
            json!({ "name": args.name, "steps_completed": completed, "steps_total": steps.len() }),
        )
    }
}

// ─── Fit to Margins ───────────────────────────────────────────────────────────
