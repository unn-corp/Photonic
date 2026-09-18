//! E2E coverage for the round-2 NLE-parity MCP tools (docs/specs/video-editor/
//! 17-nle-parity-round2.md, gap G-21): `replace_clip_source` (G-5),
//! `add_edit_all_tracks`/`close_gap` (G-1), `match_frame` (G-3),
//! `insert_adjustment_clip` (G-7), `insert_text_clip` (G-12), and
//! `set_clip_speed`'s keyframed-ramp extension (G-11).
//!
//! Mirrors `mcp_parity.rs`'s pattern: drives handler fns directly against a
//! freshly built `AppState`, bypassing the JSON-RPC/axum transport but
//! exercising the exact code path a real MCP client hits via
//! `dispatch::dispatch_tool`.

use photonic_core::history::CommandHistory;
use photonic_core::{AuditLog, Document};
use photonic_mcp::handlers;
use photonic_mcp::protocol::{ContentItem, ToolResult};
use photonic_mcp::server::{AppState, McpServerConfig};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex;

fn test_state() -> AppState {
    let (tx, _rx) = std::sync::mpsc::channel();
    AppState {
        document: Arc::new(Mutex::new(Document::new("t", 1920.0, 1080.0))),
        history: Arc::new(Mutex::new(CommandHistory::new(200))),
        document_path: Arc::new(StdMutex::new(None)),
        capture_tx: Arc::new(StdMutex::new(tx)),
        config: McpServerConfig::default(),
        path_policy: photonic_core::PathPolicy::desktop_default(),
        audit_log: Arc::new(StdMutex::new(AuditLog::new())),
        clipboard_ring: Arc::new(handlers::clipboard::new_clipboard_ring()),
        video_engine: Arc::new(handlers::video_jobs::VideoEngineHandle::new()),
        video_jobs: Arc::new(StdMutex::new(handlers::video_jobs::JobRegistry::new())),
    }
}

/// Pulls the JSON payload a handler attached via `ToolResult::with_data`.
fn tool_data(result: &ToolResult) -> Value {
    for item in &result.content {
        if let ContentItem::Text { text } = item {
            if let Ok(v) = serde_json::from_str::<Value>(text) {
                return v;
            }
        }
    }
    Value::Null
}

fn assert_ok(result: &ToolResult, ctx: &str) {
    assert_ne!(
        result.is_error,
        Some(true),
        "{ctx} failed: {:?}",
        result.content
    );
}

fn assert_err(result: &ToolResult, ctx: &str) {
    assert_eq!(
        result.is_error,
        Some(true),
        "{ctx} unexpectedly succeeded: {:?}",
        result.content
    );
}

/// Creates a sequence + one track of `kind` ("video"/"audio"). Returns
/// `(sequence_id, track_id)` as wire strings.
async fn seq_with_track(state: &AppState, kind: &str) -> (String, String) {
    let seq_args = serde_json::from_value(json!({
        "name": "Seq",
        "frame_rate": { "num": 30, "den": 1 },
        "formats": [{ "name": "Main", "width": 1920, "height": 1080 }]
    }))
    .unwrap();
    let seq_result = handlers::video::create_sequence(state, seq_args).await;
    assert_ok(&seq_result, "create_sequence");
    let sequence_id = tool_data(&seq_result)["sequence_id"]
        .as_str()
        .unwrap()
        .to_string();

    let track_args = serde_json::from_value(json!({
        "sequence_id": sequence_id,
        "kind": kind,
    }))
    .unwrap();
    let track_result = handlers::video::add_track(state, track_args).await;
    assert_ok(&track_result, "add_track");
    let track_id = tool_data(&track_result)["track_id"]
        .as_str()
        .unwrap()
        .to_string();

    (sequence_id, track_id)
}

