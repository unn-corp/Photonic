//! Shared support for the acceptance-story harness (29 §3 / CAP-019).
//!
//! `photonic-app` is the only package that depends on BOTH `photonic-gui` and
//! `photonic-mcp` (`Cargo.toml`), so it is the only place the CAP-019 gate —
//! "each acceptance story is one `#[test]` driven as an MCP tool-call script …
//! separately compared against a GUI-driven run of the same story" — can live.
//! This module carries the pieces shared across stories:
//!
//! * [`Story`] / [`Step`] — a declarative tool-call script.
//! * [`run_mcp_arm`] — drives a story through the REAL out-of-crate tool-call
//!   entry point `photonic_mcp::dispatch::dispatch_tool` (made `pub` for exactly
//!   this, see `photonic-mcp/src/lib.rs`), threading returned ids forward.
//! * [`canonicalize`] — rewrites a [`TimelineProject`] to an id/name-independent
//!   value so the MCP-arm and GUI-arm projects can be compared for structural
//!   equality even though each arm mints its own fresh UUIDs.
//!
//! The GUI arm itself is story-specific (it calls the now-`pub` `ops_bridge` /
//! `interact` / `reframe` edit fns directly — the same fns the widgets call, per
//! the standard set by `photonic-gui/tests/timeline_recovery.rs`) and lives next
//! to each story in `acceptance_stories.rs`.
//!
//! Note: the render/export/frame-compare tail of each story (auto-caption jobs,
//! `export_sequence`, golden-frame parity via `photonic_video::testing::
//! frame_compare`) is GPU- and ffmpeg-gated and additionally needs a
//! `photonic-video` dev-dependency; it is deferred. What runs here headlessly is
//! the timeline-model script-vs-GUI parity — the structural half of the gate.

// Integration-test support modules are compiled once per test binary; not every
// item is used by every story yet (PollJob, exports), so silence the churn.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use photonic_core::timeline::{TimelineProject, Track};
use photonic_mcp::protocol::{ContentItem, ToolResult};
use photonic_mcp::server::AppState;
use serde_json::{json, Value};

/// One acceptance story: a named, ordered list of steps.
pub struct Story {
    pub name: &'static str,
    pub steps: Vec<Step>,
}

/// A single step in a story script.
pub enum Step {
    /// Call MCP tool `name` with `args`. Any string value of the form `"@key"`
    /// inside `args` is replaced, before dispatch, with the id previously bound
    /// under `key` (see `bind`). After the call, if `bind` is `Some(key)`, the
    /// first id field in the tool's JSON payload is stored under `key`.
    Tool {
        name: &'static str,
        args: Value,
        bind: Option<&'static str>,
    },
    /// Poll `get_job_status` for the job bound under `job` until it reaches a
    /// terminal state or `timeout` elapses. Headless (no GPU) the video engine
    /// reports `EngineUnavailable`, so this is a tolerant best-effort step —
    /// reserved for the engine-dependent story tail.
    PollJob {
        job: &'static str,
        timeout: Duration,
    },
}

/// The observable result of running one arm of a story.
pub struct StoryOutcome {
    pub project: TimelineProject,
    /// Files an `export_sequence` step produced. Empty headlessly (export is
    /// GPU/ffmpeg-gated).
    pub exports: Vec<PathBuf>,
}

/// Convenience constructor for a plain tool step.
pub fn tool(name: &'static str, args: Value, bind: Option<&'static str>) -> Step {
    Step::Tool { name, args, bind }
}

/// Pulls the JSON payload a handler attached via `ToolResult::with_data` — the
/// first `ContentItem::Text` that parses as JSON. Mirrors the extractor in
/// `photonic-mcp/tests/mcp_parity.rs` so the two harnesses read results the
/// same way.
pub fn tool_data(result: &ToolResult) -> Value {
    for item in &result.content {
        if let ContentItem::Text { text } = item {
            if let Ok(v) = serde_json::from_str::<Value>(text) {
                return v;
            }
        }
    }
    Value::Null
}

/// The id fields a step may bind, in priority order (a split returns
/// `new_clip_id`, an import returns `asset_id`, etc.).
const ID_FIELDS: &[&str] = &[
    "clip_id",
    "new_clip_id",
    "track_id",
    "sequence_id",
    "asset_id",
    "job_id",
];

fn extract_id(data: &Value) -> Option<Value> {
    for field in ID_FIELDS {
        if let Some(v) = data.get(field) {
            if !v.is_null() {
                return Some(v.clone());
            }
        }
    }
    None
}

/// Recursively replaces every `"@key"` string in `args` with the bound value.
fn resolve_binds(args: &Value, binds: &HashMap<&'static str, Value>) -> Value {
    match args {
        Value::String(s) => {
            if let Some(key) = s.strip_prefix('@') {
                binds
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| panic!("story referenced unbound id '@{key}'"))
            } else {
                args.clone()
            }
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| resolve_binds(v, binds)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), resolve_binds(v, binds)))
                .collect(),
        ),
        _ => args.clone(),
    }
}

