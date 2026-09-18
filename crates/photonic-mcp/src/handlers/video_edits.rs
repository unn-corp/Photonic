//! Revision-checked video transactions. Existing handlers plan against isolated
//! state; only their captured timeline commands reach the live document.
use crate::{protocol::ToolResult, server::AppState};
use photonic_core::{
    history::{Command, CommandHistory},
    timeline::{Sequence, SequenceId, TimelineProject},
    Document,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub const MAX_OPERATIONS: usize = 128;
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const RETAINED_BYTES: usize = 16 * 1024 * 1024;
const RETENTION: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoEditOperation {
    pub tool: String,
    pub arguments: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyVideoEditPlanArgs {
    pub sequence_id: SequenceId,
    pub expected_revision: u64,
    pub request_id: String,
    #[serde(default)]
    pub dry_run: bool,
    pub operations: Vec<VideoEditOperation>,
}
struct Receipt {
    request: Value,
    result: Value,
    bytes: usize,
    at: Instant,
}
#[derive(Default)]
pub struct EditPlanRegistry {
    receipts: VecDeque<(String, Receipt)>,
}
impl EditPlanRegistry {
    fn gc(&mut self) {
        self.receipts
            .retain(|(_, receipt)| receipt.at.elapsed() < RETENTION);
        while self.receipts.len() > 256
            || self.receipts.iter().map(|(_, r)| r.bytes).sum::<usize>() > RETAINED_BYTES
        {
            self.receipts.pop_front();
        }
    }
}

pub const PLAN_TOOLS: &[&str] = &[
    "add_track",
    "remove_track",
    "set_track_prop",
    "reorder_track",
    "insert_clip",
    "move_clip",
    "move_clips",
    "trim_clip",
    "split_clip",
    "remove_clip",
    "roll_edit",
    "slip_clip",
    "slide_clip",
    "ripple_edit",
    "insert_edit",
    "overwrite_edit",
    "lift_edit",
    "extract_edit",
    "replace_clip_source",
    "add_edit_all_tracks",
    "close_gap",
    "insert_space",
    "remove_space",
    "remove_all_spaces_after",
    "remove_clips_after",
    "insert_adjustment_clip",
    "insert_text_clip",
    "set_clip_prop",
    "link_clips",
    "unlink_clips",
    "set_clip_speed",
    "freeze_frame",
    "set_transition",
    "add_effect",
    "remove_effect",
    "reorder_effects",
    "set_effect_param",
    "set_effect_zone",
    "set_keyframe",
    "remove_keyframe",
    "batch_set_keyframes",
    "set_work_range",
    "set_preview_zones",
    "precision_trim",
    "add_marker",
    "set_marker",
    "remove_marker",
    "set_clip_audio",
    "set_track_audio",
    "edit_transcript_word",
    "delete_transcript_range",
    "remove_filler_words",
];

async fn stage_operation(
    state: &AppState,
    operation: &VideoEditOperation,
    arguments: Value,
) -> ToolResult {
    macro_rules! call {
        ($module:ident, $handler:ident) => {
            match serde_json::from_value(arguments) {
                Ok(args) => super::$module::$handler(state, args).await,
                Err(error) => ToolResult::error_with_code("InvalidArguments", error.to_string()),
            }
        };
    }
    match operation.tool.as_str() {
        "add_track" => call!(video, add_track),
        "remove_track" => call!(video, remove_track),
        "set_track_prop" => call!(video, set_track_prop),
        "reorder_track" => call!(video, reorder_track),
        "insert_clip" => call!(video, insert_clip),
        "move_clip" => call!(video, move_clip),
        "move_clips" => call!(video, move_clips),
        "trim_clip" => call!(video, trim_clip),
        "split_clip" => call!(video, split_clip),
        "remove_clip" => call!(video, remove_clip),
        "roll_edit" => call!(video, roll_edit),
        "slip_clip" => call!(video, slip_clip),
        "slide_clip" => call!(video, slide_clip),
        "ripple_edit" => call!(video, ripple_edit),
        "insert_edit" => call!(video, insert_edit),
        "overwrite_edit" => call!(video, overwrite_edit),
        "lift_edit" => call!(video, lift_edit),
        "extract_edit" => call!(video, extract_edit),
        "replace_clip_source" => call!(video, replace_clip_source),
        "add_edit_all_tracks" => call!(video, add_edit_all_tracks),
        "close_gap" => call!(video, close_gap),
        "insert_space" => call!(video, insert_space),
        "remove_space" => call!(video, remove_space),
        "remove_all_spaces_after" => call!(video, remove_all_spaces_after),
        "remove_clips_after" => call!(video, remove_clips_after),
        "insert_adjustment_clip" => call!(video, insert_adjustment_clip),
        "insert_text_clip" => call!(video, insert_text_clip),
        "set_clip_prop" => call!(video, set_clip_prop),
        "link_clips" => call!(video, link_clips),
        "unlink_clips" => call!(video, unlink_clips),
        "set_clip_speed" => call!(video, set_clip_speed),
        "freeze_frame" => call!(video, freeze_frame),
        "set_transition" => call!(video, set_transition),
        "add_effect" => call!(video, add_effect),
        "remove_effect" => call!(video, remove_effect),
        "reorder_effects" => call!(video, reorder_effects),
        "set_effect_param" => call!(video, set_effect_param),
        "set_effect_zone" => call!(video, set_effect_zone),
        "set_keyframe" => call!(video, set_keyframe),
        "remove_keyframe" => call!(video, remove_keyframe),
        "batch_set_keyframes" => call!(video, batch_set_keyframes),
        "set_work_range" => call!(video, set_work_range),
        "set_preview_zones" => call!(video_workflows, set_preview_zones),
        "precision_trim" => call!(video_workflows, precision_trim),
        "add_marker" => call!(video, add_marker),
        "set_marker" => call!(video, set_marker),
        "remove_marker" => call!(video, remove_marker),
        "set_clip_audio" => call!(video, set_clip_audio),
        "set_track_audio" => call!(video, set_track_audio),
        "edit_transcript_word" => call!(video_transcript, edit_transcript_word),
        "delete_transcript_range" => call!(video_transcript, delete_transcript_range),
        "remove_filler_words" => call!(video_transcript, remove_filler_words),
        _ => ToolResult::error_with_code(
            "UnsupportedPlanOperation",
            "Operation is not an in-memory timeline edit",
        ),
    }
}

/// A whole-value {"$ref":"0.clip_id"} resolves a previous operation's payload.
fn resolve_references(value: &Value, results: &[Value]) -> Result<Value, String> {
    match value {
        Value::Object(object) if object.len() == 1 && object.contains_key("$ref") => {
            let reference = object["$ref"].as_str().ok_or("$ref must be a string")?;
            let mut path = reference.split('.');
            let index = path
                .next()
                .and_then(|s| s.parse::<usize>().ok())
                .ok_or("$ref must start with an operation index")?;
            let mut result = results
                .get(index)
                .ok_or("$ref must address an earlier operation")?;
            for component in path {
                result = if let Ok(index) = component.parse::<usize>() {
                    result.get(index)
                } else {
                    result.get(component)
                }
                .ok_or("$ref field does not exist")?;
            }
            Ok(result.clone())
        }
        Value::Object(object) => object
            .iter()
            .map(|(key, value)| Ok((key.clone(), resolve_references(value, results)?)))
            .collect::<Result<serde_json::Map<_, _>, String>>()
            .map(Value::Object),
        Value::Array(values) => values
            .iter()
            .map(|v| resolve_references(v, results))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        _ => Ok(value.clone()),
    }
}
fn payload(result: &ToolResult) -> Value {
    result.structured_content.clone().unwrap_or(Value::Null)
}
fn conflict(expected: u64, actual: u64) -> ToolResult {
    ToolResult::error_with_code(
        "RevisionConflict",
        "The document changed; inspect it and rebuild the plan",
    )
    .with_data(json!({"expected_revision":expected,"actual_revision":actual}))
}
fn timeline_only(command: &Command) -> bool {
    match command {
        Command::Timeline(_) => true,
        Command::Batch(commands) => commands.iter().all(timeline_only),
        _ => false,
    }
}
fn clip_map(sequence: &Sequence) -> BTreeMap<String, Value> {
    sequence
        .tracks()
        .flat_map(|track| {
            track.clips.iter().map(move |clip| {
                (
                    clip.id.to_string(),
                    json!({"track_id":track.id,"clip":clip}),
                )
            })
        })
        .collect()
}
fn changed_ids(before: &Sequence, after: &Sequence) -> Value {
    let old = clip_map(before);
    let new = clip_map(after);
    let ids: BTreeSet<_> = old.keys().chain(new.keys()).collect();
    let changed: Vec<_> = ids
        .into_iter()
        .filter(|id| old.get(*id) != new.get(*id))
        .cloned()
        .collect();
    let old_tracks: BTreeMap<_, _> = before.tracks().map(|t| (t.id.to_string(), t)).collect();
    let new_tracks: BTreeMap<_, _> = after.tracks().map(|t| (t.id.to_string(), t)).collect();
    let tracks: BTreeSet<_> = old_tracks.keys().chain(new_tracks.keys()).collect();
    let changed_tracks: Vec<_> = tracks
        .into_iter()
        .filter(|id| old_tracks.get(*id) != new_tracks.get(*id))
        .cloned()
        .collect();
    json!({"sequence_ids":[after.id],"clip_ids":changed,"track_ids":changed_tracks})
}
fn within_scope(before: &TimelineProject, after: &TimelineProject, sequence: SequenceId) -> bool {
    before.sequences.len() == after.sequences.len()
        && before
            .sequences
            .iter()
            .all(|(id, old)| *id == sequence || after.sequences.get(id) == Some(old))
}

pub async fn apply_video_edit_plan(state: &AppState, args: ApplyVideoEditPlanArgs) -> ToolResult {
    if args.operations.is_empty()
        || args.operations.len() > MAX_OPERATIONS
        || args.request_id.is_empty()
        || args.request_id.len() > 128
    {
        return ToolResult::error_with_code(
            "InvalidArguments",
            "Use 1–128 operations and a request_id of 1–128 bytes",
        );
    }
    let request = match serde_json::to_value(&args) {
        Ok(value) => value,
        Err(error) => return ToolResult::error(error.to_string()),
    };
    let request_bytes = request.to_string().len();
    if request_bytes > MAX_REQUEST_BYTES {
        return ToolResult::error_with_code("PlanTooLarge", "Edit plans are limited to 1 MiB");
    }
    let mut registry = state.video_engine.edit_plans.lock().await;
    registry.gc();
    if !args.dry_run {
        if let Some((_, receipt)) = registry
            .receipts
            .iter()
            .find(|(id, _)| id == &args.request_id)
        {
            if receipt.request != request {
                return ToolResult::error_with_code(
                    "RequestIdConflict",
                    "request_id was already used for a different plan",
                );
            }
            let mut result = receipt.result.clone();
            result["replayed"] = json!(true);
            result["current_revision"] = json!(state.history.lock().await.revision());
            return ToolResult::text("Returned the existing edit receipt").with_data(result);
        }
    }
    let before = {
        let document = state.document.lock().await;
        let history = state.history.lock().await;
        if history.revision() != args.expected_revision {
            return conflict(args.expected_revision, history.revision());
        }
        let Some(project) = document.timeline.as_ref() else {
            return ToolResult::error_with_code("NoProject", "No timeline project");
        };
        if !project.sequences.contains_key(&args.sequence_id) {
            return ToolResult::error_with_code(
                "NoSequence",
                "The requested sequence does not exist",
            );
        }
        project.clone()
    };
    let mut staging = state.clone();
    let mut document = Document::new("video edit plan", 1.0, 1.0);
    document.timeline = Some(before.clone());
    staging.document = Arc::new(Mutex::new(document));
    staging.history = Arc::new(Mutex::new(CommandHistory::new(MAX_OPERATIONS + 1)));
    let mut results = Vec::with_capacity(args.operations.len());
    for (index, operation) in args.operations.iter().enumerate() {
        let arguments = match resolve_references(&operation.arguments, &results) {
            Ok(value) => value,
            Err(message) => {
                return ToolResult::error_with_code("InvalidReference", message)
                    .with_data(json!({"operation_index":index}))
            }
        };
        let result = stage_operation(&staging, operation, arguments).await;
        if result.is_error == Some(true) {
            return ToolResult::error_with_code("PlanRejected", format!("Operation {index} ({}) failed; no edits applied", operation.tool))
                .with_data(json!({"operation_index":index,"cause":payload(&result),"revision":args.expected_revision}));
        }
        results.push(payload(&result));
    }
    let staged = staging.document.lock().await;
    let after = staged
        .timeline
        .as_ref()
        .expect("allowed operations preserve the project");
    if !within_scope(&before, after, args.sequence_id) {
        return ToolResult::error_with_code(
            "ScopeViolation",
            "Every operation must address the plan's sequence",
        );
    }
    let commands = staging.history.lock().await.snapshot_state().undo_stack;
    if !commands.iter().all(timeline_only) {
        return ToolResult::error_with_code(
            "UnsupportedPlanOperation",
            "The plan produced a non-timeline command",
        );
    }
    // Locked tracks cannot be edited through a plan, including linked partners
    // and indirectly affected tracks. Individual planners remain authoritative
    // for geometry and source/caption alignment.
    for track in before.sequences[&args.sequence_id]
        .tracks()
        .filter(|t| t.locked)
    {
        if after.sequences[&args.sequence_id].track(track.id) != Some(track) {
            return ToolResult::error_with_code(
                "TrackLocked",
                format!("Track {} is locked", track.id),
            );
        }
    }
    let changed = changed_ids(
        &before.sequences[&args.sequence_id],
        &after.sequences[&args.sequence_id],
    );
    let changed_document = before != *after;
    let mut document = state.document.lock().await;
    let mut history = state.history.lock().await;
    if history.revision() != args.expected_revision {
        return conflict(args.expected_revision, history.revision());
    }
    if !args.dry_run && changed_document {
        history.execute_discrete(Command::Batch(commands), &mut document);
    }
    let result = json!({
        "sequence_id":args.sequence_id,"request_id":args.request_id,"dry_run":args.dry_run,
        "base_revision":args.expected_revision,"revision":history.revision(),"current_revision":history.revision(),
        "replayed":false,"changed":changed_document,"changed_ids":changed,"operations":results,
        "undo_steps":if !args.dry_run && changed_document {1} else {0},
        "undo_node":if !args.dry_run && changed_document {Some(history.current_node())} else {None},
        "receipt_retention_seconds":RETENTION.as_secs(),
    });
    if !args.dry_run {
        let bytes = request_bytes + result.to_string().len();
        registry.receipts.push_back((
            args.request_id,
            Receipt {
                request,
                result: result.clone(),
                bytes,
                at: Instant::now(),
            },
        ));
        registry.gc();
    }
    ToolResult::text(if args.dry_run {
        "Edit plan validated; no changes applied"
    } else {
        "Edit plan committed"
    })
    .with_data(result)
}

/// Permit a prior-result reference at each value position while retaining the
/// complete enum, bounds, object properties and array item schemas.
fn with_references(schema: &Value) -> Value {
    let mut direct = schema.clone();
    if let Some(properties) = direct.get_mut("properties").and_then(Value::as_object_mut) {
        for property in properties.values_mut() {
            *property = with_references(property);
        }
    }
    if let Some(items) = direct.get_mut("items") {
        *items = with_references(items);
    }
    for key in ["oneOf", "anyOf", "allOf"] {
        if let Some(branches) = direct.get_mut(key).and_then(Value::as_array_mut) {
            for branch in branches {
                *branch = with_references(branch);
            }
        }
    }
    json!({"anyOf":[direct,{"type":"object","properties":{"$ref":{"type":"string","pattern":"^[0-9]+(\\.[^.]+)*$"}},"required":["$ref"],"additionalProperties":false}]})
}

pub fn tool_schema(available_tools: &[Value]) -> Value {
    let alternatives: Vec<_> = PLAN_TOOLS
        .iter()
        .filter_map(|name| {
            let tool = available_tools.iter().find(|tool| tool["name"] == *name)?;
            Some(json!({"type":"object","properties":{
            "tool":{"const":name},"arguments":with_references(&tool["inputSchema"])
        },"required":["tool","arguments"],"additionalProperties":false}))
        })
        .collect();
    crate::schema_gen::tool_definition(
        "apply_video_edit_plan",
        "Validate or commit a sequence-scoped edit plan as one undo step. Requires expected_revision from get_timeline_snapshot and a unique request_id. dry_run changes nothing. Operations execute in order against isolated state; any failure or concurrent edit rejects all. Retry the identical committed request_id to receive its receipt without repeating edits (retained 10 minutes, bounded cache). A value {\"$ref\":\"0.clip_id\"} refers to a previous operation result; dry-run generated IDs are provisional. File, job, transport and undo tools are excluded.",
        json!({"type":"object","properties":{
            "sequence_id":{"type":"string","format":"uuid"},"expected_revision":{"type":"integer","minimum":0},
            "request_id":{"type":"string","minLength":1,"maxLength":128},"dry_run":{"type":"boolean","default":false},
            "operations":{"type":"array","minItems":1,"maxItems":MAX_OPERATIONS,"items":{"oneOf":alternatives}}
        },"required":["sequence_id","expected_revision","request_id","operations"],"additionalProperties":false}),
        json!({"type":"object","properties":{
            "sequence_id":{"type":"string"},"request_id":{"type":"string"},"dry_run":{"type":"boolean"},
            "base_revision":{"type":"integer"},"revision":{"type":"integer"},"current_revision":{"type":"integer"},
            "replayed":{"type":"boolean"},"changed":{"type":"boolean"},"changed_ids":{"type":"object"},
            "operations":{"type":"array"},"undo_steps":{"type":"integer","minimum":0,"maximum":1},
            "undo_node":{"type":["integer","null"]},"receipt_retention_seconds":{"type":"integer"}
        },"required":["sequence_id","request_id","dry_run","base_revision","revision","current_revision","replayed","changed","changed_ids","operations","undo_steps","undo_node","receipt_retention_seconds"]}),
        crate::schema_gen::ToolBehavior::Mutation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::{Clip, ClipSource, FrameRate, Sequence, Tick, Track, TrackKind};

    async fn fixture() -> (
        AppState,
        SequenceId,
        photonic_core::timeline::TrackId,
        photonic_core::timeline::ClipId,
    ) {
        let state = AppState::headless_for_test();
        let mut project = TimelineProject::new();
        let mut sequence = Sequence::new("plan", FrameRate::FPS_30, 320, 180);
        let mut track = Track::new(TrackKind::Video, "V1");
        let clip = Clip::new(
            ClipSource::SolidColor {
                color: photonic_core::Color::rgb(1.0, 0.0, 0.0),
            },
            Tick(0),
            Tick(100_000),
        );
        let ids = (sequence.id, track.id, clip.id);
        track.clips.push(clip);
        sequence.video_tracks.push(track);
        project.insert_sequence(sequence);
        state.document.lock().await.timeline = Some(project);
        (state, ids.0, ids.1, ids.2)
    }
    fn plan(sequence: SequenceId, clip: photonic_core::timeline::ClipId) -> ApplyVideoEditPlanArgs {
        ApplyVideoEditPlanArgs {
            sequence_id: sequence,
            expected_revision: 0,
            request_id: "test-plan".into(),
            dry_run: false,
            operations: vec![VideoEditOperation {
                tool: "move_clip".into(),
                arguments: json!({"clip_id":clip,"new_start_ticks":200_000}),
            }],
        }
    }
    #[tokio::test]
    async fn dry_run_commit_retry_and_undo_are_one_transaction() {
        let (state, sequence, _, clip) = fixture().await;
        let before = state.document.lock().await.timeline.clone();
        let mut args = plan(sequence, clip);
        args.dry_run = true;
        let dry = apply_video_edit_plan(&state, args.clone()).await;
        assert_ne!(dry.is_error, Some(true), "{dry:?}");
        assert_eq!(before, state.document.lock().await.timeline);
        assert_eq!(state.history.lock().await.revision(), 0);
        args.dry_run = false;
        let applied = apply_video_edit_plan(&state, args.clone()).await;
        assert_ne!(applied.is_error, Some(true), "{applied:?}");
        assert_eq!(payload(&applied)["undo_steps"], 1);
        let retried = apply_video_edit_plan(&state, args).await;
        assert_eq!(payload(&retried)["replayed"], true);
        assert_eq!(state.history.lock().await.revision(), 1);
        let mut document = state.document.lock().await;
        assert!(state.history.lock().await.undo(&mut document));
        assert_eq!(before, document.timeline);
    }
    #[tokio::test]
    async fn late_failure_and_stale_revision_do_not_apply_anything() {
        let (state, sequence, _, clip) = fixture().await;
        let before = state.document.lock().await.timeline.clone();
        let mut args = plan(sequence, clip);
        args.operations.push(VideoEditOperation {
            tool: "move_clip".into(),
            arguments: json!({"clip_id":clip,"new_start_ticks":-1}),
        });
        let rejected = apply_video_edit_plan(&state, args).await;
        assert_eq!(payload(&rejected)["error_code"], "PlanRejected");
        assert_eq!(before, state.document.lock().await.timeline);
        assert_eq!(state.history.lock().await.revision(), 0);
        let mut args = plan(sequence, clip);
        args.expected_revision = 99;
        assert_eq!(
            payload(&apply_video_edit_plan(&state, args).await)["error_code"],
            "RevisionConflict"
        );
    }
    #[tokio::test]
    async fn references_allow_insert_then_edit_in_one_plan() {
        let (state, sequence, track, clip) = fixture().await;
        let mut args = plan(sequence, clip);
        args.operations = vec![
            VideoEditOperation {
                tool: "insert_clip".into(),
                arguments: json!({"track_id":track,"start_ticks":200_000,"duration_ticks":100_000,"source":{"kind":"solid_color","color":"#00ff00"}}),
            },
            VideoEditOperation {
                tool: "move_clip".into(),
                arguments: json!({"clip_id":{"$ref":"0.clip_id"},"new_start_ticks":400_000}),
            },
        ];
        let result = apply_video_edit_plan(&state, args).await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
        assert_eq!(state.history.lock().await.undo_depth(), 1);
        let document = state.document.lock().await;
        let clips = &document.timeline.as_ref().unwrap().sequences[&sequence]
            .track(track)
            .unwrap()
            .clips;
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[1].start, Tick(400_000));
    }
    #[tokio::test]
    async fn duplicate_request_id_with_new_body_and_locked_track_are_rejected() {
        let (state, sequence, track, clip) = fixture().await;
        let args = plan(sequence, clip);
        assert_ne!(
            apply_video_edit_plan(&state, args.clone()).await.is_error,
            Some(true)
        );
        let mut changed = args.clone();
        changed.expected_revision = 1;
        assert_eq!(
            payload(&apply_video_edit_plan(&state, changed).await)["error_code"],
            "RequestIdConflict"
        );
        let (state, sequence, _, clip) = fixture().await;
        let mut doc = state.document.lock().await;
        let actual_track = doc.timeline.as_ref().unwrap().sequences[&sequence]
            .tracks()
            .next()
            .unwrap()
            .id;
        doc.timeline
            .as_mut()
            .unwrap()
            .sequences
            .get_mut(&sequence)
            .unwrap()
            .track_mut(actual_track)
            .unwrap()
            .locked = true;
        drop(doc);
        let result = apply_video_edit_plan(&state, plan(sequence, clip)).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(state.history.lock().await.revision(), 0);
        let _ = track;
    }
}