/// Inserts a `SolidColor`-source clip `[start, start+duration)` on
/// `track_id` via the generic `insert_clip` tool (a fixture helper — NOT the
/// tool under test for `insert_adjustment_clip`/`insert_text_clip` below).
/// Returns the new clip's id as a wire string.
async fn insert_solid_clip(
    state: &AppState,
    track_id: &str,
    color: &str,
    start_ticks: i64,
    duration_ticks: i64,
) -> String {
    let args = serde_json::from_value(json!({
        "track_id": track_id,
        "start_ticks": start_ticks,
        "source": { "kind": "solid_color", "color": color },
        "duration_ticks": duration_ticks,
    }))
    .unwrap();
    let result = handlers::video::insert_clip(state, args).await;
    assert_ok(&result, "insert_clip (fixture)");
    tool_data(&result)["clip_id"].as_str().unwrap().to_string()
}

async fn get_clip_json(state: &AppState, clip_id: &str) -> Value {
    let args = serde_json::from_value(json!({ "clip_id": clip_id })).unwrap();
    let result = handlers::video::get_clip(state, args).await;
    assert_ok(&result, "get_clip");
    tool_data(&result)["clip"].clone()
}

async fn list_clips_on_track(state: &AppState, track_id: &str) -> Vec<Value> {
    let args = serde_json::from_value(json!({ "track_id": track_id })).unwrap();
    let result = handlers::video::list_clips(state, args).await;
    assert_ok(&result, "list_clips");
    tool_data(&result)["clips"].as_array().unwrap().clone()
}

async fn lock_track(state: &AppState, track_id: &str) {
    let args = serde_json::from_value(json!({ "track_id": track_id, "locked": true })).unwrap();
    let result = handlers::video::set_track_prop(state, args).await;
    assert_ok(&result, "set_track_prop (lock)");
}

async fn undo(state: &AppState) {
    let args = serde_json::from_value(json!({})).unwrap();
    let result = handlers::doc_state::undo(state, args).await;
    assert_ok(&result.0, "undo");
}

// ─── replace_clip_source (G-5) ──────────────────────────────────────────────

#[tokio::test]
async fn replace_clip_source_keeps_start_and_duration() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;
    let clip = insert_solid_clip(&state, &track, "#ff0000", 50, 120).await;

    let args = serde_json::from_value(json!({
        "clip_id": clip,
        "new_source": { "kind": "solid_color", "color": "#00ff00" },
    }))
    .unwrap();
    let result = handlers::video::replace_clip_source(&state, args).await;
    assert_ok(&result, "replace_clip_source");
    assert_eq!(tool_data(&result)["clip_id"].as_str(), Some(clip.as_str()));

    let after = get_clip_json(&state, &clip).await;
    assert_eq!(after["start"].as_i64(), Some(50), "start unchanged");
    assert_eq!(after["duration"].as_i64(), Some(120), "duration unchanged");
    assert_eq!(after["id"].as_str(), Some(clip.as_str()), "same clip id");
}

#[tokio::test]
async fn replace_clip_source_applies_new_source_in_when_given() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;
    let clip = insert_solid_clip(&state, &track, "#ff0000", 0, 100).await;

    let args = serde_json::from_value(json!({
        "clip_id": clip,
        "new_source": { "kind": "solid_color", "color": "#0000ff" },
        "new_source_in_ticks": 42,
    }))
    .unwrap();
    let result = handlers::video::replace_clip_source(&state, args).await;
    assert_ok(&result, "replace_clip_source (with source_in)");

    let after = get_clip_json(&state, &clip).await;
    assert_eq!(after["source_in"].as_i64(), Some(42));
}

#[tokio::test]
async fn replace_clip_source_errors_on_unknown_clip() {
    let state = test_state();
    let args = serde_json::from_value(json!({
        "clip_id": "00000000-0000-0000-0000-000000000000",
        "new_source": { "kind": "adjustment" },
    }))
    .unwrap();
    let result = handlers::video::replace_clip_source(&state, args).await;
    assert_err(&result, "replace_clip_source on unknown clip");
}

