//! E2E coverage for the CAP-019 MCP-parity tools (docs/specs/video-editor/
//! 14-nle-parity.md §9 / 16-insert-overwrite-editing.md): `insert_edit`,
//! `overwrite_edit`, `lift_edit`, `extract_edit`, `link_clips`/
//! `unlink_clips`, and `set_clip_prop`'s new `color_label` field.
//!
//! Each test drives the handler fns directly (bypassing the JSON-RPC/axum
//! transport, mirroring `handlers::nodes::create_shape_color_tests`) against
//! a freshly built `AppState`, so the assertions exercise the exact code
//! path a real MCP client hits via `dispatch::dispatch_tool`.

use photonic_mcp::handlers;
use photonic_mcp::protocol::{ContentItem, ToolResult};
use photonic_mcp::server::AppState;
use serde_json::{json, Value};

/// Builds the shared headless `AppState`. Delegates to the crate's own
/// `AppState::headless_for_test()` (server.rs) so this file and the
/// out-of-crate acceptance-story harness (29 §3 / CAP-019) construct the
/// exact same nine-field shape and cannot drift.
fn test_state() -> AppState {
    AppState::headless_for_test()
}

/// Pulls the JSON payload a handler attached via `ToolResult::with_data` —
/// the first `ContentItem::Text` that parses as JSON (the human-readable
/// message that always leads usually doesn't).
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

/// Inserts an `Adjustment`-source clip `[start, start+duration)` on
/// `track_id`. Returns the new clip's id as a wire string.
async fn insert_adjustment_clip(
    state: &AppState,
    track_id: &str,
    start_ticks: i64,
    duration_ticks: i64,
) -> String {
    let args = serde_json::from_value(json!({
        "track_id": track_id,
        "start_ticks": start_ticks,
        "source": { "kind": "adjustment" },
        "duration_ticks": duration_ticks,
    }))
    .unwrap();
    let result = handlers::video::insert_clip(state, args).await;
    assert_ok(&result, "insert_clip");
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

// ─── insert_edit ────────────────────────────────────────────────────────────

#[tokio::test]
async fn insert_edit_ripples_clips_at_and_after_the_insertion_point() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;

    let _clip_a = insert_adjustment_clip(&state, &track, 0, 100).await; // [0,100)
    let clip_b = insert_adjustment_clip(&state, &track, 100, 100).await; // [100,200)

    let args = serde_json::from_value(json!({
        "track_id": track,
        "at_ticks": 100,
        "source": { "kind": "adjustment" },
        "duration_ticks": 30,
    }))
    .unwrap();
    let result = handlers::video::insert_edit(&state, args).await;
    assert_ok(&result, "insert_edit");
    let new_clip_id = tool_data(&result)["clip_id"].as_str().unwrap().to_string();

    // The new clip lands exactly at the insertion point...
    let new_clip = get_clip_json(&state, &new_clip_id).await;
    assert_eq!(new_clip["start"].as_i64(), Some(100));
    assert_eq!(new_clip["duration"].as_i64(), Some(30));

    // ...and clip B, which started exactly at the insertion point, rippled
    // right by the inserted clip's duration.
    let b_after = get_clip_json(&state, &clip_b).await;
    assert_eq!(b_after["start"].as_i64(), Some(130));
    assert_eq!(b_after["duration"].as_i64(), Some(100));

    // Track content grew by the inserted duration: 3 clips, span now 230.
    let clips = list_clips_on_track(&state, &track).await;
    assert_eq!(clips.len(), 3);
}

// ─── overwrite_edit ─────────────────────────────────────────────────────────

#[tokio::test]
async fn overwrite_edit_replaces_covered_content_with_no_ripple() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;

    let clip_a = insert_adjustment_clip(&state, &track, 0, 100).await; // [0,100)
    let clip_b = insert_adjustment_clip(&state, &track, 100, 100).await; // [100,200)

    let args = serde_json::from_value(json!({
        "track_id": track,
        "at_ticks": 50,
        "source": { "kind": "adjustment" },
        "duration_ticks": 100, // covers [50,150) — tail of A, head of B
    }))
    .unwrap();
    let result = handlers::video::overwrite_edit(&state, args).await;
    assert_ok(&result, "overwrite_edit");

    // A trimmed to [0,50); B trimmed to [150,200) — no shift, no ripple.
    let a_after = get_clip_json(&state, &clip_a).await;
    assert_eq!(a_after["start"].as_i64(), Some(0));
    assert_eq!(a_after["duration"].as_i64(), Some(50));
    let b_after = get_clip_json(&state, &clip_b).await;
    assert_eq!(b_after["start"].as_i64(), Some(150));
    assert_eq!(b_after["duration"].as_i64(), Some(50));

    // Timeline span unchanged at 200 — three clips total (A, source, B).
    let clips = list_clips_on_track(&state, &track).await;
    assert_eq!(clips.len(), 3);
    let max_end: i64 = clips
        .iter()
        .map(|c| c["start_ticks"].as_i64().unwrap() + c["duration_ticks"].as_i64().unwrap())
        .max()
        .unwrap();
    assert_eq!(max_end, 200);
}

