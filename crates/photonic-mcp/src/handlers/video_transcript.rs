//! Explicitly scoped transcript tools. All edit semantics live in core and
//! match the GUI drawer. Speech content is never written to tracing logs.

use photonic_core::history::Command;
use photonic_core::timeline::{
    transcript::{self, TranscriptError, TranscriptTokenRef},
    CueId, SequenceId, Tick, TimelineCmd, TimelineProject, TrackId,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::protocol::ToolResult;
use crate::schema_gen::{tool_definition, ToolBehavior};
use crate::server::AppState;

#[derive(Debug, Deserialize)]
pub struct GetTranscriptArgs {
    #[serde(default)]
    pub expected_revision: Option<u64>,
    pub sequence_id: SequenceId,
    pub caption_track_id: TrackId,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct EditTranscriptWordArgs {
    pub sequence_id: SequenceId,
    pub caption_track_id: TrackId,
    pub cue_id: CueId,
    pub word_index: usize,
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct DeleteTranscriptRangeArgs {
    pub sequence_id: SequenceId,
    pub caption_track_id: TrackId,
    /// Exact participant tracks; no GUI selection or implicit target fallback.
    pub target_track_ids: Vec<TrackId>,
    pub start_ticks: i64,
    pub end_ticks: i64,
    /// true = extract and close the gap; false = lift and leave the gap.
    pub ripple: bool,
}

#[derive(Debug, Deserialize)]
pub struct FindFillerWordsArgs {
    pub sequence_id: SequenceId,
    pub caption_track_id: TrackId,
    #[serde(default)]
    pub lexicon: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct RemoveFillerWordsArgs {
    pub sequence_id: SequenceId,
    pub caption_track_id: TrackId,
    pub target_track_ids: Vec<TrackId>,
    /// Selected entries returned by find_filler_words. Omit excluded matches.
    /// Text and timing are compared with the current document before mutation.
    pub matches: Vec<TranscriptTokenRef>,
    pub ripple: bool,
}

fn error(error: TranscriptError) -> ToolResult {
    let code = match &error {
        TranscriptError::Edit(photonic_core::timeline::EditError::NoSequence(_)) => "NoSequence",
        TranscriptError::Edit(photonic_core::timeline::EditError::NoTrack(_)) => "NoTrack",
        TranscriptError::Edit(_) => "EditError",
        TranscriptError::NoCaptionTrack(_) => "NoCaptionTrack",
        TranscriptError::NoCue(_) => "NoCue",
        TranscriptError::NoWord(_) => "NoWord",
        TranscriptError::StaleToken => "StaleTranscriptSelection",
        TranscriptError::InvalidRange => "InvalidRange",
        TranscriptError::InvalidFrameRate => "InvalidFrameRate",
        TranscriptError::EmptyScope => "MissingTrackScope",
        TranscriptError::AmbiguousDialogueSelection => "AmbiguousTrackScope",
        TranscriptError::TrackLocked(_) => "TrackLocked",
        TranscriptError::OverlappingWordTiming => "OverlappingWordTiming",
        TranscriptError::InvalidWordTiming => "InvalidWordTiming",
        TranscriptError::InvalidCueTiming => "InvalidCueTiming",
        TranscriptError::NoMediaInRange => "NoMediaInRange",
    };
    ToolResult::error(error.to_string()).with_data(json!({"error_code": code}))
}

async fn commit_plan(
    state: &AppState,
    message: &str,
    planner: impl FnOnce(&TimelineProject) -> Result<Vec<TimelineCmd>, TranscriptError>,
) -> ToolResult {
    let mut document = state.document.lock().await;
    let Some(project) = document.timeline.as_ref() else {
        return ToolResult::error("no timeline project")
            .with_data(json!({"error_code": "NoProject"}));
    };
    let commands = match planner(project) {
        Ok(commands) => commands,
        Err(reason) => return error(reason),
    };
    let command_count = commands.len();
    let mut history = state.history.lock().await;
    if !commands.is_empty() {
        history.execute_discrete(
            Command::Batch(commands.into_iter().map(Command::Timeline).collect()),
            &mut document,
        );
    }
    ToolResult::text(message)
        .with_data(json!({"changed": command_count > 0, "command_count": command_count, "revision":history.revision()}))
}

pub async fn get_transcript(state: &AppState, args: GetTranscriptArgs) -> ToolResult {
    let document = state.document.lock().await;
    let revision = state.history.lock().await.revision();
    if args.offset > 0 && args.expected_revision.is_none() {
        return ToolResult::error_with_code(
            "RevisionRequired",
            "Continuation pages require expected_revision",
        );
    }
    if args
        .expected_revision
        .is_some_and(|expected| expected != revision)
    {
        return ToolResult::error_with_code("RevisionConflict", "Transcript changed between pages")
            .with_data(json!({"actual_revision":revision}));
    }
    if args
        .limit
        .is_some_and(|limit| !(1..=10_000).contains(&limit))
    {
        return ToolResult::error_with_code("InvalidArguments", "limit must be 1–10000");
    }
    let Some(project) = document.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let view = match transcript::get_transcript(project, args.sequence_id, args.caption_track_id) {
        Ok(view) => view,
        Err(reason) => return error(reason),
    };
    let total = view.tokens.len();
    let offset = args.offset.min(total);
    let end = offset
        .saturating_add(args.limit.unwrap_or(1000).clamp(1, 10_000))
        .min(total);
    ToolResult::text("Derived transcript words").with_data(json!({
        "revision":revision,
        "sequence_id": args.sequence_id,
        "caption_track_id": args.caption_track_id,
        "tokens": &view.tokens[offset..end],
        "total": total,
        "offset": offset,
        "next_offset": (end < total).then_some(end),
        "adjusted_word_count": view.adjusted_word_count,
        "has_overlapping_words": view.has_overlapping_words,
    }))
}

pub async fn edit_transcript_word(state: &AppState, args: EditTranscriptWordArgs) -> ToolResult {
    commit_plan(state, "Updated transcript wording", |project| {
        transcript::edit_transcript_word(
            project,
            args.sequence_id,
            args.caption_track_id,
            args.cue_id,
            args.word_index,
            args.text,
        )
    })
    .await
}

pub async fn delete_transcript_range(
    state: &AppState,
    args: DeleteTranscriptRangeArgs,
) -> ToolResult {
    commit_plan(
        state,
        if args.ripple {
            "Ripple-deleted transcript range"
        } else {
            "Lifted transcript range"
        },
        |project| {
            transcript::delete_transcript_range(
                project,
                args.sequence_id,
                args.caption_track_id,
                &args.target_track_ids,
                (Tick(args.start_ticks), Tick(args.end_ticks)),
                args.ripple,
            )
        },
    )
    .await
}

pub async fn find_filler_words(state: &AppState, args: FindFillerWordsArgs) -> ToolResult {
    let document = state.document.lock().await;
    let revision = state.history.lock().await.revision();
    let Some(project) = document.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let projection =
        match transcript::get_transcript(project, args.sequence_id, args.caption_track_id) {
            Ok(view) => view,
            Err(reason) => return error(reason),
        };
    let lexicon = args.lexicon.unwrap_or_else(|| {
        transcript::DEFAULT_FILLER_WORDS
            .iter()
            .map(|word| (*word).into())
            .collect()
    });
    let matches = transcript::find_filler_words(&projection, &lexicon);
    ToolResult::text("Filler preview; pass the included matches to remove_filler_words").with_data(
        json!({
            "revision":revision,
            "sequence_id": args.sequence_id,
            "caption_track_id": args.caption_track_id,
            "count": matches.len(),
            "matches": matches,
        }),
    )
}

pub async fn remove_filler_words(state: &AppState, args: RemoveFillerWordsArgs) -> ToolResult {
    commit_plan(state, "Removed the included filler words", |project| {
        transcript::remove_filler_words(
            project,
            args.sequence_id,
            args.caption_track_id,
            &args.target_track_ids,
            &args.matches,
            args.ripple,
        )
    })
    .await
}

/// Append these schemas to the canonical tool catalog. Kept with the args so
/// required explicit scope and preview semantics cannot drift between tools.
pub fn tool_schemas() -> Vec<Value> {
    let scope = json!({
        "sequence_id": {"type": "string"},
        "caption_track_id": {"type": "string"}
    });
    let targets = json!({"type": "array", "items": {"type": "string"}, "minItems": 1, "description": "All participating dialogue A/V and sync-lock track IDs. Locked participants reject the whole edit. No implicit tracks are added."});
    let token = json!({
        "type": "object",
        "properties": {
            "track": {"type": "string"}, "cue": {"type": "string"},
            "word_index": {"type": "integer", "minimum": 0}, "text": {"type": "string"},
            "start": {"type": "integer", "minimum": 0}, "end": {"type": "integer", "minimum": 0}
        },
        "required": ["track", "cue", "word_index", "text", "start", "end"]
    });
    let mutation_output = json!({
        "type": "object",
        "properties": {"revision":{"type":"integer","minimum":0}, "changed": {"type": "boolean"}, "command_count": {"type": "integer", "minimum": 0}},
        "required": ["changed", "command_count", "revision"]
    });
    let transcript_output = json!({
        "type": "object",
        "properties": {
            "revision":{"type":"integer","minimum":0}, "sequence_id": {"type": "string"}, "caption_track_id": {"type": "string"},
            "tokens": {"type": "array", "items": token},
            "total": {"type": "integer", "minimum": 0}, "offset": {"type": "integer", "minimum": 0},
            "next_offset": {"type": ["integer", "null"], "minimum": 0},
            "adjusted_word_count": {"type": "integer", "minimum": 0}, "has_overlapping_words": {"type": "boolean"}
        },
        "required": ["revision", "sequence_id", "caption_track_id", "tokens", "total", "offset", "next_offset", "adjusted_word_count", "has_overlapping_words"]
    });
    let filler_output = json!({
        "type": "object",
        "properties": {
            "revision":{"type":"integer","minimum":0}, "sequence_id": {"type": "string"}, "caption_track_id": {"type": "string"},
            "matches": {"type": "array", "items": token}, "count": {"type": "integer", "minimum": 0}
        },
        "required": ["revision", "sequence_id", "caption_track_id", "matches", "count"]
    });
    let make = |name: &str,
                description: &str,
                extra: Value,
                required: &[&str],
                output: Value,
                behavior: ToolBehavior| {
        let mut properties = scope.as_object().cloned().unwrap_or_default();
        if let Some(extra) = extra.as_object() {
            properties.extend(extra.clone());
        }
        let mut required_fields = vec!["sequence_id", "caption_track_id"];
        required_fields.extend(required);
        tool_definition(
            name,
            description,
            json!({"type": "object", "properties": properties, "required": required_fields}),
            output,
            behavior,
        )
    };
    vec![
        make("get_transcript", "Get derived word-timed transcript tokens from an explicitly chosen sequence/caption track. Read-only; paginated (default 1000, max 10000). Continuation pages require expected_revision from the first page.",
            json!({"expected_revision":{"type":"integer","minimum":0}, "offset": {"type": "integer", "minimum": 0}, "limit": {"type": "integer", "minimum": 1, "maximum": 10000}}), &[], transcript_output, ToolBehavior::ReadOnly),
        make("edit_transcript_word", "Change a caption word's text only, retaining exact timing and style. One undo step.",
            json!({"cue_id": {"type": "string"}, "word_index": {"type": "integer", "minimum": 0}, "text": {"type": "string"}}), &["cue_id", "word_index", "text"], mutation_output.clone(), ToolBehavior::Mutation),
        make("delete_transcript_range", "Delete a half-open sequence-time interval from explicit A/V tracks and the caption track, frame-snapped outward. ripple=true closes the gap; false leaves a gap. Atomic with one undo step.",
            json!({"target_track_ids": targets, "start_ticks": {"type": "integer", "minimum": 0}, "end_ticks": {"type": "integer", "minimum": 0}, "ripple": {"type": "boolean"}}), &["target_track_ids", "start_ticks", "end_ticks", "ripple"], mutation_output.clone(), ToolBehavior::Mutation),
        make("find_filler_words", "Preview local exact-token filler matches (default um, uh, erm, er, hmm). No timeline mutation. Omit excluded matches when calling remove_filler_words.",
            json!({"lexicon": {"type": "array", "items": {"type": "string"}}}), &[], filler_output, ToolBehavior::ReadOnly),
        make("remove_filler_words", "Remove chosen matches returned by find_filler_words, rejecting stale previews. Merges ranges and applies right to left across explicit A/V and caption tracks; one undo step.",
            json!({"target_track_ids": targets, "matches": {"type": "array", "items": token}, "ripple": {"type": "boolean"}}), &["target_track_ids", "matches", "ripple"], mutation_output, ToolBehavior::Mutation),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::{
        CaptionCue, CaptionTrack, CaptionWord, Clip, ClipSource, FrameRate, Sequence, Track,
        TrackKind,
    };

    async fn fixture() -> (AppState, SequenceId, TrackId, TrackId) {
        let state = AppState::headless_for_test();
        let mut project = TimelineProject::new();
        let mut sequence = Sequence::new("Transcript", FrameRate::FPS_30, 1920, 1080);
        let mut track = Track::new(TrackKind::Video, "Dialogue");
        // The cut starts at the clip boundary, so separate planner invocations
        // produce no new random IDs and can be compared byte-for-byte.
        track.clips.push(Clip::new(
            ClipSource::Adjustment,
            Tick::ZERO,
            Tick::from_seconds(4),
        ));
        let track_id = track.id;
        sequence.video_tracks.push(track);
        let mut captions = CaptionTrack::new("Dialogue captions");
        captions.cues.push(CaptionCue::new(
            Tick::ZERO,
            Tick::from_seconds(4),
            vec![
                CaptionWord::new("um", Tick::ZERO, Tick::from_seconds(1)),
                CaptionWord::new("hello", Tick::from_seconds(1), Tick::from_seconds(2)),
                CaptionWord::new("there", Tick::from_seconds(2), Tick::from_seconds(4)),
            ],
        ));
        let caption_id = captions.id;
        sequence.caption_tracks.push(captions);
        let sequence_id = project.insert_sequence(sequence);
        state.document.lock().await.timeline = Some(project);
        (state, sequence_id, caption_id, track_id)
    }

    #[tokio::test]
    async fn mcp_delete_matches_shared_gui_planner_and_one_undo() {
        let (state, sequence, caption, track) = fixture().await;
        let before = state.document.lock().await.clone();
        let plan = transcript::delete_transcript_range(
            before.timeline.as_ref().unwrap(),
            sequence,
            caption,
            &[track],
            (Tick::ZERO, Tick::from_seconds(1)),
            true,
        )
        .unwrap();
        let mut expected = before.clone();
        Command::Batch(plan.into_iter().map(Command::Timeline).collect()).apply(&mut expected);
        let result = delete_transcript_range(
            &state,
            DeleteTranscriptRangeArgs {
                sequence_id: sequence,
                caption_track_id: caption,
                target_track_ids: vec![track],
                start_ticks: 0,
                end_ticks: Tick::from_seconds(1).0,
                ripple: true,
            },
        )
        .await;
        assert!(!result.is_error.unwrap_or(false));
        assert_eq!(state.document.lock().await.timeline, expected.timeline);
        let mut doc = state.document.lock().await;
        let mut history = state.history.lock().await;
        assert_eq!(history.undo_depth(), 1);
        assert!(history.undo(&mut doc));
        assert_eq!(doc.timeline, before.timeline);
    }

    #[tokio::test]
    async fn read_tools_create_no_history_and_locked_track_refuses_mutation() {
        let (state, sequence, caption, track) = fixture().await;
        get_transcript(
            &state,
            GetTranscriptArgs {
                expected_revision: None,
                sequence_id: sequence,
                caption_track_id: caption,
                offset: 0,
                limit: None,
            },
        )
        .await;
        find_filler_words(
            &state,
            FindFillerWordsArgs {
                sequence_id: sequence,
                caption_track_id: caption,
                lexicon: None,
            },
        )
        .await;
        assert_eq!(state.history.lock().await.undo_depth(), 0);
        let before = {
            let mut doc = state.document.lock().await;
            doc.timeline
                .as_mut()
                .unwrap()
                .sequences
                .get_mut(&sequence)
                .unwrap()
                .track_mut(track)
                .unwrap()
                .locked = true;
            doc.timeline.clone()
        };
        let result = delete_transcript_range(
            &state,
            DeleteTranscriptRangeArgs {
                sequence_id: sequence,
                caption_track_id: caption,
                target_track_ids: vec![track],
                start_ticks: 0,
                end_ticks: Tick::from_seconds(1).0,
                ripple: true,
            },
        )
        .await;
        assert!(result.is_error.unwrap_or(false));
        assert_eq!(state.document.lock().await.timeline, before);
        assert_eq!(state.history.lock().await.undo_depth(), 0);
    }

    #[test]
    fn mutating_args_require_explicit_scope_and_ripple_choice() {
        let incomplete = json!({"sequence_id": SequenceId::new(), "caption_track_id": TrackId::new(), "start_ticks": 0, "end_ticks": 10});
        assert!(serde_json::from_value::<DeleteTranscriptRangeArgs>(incomplete).is_err());
        let names: Vec<_> = tool_schemas()
            .iter()
            .map(|schema| schema["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            names,
            [
                "get_transcript",
                "edit_transcript_word",
                "delete_transcript_range",
                "find_filler_words",
                "remove_filler_words"
            ]
        );
    }
}