// ─── add_edit_all_tracks (G-1) ──────────────────────────────────────────────

#[tokio::test]
async fn add_edit_all_tracks_splits_every_unlocked_track_in_one_undo_step() {
    let state = test_state();
    let (seq, video_track) = seq_with_track(&state, "video").await;
    let audio_args =
        serde_json::from_value(json!({ "sequence_id": seq, "kind": "audio" })).unwrap();
    let audio_result = handlers::video::add_track(&state, audio_args).await;
    assert_ok(&audio_result, "add_track (audio)");
    let audio_track = tool_data(&audio_result)["track_id"]
        .as_str()
        .unwrap()
        .to_string();

    insert_solid_clip(&state, &video_track, "#ff0000", 0, 200).await;
    insert_solid_clip(&state, &audio_track, "#00ff00", 0, 200).await;

    let args = serde_json::from_value(json!({ "sequence_id": seq, "at_ticks": 80 })).unwrap();
    let result = handlers::video::add_edit_all_tracks(&state, args).await;
    assert_ok(&result, "add_edit_all_tracks");
    assert_eq!(tool_data(&result)["split_count"].as_u64(), Some(2));
    assert_eq!(
        tool_data(&result)["new_clip_ids"]
            .as_array()
            .map(|a| a.len()),
        Some(2)
    );

    assert_eq!(list_clips_on_track(&state, &video_track).await.len(), 2);
    assert_eq!(list_clips_on_track(&state, &audio_track).await.len(), 2);

    // ONE undo step reverts both splits.
    undo(&state).await;
    assert_eq!(list_clips_on_track(&state, &video_track).await.len(), 1);
    assert_eq!(list_clips_on_track(&state, &audio_track).await.len(), 1);
}

#[tokio::test]
async fn add_edit_all_tracks_skips_locked_tracks_and_is_a_reported_no_op_when_empty() {
    let state = test_state();
    let (seq, track) = seq_with_track(&state, "video").await;
    insert_solid_clip(&state, &track, "#ff0000", 0, 200).await;
    lock_track(&state, &track).await;

    let args = serde_json::from_value(json!({ "sequence_id": seq, "at_ticks": 80 })).unwrap();
    let result = handlers::video::add_edit_all_tracks(&state, args).await;
    assert_ok(&result, "add_edit_all_tracks (locked track)");
    assert_eq!(tool_data(&result)["split_count"].as_u64(), Some(0));
    assert_eq!(list_clips_on_track(&state, &track).await.len(), 1);
}

// ─── close_gap (G-1) ─────────────────────────────────────────────────────────

