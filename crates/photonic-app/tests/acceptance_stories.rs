//! Acceptance-story harness (29 §3 / CAP-019).
//!
//! Each acceptance story is one `#[test]` driven as an MCP tool-call script and
//! separately replayed through the GUI's own edit fns; the two arms' resulting
//! timelines are compared for structural equality. That script-vs-GUI pair *is*
//! the CAP-019 gate (10 §9.1).
//!
//! Scope of what runs headlessly: the timeline-model half of AS-1 — the edit
//! script both arms can reproduce without a GPU or ffmpeg. The render/export/
//! frame-compare tail (auto-caption jobs, `export_sequence`, golden-frame parity
//! via `photonic_video::testing::frame_compare`, 9:16 export ffprobe) is GPU-
//! and ffmpeg-gated and additionally needs a `photonic-video` dev-dependency; it
//! is deferred (see the note in `support`). The structural parity below is the
//! part of the gate that executes today, and it drives the REAL out-of-crate
//! tool-call path (`photonic_mcp::dispatch::dispatch_tool`) that did not
//! previously exist.

mod support;

use photonic_core::timeline::time::TICKS_PER_SECOND;
use support::{canonicalize, run_mcp_arm, tool, Story, StoryOutcome};

/// One second and half a second, in ticks, at the sequence rate — used
/// identically by both arms so their clip geometry matches exactly.
const ONE_SEC: i64 = TICKS_PER_SECOND;
const HALF_SEC: i64 = TICKS_PER_SECOND / 2;

/// AS-1's structural edit script (the subset both arms reproduce headlessly):
/// create a 30 fps 16:9 sequence, add a video + audio track, lay two solid
/// clips end-to-end on the video track, split the first at its midpoint, then
/// add a 9:16 format and make it active (the CAP-012 reframe target).
///
/// Solid-color clips are used deliberately: they carry no media asset, so the
/// two arms need no shared import/probe path and the comparison stays exact.
fn as1_structural_story() -> Story {
    Story {
        name: "AS-1 social clip (structural parity)",
        steps: vec![
            tool(
                "create_sequence",
                serde_json::json!({
                    "name": "AS-1",
                    "frame_rate": { "num": 30, "den": 1 },
                    "formats": [{ "name": "16:9", "width": 1920, "height": 1080 }]
                }),
                Some("seq"),
            ),
            tool(
                "add_track",
                serde_json::json!({ "sequence_id": "@seq", "kind": "video" }),
                Some("track_v"),
            ),
            tool(
                "add_track",
                serde_json::json!({ "sequence_id": "@seq", "kind": "audio" }),
                Some("track_a"),
            ),
            tool(
                "insert_clip",
                serde_json::json!({
                    "track_id": "@track_v",
                    "source": { "kind": "solid_color", "color": "#ff0000" },
                    "start_ticks": 0,
                    "duration_ticks": ONE_SEC
                }),
                Some("clip_a"),
            ),
            tool(
                "insert_clip",
                serde_json::json!({
                    "track_id": "@track_v",
                    "source": { "kind": "solid_color", "color": "#00ff00" },
                    "start_ticks": ONE_SEC,
                    "duration_ticks": ONE_SEC
                }),
                Some("clip_b"),
            ),
            tool(
                "split_clip",
                serde_json::json!({ "clip_id": "@clip_a", "at_ticks": HALF_SEC }),
                None,
            ),
            tool(
                "set_sequence_format",
                serde_json::json!({
                    "sequence_id": "@seq",
                    "op": "add",
                    "format": { "name": "9:16", "width": 1080, "height": 1920 }
                }),
                None,
            ),
            tool(
                "set_active_format",
                serde_json::json!({ "sequence_id": "@seq", "format_index": 1 }),
                None,
            ),
        ],
    }
}

