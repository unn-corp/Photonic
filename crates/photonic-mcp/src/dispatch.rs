use crate::handlers;
use crate::protocol::*;
use crate::server::{AppState, ToolOutput};
use photonic_core::{audit_timestamp, AuditEntry, Command, Document};
use serde_json::{json, Value};

/// Notify the checkpoint system that a mutation has occurred.
/// Resets the 60-second debounce window; the background task flushes it.
async fn post_mutation(state: &AppState, tool_name: &str) {
    state
        .history
        .lock()
        .await
        .schedule_mcp_checkpoint(tool_name);
}

/// Persisted MCP edits which predate purpose-built `Command` variants. Their
/// handlers still mutate `Document` directly, so the dispatcher wraps the
/// before/after document states in one discrete history entry. Tools that
/// already use `execute_discrete` are intentionally absent: their more compact
/// domain command remains the canonical history record.
fn needs_document_snapshot(name: &str) -> bool {
    matches!(
        name,
        "add_annotation"
            | "resolve_annotation"
            | "define_grammar_rule"
            | "delete_grammar_rule"
            | "define_action"
            | "delete_action"
            | "register_event_trigger"
            | "remove_event_trigger"
            | "save_workspace"
            | "delete_workspace"
            | "set_constraint"
            | "remove_constraint"
            | "define_variable"
            | "set_variable_value"
            | "delete_variable"
            | "add_export_profile"
            | "remove_export_profile"
            | "import_design_tokens"
            | "apply_document_template"
            | "add_construction_line"
            | "set_document_bleed"
            | "set_document_color_mode"
            | "set_document_dpi"
            | "set_artboard_margins"
            | "add_dimension"
            | "remove_dimension"
            | "define_graphic_style"
            | "delete_graphic_style"
            | "define_width_profile"
            | "delete_width_profile"
            | "define_symbol"
            | "delete_symbol"
            | "add_color_swatch"
            | "update_color_swatch"
            | "delete_color_swatch"
            | "load_swatch_library"
            | "define_pattern"
            | "delete_pattern"
            | "save_gradient_swatch"
            | "delete_gradient_swatch"
            | "define_spot_color"
            | "delete_spot_color"
            | "pin_object_guides"
            | "create_character_style"
            | "delete_character_style"
            | "create_paragraph_style"
            | "delete_paragraph_style"
    )
}

fn document_changed(before: &Document, after: &Document) -> bool {
    serde_json::to_value(before).ok() != serde_json::to_value(after).ok()
}

/// Keep transcript speech in the document/undo history rather than duplicating
/// it into the separate tool audit log. Nested edit plans follow the same rule.
fn audit_arguments(name: &str, mut args: Value) -> Value {
    match name {
        "edit_transcript_word" => {
            if let Some(text) = args.get_mut("text") {
                *text = json!("[transcript text]");
            }
        }
        "remove_filler_words" => {
            if let Some(matches) = args.get_mut("matches").and_then(Value::as_array_mut) {
                for token in matches {
                    if let Some(text) = token.get_mut("text") {
                        *text = json!("[transcript text]");
                    }
                }
            }
        }
        "apply_video_edit_plan" => {
            if let Some(operations) = args.get_mut("operations").and_then(Value::as_array_mut) {
                for operation in operations {
                    if let Some(tool) = operation
                        .get("tool")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                    {
                        if let Some(arguments) = operation.get_mut("arguments") {
                            *arguments = audit_arguments(&tool, arguments.take());
                        }
                    }
                }
            }
        }
        "execute_action" => {
            if let Some(tool) = args.get("name").and_then(Value::as_str).map(str::to_owned) {
                if let Some(arguments) = args.get_mut("arguments") {
                    *arguments = audit_arguments(&tool, arguments.take());
                }
            }
        }
        _ => {}
    }
    args
}