// ─── lift_edit ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn lift_edit_removes_range_and_leaves_a_gap() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;
    let _clip = insert_adjustment_clip(&state, &track, 0, 200).await; // [0,200)

    let args = serde_json::from_value(json!({
        "track_id": track,
        "range": { "start_ticks": 50, "end_ticks": 150 },
    }))
    .unwrap();
    let result = handlers::video::lift_edit(&state, args).await;
    assert_ok(&result, "lift_edit");

    // Head [0,50) and tail [150,200) remain — no ripple, so the tail did not
    // move to close the gap.
    let clips = list_clips_on_track(&state, &track).await;
    assert_eq!(clips.len(), 2);
    let max_end: i64 = clips
        .iter()
        .map(|c| c["start_ticks"].as_i64().unwrap() + c["duration_ticks"].as_i64().unwrap())
        .max()
        .unwrap();
    assert_eq!(max_end, 200, "lift must not ripple — span stays 200");
    let min_start: i64 = clips
        .iter()
        .map(|c| c["start_ticks"].as_i64().unwrap())
        .min()
        .unwrap();
    assert_eq!(min_start, 0);
}

#[tokio::test]
async fn lift_edit_on_an_empty_range_is_a_reported_no_op() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;
    let _clip = insert_adjustment_clip(&state, &track, 0, 10).await; // [0,10)

    // Range [500,600) overlaps nothing on the track.
    let args = serde_json::from_value(json!({
        "track_id": track,
        "range": { "start_ticks": 500, "end_ticks": 600 },
    }))
    .unwrap();
    let result = handlers::video::lift_edit(&state, args).await;
    assert_ok(&result, "lift_edit (no-op)");

    // Nothing changed.
    let clips = list_clips_on_track(&state, &track).await;
    assert_eq!(clips.len(), 1);
}

// ─── extract_edit ───────────────────────────────────────────────────────────

#[tokio::test]
async fn extract_edit_removes_range_and_ripples_everything_after_left() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;
    let _clip1 = insert_adjustment_clip(&state, &track, 0, 200).await; // [0,200)
    let clip2 = insert_adjustment_clip(&state, &track, 200, 100).await; // [200,300)

    let args = serde_json::from_value(json!({
        "track_id": track,
        "range": { "start_ticks": 50, "end_ticks": 150 },
    }))
    .unwrap();
    let result = handlers::video::extract_edit(&state, args).await;
    assert_ok(&result, "extract_edit");

    // clip1's tail [150,200) becomes a fresh clip shifted left to [50,100);
    // clip2, entirely right of the range, shifts left by the full range
    // width (100) to [100,200).
    let clip2_after = get_clip_json(&state, &clip2).await;
    assert_eq!(clip2_after["start"].as_i64(), Some(100));
    assert_eq!(clip2_after["duration"].as_i64(), Some(100));

    // Track content shrank by the range width: span 300 -> 200, one extra
    // clip from the split tail (head, tail, clip2 = 3 total).
    let clips = list_clips_on_track(&state, &track).await;
    assert_eq!(clips.len(), 3);
    let max_end: i64 = clips
        .iter()
        .map(|c| c["start_ticks"].as_i64().unwrap() + c["duration_ticks"].as_i64().unwrap())
        .max()
        .unwrap();
    assert_eq!(max_end, 200);
}

// ─── link_clips / unlink_clips ──────────────────────────────────────────────

