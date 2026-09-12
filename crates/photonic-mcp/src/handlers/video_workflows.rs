//! Explicit workflow controls shared with the GUI's core editing planners.
use super::video::engine_bridge;
use crate::{protocol::ToolResult, server::AppState};
use photonic_core::{
    history::Command,
    timeline::{
        ops,
        precision_trim::{self, TrimMode, TrimTarget},
        AssetId, ClipId, PreviewZone, SequenceId, Tick, TrackId,
    },
};
use photonic_video::EngineCmd;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetPreviewZonesArgs {
    pub sequence_id: SequenceId,
    pub zones: Vec<PreviewZone>,
}
pub async fn set_preview_zones(state: &AppState, args: SetPreviewZonesArgs) -> ToolResult {
    let mut document = state.document.lock().await;
    let Some(project) = &document.timeline else {
        return ToolResult::error_with_code("NoProject", "No timeline project");
    };
    let command = match ops::set_preview_zones(project, args.sequence_id, &args.zones) {
        Ok(command) => command,
        Err(error) => return ToolResult::error_with_code("InvalidPreviewZone", error.to_string()),
    };
    let mut history = state.history.lock().await;
    let changed = !matches!(&command,photonic_core::timeline::TimelineCmd::SetPreviewZones{old,new,..} if old == new);
    if changed {
        history.execute_discrete(Command::Timeline(command), &mut document);
    }
    ToolResult::text("Updated preview zones").with_data(json!({"sequence_id":args.sequence_id,"revision":history.revision(),"changed":changed,"zones":document.timeline.as_ref().unwrap().sequences[&args.sequence_id].preview_zones}))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewRangeArgs {
    pub sequence_id: SequenceId,
    #[serde(default)]
    pub range: Option<[i64; 2]>,
    #[serde(default)]
    pub expected_revision: Option<u64>,
    #[serde(default)]
    pub profile: Option<photonic_video::preview::PreviewProfile>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceArgs {
    pub sequence_id: SequenceId,
}

pub async fn preview_command(state: &AppState, args: PreviewRangeArgs, clear: bool) -> ToolResult {
    if args
        .range
        .is_some_and(|[start, end]| start < 0 || end <= start)
    {
        return ToolResult::error_with_code(
            "InvalidArguments",
            "range must be [nonnegative start, greater end] in ticks",
        );
    }
    let revision = {
        let document = state.document.lock().await;
        if !document
            .timeline
            .as_ref()
            .is_some_and(|project| project.sequences.contains_key(&args.sequence_id))
        {
            return ToolResult::error_with_code("NoSequence", "Requested sequence does not exist");
        }
        state.history.lock().await.revision()
    };
    if args
        .expected_revision
        .is_some_and(|expected| expected != revision)
    {
        return ToolResult::error_with_code(
            "RevisionConflict",
            "Document changed before preview request",
        )
        .with_data(json!({"actual_revision":revision}));
    }
    let bridge = match engine_bridge(state) {
        Ok(bridge) => bridge,
        Err(error) => return error,
    };
    let _transport = bridge.lock_transport().await;
    bridge.sync(state).await;
    if bridge.shadow_revision() != revision {
        return ToolResult::error_with_code(
            "RevisionConflict",
            "Document changed before preview request",
        );
    }
    if let Some(profile) = args.profile {
        if clear || profile.quality > 51 {
            return ToolResult::error_with_code(
                "InvalidArguments",
                "Profiles apply only to render_preview; quality must be 0–51",
            );
        }
        if !bridge.session().send(EngineCmd::SetPreviewProfile(profile)) {
            return ToolResult::error_with_code(
                "EngineBusy",
                "Engine command queue is full or closed",
            );
        }
    }
    let range = args.range.map(|[start, end]| (Tick(start), Tick(end)));
    let command = if clear {
        EngineCmd::ClearPreview {
            sequence: args.sequence_id,
            range,
        }
    } else {
        EngineCmd::RenderPreview {
            sequence: args.sequence_id,
            range,
        }
    };
    if !bridge.session().send(command) {
        return ToolResult::error_with_code("EngineBusy", "Engine command queue is full or closed")
            .with_data(json!({"retryable":true}));
    }
    ToolResult::text(if clear {
        "Preview clear requested"
    } else {
        "Preview render requested; poll get_preview_status"
    })
    .with_data(json!({"accepted":true,"sequence_id":args.sequence_id,"revision":revision}))
}
pub async fn cancel_preview(state: &AppState, args: SequenceArgs) -> ToolResult {
    let bridge = match engine_bridge(state) {
        Ok(bridge) => bridge,
        Err(error) => return error,
    };
    if !bridge.session().send(EngineCmd::CancelPreview {
        sequence: args.sequence_id,
    }) {
        return ToolResult::error_with_code("EngineBusy", "Engine command queue is full or closed");
    }
    ToolResult::text("Preview cancellation requested")
        .with_data(json!({"accepted":true,"sequence_id":args.sequence_id}))
}
pub async fn get_preview_status(state: &AppState, args: SequenceArgs) -> ToolResult {
    if !state
        .document
        .lock()
        .await
        .timeline
        .as_ref()
        .is_some_and(|project| project.sequences.contains_key(&args.sequence_id))
    {
        return ToolResult::error_with_code("NoSequence", "Requested sequence does not exist");
    }
    let bridge = match engine_bridge(state) {
        Ok(bridge) => bridge,
        Err(error) => return error,
    };
    let _transport = bridge.lock_transport().await;
    bridge.sync(state).await;
    if !bridge
        .wait_engine_synced(std::time::Duration::from_secs(2))
        .await
    {
        return ToolResult::error_with_code("EngineBusy", "Engine snapshot is still synchronizing")
            .with_data(json!({"retryable":true}));
    }
    let mut status = bridge.session().preview_status();
    status
        .chunks
        .retain(|chunk| chunk.sequence == args.sequence_id);
    let mut data = serde_json::to_value(status).expect("preview status serializes");
    data["sequence_id"] = json!(args.sequence_id);
    ToolResult::text("Preview cache and rendering status").with_data(data)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrimModeArg {
    Roll,
    RippleOutgoing,
    RippleIncoming,
    SlipOutgoing,
    SlipIncoming,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecisionTrimArgs {
    pub sequence_id: SequenceId,
    pub track_id: TrackId,
    pub outgoing_clip_id: ClipId,
    pub incoming_clip_id: ClipId,
    pub mode: TrimModeArg,
    pub delta_ticks: i64,
}
pub async fn precision_trim(state: &AppState, args: PrecisionTrimArgs) -> ToolResult {
    let mut document = state.document.lock().await;
    let Some(project) = &document.timeline else {
        return ToolResult::error_with_code("NoProject", "No timeline project");
    };
    let target = TrimTarget {
        sequence: args.sequence_id,
        track: args.track_id,
        outgoing: args.outgoing_clip_id,
        incoming: args.incoming_clip_id,
    };
    let mode = match args.mode {
        TrimModeArg::Roll => TrimMode::Roll,
        TrimModeArg::RippleOutgoing => TrimMode::RippleOutgoing,
        TrimModeArg::RippleIncoming => TrimMode::RippleIncoming,
        TrimModeArg::SlipOutgoing => TrimMode::SlipOutgoing,
        TrimModeArg::SlipIncoming => TrimMode::SlipIncoming,
    };
    let commands = match precision_trim::plan_trim(project, target, mode, Tick(args.delta_ticks)) {
        Ok(commands) => commands,
        Err(error) => return ToolResult::error_with_code("TrimRejected", error.to_string()),
    };
    let count = commands.len();
    let mut history = state.history.lock().await;
    if count > 0 {
        history.execute_discrete(
            Command::Batch(commands.into_iter().map(Command::Timeline).collect()),
            &mut document,
        );
    }
    ToolResult::text("Applied precision trim").with_data(json!({"sequence_id":args.sequence_id,"revision":history.revision(),"changed":count>0,"command_count":count}))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditionSourceArgs {
    pub asset_id: AssetId,
    pub start_ticks: i64,
    pub end_ticks: i64,
}
pub async fn audition_source(state: &AppState, args: AuditionSourceArgs) -> ToolResult {
    if args.start_ticks < 0 || args.end_ticks <= args.start_ticks {
        return ToolResult::error_with_code(
            "InvalidArguments",
            "Audition requires a positive source range",
        );
    }
    {
        let document = state.document.lock().await;
        let Some(asset) = document
            .timeline
            .as_ref()
            .and_then(|project| project.media.assets.get(&args.asset_id))
        else {
            return ToolResult::error_with_code("NoAsset", "Requested source asset does not exist");
        };
        if let Err(error) = photonic_video::source_audition::plan_source_audition(
            asset,
            (Tick(args.start_ticks), Tick(args.end_ticks)),
        ) {
            return ToolResult::error_with_code("SourceNotReady", error.to_string());
        }
    }
    let bridge = match engine_bridge(state) {
        Ok(bridge) => bridge,
        Err(error) => return error,
    };
    let _transport = bridge.lock_transport().await;
    bridge.sync(state).await;
    if !bridge.session().send(EngineCmd::AuditionSource {
        asset: args.asset_id,
        start: Tick(args.start_ticks),
        end: Tick(args.end_ticks),
    }) {
        return ToolResult::error_with_code("EngineBusy", "Engine command queue is full or closed");
    }
    ToolResult::text(
        "Source audition requested; get_engine_status reports playback or audio-device errors",
    )
    .with_data(json!({"accepted":true,"asset_id":args.asset_id}))
}
pub async fn stop_source_audition(state: &AppState) -> ToolResult {
    let bridge = match engine_bridge(state) {
        Ok(bridge) => bridge,
        Err(error) => return error,
    };
    if !bridge.session().send(EngineCmd::StopSourceAudition) {
        return ToolResult::error_with_code("EngineBusy", "Engine command queue is full or closed");
    }
    ToolResult::text("Source audition stop requested").with_data(json!({"accepted":true}))
}

pub fn tool_schemas() -> Vec<Value> {
    use crate::schema_gen::{tool_definition as tool, ToolBehavior as Behavior};
    let uuid = json!({"type":"string","format":"uuid"});
    let sequence = json!({"type":"object","properties":{"sequence_id":uuid},"required":["sequence_id"],"additionalProperties":false});
    let range = json!({"type":"object","properties":{"sequence_id":uuid,"range":{"type":"array","minItems":2,"maxItems":2,"items":{"type":"integer","minimum":0}},"expected_revision":{"type":"integer","minimum":0}},"required":["sequence_id"],"additionalProperties":false});
    let mut render = range.clone();
    render["properties"]["profile"] = json!({"type":"object","properties":{"codec":{"type":"string","enum":["IntraH264","IntraProResLike","Lossless"]},"quality":{"type":"integer","minimum":0,"maximum":51,"description":"H264 CRF; other codecs use fixed encoder modes."},"scale":{"type":"string","enum":["Full","Half"]}},"required":["codec","quality","scale"],"additionalProperties":false});
    let accepted = json!({"type":"object","properties":{"accepted":{"type":"boolean"}},"required":["accepted"]});
    vec![
        tool("set_preview_zones","Replace undoable preview zones; frame-snaps outward and merges overlaps. Empty clears all zones. Cache files are not document state. Use apply_video_edit_plan for dry-run/revision/retry safety.",
            json!({"type":"object","properties":{"sequence_id":uuid,"zones":{"type":"array","maxItems":128,"items":{"type":"object","properties":{"start":{"type":"integer","minimum":0},"end":{"type":"integer","minimum":1}},"required":["start","end"],"additionalProperties":false}}},"required":["sequence_id","zones"],"additionalProperties":false}),
            json!({"type":"object","properties":{"sequence_id":uuid,"revision":{"type":"integer"},"changed":{"type":"boolean"},"zones":{"type":"array"}},"required":["sequence_id","revision","changed","zones"]}),Behavior::Mutation),
        tool("render_preview","Render marked zones (or explicit tick range) into bounded background playback cache. Defaults to Full resolution, original sources, intra H.264. Optional explicit ProRes/Lossless profile preserves alpha; Half scales playback only. Playback takes priority. Poll get_preview_status; export and exact inspection always evaluate originals.",render,accepted.clone(),Behavior::ExternalMutation),
        tool("clear_preview","Clear cached playback chunks for this sequence and optional tick range. Active readers finish safely; document zones remain.",range,accepted.clone(),Behavior::ExternalMutation),
        tool("cancel_preview","Cancel queued/current preview rendering for this sequence; no partial chunk is published.",sequence.clone(),accepted.clone(),Behavior::ExternalMutation),
        tool("get_preview_status","Read per-chunk preview state and aggregate cache pressure for a sequence.",sequence,
            json!({"type":"object","properties":{"sequence_id":uuid,"chunks":{"type":"array"},"cache":{"type":"object"}},"required":["sequence_id","chunks","cache"]}),Behavior::ExternalRead),
        tool("precision_trim","Apply roll, ripple or slip at an explicit adjacent cut with linked/sync-lock participants validated together. Same planner as the GUI precision editor. Use within apply_video_edit_plan for dry run, one undo and revision safety.",
            json!({"type":"object","properties":{"sequence_id":uuid,"track_id":uuid,"outgoing_clip_id":uuid,"incoming_clip_id":uuid,"mode":{"type":"string","enum":["roll","ripple_outgoing","ripple_incoming","slip_outgoing","slip_incoming"]},"delta_ticks":{"type":"integer"}},"required":["sequence_id","track_id","outgoing_clip_id","incoming_clip_id","mode","delta_ticks"],"additionalProperties":false}),
            json!({"type":"object","properties":{"sequence_id":uuid,"revision":{"type":"integer"},"changed":{"type":"boolean"},"command_count":{"type":"integer"}},"required":["sequence_id","revision","changed","command_count"]}),Behavior::Mutation),
        tool("audition_source","Play an explicit probed source interval on the source monitor and local audio output. Program playback pauses; document and sequence playhead stay unchanged. Poll get_engine_status for source_audition and source_audition_error. Requires audio output for audible sources.",
            json!({"type":"object","properties":{"asset_id":uuid,"start_ticks":{"type":"integer","minimum":0},"end_ticks":{"type":"integer","minimum":1}},"required":["asset_id","start_ticks","end_ticks"],"additionalProperties":false}),accepted.clone(),Behavior::ExternalMutation),
        tool("stop_source_audition","Stop source audition and release its audio output without editing the document.",json!({"type":"object","properties":{},"additionalProperties":false}),accepted,Behavior::ExternalMutation),
    ]
}