/// Drives `story` through the real `dispatch_tool` entry point against a fresh
/// headless `AppState`, threading bound ids forward, and returns the resulting
/// timeline. Panics with step context on any tool error — the caller's story is
/// expected to be a valid script.
pub async fn run_mcp_arm(story: &Story) -> StoryOutcome {
    let state = AppState::headless_for_test();
    let mut binds: HashMap<&'static str, Value> = HashMap::new();

    for (i, step) in story.steps.iter().enumerate() {
        match step {
            Step::Tool { name, args, bind } => {
                let resolved = resolve_binds(args, &binds);
                let result = photonic_mcp::dispatch::dispatch_tool(&state, name, resolved)
                    .await
                    .unwrap_or_else(|e| {
                        panic!("[{}] step {i} ({name}) transport error: {e}", story.name)
                    });
                assert_ne!(
                    result.is_error,
                    Some(true),
                    "[{}] step {i} ({name}) reported a tool error: {:?}",
                    story.name,
                    result.content
                );
                if let Some(key) = bind {
                    let data = tool_data(&result);
                    let id = extract_id(&data).unwrap_or_else(|| {
                        panic!(
                            "[{}] step {i} ({name}) is bound to '{key}' but returned no id field: {data}",
                            story.name
                        )
                    });
                    binds.insert(key, id);
                }
            }
            Step::PollJob { .. } => {
                // Headless: the video engine is unavailable, so there is no job
                // to reach a terminal state. Engine-dependent stories skip this.
            }
        }
    }

    let doc = state.document.lock().await;
    let project = doc
        .timeline
        .clone()
        .unwrap_or_else(|| panic!("[{}] story produced no timeline project", story.name));
    StoryOutcome {
        project,
        exports: Vec::new(),
    }
}

/// Rewrites a [`TimelineProject`] into a value that depends only on its timeline
/// *structure*, not on the per-arm-fresh UUIDs or incidental display strings.
///
/// The two arms mint their own `SequenceId`/`TrackId`/`ClipId`s and pick their
/// own sequence/track/format *names* (the GUI's `ensure_project_and_sequence`
/// hard-codes `"Sequence 1"` / a `"16:9"` default format; the MCP arm names them
/// from tool args), so a byte comparison of serialized projects would never
/// match. This normalizer keeps only the load-bearing structure — the same
/// class of stripping the brief mandates for ids (→ ordinals) and paths (→ file
/// names), extended to those incidental names — so the comparison asserts the
/// two arms built the *same edit*, which is the CAP-019 intent.
///
/// Traversal is deterministic: sequences in `sequence_order` (else id-sorted),
/// then `video_tracks` then `audio_tracks` in vector order, then clips sorted by
/// `start`.
pub fn canonicalize(p: &TimelineProject) -> Value {
    let seq_ids = if p.sequence_order.is_empty() {
        let mut v: Vec<_> = p.sequences.keys().copied().collect();
        v.sort_by_key(|id| id.to_string());
        v
    } else {
        p.sequence_order.clone()
    };

    let active_ordinal = p
        .active_sequence
        .and_then(|a| seq_ids.iter().position(|s| *s == a));

    let sequences: Vec<Value> = seq_ids
        .iter()
        .enumerate()
        .filter_map(|(ordinal, sid)| {
            let seq = p.sequences.get(sid)?;
            Some(json!({
                "ordinal": ordinal,
                "frame_rate": { "num": seq.frame_rate.num, "den": seq.frame_rate.den },
                // Format identity is (width, height) + order; names are incidental.
                "formats": seq
                    .formats
                    .iter()
                    .map(|f| json!([f.width, f.height]))
                    .collect::<Vec<_>>(),
                "active_format": seq.active_format,
                "video_tracks": seq.video_tracks.iter().map(canon_track).collect::<Vec<_>>(),
                "audio_tracks": seq.audio_tracks.iter().map(canon_track).collect::<Vec<_>>(),
                "caption_track_count": seq.caption_tracks.len(),
                "marker_count": seq.markers.len(),
            }))
        })
        .collect();

    json!({
        "active_sequence_ordinal": active_ordinal,
        "sequences": sequences,
    })
}

fn canon_track(t: &Track) -> Value {
    let mut clips: Vec<_> = t.clips.iter().collect();
    clips.sort_by_key(|c| c.start.0);
    json!({
        "kind": format!("{:?}", t.kind),
        "clips": clips.iter().map(|c| canon_clip(c)).collect::<Vec<_>>(),
    })
}

fn canon_clip(c: &photonic_core::timeline::Clip) -> Value {
    let mut reframe_formats: Vec<usize> = c.reframe.keys().copied().collect();
    reframe_formats.sort_unstable();
    json!({
        "start": c.start.0,
        "duration": c.duration.0,
        "source_in": c.source_in.0,
        "source": canon_source(&c.source),
        "graded": c.grade.is_some(),
        "effect_count": c.effects.len(),
        "reframe_formats": reframe_formats,
    })
}

/// Normalizes a clip source to an id-independent shape. `SolidColor` (the source
/// the headless structural stories use) carries no id, so it is exact; asset /
/// nested-sequence variants keep only their tag here (an id→ordinal rewrite
/// would be needed once a story imports real media).
fn canon_source(s: &photonic_core::timeline::ClipSource) -> Value {
    use photonic_core::timeline::ClipSource;
    match s {
        ClipSource::SolidColor { color } => json!({
            "kind": "solid_color",
            "rgba": [color.r, color.g, color.b, color.a],
        }),
        ClipSource::Asset { .. } => json!({ "kind": "asset" }),
        ClipSource::Vector { .. } => json!({ "kind": "vector" }),
        ClipSource::NestedSequence { .. } => json!({ "kind": "nested_sequence" }),
        ClipSource::Adjustment => json!({ "kind": "adjustment" }),
        ClipSource::Text { .. } => json!({ "kind": "text" }),
        ClipSource::Unknown(map) => json!({ "kind": "unknown", "tags": map.keys().count() }),
    }
}