/// Entry point for a single MCP tool call.
///
/// `pub` (29 §3 / CAP-019): the out-of-crate acceptance-story harness scripts
/// real tool calls through this fn precisely because it — not
/// `dispatch_tool_inner` — carries the document-snapshot, undo-history and
/// audit-log side effects a client observes.
pub async fn dispatch_tool(
    state: &AppState,
    name: &str,
    args: Value,
) -> Result<ToolResult, String> {
    let start = std::time::Instant::now();
    let effective_name = if name == "execute_action" {
        args["name"].as_str().unwrap_or(name)
    } else {
        name
    };
    let snapshot_before = if needs_document_snapshot(effective_name) {
        Some(state.document.lock().await.clone())
    } else {
        None
    };
    let history_before = if snapshot_before.is_some() {
        Some(state.history.lock().await.current_node())
    } else {
        None
    };
    let output = match dispatch_tool_inner(state, name, args.clone()).await {
        Err(message) if crate::catalog::tool_schema(name).is_some() => Ok(ToolOutput::readonly(
            ToolResult::error_with_code("InvalidArguments", message),
        )),
        output => output,
    };
    let duration_ms = start.elapsed().as_millis() as u64;

    // Direct handlers leave the history head unchanged. Turn their completed
    // before/after state into exactly one discrete undo step; command-based
    // handlers have already advanced the head and are never double-recorded.
    if let (Some(before), Some(history_before), Ok(tool_output)) =
        (&snapshot_before, history_before, &output)
    {
        if tool_output.result.is_error != Some(true) {
            let mut doc = state.document.lock().await;
            let mut history = state.history.lock().await;
            if history.current_node() == history_before && document_changed(before, &doc) {
                let after = doc.clone();
                history.execute_discrete(
                    Command::ReplaceDocument {
                        old: before.clone(),
                        new: after,
                        description: format!("MCP: {name}"),
                    },
                    &mut doc,
                );
            }
        }
    }

    // Record in the audit log.
    let (result_summary, is_error) = match &output {
        Ok(o) => {
            let text = o
                .result
                .content
                .first()
                .and_then(|c| {
                    if let crate::protocol::ContentItem::Text { text } = c {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .unwrap_or("")
                .chars()
                .take(200)
                .collect::<String>();
            (text, o.result.is_error == Some(true))
        }
        Err(e) => (format!("error: {e}"), true),
    };
    let entry = AuditEntry {
        id: 0, // assigned by AuditLog::record
        timestamp: audit_timestamp(),
        tool_name: name.to_string(),
        args: audit_arguments(name, args),
        result_summary,
        duration_ms,
        is_error,
    };
    if let Ok(mut log) = state.audit_log.lock() {
        log.record(entry);
    }

    // After any successful mutation, reset the checkpoint debounce timer.
    if let Ok(ref o) = output {
        if o.mutates && o.result.is_error != Some(true) {
            post_mutation(state, name).await;
        }
    }

    output.map(|o| o.result)
}

pub(crate) async fn dispatch_tool_inner(
    state: &AppState,
    name: &str,
    args: Value,
) -> Result<ToolOutput, String> {
    match name {
        "set_preview_zones" => {
            let args = serde_json::from_value(args).map_err(|error| error.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video_workflows::set_preview_zones(state, args).await,
            ))
        }
        "precision_trim" => {
            let args = serde_json::from_value(args).map_err(|error| error.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video_workflows::precision_trim(state, args).await,
            ))
        }
        "cancel_preview" => {
            let args = serde_json::from_value(args).map_err(|error| error.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video_workflows::cancel_preview(state, args).await,
            ))
        }
        "get_preview_status" => {
            let args = serde_json::from_value(args).map_err(|error| error.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video_workflows::get_preview_status(state, args).await,
            ))
        }
        "audition_source" => {
            let args = serde_json::from_value(args).map_err(|error| error.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video_workflows::audition_source(state, args).await,
            ))
        }
        "render_preview" | "clear_preview" => {
            let args = serde_json::from_value(args).map_err(|error| error.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video_workflows::preview_command(state, args, name == "clear_preview")
                    .await,
            ))
        }
        "stop_source_audition" => {
            let _: handlers::video_inspect::EmptyArgs =
                serde_json::from_value(args).map_err(|error| error.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video_workflows::stop_source_audition(state).await,
            ))
        }
        "get_transcript" => {
            let args = serde_json::from_value(args)
                .map_err(|error| format!("Invalid arguments: {error}"))?;
            Ok(ToolOutput::readonly(
                handlers::video_transcript::get_transcript(state, args).await,
            ))
        }
        "edit_transcript_word" => {
            let args = serde_json::from_value(args)
                .map_err(|error| format!("Invalid arguments: {error}"))?;
            Ok(ToolOutput::mutating(
                handlers::video_transcript::edit_transcript_word(state, args).await,
            ))
        }
        "delete_transcript_range" => {
            let args = serde_json::from_value(args)
                .map_err(|error| format!("Invalid arguments: {error}"))?;
            Ok(ToolOutput::mutating(
                handlers::video_transcript::delete_transcript_range(state, args).await,
            ))
        }
        "find_filler_words" => {
            let args = serde_json::from_value(args)
                .map_err(|error| format!("Invalid arguments: {error}"))?;
            Ok(ToolOutput::readonly(
                handlers::video_transcript::find_filler_words(state, args).await,
            ))
        }
        "remove_filler_words" => {
            let args = serde_json::from_value(args)
                .map_err(|error| format!("Invalid arguments: {error}"))?;
            Ok(ToolOutput::mutating(
                handlers::video_transcript::remove_filler_words(state, args).await,
            ))
        }
        "get_video_capabilities" => {
            let _: handlers::video_inspect::EmptyArgs =
                serde_json::from_value(args).map_err(|error| error.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video_inspect::get_video_capabilities(state).await,
            ))
        }
        "render_frames_at" => {
            let args = serde_json::from_value(args)
                .map_err(|error| format!("Invalid arguments: {error}"))?;
            Ok(ToolOutput::readonly(
                handlers::video_inspect::render_frames_at(state, args).await,
            ))
        }
        "get_timeline_snapshot" => {
            let args = serde_json::from_value(args)
                .map_err(|error| format!("Invalid arguments: {error}"))?;
            Ok(ToolOutput::readonly(
                handlers::video_inspect::get_timeline_snapshot(state, args).await,
            ))
        }
        "apply_video_edit_plan" => {
            let args = serde_json::from_value(args)
                .map_err(|error| format!("Invalid arguments: {error}"))?;
            let result = handlers::video_edits::apply_video_edit_plan(state, args).await;
            let mutated = result
                .structured_content
                .as_ref()
                .is_some_and(|data| data["undo_steps"] == 1 && data["replayed"] == false);
            Ok(if mutated {
                ToolOutput::mutating(result)
            } else {
                ToolOutput::readonly(result)
            })
        }
        "get_action_schema" => {
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or("name is required")?;
            let result = match crate::catalog::tool_schema(name) {
                Some(schema) => {
                    ToolResult::text(format!("Complete schema for {name}")).with_data(schema)
                }
                None => {
                    ToolResult::error_with_code("UnknownTool", format!("No action named {name}"))
                }
            };
            Ok(ToolOutput::readonly(result))
        }
        // ── Mutating tools (write to the document) ──────────────────────────────
        "search_actions" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(15) as usize;
            let data = crate::catalog::search_actions(&query, limit);
            Ok(ToolOutput::readonly(
                crate::protocol::ToolResult::text(format!(
                    "Found {} action(s) for {query:?}",
                    data["count"]
                ))
                .with_data(data),
            ))
        }
        "execute_action" => {
            let action = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "execute_action requires name".to_string())?
                .to_string();
            if action == "search_actions" || action == "execute_action" {
                return Ok(ToolOutput::readonly(crate::protocol::ToolResult::error(
                    "execute_action cannot invoke itself or search_actions",
                )));
            }
            let nested = args.get("arguments").cloned().unwrap_or(json!({}));
            if crate::catalog::tool_schema(&action).is_none() {
                return Ok(ToolOutput::readonly(ToolResult::error_with_code(
                    "UnknownAction",
                    format!("Unknown action: {action}"),
                )));
            }
            // Box::pin: async recursion through dispatch_tool_inner.
            return Box::pin(dispatch_tool_inner(state, &action, nested)).await;
        }
        "create_shape" => {
            let a: CreateShapeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_shape(state, a).await,
            ))
        }
        "create_path" => {
            let a: CreatePathArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_path(state, a).await,
            ))
        }
        "create_vectors_from_css" => {
            let a: CreateVectorsFromCssArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::css_vectors::create_vectors_from_css(state, a).await,
            ))
        }
        "create_vectors_from_react" => {
            let a: CreateVectorsFromReactArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            let dry_run = a.dry_run;
            let result = handlers::react_vectors::create_vectors_from_react(state, a).await;
            Ok(if dry_run {
                ToolOutput::readonly(result)
            } else {
                ToolOutput::mutating(result)
            })
        }
        "create_curvature_path" => {
            let a: CreateCurvaturePathArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_curvature_path(state, a).await,
            ))
        }
        "create_spiral" => {
            let a: CreateSpiralArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_spiral(state, a).await,
            ))
        }
        "create_grid" => {
            let a: CreateGridArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_grid(state, a).await,
            ))
        }
        "create_polar_grid" => {
            let a: CreatePolarGridArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_polar_grid(state, a).await,
            ))
        }
        "create_text" => {
            let a: CreateTextArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_text(state, a).await,
            ))
        }

        // ── Raster (pixel) image editing ────────────────────────────────────────
        "place_image" => {
            let a = serde_json::from_value(args).map_err(|e: serde_json::Error| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::raster::place_image(state, a).await,
            ))
        }
        "create_raster_layer" => {
            let a = serde_json::from_value(args).map_err(|e: serde_json::Error| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::raster::create_raster_layer(state, a).await,
            ))
        }
        "apply_adjustment" => {
            let a = serde_json::from_value(args).map_err(|e: serde_json::Error| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::raster::apply_adjustment(state, a).await,
            ))
        }
        "create_adjustment_layer" => {
            let a = serde_json::from_value(args).map_err(|e: serde_json::Error| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::raster::create_adjustment_layer(state, a).await,
            ))
        }
        "apply_filter" => {
            let a = serde_json::from_value(args).map_err(|e: serde_json::Error| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::raster::apply_filter(state, a).await,
            ))
        }
        "brush_stroke" => {
            let a = serde_json::from_value(args).map_err(|e: serde_json::Error| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::raster::brush_stroke(state, a).await,
            ))
        }
        "bucket_fill" => {
            let a = serde_json::from_value(args).map_err(|e: serde_json::Error| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::raster::bucket_fill(state, a).await,
            ))
        }
        "gradient_fill" => {
            let a = serde_json::from_value(args).map_err(|e: serde_json::Error| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::raster::gradient_fill(state, a).await,
            ))
        }
        "transform_image" => {
            let a = serde_json::from_value(args).map_err(|e: serde_json::Error| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::raster::transform_image(state, a).await,
            ))
        }
        "set_layer_mask" => {
            let a = serde_json::from_value(args).map_err(|e: serde_json::Error| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::raster::set_layer_mask(state, a).await,
            ))
        }
        "clear_layer_mask" => {
            let a = serde_json::from_value(args).map_err(|e: serde_json::Error| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::raster::clear_layer_mask(state, a).await,
            ))
        }
        "remove_background" => {
            let a = serde_json::from_value(args).map_err(|e: serde_json::Error| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::raster::remove_background(state, a).await,
            ))
        }
        "get_raster_info" => {
            let a = serde_json::from_value(args).map_err(|e: serde_json::Error| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::raster::get_raster_info(state, a).await,
            ))
        }
        "retouch" => {
            let a = serde_json::from_value(args).map_err(|e: serde_json::Error| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::raster::retouch(state, a).await,
            ))
        }
        "liquify" => {
            let a = serde_json::from_value(args).map_err(|e: serde_json::Error| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::raster::liquify(state, a).await,
            ))
        }
        "build_shape_from_points" => {
            let a: BuildShapeFromPointsArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::build_shape_from_points(state, a).await,
            ))
        }
        "update_node" => {
            let a: UpdateNodeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::update_node(state, a).await,
            ))
        }
        "delete_nodes" => {
            let a: DeleteNodeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::delete_nodes(state, a).await,
            ))
        }
        "reorder_node" => {
            let a: ReorderNodeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::reorder_node(state, a).await,
            ))
        }
        "group_nodes" => {
            let a: GroupNodesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::group_nodes(state, a).await,
            ))
        }
        "ungroup_nodes" => {
            let a: UngroupNodesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::ungroup_nodes(state, a).await,
            ))
        }
        "boolean_operation" => {
            let a: BooleanOperationArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::boolean_operation(state, a).await,
            ))
        }
        "apply_transform" => {
            let a: ApplyTransformArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::apply_transform(state, a).await,
            ))
        }
        "create_layer" => {
            let a: CreateLayerArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::layers::create_layer(state, a).await,
            ))
        }
        "collect_in_new_layer" => {
            let a: CollectInNewLayerArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::layers::collect_in_new_layer(state, a).await,
            ))
        }
        "release_to_layers" => {
            let a: ReleaseToLayersArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::layers::release_to_layers(state, a).await,
            ))
        }
        "merge_layers" => {
            let a: MergeLayersArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::layers::merge_layers(state, a).await,
            ))
        }
        "flatten_artwork" => {
            let a: FlattenArtworkArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::layers::flatten_artwork(state, a).await,
            ))
        }
        "update_layer" => {
            let a: UpdateLayerArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::layers::update_layer(state, a).await,
            ))
        }
        "align_nodes" => {
            let a: AlignNodesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::align_nodes(state, a).await,
            ))
        }
        "duplicate_nodes" => {
            let a: DuplicateNodesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::duplicate_nodes(state, a).await,
            ))
        }
        "create_array" => {
            let a: CreateArrayArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_array(state, a).await,
            ))
        }
        "style_transfer" => {
            let a: StyleTransferArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::style_transfer(state, a).await,
            ))
        }
        "set_node_size" => {
            let a: SetNodeSizeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_node_size(state, a).await,
            ))
        }
        "find_replace_style" => {
            let a: FindReplaceStyleArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::find_replace_style(state, a).await,
            ))
        }
        "find_replace_text" => {
            let a: FindReplaceTextArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::find_replace_text(state, a).await,
            ))
        }
        "layout_nodes" => {
            let a: LayoutNodesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::layout_nodes(state, a).await,
            ))
        }
        "add_annotation" => {
            let a: AddAnnotationArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::annotations::add_annotation(state, a).await,
            ))
        }
        "resolve_annotation" => {
            let a: ResolveAnnotationArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::annotations::resolve_annotation(state, a).await,
            ))
        }
        "paste_from_history" => {
            let a: PasteFromHistoryArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::clipboard::paste_from_history(state, a).await,
            ))
        }
        "auto_name_nodes" => {
            let a: AutoNameNodesArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::auto_name_nodes(state, a).await,
            ))
        }
        "add_anchor_points" => {
            let a: AddAnchorPointsArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::add_anchor_points(state, a).await,
            ))
        }
        "delete_anchor_point" => {
            let a: DeleteAnchorPointArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::delete_anchor_point(state, a).await,
            ))
        }
        "zig_zag_path" => {
            let a: ZigZagPathArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::zig_zag_path(state, a).await,
            ))
        }
        "pucker_bloat" => {
            let a: PuckerBloatArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::pucker_bloat(state, a).await,
            ))
        }
        "roughen_path" => {
            let a: RoughenPathArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::roughen_path(state, a).await,
            ))
        }
        "twirl_path" => {
            let a: TwirlPathArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::twirl_path(state, a).await,
            ))
        }
        "proportional_move_anchor" => {
            let a: ProportionalMoveAnchorArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::proportional_move_anchor(state, a).await,
            ))
        }
        "blend_objects" => {
            let a: BlendObjectsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::blend_objects(state, a).await,
            ))
        }
        "scallop_path" => {
            let a: ScallopPathArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::scallop_path(state, a).await,
            ))
        }
        "crystallize_path" => {
            let a: CrystallizePathArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::crystallize_path(state, a).await,
            ))
        }
        "create_heart" => {
            let a: CreateHeartArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_heart(state, a).await,
            ))
        }
        "create_parametric_shape" => {
            let a: CreateParametricShapeArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_parametric_shape(state, a).await,
            ))
        }
        "create_gear" => {
            let a: CreateGearArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_gear(state, a).await,
            ))
        }
        "create_qr_code" => {
            let a: CreateQrCodeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_qr_code(state, a).await,
            ))
        }
        "tag_nodes" => {
            let a: TagNodesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::tag_nodes(state, a).await,
            ))
        }
        "sample_color_at" => {
            let a: SampleColorAtArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::nodes::sample_color_at(state, a).await,
            ))
        }
        "set_active_layer" => {
            let a: SetActiveLayerArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::set_active_layer(state, a).await,
            ))
        }
        "delete_layer" => {
            let a: DeleteLayerArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::delete_layer(state, a).await,
            ))
        }
        "move_to_layer" => {
            let a: MoveToLayerArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::move_to_layer(state, a).await,
            ))
        }
        "add_dimension_line" => {
            let a: AddDimensionLineArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::add_dimension_line(state, a).await,
            ))
        }
        "reorder_layers" => {
            let a: ReorderLayersArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::reorder_layers(state, a).await,
            ))
        }
        "set_selection" => {
            let a: SetSelectionArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_selection(state, a).await,
            ))
        }
        "get_selection" => Ok(ToolOutput::readonly(
            handlers::nodes::get_selection(state).await,
        )),
        "flatten_group" => {
            let a: FlattenGroupArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::flatten_group(state, a).await,
            ))
        }
        "center_on_canvas" => {
            let a: CenterOnCanvasArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::center_on_canvas(state, a).await,
            ))
        }
        "remove_fill" => {
            let a: RemoveStyleArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::remove_fill(state, a).await,
            ))
        }
        "remove_stroke" => {
            let a: RemoveStyleArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::remove_stroke(state, a).await,
            ))
        }
        "fit_to_canvas" => {
            let a: FitToCanvasArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::fit_to_canvas(state, a).await,
            ))
        }
        "create_scatter_plot" => {
            let a: CreateScatterPlotArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_scatter_plot(state, a).await,
            ))
        }
        "scatter_copies" => {
            let a: ScatterCopiesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::scatter_copies(state, a).await,
            ))
        }
        "create_line_chart" => {
            let a: CreateLineChartArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_line_chart(state, a).await,
            ))
        }
        "create_bar_chart" => {
            let a: CreateBarChartArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_bar_chart(state, a).await,
            ))
        }
        "create_stacked_bar_chart" => {
            let a: CreateStackedBarChartArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_stacked_bar_chart(state, a).await,
            ))
        }
        "create_pie_chart" => {
            let a: CreatePieChartArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_pie_chart(state, a).await,
            ))
        }
        "create_radar_chart" => {
            let a: CreateRadarChartArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_radar_chart(state, a).await,
            ))
        }
        "create_truchet_tiling" => {
            let a: CreateTruchetTilingArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_truchet_tiling(state, a).await,
            ))
        }
        "point_on_path" => {
            let a: PointOnPathArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::nodes::point_on_path(state, a).await,
            ))
        }
        "create_speech_bubble" => {
            let a: CreateSpeechBubbleArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_speech_bubble(state, a).await,
            ))
        }
        "set_visibility" => {
            let a: SetVisibilityArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_visibility(state, a).await,
            ))
        }
        "set_locked" => {
            let a: SetLockedArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_locked(state, a).await,
            ))
        }
        "select_all" => {
            let a: SelectAllArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::select_all(state, a).await,
            ))
        }
        "deselect_all" => {
            let a: DeselectAllArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::deselect_all(state, a).await,
            ))
        }
        "set_blend_mode" => {
            let a: SetBlendModeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_blend_mode(state, a).await,
            ))
        }
        "set_opacity" => {
            let a: SetOpacityArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_opacity(state, a).await,
            ))
        }
        "randomize_colors" => {
            let a: RandomizeColorsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::randomize_colors(state, a).await,
            ))
        }
        "swap_fill_stroke" => {
            let a: SwapFillStrokeArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::swap_fill_stroke(state, a).await,
            ))
        }
        "flip_nodes" => {
            let a: FlipNodesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::flip_nodes(state, a).await,
            ))
        }
        "create_cross" => {
            let a: CreateCrossArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_cross(state, a).await,
            ))
        }
        "measure_path" => {
            let a: MeasurePathArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::nodes::measure_path(state, a).await,
            ))
        }
        "measure_distance" => {
            let a: MeasureDistanceArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::nodes::measure_distance(state, a).await,
            ))
        }
        "create_arrow_shape" => {
            let a: CreateArrowShapeArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_arrow_shape(state, a).await,
            ))
        }
        "create_donut" => {
            let a: CreateDonutArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_donut(state, a).await,
            ))
        }
        "create_sunburst" => {
            let a: CreateSunburstArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_sunburst(state, a).await,
            ))
        }
        "create_wave_pattern" => {
            let a: CreateWavePatternArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_wave_pattern(state, a).await,
            ))
        }
        "hatch_fill" => {
            let a: HatchFillArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::hatch_fill(state, a).await,
            ))
        }
        "stipple_fill" => {
            let a: StippleFillArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::stipple_fill(state, a).await,
            ))
        }
        "add_drop_shadow" => {
            let a: AddDropShadowArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::add_drop_shadow(state, a).await,
            ))
        }
        "transform_copies" => {
            let a: TransformCopiesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::transform_copies(state, a).await,
            ))
        }
        "round_corners" => {
            let a: RoundCornersArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::round_corners(state, a).await,
            ))
        }
        "warp_envelope" => {
            let a: WarpEnvelopeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::warp_envelope(state, a).await,
            ))
        }
        "create_flare" => {
            let a: CreateFlareArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_flare(state, a).await,
            ))
        }
        "clean_up" => {
            let a: CleanUpArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::clean_up(state, a).await,
            ))
        }
        "join_paths" => {
            let a: JoinPathsArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::join_paths(state, a).await,
            ))
        }
        "pathfinder_crop" => {
            let a: PathfinderCropArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::pathfinder_crop(state, a).await,
            ))
        }
        "pathfinder_minus_back" => {
            let a: PathfinderMinusBackArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::pathfinder_minus_back(state, a).await,
            ))
        }
        "pathfinder_minus_front" => {
            let a: PathfinderMinusFrontArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::pathfinder_minus_front(state, a).await,
            ))
        }
        "pathfinder_trim" => {
            let a: PathfinderTrimArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::pathfinder_trim(state, a).await,
            ))
        }
        "pathfinder_outline" => {
            let a: PathfinderOutlineArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::pathfinder_outline(state, a).await,
            ))
        }
        "pathfinder_divide" => {
            let a: PathfinderDivideArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::pathfinder_divide(state, a).await,
            ))
        }
        "pathfinder_merge" => {
            let a: PathfinderMergeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::pathfinder_merge(state, a).await,
            ))
        }
        "divide_objects_below" => {
            let a: DivideObjectsBelowArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::divide_objects_below(state, a).await,
            ))
        }
        "reverse_path_direction" => {
            let a: ReversePathDirectionArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::reverse_path_direction(state, a).await,
            ))
        }
        "average_anchor_points" => {
            let a: AverageAnchorPointsArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::average_anchor_points(state, a).await,
            ))
        }

        // ── Read-only tools (no document writes) ────────────────────────────────
        "get_node" => {
            let a: GetNodeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::nodes::get_node(state, a).await,
            ))
        }
        "find_nodes" => {
            let a: FindNodesArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::nodes::find_nodes(state, a).await,
            ))
        }
        "select_same" => {
            let a: SelectSameArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::select_same(state, a).await,
            ))
        }
        "get_document_info" => Ok(ToolOutput::readonly(
            handlers::document::get_document_info(state).await,
        )),
        "get_document_state" => {
            let a: GetDocumentStateArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::document::get_document_state(state, a).await,
            ))
        }
        "save_document" => {
            let a: SaveDocumentArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::document::save_document(state, a).await,
            ))
        }
        "undo" => {
            let a: UndoRedoArgs = serde_json::from_value(args).unwrap_or_default();
            let (result, moved) = handlers::document::undo(state, a).await;
            Ok(if moved {
                ToolOutput::mutating(result)
            } else {
                ToolOutput::readonly(result)
            })
        }
        "redo" => {
            let a: UndoRedoArgs = serde_json::from_value(args).unwrap_or_default();
            let (result, moved) = handlers::document::redo(state, a).await;
            Ok(if moved {
                ToolOutput::mutating(result)
            } else {
                ToolOutput::readonly(result)
            })
        }
        "screenshot" => {
            let a: ScreenshotArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::canvas::screenshot(state, a).await,
            ))
        }
        "measure_nodes" => {
            let a: MeasureNodesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::nodes::measure_nodes(state, a).await,
            ))
        }
        "duplicate_layer" => {
            let a: DuplicateLayerArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::duplicate_layer(state, a).await,
            ))
        }
        "resize_canvas" => {
            let a: ResizeCanvasArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::resize_canvas(state, a).await,
            ))
        }
        "export_svg" => {
            let a: ExportSvgArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::document::export_svg(state, a).await,
            ))
        }
        "export_pdf" => {
            let a: ExportPdfArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::document::export_pdf(state, a).await,
            ))
        }
        "export_selection_as_svg" => {
            let a: ExportSelectionArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::document::export_selection_as_svg(state, a).await,
            ))
        }
        "export_icon_set" => {
            let a: ExportIconSetArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::document::export_icon_set(state, a).await,
            ))
        }
        "preview_selection" => {
            let a: PreviewSelectionArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::document::preview_selection(state, a).await,
            ))
        }
        "inspect_node" => {
            let a: InspectNodeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::nodes::inspect_node(state, a).await,
            ))
        }
        "list_annotations" => {
            let a: ListAnnotationsArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::annotations::list_annotations(state, a).await,
            ))
        }
        "copy_nodes_to_clipboard" => {
            let a: CopyNodesToClipboardArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::clipboard::copy_nodes_to_clipboard(state, a).await,
            ))
        }
        "get_clipboard_history" => {
            let a: GetClipboardHistoryArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::clipboard::get_clipboard_history(state, a).await,
            ))
        }
        "export_raster" => {
            let a: ExportRasterArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::document::export_raster(state, a).await,
            ))
        }
        "export_artboards" => {
            let a: ExportArtboardsArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::document::export_artboards(state, a).await,
            ))
        }
        "add_export_profile" => {
            let a: AddExportProfileArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::add_export_profile(state, a).await,
            ))
        }
        "list_export_profiles" => Ok(ToolOutput::readonly(
            handlers::document::list_export_profiles(state).await,
        )),
        "remove_export_profile" => {
            let a: RemoveExportProfileArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::remove_export_profile(state, a).await,
            ))
        }
        "run_export_profile" => {
            let a: RunExportProfileArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::document::run_export_profile(state, a).await,
            ))
        }
        "export_design_tokens" => {
            let a: ExportDesignTokensArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::document::export_design_tokens(state, a).await,
            ))
        }
        "get_css_preview" => {
            let a: GetCssPreviewArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::nodes::get_css_preview(state, a).await,
            ))
        }
        "check_style_continuity" => {
            let a: CheckStyleContinuityArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::nodes::check_style_continuity(state, a).await,
            ))
        }
        "list_audit_log" => {
            let a: ListAuditLogArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::audit::list_audit_log(state, a).await,
            ))
        }
        "export_audit_log" => {
            let a: ExportAuditLogArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::audit::export_audit_log(state, a).await,
            ))
        }
        "list_checkpoints" => Ok(ToolOutput::readonly(
            handlers::document::list_checkpoints(state).await,
        )),
        "restore_checkpoint" => {
            let a: RestoreCheckpointArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::restore_checkpoint(state, a).await,
            ))
        }
        "diff_checkpoints" => {
            let a: DiffCheckpointsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::document::diff_checkpoints(state, a).await,
            ))
        }
        "simplify_path" => {
            let a: SimplifyPathArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::simplify_path(state, a).await,
            ))
        }
        "smooth_path" => {
            let a: SmoothPathArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::smooth_path(state, a).await,
            ))
        }
        "snap_to_pixel" => {
            let a: SnapToPixelArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::snap_to_pixel(state, a).await,
            ))
        }
        "distribute_no_overlap" => {
            let a: DistributeNoOverlapArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::distribute_no_overlap(state, a).await,
            ))
        }
        "noise_deform" => {
            let a: NoiseDeformArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::noise_deform(state, a).await,
            ))
        }
        "mirror_copy" => {
            let a: MirrorCopyArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::mirror_copy(state, a).await,
            ))
        }
        "rotate_copies" => {
            let a: RotateCopiesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::rotate_copies(state, a).await,
            ))
        }
        "copy_appearance" => {
            let a: CopyAppearanceArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::copy_appearance(state, a).await,
            ))
        }
        "pin_object_guides" => {
            let a: PinObjectGuidesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::pin_object_guides(state, a).await,
            ))
        }
        "reverse_node_order" => {
            let a: ReverseNodeOrderArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::reverse_node_order(state, a).await,
            ))
        }
        "set_node_prompt" => {
            let a: SetNodePromptArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_node_prompt(state, a).await,
            ))
        }
        "get_node_prompts" => {
            let a: GetNodePromptsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::nodes::get_node_prompts(state, a).await,
            ))
        }
        "distribute_on_path" => {
            let a: DistributeOnPathArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::distribute_on_path(state, a).await,
            ))
        }
        "recolor_artwork" => {
            let a: RecolorArtworkArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::recolor_artwork(state, a).await,
            ))
        }
        "invert_colors" => {
            let a: InvertColorsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::invert_colors(state, a).await,
            ))
        }
        "adjust_colors" => {
            let a: AdjustColorsArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::adjust_colors(state, a).await,
            ))
        }
        "make_compound_path" => {
            let a: MakeCompoundPathArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::make_compound_path(state, a).await,
            ))
        }
        "make_live_boolean" => {
            let a: MakeLiveBooleanArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::make_live_boolean(state, a).await,
            ))
        }
        "release_compound_path" => {
            let a: ReleaseCompoundPathArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::release_compound_path(state, a).await,
            ))
        }
        "convert_to_grayscale" => {
            let a: ConvertToGrayscaleArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::convert_to_grayscale(state, a).await,
            ))
        }
        "outline_stroke" => {
            let a: OutlineStrokeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::outline_stroke(state, a).await,
            ))
        }
        "offset_path" => {
            let a: OffsetPathArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::offset_path(state, a).await,
            ))
        }
        "split_into_grid" => {
            let a: SplitIntoGridArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::split_into_grid(state, a).await,
            ))
        }
        "blend_colors" => {
            let a: BlendColorsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::blend_colors(state, a).await,
            ))
        }
        "color_guide" => {
            let a: ColorGuideArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::color_guide::color_guide(state, a).await,
            ))
        }
        "scissors_cut" => {
            let a: ScissorsCutArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::scissors_cut(state, a).await,
            ))
        }
        "add_guide" => {
            let a: AddGuideArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::add_guide(state, a).await,
            ))
        }
        "add_construction_line" => {
            let a: AddConstructionLineArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::add_construction_line(state, a).await,
            ))
        }
        "remove_guide" => {
            let a: RemoveGuideArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::remove_guide(state, a).await,
            ))
        }
        "list_guides" => {
            let a: ListGuidesArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::nodes::list_guides(state, a).await,
            ))
        }
        "clear_guides" => {
            let a: ClearGuidesArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::clear_guides(state, a).await,
            ))
        }
        "magic_wand_select" => {
            let a: MagicWandSelectArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::magic_wand_select(state, a).await,
            ))
        }
        "convert_anchor_points" => {
            let a: ConvertAnchorPointsArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::convert_anchor_points(state, a).await,
            ))
        }
        "lasso_select" => {
            let a: LassoSelectArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::lasso_select(state, a).await,
            ))
        }
        "get_recent_colors" => {
            let a: GetRecentColorsArgs =
                serde_json::from_value(args).unwrap_or(GetRecentColorsArgs {});
            Ok(ToolOutput::readonly(
                handlers::nodes::get_recent_colors(state, a).await,
            ))
        }
        "select_inside_group" => {
            let a: SelectInsideGroupArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::select_inside_group(state, a).await,
            ))
        }
        "select_by_kind" => {
            let a: SelectByKindArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::select_by_kind(state, a).await,
            ))
        }
        "create_freehand_path" => {
            let a: CreateFreehandPathArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_freehand_path(state, a).await,
            ))
        }
        "enter_isolation_mode" => {
            let a: EnterIsolationModeArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::enter_isolation_mode(state, a).await,
            ))
        }
        "exit_isolation_mode" => {
            let a: ExitIsolationModeArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::mutating(
                handlers::nodes::exit_isolation_mode(state, a).await,
            ))
        }
        "create_paragraph_style" => {
            let a: CreateParagraphStyleArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_paragraph_style(state, a).await,
            ))
        }
        "list_paragraph_styles" => Ok(ToolOutput::readonly(
            handlers::nodes::list_paragraph_styles(state).await,
        )),
        "apply_paragraph_style" => {
            let a: ApplyParagraphStyleArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::apply_paragraph_style(state, a).await,
            ))
        }
        "delete_paragraph_style" => {
            let a: DeleteParagraphStyleArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::delete_paragraph_style(state, a).await,
            ))
        }
        "create_character_style" => {
            let a: CreateCharacterStyleArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::create_character_style(state, a).await,
            ))
        }
        "list_character_styles" => Ok(ToolOutput::readonly(
            handlers::nodes::list_character_styles(state).await,
        )),
        "apply_character_style" => {
            let a: ApplyCharacterStyleArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::apply_character_style(state, a).await,
            ))
        }
        "delete_character_style" => {
            let a: DeleteCharacterStyleArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::delete_character_style(state, a).await,
            ))
        }
        "tag_node_for_export" => {
            let a: TagNodeForExportArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::tag_node_for_export(state, a).await,
            ))
        }
        "export_tagged_assets" => {
            let a: ExportTaggedAssetsArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::nodes::export_tagged_assets(state, a).await,
            ))
        }
        "select_similar" => {
            let a: SelectSimilarArgs = serde_json::from_value(args).unwrap_or(SelectSimilarArgs {
                node_ids: vec![],
                match_by: None,
                tolerance: None,
                additive: false,
            });
            Ok(ToolOutput::mutating(
                handlers::nodes::select_similar(state, a).await,
            ))
        }
        "get_document_template" => Ok(ToolOutput::readonly(
            handlers::document::get_document_template(state).await,
        )),
        "apply_document_template" => {
            let a: ApplyDocumentTemplateArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::apply_document_template(state, a).await,
            ))
        }
        "add_color_swatch" => {
            let a: AddColorSwatchArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::add_color_swatch(state, a).await,
            ))
        }
        "list_color_swatches" => Ok(ToolOutput::readonly(
            handlers::document::list_color_swatches(state).await,
        )),
        "apply_color_swatch" => {
            let a: ApplyColorSwatchArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::apply_color_swatch(state, a).await,
            ))
        }
        "update_color_swatch" => {
            let a: UpdateColorSwatchArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::update_color_swatch(state, a).await,
            ))
        }
        "delete_color_swatch" => {
            let a: DeleteColorSwatchArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::delete_color_swatch(state, a).await,
            ))
        }
        "load_swatch_library" => {
            let a: LoadSwatchLibraryArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::load_swatch_library(state, a).await,
            ))
        }
        "import_design_tokens" => {
            let a: ImportDesignTokensArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::import_design_tokens(state, a).await,
            ))
        }
        "define_graphic_style" => {
            let a: DefineGraphicStyleArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::define_graphic_style(state, a).await,
            ))
        }
        "list_graphic_styles" => Ok(ToolOutput::readonly(
            handlers::document::list_graphic_styles(state).await,
        )),
        "apply_graphic_style" => {
            let a: ApplyGraphicStyleArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::apply_graphic_style(state, a).await,
            ))
        }
        "delete_graphic_style" => {
            let a: DeleteGraphicStyleArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::delete_graphic_style(state, a).await,
            ))
        }
        "define_width_profile" => {
            let a: DefineWidthProfileArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::define_width_profile(state, a).await,
            ))
        }
        "list_width_profiles" => Ok(ToolOutput::readonly(
            handlers::document::list_width_profiles(state).await,
        )),
        "apply_width_profile" => {
            let a: ApplyWidthProfileArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::apply_width_profile(state, a).await,
            ))
        }
        "delete_width_profile" => {
            let a: DeleteWidthProfileArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::delete_width_profile(state, a).await,
            ))
        }
        "define_pattern" => {
            let a: DefinePatternArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::define_pattern(state, a).await,
            ))
        }
        "list_patterns" => Ok(ToolOutput::readonly(
            handlers::document::list_patterns(state).await,
        )),
        "apply_pattern_fill" => {
            let a: ApplyPatternFillArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::apply_pattern_fill(state, a).await,
            ))
        }
        "delete_pattern" => {
            let a: DeletePatternArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::delete_pattern(state, a).await,
            ))
        }
        "set_constraint" => {
            let a: crate::protocol::SetConstraintArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::set_constraint(state, a).await,
            ))
        }
        "list_constraints" => Ok(ToolOutput::readonly(
            handlers::document::list_constraints(state).await,
        )),
        "remove_constraint" => {
            let a: crate::protocol::RemoveConstraintArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::remove_constraint(state, a).await,
            ))
        }
        "define_symbol" => {
            let a: DefineSymbolArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::define_symbol(state, a).await,
            ))
        }
        "list_symbols" => Ok(ToolOutput::readonly(
            handlers::document::list_symbols(state).await,
        )),
        "place_symbol" => {
            let a: PlaceSymbolArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::place_symbol(state, a).await,
            ))
        }
        "break_link_to_symbol" => {
            let a: BreakLinkToSymbolArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::break_link_to_symbol(state, a).await,
            ))
        }
        "delete_symbol" => {
            let a: DeleteSymbolArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::delete_symbol(state, a).await,
            ))
        }
        "get_canvas_overview" => {
            let a: GetCanvasOverviewArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::document::get_canvas_overview(state, a).await,
            ))
        }
        "save_gradient_swatch" => {
            let a: SaveGradientSwatchArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::save_gradient_swatch(state, a).await,
            ))
        }
        "list_gradient_swatches" => Ok(ToolOutput::readonly(
            handlers::document::list_gradient_swatches(state).await,
        )),
        "apply_gradient_swatch" => {
            let a: ApplyGradientSwatchArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::apply_gradient_swatch(state, a).await,
            ))
        }
        "set_paint" => {
            let a: SetPaintArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_paint(state, a).await,
            ))
        }
        "delete_gradient_swatch" => {
            let a: DeleteGradientSwatchArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::delete_gradient_swatch(state, a).await,
            ))
        }
        "analyze_composition" => {
            let a: AnalyzeCompositionArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::document::analyze_composition(state, a).await,
            ))
        }
        "detect_rhythms" => {
            let a: DetectRhythmsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::document::detect_rhythms(state, a).await,
            ))
        }
        "measure_distances" => {
            let a: MeasureDistancesArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::document::measure_distances(state, a).await,
            ))
        }
        "define_action" => {
            let a: DefineActionArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::define_action(state, a).await,
            ))
        }
        "list_actions" => Ok(ToolOutput::readonly(
            handlers::document::list_actions(state).await,
        )),
        "delete_action" => {
            let a: DeleteActionArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::delete_action(state, a).await,
            ))
        }
        "play_action" => {
            let a: PlayActionArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::play_action(state, a).await,
            ))
        }
        "register_event_trigger" => {
            let a: RegisterEventTriggerArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::register_event_trigger(state, a).await,
            ))
        }
        "list_event_triggers" => Ok(ToolOutput::readonly(
            handlers::document::list_event_triggers(state).await,
        )),
        "remove_event_trigger" => {
            let a: RemoveEventTriggerArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::remove_event_trigger(state, a).await,
            ))
        }
        "save_workspace" => {
            let a: SaveWorkspaceArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::save_workspace(state, a).await,
            ))
        }
        "load_workspace" => {
            let a: LoadWorkspaceArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::document::load_workspace(state, a).await,
            ))
        }
        "list_workspaces" => Ok(ToolOutput::readonly(
            handlers::document::list_workspaces(state).await,
        )),
        "delete_workspace" => {
            let a: DeleteWorkspaceArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::delete_workspace(state, a).await,
            ))
        }
        "spray_symbol_instances" => {
            let a: SpraySymbolInstancesArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::spray_symbol_instances(state, a).await,
            ))
        }
        "load_symbol_library" => {
            let a: LoadSymbolLibraryArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::load_symbol_library(state, a).await,
            ))
        }
        "define_grammar_rule" => {
            let a: DefineGrammarRuleArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::define_grammar_rule(state, a).await,
            ))
        }
        "list_grammar_rules" => Ok(ToolOutput::readonly(
            handlers::document::list_grammar_rules(state).await,
        )),
        "delete_grammar_rule" => {
            let a: DeleteGrammarRuleArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::delete_grammar_rule(state, a).await,
            ))
        }
        "check_grammar" => {
            let a: CheckGrammarArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::document::check_grammar(state, a).await,
            ))
        }
        "list_history" => {
            let a: ListHistoryArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::document::list_history(state, a).await,
            ))
        }
        "jump_to_history" => {
            let a: JumpToHistoryArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::jump_to_history(state, a).await,
            ))
        }
        "fit_to_margins" => {
            let a: FitToMarginsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::fit_to_margins(state, a).await,
            ))
        }
        "add_dimension" => {
            let a: AddDimensionArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::add_dimension(state, a).await,
            ))
        }
        "list_dimensions" => Ok(ToolOutput::readonly(
            handlers::document::list_dimensions(state).await,
        )),
        "remove_dimension" => {
            let a: RemoveDimensionArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::remove_dimension(state, a).await,
            ))
        }
        "set_document_bleed" => {
            let a: SetDocumentBleedArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::set_document_bleed(state, a).await,
            ))
        }
        "get_document_bleed" => Ok(ToolOutput::readonly(
            handlers::document::get_document_bleed(state).await,
        )),
        "set_document_color_mode" => {
            let a: SetDocumentColorModeArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::set_document_color_mode(state, a).await,
            ))
        }
        "get_document_color_mode" => Ok(ToolOutput::readonly(
            handlers::document::get_document_color_mode(state).await,
        )),
        "set_document_dpi" => {
            let a: SetDocumentDpiArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::set_document_dpi(state, a).await,
            ))
        }
        "get_document_dpi" => Ok(ToolOutput::readonly(
            handlers::document::get_document_dpi(state).await,
        )),
        "set_artboard_margins" => {
            let a: SetArtboardMarginsArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::set_artboard_margins(state, a).await,
            ))
        }
        "get_artboard_margins" => Ok(ToolOutput::readonly(
            handlers::document::get_artboard_margins(state).await,
        )),
        "list_artboards" => {
            let a: ListArtboardsArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::artboards::list_artboards(state, a).await,
            ))
        }
        "add_artboard" => {
            let a: AddArtboardArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::artboards::add_artboard(state, a).await,
            ))
        }
        "update_artboard" => {
            let a: UpdateArtboardArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::artboards::update_artboard(state, a).await,
            ))
        }
        "duplicate_artboard" => {
            let a: DuplicateArtboardArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::artboards::duplicate_artboard(state, a).await,
            ))
        }
        "move_artboard" => {
            let a: MoveArtboardArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::artboards::move_artboard(state, a).await,
            ))
        }
        "remove_artboard" => {
            let a: RemoveArtboardArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::artboards::remove_artboard(state, a).await,
            ))
        }
        "set_active_artboard" => {
            let a: SetActiveArtboardArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::artboards::set_active_artboard(state, a).await,
            ))
        }
        "define_spot_color" => {
            let a: DefineSpotColorArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::define_spot_color(state, a).await,
            ))
        }
        "list_spot_colors" => Ok(ToolOutput::readonly(
            handlers::document::list_spot_colors(state).await,
        )),
        "apply_spot_color" => {
            let a: ApplySpotColorArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::apply_spot_color(state, a).await,
            ))
        }
        "delete_spot_color" => {
            let a: DeleteSpotColorArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::delete_spot_color(state, a).await,
            ))
        }
        "branch_create" => {
            let a: BranchCreateArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::document::branch_create(state, a).await,
            ))
        }
        "branch_list" => Ok(ToolOutput::readonly(
            handlers::document::branch_list(state).await,
        )),
        "branch_switch" => {
            let a: BranchSwitchArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::branch_switch(state, a).await,
            ))
        }
        "branch_delete" => {
            let a: BranchDeleteArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::document::branch_delete(state, a).await,
            ))
        }
        "define_variable" => {
            let a: DefineVariableArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::define_variable(state, a).await,
            ))
        }
        "list_variables" => Ok(ToolOutput::readonly(
            handlers::document::list_variables(state).await,
        )),
        "set_variable_value" => {
            let a: SetVariableValueArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::set_variable_value(state, a).await,
            ))
        }
        "delete_variable" => {
            let a: DeleteVariableArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::document::delete_variable(state, a).await,
            ))
        }
        "apply_variables" => Ok(ToolOutput::mutating(
            handlers::document::apply_variables(state).await,
        )),
        "bind_text_variable" => {
            let a: BindTextVariableArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::bind_text_variable(state, a).await,
            ))
        }
        "unbind_text_variable" => {
            let a: UnbindTextVariableArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::unbind_text_variable(state, a).await,
            ))
        }
        "set_text_area" => {
            let a: SetTextAreaArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_text_area(state, a).await,
            ))
        }
        "clear_text_area" => {
            let a: ClearTextAreaArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::clear_text_area(state, a).await,
            ))
        }
        "set_paragraph_options" => {
            let a: SetParagraphOptionsArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_paragraph_options(state, a).await,
            ))
        }
        "set_tab_stops" => {
            let a: SetTabStopsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_tab_stops(state, a).await,
            ))
        }
        "clear_tab_stops" => {
            let a: ClearTabStopsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::clear_tab_stops(state, a).await,
            ))
        }
        "set_text_decoration" => {
            let a: SetTextDecorationArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_text_decoration(state, a).await,
            ))
        }
        "set_character_metrics" => {
            let a: SetCharacterMetricsArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_character_metrics(state, a).await,
            ))
        }
        "set_opentype_features" => {
            let a: SetOpenTypeFeaturesArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_opentype_features(state, a).await,
            ))
        }
        "get_opentype_features" => {
            let a: GetOpenTypeFeaturesArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::nodes::get_opentype_features(state, a).await,
            ))
        }
        "link_text_frames" => {
            let a: LinkTextFramesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::link_text_frames(state, a).await,
            ))
        }
        "unlink_text_frames" => {
            let a: UnlinkTextFramesArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::unlink_text_frames(state, a).await,
            ))
        }
        "set_blend_spine" => {
            let a: SetBlendSpineArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_blend_spine(state, a).await,
            ))
        }
        "clear_blend_spine" => {
            let a: ClearBlendSpineArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::clear_blend_spine(state, a).await,
            ))
        }
        "reverse_blend_spine" => {
            let a: ReverseBlendSpineArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::reverse_blend_spine(state, a).await,
            ))
        }
        "expand_blend" => {
            let a: ExpandBlendArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::expand_blend(state, a).await,
            ))
        }
        "set_symbol_override" => {
            let a: SetSymbolOverrideArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_symbol_override(state, a).await,
            ))
        }
        "clear_symbol_overrides" => {
            let a: ClearSymbolOverridesArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::clear_symbol_overrides(state, a).await,
            ))
        }
        "set_text_direction" => {
            let a: SetTextDirectionArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_text_direction(state, a).await,
            ))
        }
        "set_font_style" => {
            let a: SetFontStyleArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_font_style(state, a).await,
            ))
        }
        "set_font_weight" => {
            let a: SetFontWeightArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_font_weight(state, a).await,
            ))
        }
        "flatten_transparency" => {
            let a: FlattenTransparencyArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::flatten_transparency(state, a).await,
            ))
        }
        "apply_flex_layout" => {
            let a: ApplyFlexLayoutArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::apply_flex_layout(state, a).await,
            ))
        }
        "undo_node" => {
            let a: UndoNodeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::undo_node(state, a).await,
            ))
        }
        "apply_grid_layout" => {
            let a: ApplyGridLayoutArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::apply_grid_layout(state, a).await,
            ))
        }
        "apply_stack_layout" => {
            let a: ApplyStackLayoutArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::apply_stack_layout(state, a).await,
            ))
        }
        "set_text_path" => {
            let a: SetTextPathArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::set_text_path(state, a).await,
            ))
        }
        "clear_text_path" => {
            let a: ClearTextPathArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::clear_text_path(state, a).await,
            ))
        }
        "make_clipping_mask" => {
            let a: MakeClippingMaskArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::make_clipping_mask(state, a).await,
            ))
        }
        "release_clipping_mask" => {
            let a: ReleaseClippingMaskArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::nodes::release_clipping_mask(state, a).await,
            ))
        }

        // ── Video domain (10-mcp-tools.md, P2: timeline-EDIT tools) ─────────────
        // Sequence (10 §3.2)
        "create_sequence" => {
            let a: CreateSequenceArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::create_sequence(state, a).await,
            ))
        }
        "delete_sequence" => {
            let a: DeleteSequenceArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::delete_sequence(state, a).await,
            ))
        }
        "list_sequences" => {
            let a: ListSequencesArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::video::list_sequences(state, a).await,
            ))
        }
        "set_active_sequence" => {
            let a: SetActiveSequenceArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_active_sequence(state, a).await,
            ))
        }
        "set_sequence_format" => {
            let a: SetSequenceFormatArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_sequence_format(state, a).await,
            ))
        }
        "set_active_format" => {
            let a: SetActiveFormatArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_active_format(state, a).await,
            ))
        }
        "set_work_range" => {
            let a: SetWorkRangeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_work_range(state, a).await,
            ))
        }
        "add_marker" => {
            let a: AddMarkerArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::add_marker(state, a).await,
            ))
        }
        "remove_marker" => {
            let a: RemoveMarkerArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::remove_marker(state, a).await,
            ))
        }
        "list_markers" => {
            let a: ListMarkersArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::list_markers(state, a).await,
            ))
        }

        // Marker depth: editing, clip scope, categories (26 K-A2)
        "set_marker" => {
            let a: SetMarkerArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_marker(state, a).await,
            ))
        }
        "add_clip_marker" => {
            let a: AddClipMarkerArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::add_clip_marker(state, a).await,
            ))
        }
        "remove_clip_marker" => {
            let a: RemoveClipMarkerArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::remove_clip_marker(state, a).await,
            ))
        }
        "list_clip_markers" => {
            let a: ListClipMarkersArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::list_clip_markers(state, a).await,
            ))
        }
        "list_marker_categories" => {
            let a: ListMarkerCategoriesArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::list_marker_categories(state, a).await,
            ))
        }
        "add_marker_category" => {
            let a: AddMarkerCategoryArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::add_marker_category(state, a).await,
            ))
        }
        "seed_marker_categories" => {
            let a: SeedMarkerCategoriesArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::seed_marker_categories(state, a).await,
            ))
        }
        "update_marker_category" => {
            let a: UpdateMarkerCategoryArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::update_marker_category(state, a).await,
            ))
        }
        "remove_marker_category" => {
            let a: RemoveMarkerCategoryArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::remove_marker_category(state, a).await,
            ))
        }

        // Track (10 §3.3)
        "add_track" => {
            let a: AddTrackArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::add_track(state, a).await,
            ))
        }
        "remove_track" => {
            let a: RemoveTrackArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::remove_track(state, a).await,
            ))
        }
        "set_track_prop" => {
            let a: SetTrackPropArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_track_prop(state, a).await,
            ))
        }
        "reorder_track" => {
            let a: ReorderTrackArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::reorder_track(state, a).await,
            ))
        }

        // Clip edit ops (10 §3.4)
        "insert_clip" => {
            let a: InsertClipArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::insert_clip(state, a).await,
            ))
        }
        "move_clip" => {
            let a: MoveClipArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::move_clip(state, a).await,
            ))
        }
        "move_clips" => {
            let a: MoveClipsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::move_clips(state, a).await,
            ))
        }
        "trim_clip" => {
            let a: TrimClipArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::trim_clip(state, a).await,
            ))
        }
        "split_clip" => {
            let a: SplitClipArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::split_clip(state, a).await,
            ))
        }
        "remove_clip" => {
            let a: RemoveClipArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::remove_clip(state, a).await,
            ))
        }
        "roll_edit" => {
            let a: RollEditArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::roll_edit(state, a).await,
            ))
        }
        "slip_clip" => {
            let a: SlipClipArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::slip_clip(state, a).await,
            ))
        }
        "slide_clip" => {
            let a: SlideClipArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::slide_clip(state, a).await,
            ))
        }
        "ripple_edit" => {
            let a: RippleEditArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::ripple_edit(state, a).await,
            ))
        }

        // 3/4-point editing (16 §2, CAP-019 MCP parity)
        "insert_edit" => {
            let a: InsertEditArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::insert_edit(state, a).await,
            ))
        }
        "overwrite_edit" => {
            let a: OverwriteEditArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::overwrite_edit(state, a).await,
            ))
        }
        "lift_edit" => {
            let a: LiftEditArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::lift_edit(state, a).await,
            ))
        }
        "extract_edit" => {
            let a: ExtractEditArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::extract_edit(state, a).await,
            ))
        }

        // NLE parity round-2 (17-nle-parity-round2.md, G21 CAP-019 MCP parity)
        "replace_clip_source" => {
            let a: ReplaceClipSourceArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::replace_clip_source(state, a).await,
            ))
        }
        "add_edit_all_tracks" => {
            let a: AddEditAllTracksArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::add_edit_all_tracks(state, a).await,
            ))
        }
        "close_gap" => {
            let a: CloseGapArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::close_gap(state, a).await,
            ))
        }
        "insert_space" => {
            let a: InsertSpaceArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::insert_space(state, a).await,
            ))
        }
        "remove_space" => {
            let a: RemoveSpaceArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::remove_space(state, a).await,
            ))
        }
        "remove_all_spaces_after" => {
            let a: SpaceAfterArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::remove_all_spaces_after(state, a).await,
            ))
        }
        "remove_clips_after" => {
            let a: SpaceAfterArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::remove_clips_after(state, a).await,
            ))
        }
        "match_frame" => {
            let a: MatchFrameArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::match_frame(state, a).await,
            ))
        }
        "insert_adjustment_clip" => {
            let a: InsertAdjustmentClipArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::insert_adjustment_clip(state, a).await,
            ))
        }
        "insert_text_clip" => {
            let a: InsertTextClipArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::insert_text_clip(state, a).await,
            ))
        }

        // Clip properties (10 §3.5)
        "set_clip_prop" => {
            let a: SetClipPropArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_clip_prop(state, a).await,
            ))
        }
        // D-12 gyro stabilization (22 §6.5)
        "import_motion_metadata" => {
            let a: ImportMotionMetadataArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::import_motion_metadata(state, a).await,
            ))
        }
        "set_stabilization" => {
            let a: SetStabilizationArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_stabilization(state, a).await,
            ))
        }
        "analyze_stabilization" => {
            let a: AnalyzeStabilizationArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            // Analysis is generation, not history (22 §6.5): it produces a
            // cache entry, never an undo step, so it is not `mutating`.
            Ok(ToolOutput::readonly(
                handlers::video::analyze_stabilization(state, a).await,
            ))
        }
        "get_stabilization_status" => {
            let a: GetStabilizationStatusArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::get_stabilization_status(state, a).await,
            ))
        }
        "set_clip_speed" => {
            let a: SetClipSpeedArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_clip_speed(state, a).await,
            ))
        }
        "set_transition" => {
            let a: SetTransitionArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_transition(state, a).await,
            ))
        }

        // Clip organization: linking (14 §M-2, CAP-019 MCP parity)
        "link_clips" => {
            let a: LinkClipsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::link_clips(state, a).await,
            ))
        }
        "unlink_clips" => {
            let a: UnlinkClipsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::unlink_clips(state, a).await,
            ))
        }

        "list_clips" => {
            let a: ListClipsArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::video::list_clips(state, a).await,
            ))
        }
        "get_clip" => {
            let a: GetClipArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::get_clip(state, a).await,
            ))
        }

        // Effects (10 §3.6)
        "add_effect" => {
            let a: AddEffectArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::add_effect(state, a).await,
            ))
        }
        "remove_effect" => {
            let a: RemoveEffectArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::remove_effect(state, a).await,
            ))
        }
        "reorder_effects" => {
            let a: ReorderEffectsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::reorder_effects(state, a).await,
            ))
        }
        "set_effect_param" => {
            let a: SetEffectParamArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_effect_param(state, a).await,
            ))
        }
        "set_effect_zone" => {
            let a: SetEffectZoneArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_effect_zone(state, a).await,
            ))
        }
        "effect_stack" => {
            let a: EffectStackArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            let readonly = a.op == EffectStackOp::List;
            let res = handlers::video::effect_stack(state, a).await;
            Ok(if readonly {
                ToolOutput::readonly(res)
            } else {
                ToolOutput::mutating(res)
            })
        }
        "paste_attributes" => {
            let a: PasteAttributesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::paste_attributes(state, a).await,
            ))
        }
        "freeze_frame" => {
            let a: FreezeFrameArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::freeze_frame(state, a).await,
            ))
        }

        // Effect presets, custom stacks and favourites (26 §10 K-B4). The
        // library is a config file, so only `effect_preset_apply` is
        // `mutating` — the rest never produce a document edit or an undo step.
        "effect_preset_list" => {
            let a: EffectPresetListArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::video::effect_preset_list(state, a).await,
            ))
        }
        "effect_preset_save" => {
            let a: EffectPresetSaveArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::effect_preset_save(state, a).await,
            ))
        }
        "effect_preset_apply" => {
            let a: EffectPresetApplyArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::effect_preset_apply(state, a).await,
            ))
        }
        "effect_preset_delete" => {
            let a: EffectPresetDeleteArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::effect_preset_delete(state, a).await,
            ))
        }
        "effect_preset_rename" => {
            let a: EffectPresetRenameArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::effect_preset_rename(state, a).await,
            ))
        }
        "effect_favourite_list" => {
            let a: EffectFavouriteListArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::video::effect_favourite_list(state, a).await,
            ))
        }
        "effect_favourite_set" => {
            let a: EffectFavouriteSetArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::effect_favourite_set(state, a).await,
            ))
        }
        "list_effect_kinds" => {
            let a: ListEffectKindsArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::video::list_effect_kinds(state, a).await,
            ))
        }

        // Keyframes (10 §3.7)
        "set_keyframe" => {
            let a: SetKeyframeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_keyframe(state, a).await,
            ))
        }
        "remove_keyframe" => {
            let a: RemoveKeyframeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::remove_keyframe(state, a).await,
            ))
        }
        "batch_set_keyframes" => {
            let a: BatchSetKeyframesArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::batch_set_keyframes(state, a).await,
            ))
        }
        "get_keyframes" => {
            let a: GetKeyframesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::get_keyframes(state, a).await,
            ))
        }
        "copy_keyframes" => {
            let a: CopyKeyframesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::copy_keyframes(state, a).await,
            ))
        }
        "paste_keyframes" => {
            let a: PasteKeyframesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::paste_keyframes(state, a).await,
            ))
        }

        // Media (P2 subset)
        "import_media" => {
            let a: ImportMediaArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::import_media(state, a).await,
            ))
        }
        "relink_media" => {
            let a: RelinkMediaArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::relink_media(state, a).await,
            ))
        }
        "find_offline_media" => {
            let a: FindOfflineMediaArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::video::find_offline_media(state, a).await,
            ))
        }
        "relink_media_batch" => {
            let a: RelinkMediaBatchArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            // A dry run mutates nothing, so it must not schedule a checkpoint
            // (same rule `effect_stack`'s list op follows).
            let readonly = a.dry_run.unwrap_or(false);
            let res = handlers::video::relink_media_batch(state, a).await;
            Ok(if readonly {
                ToolOutput::readonly(res)
            } else {
                ToolOutput::mutating(res)
            })
        }
        "list_media" => {
            let a: ListMediaArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::video::list_media(state, a).await,
            ))
        }
        "remove_asset" => {
            let a: RemoveAssetArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::remove_asset(state, a).await,
            ))
        }
        "create_subclip" => {
            let a: CreateSubclipArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::create_subclip(state, a).await,
            ))
        }
        "set_asset_tags" => {
            let a: SetAssetTagsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_asset_tags(state, a).await,
            ))
        }
        "create_bin" => {
            let a: CreateBinArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::create_bin(state, a).await,
            ))
        }
        "remove_bin" => {
            let a: RemoveBinArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::remove_bin(state, a).await,
            ))
        }
        "set_asset_bin" => {
            let a: SetAssetBinArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_asset_bin(state, a).await,
            ))
        }
        "list_bins" => {
            let a: ListBinsArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::video::list_bins(state, a).await,
            ))
        }

        // ── Video domain (10-mcp-tools.md, P3: engine-backed tools) ─────────────
        // Playback (10 §3.13): these mutate ENGINE/SESSION state, never the
        // document — §3.13's `mutating*`. Classified readonly here so no
        // checkpoint debounce is scheduled (design rule: playhead is session
        // state, 01 §11 — no Command, no undo step).
        "play" => {
            let a: PlayArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(handlers::video::play(state, a).await))
        }
        "pause" => {
            let a: PauseArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(handlers::video::pause(state, a).await))
        }
        "seek" => {
            let a: SeekArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(handlers::video::seek(state, a).await))
        }
        "step" => {
            let a: StepArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(handlers::video::step(state, a).await))
        }
        "set_loop_range" => {
            let a: SetLoopRangeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::set_loop_range(state, a).await,
            ))
        }
        "set_proxy_mode" => {
            let a: SetProxyModeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::set_proxy_mode(state, a).await,
            ))
        }
        "get_engine_status" => {
            let a: GetEngineStatusArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::video::get_engine_status(state, a).await,
            ))
        }

        // Render (10 §3.14 / §4)
        "render_frame_at" => {
            let a: RenderFrameAtArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::render_frame_at(state, a).await,
            ))
        }

        // Media engine ops (10 §3.1). `probe_media` starts a job that commits
        // its asset update from the worker (design rule 6 — the worker
        // schedules its own checkpoint); the start call itself mutates nothing.
        "probe_media" => {
            let a: ProbeMediaArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::probe_media(state, a).await,
            ))
        }
        "generate_proxies" => {
            let a: GenerateProxiesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::generate_proxies(state, a).await,
            ))
        }
        "remove_proxy" => {
            let a: RemoveProxyArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::remove_proxy(state, a).await,
            ))
        }
        "attach_proxy" => {
            let a: AttachProxyArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            // Document mutation happens inside the handler (set_asset_proxy +
            // MCP checkpoint); treat as readonly dispatch like generate/remove.
            Ok(ToolOutput::readonly(
                handlers::video::attach_proxy(state, a).await,
            ))
        }
        "detach_proxy" => {
            let a: DetachProxyArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::detach_proxy(state, a).await,
            ))
        }
        "transcode_media" => {
            let a: TranscodeMediaArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::transcode_media(state, a).await,
            ))
        }

        // Export + jobs (10 §3.15 / §6). `export_sequence` is readonly at call
        // time (10 §6: "the export itself never mutates the timeline");
        // preset save/delete are app-config side effects, not document
        // mutations (10 §3.15).
        "export_sequences" => {
            let args = serde_json::from_value(args).map_err(|error| error.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video_export::export_sequences(state, args).await,
            ))
        }
        "export_sequence" => {
            let a: ExportSequenceArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::export_sequence(state, a).await,
            ))
        }
        "get_job_status" => {
            let a: GetJobStatusArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::get_job_status(state, a).await,
            ))
        }
        "cancel_job" => {
            let a: CancelJobArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::cancel_job(state, a).await,
            ))
        }
        "list_export_presets" => {
            let a: ListExportPresetsArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::video::list_export_presets(state, a).await,
            ))
        }
        "save_export_preset" => {
            let a: SaveExportPresetArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::save_export_preset(state, a).await,
            ))
        }
        "delete_export_preset" => {
            let a: DeleteExportPresetArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::delete_export_preset(state, a).await,
            ))
        }

        // ── Video domain, P4+ slice (10-mcp-tools.md §3.8–§3.12, 05 §4b) ────────
        // Captions (10 §3.8)
        "auto_caption" => {
            let a: AutoCaptionArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::auto_caption(state, a).await,
            ))
        }
        "add_caption_track" => {
            let a: AddCaptionTrackArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::add_caption_track(state, a).await,
            ))
        }
        "remove_caption_track" => {
            let a: RemoveCaptionTrackArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::remove_caption_track(state, a).await,
            ))
        }
        "get_caption_track" => {
            let a: GetCaptionTrackArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::get_caption_track(state, a).await,
            ))
        }
        "set_caption_cue" => {
            let a: SetCaptionCueArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_caption_cue(state, a).await,
            ))
        }
        "split_caption_cue" => {
            let a: SplitCaptionCueArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::split_caption_cue(state, a).await,
            ))
        }
        "merge_caption_cues" => {
            let a: MergeCaptionCuesArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::merge_caption_cues(state, a).await,
            ))
        }
        "set_caption_word" => {
            let a: SetCaptionWordArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_caption_word(state, a).await,
            ))
        }
        "set_caption_style" => {
            let a: SetCaptionStyleArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_caption_style(state, a).await,
            ))
        }
        "import_captions" => {
            let a: ImportCaptionsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::import_captions(state, a).await,
            ))
        }
        "export_captions" => {
            let a: ExportCaptionsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::export_captions(state, a).await,
            ))
        }

        // TTS (10 §3.9). Job start touches no document; the worker commits.
        "generate_voiceover" => {
            let a: GenerateVoiceoverArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::generate_voiceover(state, a).await,
            ))
        }

        // Grade (10 §3.10)
        "set_grade" => {
            let a: SetGradeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_grade(state, a).await,
            ))
        }
        "apply_lut" => {
            let a: ApplyLutArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::apply_lut(state, a).await,
            ))
        }
        "copy_grade" => {
            let a: CopyGradeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::copy_grade(state, a).await,
            ))
        }
        "grade_preset" => {
            let a: GradePresetArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::grade_preset(state, a).await,
            ))
        }
        "get_scopes" => {
            let a: GetScopesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::get_scopes(state, a).await,
            ))
        }

        // Node graph (10 §3.11)
        "create_clip_composition" => {
            let a: CreateClipCompositionArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::create_clip_composition(state, a).await,
            ))
        }
        "add_graph_node" => {
            let a: AddGraphNodeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::add_graph_node(state, a).await,
            ))
        }
        "remove_graph_node" => {
            let a: RemoveGraphNodeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::remove_graph_node(state, a).await,
            ))
        }
        "add_graph_edge" => {
            let a: AddGraphEdgeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::add_graph_edge(state, a).await,
            ))
        }
        "remove_graph_edge" => {
            let a: RemoveGraphEdgeArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::remove_graph_edge(state, a).await,
            ))
        }
        "set_graph_node_param" => {
            let a: SetGraphNodeParamArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_graph_node_param(state, a).await,
            ))
        }
        "set_project_graph" => {
            let a: SetProjectGraphArgs =
                serde_json::from_value(args).unwrap_or(SetProjectGraphArgs {
                    graph_id: None,
                    clear: false,
                });
            Ok(ToolOutput::mutating(
                handlers::video::set_project_graph(state, a).await,
            ))
        }
        "get_graph" => {
            let a: GetGraphArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::get_graph(state, a).await,
            ))
        }

        // Audio (10 §3.12)
        "set_clip_audio" => {
            let a: SetClipAudioArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_clip_audio(state, a).await,
            ))
        }
        "set_track_audio" => {
            let a: SetTrackAudioArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_track_audio(state, a).await,
            ))
        }
        "audio_fx" => {
            let a: AudioFxArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::audio_fx(state, a).await,
            ))
        }
        "set_master_bus" => {
            let a: SetMasterBusArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::set_master_bus(state, a).await,
            ))
        }
        "get_audio_meters" => {
            let a: GetAudioMetersArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::readonly(
                handlers::video::get_audio_meters(state, a).await,
            ))
        }
        "get_waveform" => {
            let a: GetWaveformArgs = serde_json::from_value(args).unwrap_or(GetWaveformArgs {
                asset_id: None,
                clip_id: None,
                resolution: None,
            });
            Ok(ToolOutput::readonly(
                handlers::video::get_waveform(state, a).await,
            ))
        }

        // Title templates (05 §4b)
        "list_title_templates" => {
            let a: ListTitleTemplatesArgs = serde_json::from_value(args).unwrap_or_default();
            Ok(ToolOutput::readonly(
                handlers::video::list_title_templates(state, a).await,
            ))
        }
        "insert_title_template" => {
            let a: InsertTitleTemplateArgs =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            Ok(ToolOutput::mutating(
                handlers::video::insert_title_template(state, a).await,
            ))
        }

        _ => Err(format!("Unknown tool: {}", name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::clipboard::new_clipboard_ring;
    use crate::server::McpServerConfig;
    use photonic_core::{
        color::Color,
        document::{ExportProfile, Guide, GuideOrientation},
        layer::Layer,
        node::{PathNode, TextNode},
        style::Fill,
        AuditLog, ColorSwatch, Document, PathData, SceneNode, SceneNodeKind,
    };
    use serde_json::json;
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::Mutex;

    fn test_state() -> AppState {
        let (tx, _rx) = std::sync::mpsc::channel();
        AppState {
            document: Arc::new(Mutex::new(Document::new("history test", 200.0, 100.0))),
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

    async fn undo(state: &AppState) {
        let result = dispatch_tool(state, "undo", json!({})).await.unwrap();
        assert_ne!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn mcp_text_delete_and_artboard_edits_undo_to_their_prior_states() {
        let state = test_state();
        let (text_id, path_id, artboard_id, original_artboard_name) = {
            let mut doc = state.document.lock().await;
            let layer_id = doc.active_layer_id.expect("default layer");
            let text = SceneNode::new(
                "Greeting",
                layer_id,
                SceneNodeKind::Text(TextNode::new("before")),
            );
            let text_id = text.id;
            doc.add_node(text, Some(layer_id));
            let path = SceneNode::new(
                "Delete me",
                layer_id,
                SceneNodeKind::Path(PathNode::new(PathData::rect(1.0, 2.0, 3.0, 4.0))),
            );
            let path_id = path.id;
            doc.add_node(path, Some(layer_id));
            let artboard = doc.artboards.first().unwrap();
            (text_id, path_id, artboard.id, artboard.name.clone())
        };

        let result = dispatch_tool(
            &state,
            "find_replace_text",
            json!({ "find": "before", "replace": "after" }),
        )
        .await
        .unwrap();
        assert_ne!(result.is_error, Some(true));
        undo(&state).await;
        {
            let doc = state.document.lock().await;
            let SceneNodeKind::Text(text) = &doc.nodes[&text_id].kind else {
                panic!("text node");
            };
            assert_eq!(text.content, "before");
        }

        let result = dispatch_tool(&state, "delete_nodes", json!({ "node_ids": [path_id] }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        assert!(!state.document.lock().await.nodes.contains_key(&path_id));
        undo(&state).await;
        assert!(state.document.lock().await.nodes.contains_key(&path_id));

        let result = dispatch_tool(
            &state,
            "update_artboard",
            json!({ "artboard_id": artboard_id, "name": "Changed" }),
        )
        .await
        .unwrap();
        assert_ne!(result.is_error, Some(true));
        undo(&state).await;
        let doc = state.document.lock().await;
        assert_eq!(doc.artboards.first().unwrap().name, original_artboard_name);
    }

    #[tokio::test]
    async fn direct_document_mutator_uses_snapshot_history_fallback() {
        let state = test_state();
        let result = dispatch_tool(&state, "set_document_bleed", json!({ "bleed_mm": 3.0 }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        assert_eq!(state.history.lock().await.undo_depth(), 1);
        undo(&state).await;
        assert_eq!(state.document.lock().await.bleed_mm, 0.0);
    }

    #[test]
    fn tool_list_exposes_checkpoint_lifecycle() {
        let tools = crate::schema_gen::tool_list();
        let tools = tools.as_array().expect("tool list array");
        let find = |name: &str| {
            tools
                .iter()
                .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
                .unwrap_or_else(|| panic!("missing tool {name}"))
        };

        let list = find("list_checkpoints");
        assert_eq!(list["inputSchema"]["type"], "object");
        assert_eq!(list["inputSchema"]["properties"], json!({}));
        assert_eq!(list["inputSchema"]["required"], json!([]));

        let restore = find("restore_checkpoint");
        assert_eq!(restore["inputSchema"]["type"], "object");
        assert_eq!(
            restore["inputSchema"]["properties"]["checkpoint_id"]["type"],
            "string"
        );
        assert_eq!(restore["inputSchema"]["required"], json!(["checkpoint_id"]));
    }

    fn structured_data(result: &ToolResult) -> Value {
        let Some(ContentItem::Text { text }) = result.content.last() else {
            panic!("expected structured JSON content");
        };
        serde_json::from_str(text).expect("valid structured JSON content")
    }

    #[tokio::test]
    async fn checkpoint_lifecycle_is_reachable_through_dispatcher() {
        let state = test_state();
        let checkpoint_id = {
            let doc = state.document.lock().await;
            state
                .history
                .lock()
                .await
                .create_checkpoint("baseline".to_string(), &doc)
        };

        let listed = dispatch_tool(&state, "list_checkpoints", json!({}))
            .await
            .unwrap();
        assert_ne!(listed.is_error, Some(true));
        let listed_data = structured_data(&listed);
        assert_eq!(
            listed_data["checkpoints"][0]["id"],
            checkpoint_id.to_string()
        );
        assert_eq!(listed_data["checkpoints"][0]["name"], "baseline");

        let created = dispatch_tool(
            &state,
            "create_shape",
            json!({
                "shape_type": "rectangle",
                "x": 0.0,
                "y": 0.0,
                "width": 10.0,
                "height": 10.0,
                "name": "checkpoint-node"
            }),
        )
        .await
        .unwrap();
        assert_ne!(created.is_error, Some(true));

        let route = dispatch_tool_inner(
            &state,
            "restore_checkpoint",
            json!({ "checkpoint_id": "not-a-uuid" }),
        )
        .await
        .unwrap();
        assert!(route.mutates, "restore_checkpoint must be marked mutating");
        assert_eq!(route.result.is_error, Some(true));

        let restored = dispatch_tool(
            &state,
            "restore_checkpoint",
            json!({ "checkpoint_id": checkpoint_id.to_string() }),
        )
        .await
        .unwrap();
        assert_ne!(restored.is_error, Some(true));
        assert!(state.document.lock().await.nodes.is_empty());
        assert_eq!(state.history.lock().await.undo_depth(), 0);
        assert_eq!(state.history.lock().await.list_checkpoints().len(), 1);

        let audit = state.audit_log.lock().unwrap();
        let entry = audit.entries().back().expect("restore audit entry");
        assert_eq!(entry.tool_name, "restore_checkpoint");
        assert_eq!(entry.args["checkpoint_id"], checkpoint_id.to_string());
        assert!(!entry.is_error);
    }

    async fn swatch_state(with_matching_node: bool) -> AppState {
        let state = test_state();
        let mut doc = state.document.lock().await;
        doc.color_swatches
            .push(ColorSwatch::new("Brand", "#112233"));
        let layer_id = doc.active_layer_id.expect("default layer");
        let fill = if with_matching_node {
            Fill::solid(Color::from_hex("#112233").unwrap())
        } else {
            Fill::solid(Color::from_hex("#AABBCC").unwrap())
        };
        doc.add_node(
            SceneNode::new(
                "swatch target",
                layer_id,
                SceneNodeKind::Path(
                    PathNode::new(PathData::rect(0.0, 0.0, 10.0, 10.0)).with_fill(fill),
                ),
            ),
            Some(layer_id),
        );
        drop(doc);
        state
    }

    #[tokio::test]
    async fn update_color_swatch_undo_redo_restores_palette_and_nodes() {
        let cases = [
            (
                "recolor without propagation",
                false,
                json!({
                    "name": "Brand",
                    "new_color_hex": "#445566",
                    "propagate": false
                }),
            ),
            (
                "recolor with matching nodes",
                true,
                json!({
                    "name": "Brand",
                    "new_color_hex": "#445566"
                }),
            ),
        ];

        for (label, with_matching_node, args) in cases {
            let state = swatch_state(with_matching_node).await;
            let before = serde_json::to_value(&*state.document.lock().await).unwrap();

            let result = dispatch_tool(&state, "update_color_swatch", args)
                .await
                .unwrap();
            assert_ne!(result.is_error, Some(true), "{label}: update failed");
            let after = serde_json::to_value(&*state.document.lock().await).unwrap();
            assert_ne!(after, before, "{label}: update was a no-op");
            assert_eq!(
                state.history.lock().await.undo_depth(),
                1,
                "{label}: update must be one undo step"
            );

            undo(&state).await;
            assert_eq!(
                serde_json::to_value(&*state.document.lock().await).unwrap(),
                before,
                "{label}: undo did not restore the document"
            );

            let result = dispatch_tool(&state, "redo", json!({})).await.unwrap();
            assert_ne!(result.is_error, Some(true), "{label}: redo failed");
            assert_eq!(
                serde_json::to_value(&*state.document.lock().await).unwrap(),
                after,
                "{label}: redo did not reapply the document"
            );
        }
    }

    #[tokio::test]
    async fn apply_document_template_records_all_changes_in_one_step() {
        let state = test_state();
        let (template_json, before) = {
            let mut doc = state.document.lock().await;
            doc.guides
                .push(Guide::new(GuideOrientation::Horizontal, 12.0));
            doc.export_profiles
                .push(ExportProfile::new_png("print", Some(400), Some(300)));
            let before = serde_json::to_value(&*doc).unwrap();

            let mut template = doc.clone();
            template.width = 640.0;
            template.height = 480.0;
            template
                .guides
                .push(Guide::new(GuideOrientation::Vertical, 37.0));
            template.export_profiles = vec![ExportProfile {
                name: "print".to_string(),
                format: "png".to_string(),
                width: Some(1600),
                height: Some(1200),
                semantic_ids: Some(true),
                precision: Some(4),
            }];
            let template_layer = Layer::new("Template overlay");
            template.layer_order.push(template_layer.id);
            template.layers.insert(template_layer.id, template_layer);
            (template.to_json().unwrap(), before)
        };

        let result = dispatch_tool(
            &state,
            "apply_document_template",
            json!({ "template_json": template_json }),
        )
        .await
        .unwrap();
        assert_ne!(result.is_error, Some(true));
        assert_eq!(state.history.lock().await.undo_depth(), 1);

        let after = serde_json::to_value(&*state.document.lock().await).unwrap();
        {
            let doc = state.document.lock().await;
            assert_eq!((doc.width, doc.height), (640.0, 480.0));
            assert_eq!(doc.guides.len(), 2);
            assert!(doc.layers.values().any(|l| l.name == "Template overlay"));
            let profile = doc
                .export_profiles
                .iter()
                .find(|p| p.name == "print")
                .unwrap();
            assert_eq!((profile.width, profile.height), (Some(1600), Some(1200)));
        }

        undo(&state).await;
        assert_eq!(
            serde_json::to_value(&*state.document.lock().await).unwrap(),
            before
        );
        let redo_result = dispatch_tool(&state, "redo", json!({})).await.unwrap();
        assert_ne!(redo_result.is_error, Some(true));
        assert_eq!(
            serde_json::to_value(&*state.document.lock().await).unwrap(),
            after
        );
    }

    #[tokio::test]
    async fn undo_and_redo_report_mutation_only_when_history_moves() {
        let state = test_state();

        let output = dispatch_tool_inner(&state, "undo", json!({}))
            .await
            .unwrap();
        assert!(!output.mutates, "undo with no history must be read-only");

        dispatch_tool(&state, "set_document_bleed", json!({ "bleed_mm": 3.0 }))
            .await
            .unwrap();

        let output = dispatch_tool_inner(&state, "undo", json!({}))
            .await
            .unwrap();
        assert!(output.mutates, "successful undo must be mutating");
        let output = dispatch_tool_inner(&state, "undo", json!({}))
            .await
            .unwrap();
        assert!(
            !output.mutates,
            "undo at the history root must be read-only"
        );

        let output = dispatch_tool_inner(&state, "redo", json!({}))
            .await
            .unwrap();
        assert!(output.mutates, "successful redo must be mutating");
        let output = dispatch_tool_inner(&state, "redo", json!({}))
            .await
            .unwrap();
        assert!(!output.mutates, "redo at the history tip must be read-only");
    }

    #[tokio::test]
    async fn save_document_writes_native_file_round_trips_and_remembers_path() {
        let state = test_state();
        assert!(crate::schema_gen::tool_list()
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| {
                tool.get("name").and_then(|name| name.as_str()) == Some("save_document")
            }));
        let expected_counts = {
            let mut doc = state.document.lock().await;
            let layer_id = doc.active_layer_id.expect("default layer");
            doc.add_node(
                SceneNode::new(
                    "Tiny rectangle",
                    layer_id,
                    SceneNodeKind::Path(PathNode::new(PathData::rect(1.0, 2.0, 3.0, 4.0))),
                ),
                Some(layer_id),
            );
            (doc.artboards.len(), doc.nodes.len())
        };
        let base = std::env::temp_dir().join(format!("photonic-save-{}", uuid::Uuid::new_v4()));
        let path = base.join("nested").join("tiny.photon");

        let result = dispatch_tool(&state, "save_document", json!({ "path": path }))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        assert!(
            path.exists(),
            "save_document must create its parent directories"
        );
        let contents = std::fs::read_to_string(&path).unwrap();
        let (loaded, _) = photonic_core::load_photon(&contents).unwrap();
        assert_eq!(
            (loaded.artboards.len(), loaded.nodes.len()),
            expected_counts
        );
        assert_eq!(
            state.document_path.lock().unwrap().as_deref(),
            Some(path.as_path())
        );

        let repeat = dispatch_tool(&state, "save_document", json!({}))
            .await
            .unwrap();
        assert_ne!(
            repeat.is_error,
            Some(true),
            "pathless save should use the remembered path"
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn save_document_write_failure_preserves_existing_destination_and_path() {
        use std::os::unix::fs::PermissionsExt;

        let state = test_state();
        let base =
            std::env::temp_dir().join(format!("photonic-save-failure-{}", uuid::Uuid::new_v4()));
        let parent = base.join("readonly");
        let path = parent.join("existing.photon");
        let original = b"last known good project bytes";

        std::fs::create_dir_all(&parent).unwrap();
        std::fs::write(&path, original).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();

        let result = dispatch_tool(&state, "save_document", json!({ "path": path }))
            .await
            .unwrap();

        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(result.is_error, Some(true));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(
            state.document_path.lock().unwrap().is_none(),
            "a failed save must not establish a current path"
        );
        assert!(
            std::fs::read_dir(&parent).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".photonic-tmp-")
            }),
            "a failed save must not leave a staging file"
        );

        std::fs::remove_dir_all(base).unwrap();
    }
}