#[tokio::test]
async fn link_clips_groups_two_clips_and_unlink_clips_removes_just_one() {
    let state = test_state();
    let (seq, video_track) = seq_with_track(&state, "video").await;
    let audio_track_args = serde_json::from_value(json!({
        "sequence_id": seq,
        "kind": "audio",
    }))
    .unwrap();
    let audio_track_result = handlers::video::add_track(&state, audio_track_args).await;
    assert_ok(&audio_track_result, "add_track (audio)");
    let audio_track = tool_data(&audio_track_result)["track_id"]
        .as_str()
        .unwrap()
        .to_string();

    let clip_v = insert_adjustment_clip(&state, &video_track, 0, 100).await;
    let clip_a = insert_adjustment_clip(&state, &audio_track, 0, 100).await;

    // Not linked yet.
    let v_before = get_clip_json(&state, &clip_v).await;
    assert!(v_before.get("link_group").is_none());

    let link_args = serde_json::from_value(json!({
        "clip_id_a": clip_v,
        "clip_id_b": clip_a,
    }))
    .unwrap();
    let link_result = handlers::video::link_clips(&state, link_args).await;
    assert_ok(&link_result, "link_clips");

    let v_linked = get_clip_json(&state, &clip_v).await;
    let a_linked = get_clip_json(&state, &clip_a).await;
    let group_v = v_linked["link_group"].as_str().expect("V linked");
    let group_a = a_linked["link_group"].as_str().expect("A linked");
    assert_eq!(group_v, group_a, "both clips share one link group");

    // Unlink just the video clip — its former partner stays linked (to the
    // same group id, now a group of one).
    let unlink_args = serde_json::from_value(json!({ "clip_id": clip_v })).unwrap();
    let unlink_result = handlers::video::unlink_clips(&state, unlink_args).await;
    assert_ok(&unlink_result, "unlink_clips");

    let v_after = get_clip_json(&state, &clip_v).await;
    let a_after = get_clip_json(&state, &clip_a).await;
    assert!(v_after.get("link_group").is_none(), "V left the group");
    assert_eq!(
        a_after["link_group"].as_str(),
        Some(group_a),
        "A is still in its group"
    );
}

// ─── set_clip_prop: color_label tri-state ──────────────────────────────────

#[tokio::test]
async fn set_clip_prop_color_label_set_omit_and_clear_are_distinct() {
    let state = test_state();
    let (_seq, track) = seq_with_track(&state, "video").await;
    let clip = insert_adjustment_clip(&state, &track, 0, 100).await;

    // Unset by default.
    let initial = get_clip_json(&state, &clip).await;
    assert!(initial.get("color_label").is_none());

    // Set to 3.
    let set_args = serde_json::from_value(json!({
        "clip_id": clip,
        "color_label": 3,
    }))
    .unwrap();
    let set_result = handlers::video::set_clip_prop(&state, set_args).await;
    assert_ok(&set_result, "set_clip_prop (set label)");
    let after_set = get_clip_json(&state, &clip).await;
    assert_eq!(after_set["color_label"].as_i64(), Some(3));

    // Omitting the field entirely must leave the label unchanged (this is
    // the whole point of `deserialize_present`): rename without touching
    // color_label.
    let rename_args = serde_json::from_value(json!({
        "clip_id": clip,
        "name": "renamed",
    }))
    .unwrap();
    let rename_result = handlers::video::set_clip_prop(&state, rename_args).await;
    assert_ok(&rename_result, "set_clip_prop (omit label)");
    let after_rename = get_clip_json(&state, &clip).await;
    assert_eq!(
        after_rename["color_label"].as_i64(),
        Some(3),
        "label survives an omitted field"
    );
    assert_eq!(after_rename["name"].as_str(), Some("renamed"));

    // Explicit null clears it.
    let clear_args = serde_json::from_value(json!({
        "clip_id": clip,
        "color_label": null,
    }))
    .unwrap();
    let clear_result = handlers::video::set_clip_prop(&state, clear_args).await;
    assert_ok(&clear_result, "set_clip_prop (clear label)");
    let after_clear = get_clip_json(&state, &clip).await;
    assert!(
        after_clear.get("color_label").is_none(),
        "explicit null clears the label"
    );
}

/// Proves the real tool-call entry point is reachable from an out-of-crate
/// integration test (29 §3 / CAP-019). The acceptance-story harness relies on
/// `photonic_mcp::dispatch::dispatch_tool` being `pub`; this smoke test guards
/// against it silently re-narrowing to `pub(crate)`.
#[tokio::test]
async fn dispatch_tool_is_reachable_from_integration_tests() {
    let state = AppState::headless_for_test();
    let result = photonic_mcp::dispatch::dispatch_tool(
        &state,
        "create_sequence",
        json!({
            "name": "t",
            "frame_rate": { "num": 30, "den": 1 },
            "formats": [{ "name": "Main", "width": 1920, "height": 1080 }]
        }),
    )
    .await
    .expect("dispatch_tool returned a transport error");
    assert_ne!(
        result.is_error,
        Some(true),
        "create_sequence via dispatch_tool errored: {:?}",
        result.content
    );
}