/// AS-1 replayed through the GUI's own edit fns — the SAME functions the widgets
/// call (`ops_bridge` / `interact`), not a reimplementation, per the standard
/// set by `photonic-gui/tests/timeline_recovery.rs`. Produces the timeline the
/// GUI arm ends up with.
fn as1_gui_arm() -> StoryOutcome {
    use photonic_core::timeline::{Clip, ClipSource, FrameRate, Tick, TrackKind};
    use photonic_core::{Color, CommandHistory, Document};
    use photonic_gui::app::timeline::{interact, ops_bridge};

    let mut doc = Document::new("AS-1", 1920.0, 1080.0);
    let mut hist = CommandHistory::new(200);

    let seq = ops_bridge::ensure_project_and_sequence(&mut doc, &mut hist, FrameRate::FPS_30)
        .expect("GUI arm: sequence created");
    ops_bridge::add_track(&mut doc, &mut hist, seq, TrackKind::Video);
    ops_bridge::add_track(&mut doc, &mut hist, seq, TrackKind::Audio);

    let track_v = first_track_id(&doc, seq, TrackKind::Video);

    // Two solid clips laid end-to-end. `do_insert_edit` (Premiere `,`) into
    // empty space downstream is identical to the MCP arm's plain `insert_clip`
    // (both route through `ops::insert_edit`/`ops::insert_clip` with no clip to
    // ripple), so the resulting geometry matches.
    let clip_a = Clip::new(
        ClipSource::SolidColor {
            color: Color::from_hex("#ff0000").expect("valid hex"),
        },
        Tick(0),
        Tick(ONE_SEC),
    );
    let clip_a_id = clip_a.id;
    assert!(
        interact::do_insert_edit(&mut doc, &mut hist, seq, track_v, Tick(0), clip_a),
        "GUI arm: insert clip A"
    );
    let clip_b = Clip::new(
        ClipSource::SolidColor {
            color: Color::from_hex("#00ff00").expect("valid hex"),
        },
        Tick(ONE_SEC),
        Tick(ONE_SEC),
    );
    assert!(
        interact::do_insert_edit(&mut doc, &mut hist, seq, track_v, Tick(ONE_SEC), clip_b),
        "GUI arm: insert clip B"
    );

    ops_bridge::split(&mut doc, &mut hist, seq, track_v, clip_a_id, Tick(HALF_SEC));
    ops_bridge::switch_to_aspect(&mut hist, &mut doc, seq, "9:16", 1080, 1920);

    let project = doc.timeline.clone().expect("GUI arm: timeline project");
    StoryOutcome {
        project,
        exports: Vec::new(),
    }
}

/// The id of the first track of `kind` in `seq`.
fn first_track_id(
    doc: &photonic_core::Document,
    seq: photonic_core::timeline::SequenceId,
    kind: photonic_core::timeline::TrackKind,
) -> photonic_core::timeline::TrackId {
    let project = doc.timeline.as_ref().expect("timeline");
    let sequence = project.sequences.get(&seq).expect("sequence");
    let lane = if kind.is_visual() {
        &sequence.video_tracks
    } else {
        &sequence.audio_tracks
    };
    lane.first().expect("a track of the requested kind").id
}

/// The CAP-019 gate for AS-1's structural half: the MCP-scripted timeline and
/// the GUI-driven timeline must agree.
#[tokio::test]
async fn as1_social_clip_mcp_and_gui_arms_agree() {
    let story = as1_structural_story();
    let mcp = run_mcp_arm(&story).await;
    let gui = as1_gui_arm();

    // Guard against a vacuous equality: prove both arms actually built the
    // non-trivial timeline the script describes before comparing them.
    let mcp_video = mcp
        .project
        .sequences
        .values()
        .flat_map(|s| s.video_tracks.iter())
        .next()
        .expect("MCP arm: a video track exists");
    assert_eq!(
        mcp_video.clips.len(),
        3,
        "MCP arm: two inserted clips + one from the split = 3 on the video track"
    );
    let mcp_seq = mcp.project.sequences.values().next().unwrap();
    assert_eq!(mcp_seq.formats.len(), 2, "MCP arm: 16:9 + 9:16 formats");
    assert_eq!(mcp_seq.active_format, 1, "MCP arm: 9:16 is active");
    assert_eq!(
        (mcp_seq.formats[1].width, mcp_seq.formats[1].height),
        (1080, 1920),
        "MCP arm: the active format is portrait 9:16"
    );
    assert_eq!(
        mcp.exports.len(),
        0,
        "export tail is GPU/ffmpeg-gated and deferred"
    );

    // The gate itself.
    assert_eq!(
        canonicalize(&mcp.project),
        canonicalize(&gui.project),
        "AS-1 CAP-019: the MCP-scripted timeline and the GUI-driven timeline diverge.\n\
         MCP: {:#}\nGUI: {:#}",
        canonicalize(&mcp.project),
        canonicalize(&gui.project),
    );
}