#[tokio::test]
async fn close_gap_on_one_track_shifts_only_that_track() {
    let state = test_state();
    let (seq, video_track) = seq_with_track(&state, "video").await;
    let audio_args =
        serde_json::from_value(json!({ "sequence_id": seq, "kind": "audio" })).unwrap();
    let audio_result = handlers::video::add_track(&state, audio_args).await;
    assert_ok(&audio_result, "add_track (audio)");
    let audio_track = tool_data(&audio_result)["track_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Video: [0,50) gap [100,150) — gap of 50 at tick 75.
    insert_solid_clip(&state, &video_track, "#ff0000", 0, 50).await;
    let v_after_gap = insert_solid_clip(&state, &video_track, "#ff0000", 100, 50).await;
    // Audio: same shape, gap at the same tick — must NOT move (track_id scoped).
    let a_after_gap = insert_solid_clip(&state, &audio_track, "#00ff00", 100, 50).await;
    insert_solid_clip(&state, &audio_track, "#00ff00", 0, 50).await;

    let args = serde_json::from_value(json!({
        "sequence_id": seq,
        "track_id": video_track,
        "at_ticks": 75,
    }))
    .unwrap();
    let result = handlers::video::close_gap(&state, args).await;
    assert_ok(&result, "close_gap (single track)");
    assert_eq!(tool_data(&result)["tracks_changed"].as_u64(), Some(1));

    let v_clip = get_clip_json(&state, &v_after_gap).await;
    assert_eq!(
        v_clip["start"].as_i64(),
        Some(50),
        "video clip shifted left to close the gap"
    );

    let a_clip = get_clip_json(&state, &a_after_gap).await;
    assert_eq!(a_clip["start"].as_i64(), Some(100), "audio track untouched");
}

#[tokio::test]
async fn close_gap_across_all_tracks_when_track_id_omitted() {
    let state = test_state();
    let (seq, video_track) = seq_with_track(&state, "video").await;
    let audio_args =
        serde_json::from_value(json!({ "sequence_id": seq, "kind": "audio" })).unwrap();
    let audio_result = handlers::video::add_track(&state, audio_args).await;
    assert_ok(&audio_result, "add_track (audio)");
    let audio_track = tool_data(&audio_result)["track_id"]
        .as_str()
        .unwrap()
        .to_string();

    let v_tail = insert_solid_clip(&state, &video_track, "#ff0000", 100, 50).await;
    let a_tail = insert_solid_clip(&state, &audio_track, "#00ff00", 100, 50).await;

    let args = serde_json::from_value(json!({ "sequence_id": seq, "at_ticks": 10 })).unwrap();
    let result = handlers::video::close_gap(&state, args).await;
    assert_ok(&result, "close_gap (all tracks)");
    assert_eq!(tool_data(&result)["tracks_changed"].as_u64(), Some(2));

    assert_eq!(
        get_clip_json(&state, &v_tail).await["start"].as_i64(),
        Some(0)
    );
    assert_eq!(
        get_clip_json(&state, &a_tail).await["start"].as_i64(),
        Some(0)
    );
}

#[tokio::test]
async fn close_gap_with_no_gap_is_a_reported_no_op() {
    let state = test_state();
    let (seq, track) = seq_with_track(&state, "video").await;
    insert_solid_clip(&state, &track, "#ff0000", 0, 200).await;

    let args = serde_json::from_value(json!({ "sequence_id": seq, "at_ticks": 100 })).unwrap();
    let result = handlers::video::close_gap(&state, args).await;
    assert_ok(&result, "close_gap (no gap)");
    assert_eq!(tool_data(&result)["tracks_changed"].as_u64(), Some(0));
}

// ─── match_frame (G-3) ───────────────────────────────────────────────────────

#[tokio::test]
async fn match_frame_computes_source_tick_from_speed_and_trim() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;
    let clip = insert_solid_clip(&state, &track, "#ff0000", 100, 200).await; // [100,300)

    // 2x speed from a source_in of 10.
    let speed_args = serde_json::from_value(json!({
        "clip_id": clip,
        "ratio": { "num": 2, "den": 1 },
    }))
    .unwrap();
    assert_ok(
        &handlers::video::set_clip_speed(&state, speed_args).await,
        "set_clip_speed",
    );

    let mf_args = serde_json::from_value(json!({ "clip_id": clip, "at_ticks": 150 })).unwrap();
    let result = handlers::video::match_frame(&state, mf_args).await;
    assert_ok(&result, "match_frame");
    // source_in (0, SolidColor default) + (150-100)*2 = 100.
    assert_eq!(tool_data(&result)["source_tick"].as_i64(), Some(100));
    assert!(
        tool_data(&result)["asset_id"].is_null(),
        "SolidColor has no asset"
    );
}

#[tokio::test]
async fn match_frame_returns_the_asset_id_for_an_asset_backed_clip() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;

    let tmp = std::env::temp_dir().join(format!(
        "photonic_mcp_match_frame_test_{}.mp4",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&tmp, b"fake media bytes").unwrap();
    let import_args = serde_json::from_value(json!({ "paths": [tmp.to_string_lossy()] })).unwrap();
    let import_result = handlers::video::import_media(&state, import_args).await;
    assert_ok(&import_result, "import_media");
    let asset_id = tool_data(&import_result)["assets"][0]["asset_id"]
        .as_str()
        .unwrap()
        .to_string();

    let clip_args = serde_json::from_value(json!({
        "track_id": track,
        "start_ticks": 0,
        "source": { "kind": "asset", "asset_id": asset_id },
        "duration_ticks": 100,
    }))
    .unwrap();
    let clip_result = handlers::video::insert_clip(&state, clip_args).await;
    assert_ok(&clip_result, "insert_clip (asset)");
    let clip_id = tool_data(&clip_result)["clip_id"]
        .as_str()
        .unwrap()
        .to_string();

    let mf_args = serde_json::from_value(json!({ "clip_id": clip_id, "at_ticks": 10 })).unwrap();
    let result = handlers::video::match_frame(&state, mf_args).await;
    assert_ok(&result, "match_frame (asset)");
    assert_eq!(
        tool_data(&result)["asset_id"].as_str(),
        Some(asset_id.as_str())
    );
    assert_eq!(tool_data(&result)["source_tick"].as_i64(), Some(10));

    let _ = std::fs::remove_file(&tmp);
}

#[tokio::test]
async fn match_frame_rejects_a_tick_outside_the_clip_span() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;
    let clip = insert_solid_clip(&state, &track, "#ff0000", 100, 50).await; // [100,150)

    let args = serde_json::from_value(json!({ "clip_id": clip, "at_ticks": 200 })).unwrap();
    let result = handlers::video::match_frame(&state, args).await;
    assert_err(&result, "match_frame outside span");
}

// ─── insert_adjustment_clip (G-7) ──────────────────────────────────────────

#[tokio::test]
async fn insert_adjustment_clip_creates_a_no_media_adjustment_clip() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;

    let args = serde_json::from_value(json!({
        "track_id": track,
        "start_ticks": 20,
        "duration_ticks": 80,
    }))
    .unwrap();
    let result = handlers::video::insert_adjustment_clip(&state, args).await;
    assert_ok(&result, "insert_adjustment_clip");
    let clip_id = tool_data(&result)["clip_id"].as_str().unwrap().to_string();

    let clip = get_clip_json(&state, &clip_id).await;
    assert_eq!(clip["start"].as_i64(), Some(20));
    assert_eq!(clip["duration"].as_i64(), Some(80));
    assert_eq!(clip["source"]["source"].as_str(), Some("adjustment"));
}

#[tokio::test]
async fn insert_adjustment_clip_rejects_non_positive_duration() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;
    let args = serde_json::from_value(json!({
        "track_id": track,
        "start_ticks": 0,
        "duration_ticks": 0,
    }))
    .unwrap();
    let result = handlers::video::insert_adjustment_clip(&state, args).await;
    assert_err(&result, "insert_adjustment_clip (zero duration)");
}

// ─── insert_text_clip (G-12) ────────────────────────────────────────────────

#[tokio::test]
async fn insert_text_clip_creates_a_text_clip_with_default_style() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;

    let args = serde_json::from_value(json!({
        "track_id": track,
        "text": "Lower Third",
        "start_ticks": 0,
        "duration_ticks": 150,
    }))
    .unwrap();
    let result = handlers::video::insert_text_clip(&state, args).await;
    assert_ok(&result, "insert_text_clip");
    let clip_id = tool_data(&result)["clip_id"].as_str().unwrap().to_string();

    let clip = get_clip_json(&state, &clip_id).await;
    assert_eq!(clip["source"]["source"].as_str(), Some("text"));
    assert_eq!(
        clip["source"]["content"]["text"].as_str(),
        Some("Lower Third")
    );
    assert_eq!(
        clip["name"].as_str(),
        Some("Lower Third"),
        "name defaults to the text"
    );
}

#[tokio::test]
async fn insert_text_clip_applies_a_partial_style_patch() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;

    let args = serde_json::from_value(json!({
        "track_id": track,
        "text": "Styled",
        "style": { "font_size": 96.0, "fill": "#ff0000" },
        "start_ticks": 0,
        "duration_ticks": 60,
    }))
    .unwrap();
    let result = handlers::video::insert_text_clip(&state, args).await;
    assert_ok(&result, "insert_text_clip (styled)");
    let clip_id = tool_data(&result)["clip_id"].as_str().unwrap().to_string();

    let clip = get_clip_json(&state, &clip_id).await;
    let style = &clip["source"]["content"]["style"];
    assert_eq!(style["font_size"].as_f64(), Some(96.0), "override applied");
    // Unspecified fields keep the default (font_family stays "sans-serif").
    assert_eq!(style["font_family"].as_str(), Some("sans-serif"));
}

// ─── set_clip_speed keyframed ramp (G-11) ──────────────────────────────────

#[tokio::test]
async fn set_clip_speed_accepts_a_keyframed_ramp() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;
    let clip = insert_solid_clip(&state, &track, "#ff0000", 0, 200).await;

    let args = serde_json::from_value(json!({
        "clip_id": clip,
        "keys": [
            { "at_ticks": 0, "ratio": { "num": 1, "den": 2 } },
            { "at_ticks": 100, "ratio": { "num": 2, "den": 1 } },
        ],
    }))
    .unwrap();
    let result = handlers::video::set_clip_speed(&state, args).await;
    assert_ok(&result, "set_clip_speed (ramp)");

    let clip_json = get_clip_json(&state, &clip).await;
    assert_eq!(clip_json["speed"]["speed"].as_str(), Some("keyframed"));
    let keys = clip_json["speed"]["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0]["at"].as_i64(), Some(0));
    assert_eq!(keys[1]["at"].as_i64(), Some(100));
}

#[tokio::test]
async fn set_clip_speed_preserves_bezier_interpolation() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;
    let clip = insert_solid_clip(&state, &track, "#ff0000", 0, 200).await;

    let args = serde_json::from_value(json!({
        "clip_id": clip,
        "keys": [
            {
                "at_ticks": 0,
                "ratio": { "num": 1, "den": 1 },
                "interp": {
                    "kind": "bezier",
                    "out_handle": [0.42, 0.0],
                    "in_handle": [0.58, 1.0]
                }
            },
            { "at_ticks": 100, "ratio": { "num": 2, "den": 1 } }
        ]
    }))
    .unwrap();
    let result = handlers::video::set_clip_speed(&state, args).await;
    assert_ok(&result, "set_clip_speed (bezier ramp)");

    let clip_json = get_clip_json(&state, &clip).await;
    assert_eq!(
        clip_json["speed"]["keys"][0]["interp"]["kind"].as_str(),
        Some("bezier")
    );
}

#[tokio::test]
async fn set_clip_speed_rejects_supplying_both_ratio_and_keys() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;
    let clip = insert_solid_clip(&state, &track, "#ff0000", 0, 200).await;

    let args = serde_json::from_value(json!({
        "clip_id": clip,
        "ratio": { "num": 2, "den": 1 },
        "keys": [{ "at_ticks": 0, "ratio": { "num": 1, "den": 1 } }],
    }))
    .unwrap();
    let result = handlers::video::set_clip_speed(&state, args).await;
    assert_err(&result, "set_clip_speed (ratio + keys both supplied)");
}

#[tokio::test]
async fn set_clip_speed_rejects_supplying_neither_ratio_nor_keys() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;
    let clip = insert_solid_clip(&state, &track, "#ff0000", 0, 200).await;

    let args = serde_json::from_value(json!({ "clip_id": clip })).unwrap();
    let result = handlers::video::set_clip_speed(&state, args).await;
    assert_err(&result, "set_clip_speed (neither supplied)");
}
