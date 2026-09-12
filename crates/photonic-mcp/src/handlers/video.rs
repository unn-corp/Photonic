//! Video-domain MCP tool handlers (10-mcp-tools.md, P2 slice: timeline-EDIT
//! tools only — sequence/track/clip/effects/keyframes/media). Every mutating
//! handler here follows design rule 1 (10 §1): deserialize args, resolve
//! addressing (which sequence/track a clip/track id lives under — a plain
//! project scan, not edit logic), call the matching pure fn in
//! `photonic_core::timeline::ops`, wrap the returned `TimelineCmd` in
//! `Command::Timeline` (or `Command::Batch` for multi-command ops), and
//! execute via `history.execute_discrete` (design rule 4: one command per
//! call). Lock order is always document before history (design rule 7).
//!
//! ## P3 engine slice (this file, lower half)
//! Playback transport (10 §3.13), `render_frame_at` (10 §4), real
//! `probe_media`/`transcode_media`, and the export/job tools (10 §3.15, §6)
//! are implemented against the `photonic_video::VideoEngine` facade via the
//! lazy [`EngineBridge`] (`handlers/video_jobs.rs`). Captions/tts/grade
//! scopes remain P4+.
//!
//! ## Resolved gaps (P2 top-up, photonic-core commit ab7557f)
//! `set_work_range`/`add_marker`/`remove_marker`/`list_markers`,
//! `ripple_edit` (via `ops::ripple_trim`), `move_clip`'s cross-track path
//! (via `ops::move_clip_to_track`), and media bins (`MediaAsset.bin` +
//! `ops::{create_bin,remove_bin,assign_asset_bin}`) all landed in core and
//! are implemented below.
//!
//! ## Remaining gap
//! - `import_media`'s `content_hash` uses a stopgap `DefaultHasher` (SipHash)
//!   digest over head+tail+len, not the `xxh3` the core doc comment
//!   (media.rs:6) describes as the relink identity. `probe_media` (P3, below)
//!   refreshes it with the real `photonic_video::media::probe::content_hash`
//!   xxh3 digest.

use crate::handlers::video_jobs::{set_job_status, EngineBridge, JobStatus};
use crate::protocol::*;
use crate::server::AppState;
use base64::{engine::general_purpose, Engine as _};
use photonic_core::history::Command;
use photonic_core::timeline::{
    effect_preset, ops, AnimTarget, AssetId, AssetKind, Clip, ClipEffect, ClipId, ClipSource,
    ClipTiming, EditError, FormatOp, FrameRate, Keyframe, Marker, MarkerCategory, MarkerId,
    PropPath, ProxyRef, ProxyStatus, Ratio, Sequence, SequenceId, SpeedMap, Tick, TimelineCmd,
    TimelineProject, Track, TrackId, TrackSettings, Transition, TICKS_PER_SECOND,
};
use photonic_core::Color;
use photonic_video::export::convert as export_convert;
use photonic_video::export::presets as export_presets;
use photonic_video::graph::eval::read_texture_rgba16f;
use photonic_video::graph::ScopeTapPoint;
use photonic_video::media::ffmpeg_locate;
use photonic_video::media::probe as video_probe;
use photonic_video::media::proxy as video_proxy;
use photonic_video::{EngineCmd, EngineSession, PreviewQuality, ProxyMode};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
#[cfg(test)]
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

// P4+ slice (captions/tts/grade/graph/audio/titles) type imports.
use photonic_core::timeline::{
    graph_ops, AudioCmd, AudioFade, AudioFxUnit, CaptionCmd, CaptionCue, CaptionStyle,
    CaptionTrack, CaptionWord, ClipAudio, ClipAudioParams, CueId, FadeEdge, FadeShape, FxOwner,
    Grade, GradeOp, GradeOpKind, GradeOpParams, GraphNode, GraphNodeParams, GraphOp, InPort,
    LoudnessTarget, MasterBusParams, NodePos, OutPort, StyleTarget, TrackAudio, TrackAudioParams,
};
use photonic_video::captions;

// G-11/G-12 (17-nle-parity-round2.md) types: `SpeedKey`/`TextClipContent` live
// in the `clip` submodule but aren't in the curated `timeline::{..}`
// re-export yet (core-timeline's territory, not this story's) — reached via
// the fully-qualified submodule path instead.
use photonic_core::timeline::clip::{SpeedKey, TextClipContent};
// K-B1/K-B2 effect-stack scope — same "not in the curated re-export yet" note
// as `SpeedKey` above.
use photonic_core::timeline::commands::VfxOwner;

// ─── Shared helpers ──────────────────────────────────────────────────────────

/// A structured-error `ToolResult` per 10 §8's taxonomy.
fn err_code(code: &str, msg: impl Into<String>) -> ToolResult {
    ToolResult::error(msg).with_data(json!({ "error_code": code }))
}

/// Design rule 3: `at_ticks` > `at_tc` > `at_seconds`. `at_tc` requires a
/// resolvable sequence (frame rate) — `MissingSequenceContext` otherwise.
pub(crate) fn resolve_tick(
    ticks: Option<i64>,
    tc: Option<&str>,
    seconds: Option<f64>,
    frame_rate: Option<FrameRate>,
) -> Result<Tick, ToolResult> {
    if let Some(t) = ticks {
        return Ok(Tick(t));
    }
    if let Some(tc) = tc {
        let fr = frame_rate.ok_or_else(|| {
            err_code(
                "MissingSequenceContext",
                "at_tc given without a resolvable sequence context",
            )
        })?;
        return parse_timecode(tc, fr).ok_or_else(|| {
            ToolResult::error(format!(
                "invalid timecode {tc:?} — expected HH:MM:SS:FF or HH:MM:SS;FF"
            ))
        });
    }
    if let Some(s) = seconds {
        return Ok(Tick((s * TICKS_PER_SECOND as f64).round() as i64));
    }
    Err(ToolResult::error(
        "missing time value — supply one of *_ticks / *_tc / *_seconds",
    ))
}

/// `HH:MM:SS:FF` (non-drop) or `HH:MM:SS;FF` (drop-frame, K-A12).
///
/// Delegates to [`photonic_core::timeline::Timecode::parse_to_tick`]: `;` is
/// real SMPTE drop-frame compensation on 29.97/59.94 (not a synonym for `:`).
/// Non-drop always uses the frame-grid nominal-fps count so 1001 rates land
/// on exact frame boundaries (spec 10 §1.3, 01 §1).
fn parse_timecode(tc: &str, fr: FrameRate) -> Option<Tick> {
    photonic_core::timeline::Timecode::parse_to_tick(tc, fr)
}

/// Which `(SequenceId, TrackId)` owns a clip — a plain project scan, not edit
/// logic (mirrors `commands.rs::find_clip_mut`'s search, read-only here).
fn locate_clip(p: &TimelineProject, clip: ClipId) -> Option<(SequenceId, TrackId)> {
    for (sid, s) in &p.sequences {
        for t in s.video_tracks.iter().chain(s.audio_tracks.iter()) {
            if t.clips.iter().any(|c| c.id == clip) {
                return Some((*sid, t.id));
            }
        }
    }
    None
}

// ── Linked clip expansion (INTEG-MCP-LINK, 14 §M-2) ─────────────────────────
//
// The GUI (`photonic_gui::app::timeline::ops_bridge`) expands a single-clip
// move/delete intent across `ops::clips_in_link_group` so linked A/V pairs
// move and delete as a unit. MCP has no dependency on the GUI crate, so this
// is a parallel implementation over the same core primitive (`ops::*`),
// mirroring `ops_bridge::link_partners` / `ops::move_linked_clip` /
// `expand_link_group_delete` / `commit_group` field-for-field — an
// agent-driven `move_clip`/`remove_clip` must leave a linked partner in the
// same state a GUI drag/delete would. Trim intentionally does NOT propagate
// (matches the GUI: "trim is independent, move is linked"), nor does
// `ripple_delete` (Sync Lock, 14 §M-9, isn't built yet).

/// Every OTHER clip in `clip`'s link group within `seq`, paired with the
/// track that currently owns it. Empty when `clip` isn't linked or has no
/// partners.
fn link_partners(
    p: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    clip: ClipId,
) -> Vec<(TrackId, ClipId)> {
    let Some(s) = p.sequences.get(&seq) else {
        return Vec::new();
    };
    let Some(group) = s
        .track(track)
        .and_then(|t| t.clips.iter().find(|c| c.id == clip))
        .and_then(|c| c.link_group)
    else {
        return Vec::new();
    };
    ops::clips_in_link_group(p, group)
        .into_iter()
        .filter(|id| *id != clip)
        .filter_map(|id| {
            s.tracks()
                .find(|t| t.clips.iter().any(|c| c.id == id))
                .map(|t| (t.id, id))
        })
        .collect()
}

/// Expand a delete (`remove_clip`'s "lift" semantics — leaves a gap, no
/// ripple) to every linked partner of `clip` within `seq`.
fn expand_link_group_delete(
    p: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    clip: ClipId,
) -> Vec<TimelineCmd> {
    link_partners(p, seq, track, clip)
        .into_iter()
        .filter_map(|(ptrack, pclip)| ops::remove_clip(p, seq, ptrack, pclip).ok())
        .collect()
}

/// Which `SequenceId` owns a track.
fn locate_track(p: &TimelineProject, track: TrackId) -> Option<SequenceId> {
    p.sequences
        .iter()
        .find(|(_, s)| s.track(track).is_some())
        .map(|(sid, _)| *sid)
}

/// Which `SequenceId` owns a marker.
fn locate_marker(p: &TimelineProject, marker: MarkerId) -> Option<SequenceId> {
    p.sequences
        .iter()
        .find(|(_, s)| s.markers.iter().any(|m| m.id == marker))
        .map(|(sid, _)| *sid)
}

/// Resolve a bin by exact name.
fn find_bin_by_name<'a>(
    p: &'a TimelineProject,
    name: &str,
) -> Option<&'a photonic_core::timeline::MediaBin> {
    p.media.bins.iter().find(|b| b.name == name)
}

fn map_edit_error(e: EditError) -> ToolResult {
    match e {
        EditError::NoProject => ToolResult::error("no timeline project"),
        EditError::NoSequence(id) => ToolResult::error(format!("sequence {id} not found")),
        EditError::NoTrack(id) => ToolResult::error(format!("track {id} not found")),
        EditError::NoClip(id) => ToolResult::error(format!("clip {id} not found")),
        EditError::NoAsset(id) => ToolResult::error(format!("asset {id} not found")),
        EditError::Overlap => ToolResult::error("edit would overlap another clip on the track"),
        EditError::NonPositiveDuration => {
            err_code("TickOutOfRange", "resulting clip duration must be > 0")
        }
        EditError::InvalidSplit => err_code(
            "TickOutOfRange",
            "split point must be strictly inside the clip",
        ),
        EditError::IndexOutOfRange => ToolResult::error("index out of range"),
        EditError::SequenceCycle => {
            err_code("CycleDetected", "edit would create a nested-sequence cycle")
        }
        other => ToolResult::error(format!("{other}")),
    }
}

/// Read the clip currently on `(seq, track)` — callers have already resolved
/// addressing via `locate_clip`.
fn find_clip(
    project: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    clip: ClipId,
) -> Option<&Clip> {
    project
        .sequences
        .get(&seq)?
        .track(track)?
        .clips
        .iter()
        .find(|c| c.id == clip)
}

// ─── Sequence (10 §3.2) ─────────────────────────────────────────────────────

pub async fn create_sequence(state: &AppState, args: CreateSequenceArgs) -> ToolResult {
    tracing::debug!("tool: create_sequence {}", args.name);
    if args.formats.is_empty() {
        return ToolResult::error("formats must have at least one entry");
    }
    let first = &args.formats[0];
    let mut seq = photonic_core::timeline::Sequence::new(
        &args.name,
        args.frame_rate,
        first.width,
        first.height,
    );
    seq.formats = args.formats;
    let seq_id = seq.id;

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let needs_project = doc.timeline.is_none();
    let mut cmds = Vec::new();
    if needs_project {
        cmds.push(Command::Timeline(ops::create_project()));
    }
    cmds.push(Command::Timeline(ops::add_sequence(seq)));
    let cmd = if cmds.len() == 1 {
        cmds.into_iter().next().unwrap()
    } else {
        Command::Batch(cmds)
    };
    history.execute_discrete(cmd, &mut doc);

    ToolResult::text(format!("Created sequence \"{}\"", args.name))
        .with_data(json!({ "sequence_id": seq_id }))
}

pub async fn delete_sequence(state: &AppState, args: DeleteSequenceArgs) -> ToolResult {
    tracing::debug!("tool: delete_sequence {}", args.sequence_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    // Dangling-ref guard (10 §3.2): refuse if any clip elsewhere nests this
    // sequence. `ops::remove_sequence` does not check this itself (core
    // gap); this is a read-only precondition scan, not edit logic — mirrors
    // `ops::nests_into`'s existing cycle check used by `insert_clip`.
    let referenced = project.sequences.values().any(|s| {
        s.video_tracks.iter().chain(s.audio_tracks.iter()).any(|t| {
            t.clips.iter().any(|c| {
                matches!(&c.source, ClipSource::NestedSequence { sequence } if *sequence == args.sequence_id)
            })
        })
    });
    if referenced {
        return err_code(
            "CycleDetected",
            format!(
                "sequence {} is referenced by a NestedSequence clip elsewhere — remove that clip first",
                args.sequence_id
            ),
        );
    }
    match ops::remove_sequence(project, args.sequence_id) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Deleted sequence")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn list_sequences(state: &AppState, _args: ListSequencesArgs) -> ToolResult {
    tracing::debug!("tool: list_sequences");
    let doc = state.document.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::text("No timeline project yet").with_data(json!({ "sequences": [] }));
    };
    let list: Vec<_> = project
        .sequence_order
        .iter()
        .filter_map(|id| project.sequences.get(id))
        .map(|s| {
            json!({
                "sequence_id": s.id,
                "name": s.name,
                "frame_rate": s.frame_rate,
                "formats": s.formats,
                "active_format": s.active_format,
                "video_track_count": s.video_tracks.len(),
                "audio_track_count": s.audio_tracks.len(),
                "is_active": project.active_sequence == Some(s.id),
            })
        })
        .collect();
    ToolResult::text(format!("{} sequence(s)", list.len())).with_data(json!({ "sequences": list }))
}

pub async fn set_active_sequence(state: &AppState, args: SetActiveSequenceArgs) -> ToolResult {
    tracing::debug!("tool: set_active_sequence");
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    if let Some(id) = args.sequence_id {
        if !project.sequences.contains_key(&id) {
            return ToolResult::error(format!("sequence {id} not found"));
        }
    }
    let cmd = ops::set_active_sequence(project, args.sequence_id);
    history.execute_discrete(Command::Timeline(cmd), &mut doc);
    ToolResult::text("Active sequence updated")
}

pub async fn set_sequence_format(state: &AppState, args: SetSequenceFormatArgs) -> ToolResult {
    tracing::debug!("tool: set_sequence_format {}", args.sequence_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq) = project.sequences.get(&args.sequence_id) else {
        return ToolResult::error(format!("sequence {} not found", args.sequence_id));
    };
    let op = match args.op {
        FormatOpKind::Add => {
            let Some(format) = args.format else {
                return ToolResult::error("format is required for op=add");
            };
            FormatOp::Add { format }
        }
        FormatOpKind::Update => {
            let (Some(idx), Some(new)) = (args.format_index, args.format) else {
                return ToolResult::error("format_index and format are required for op=update");
            };
            let Some(old) = seq.formats.get(idx).cloned() else {
                return ToolResult::error(format!("format_index {idx} out of range"));
            };
            FormatOp::Update {
                index: idx,
                old,
                new,
            }
        }
        FormatOpKind::Remove => {
            let Some(idx) = args.format_index else {
                return ToolResult::error("format_index is required for op=remove");
            };
            if seq.formats.len() <= 1 {
                return ToolResult::error(
                    "cannot remove the last format — a sequence needs at least one",
                );
            }
            let Some(format) = seq.formats.get(idx).cloned() else {
                return ToolResult::error(format!("format_index {idx} out of range"));
            };
            FormatOp::Remove { index: idx, format }
        }
    };
    let cmd = ops::set_sequence_format(args.sequence_id, op);
    history.execute_discrete(Command::Timeline(cmd), &mut doc);
    ToolResult::text("Sequence format updated")
}

pub async fn set_active_format(state: &AppState, args: SetActiveFormatArgs) -> ToolResult {
    tracing::debug!("tool: set_active_format {}", args.sequence_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    match ops::set_active_format(project, args.sequence_id, args.format_index) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Active format updated")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn set_work_range(state: &AppState, args: SetWorkRangeArgs) -> ToolResult {
    tracing::debug!("tool: set_work_range {}", args.sequence_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq) = project.sequences.get(&args.sequence_id) else {
        return ToolResult::error(format!("sequence {} not found", args.sequence_id));
    };
    let fr = seq.frame_rate;
    let new_range = match args.range {
        None => None,
        Some(r) => {
            let start = match resolve_tick(
                r.start_ticks,
                r.start_tc.as_deref(),
                r.start_seconds,
                Some(fr),
            ) {
                Ok(t) => t,
                Err(e) => return e,
            };
            let end = match resolve_tick(r.end_ticks, r.end_tc.as_deref(), r.end_seconds, Some(fr))
            {
                Ok(t) => t,
                Err(e) => return e,
            };
            if end <= start {
                return err_code("TickOutOfRange", "work range end must be after start");
            }
            Some((start, end))
        }
    };
    match ops::set_work_range(project, args.sequence_id, new_range) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Work range updated")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn add_marker(state: &AppState, args: AddMarkerArgs) -> ToolResult {
    tracing::debug!("tool: add_marker {}", args.sequence_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq) = project.sequences.get(&args.sequence_id) else {
        return ToolResult::error(format!("sequence {} not found", args.sequence_id));
    };
    let at = match resolve_tick(
        args.at_ticks,
        args.at_tc.as_deref(),
        args.at_seconds,
        Some(seq.frame_rate),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let color = match args.color {
        Some(hex) => match Color::from_hex(&hex) {
            Some(c) => Some(c),
            None => {
                return ToolResult::error(format!(
                    "invalid color {hex:?} — expected #rrggbb or #rrggbbaa"
                ))
            }
        },
        None => None,
    };
    let duration = match resolve_marker_duration(
        at,
        args.duration_ticks,
        args.duration_seconds,
        args.end_tc.as_deref(),
        Some(seq.frame_rate),
    ) {
        Ok(d) => d,
        Err(e) => return e,
    };
    if let Some(cat) = args.category_id {
        if project.marker_category(cat).is_none() {
            return ToolResult::error(format!("marker category {cat} not found"));
        }
    }
    let mut marker = Marker::new(at, args.name.unwrap_or_default());
    marker.color = color;
    marker.duration = duration;
    marker.category = args.category_id;
    if let Some(note) = args.note {
        marker.note = note;
    }
    let marker_id = marker.id;
    match ops::add_marker(project, args.sequence_id, marker) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Added marker").with_data(json!({ "marker_id": marker_id }))
        }
        Err(e) => map_edit_error(e),
    }
}

/// A marker's `duration` from any of the three spellings: an explicit tick or
/// second count, or an `end_tc` the duration is derived from (`end - at`).
/// Missing/none = `0`, i.e. a point marker (35 §1: `duration` is never
/// `Option`). A negative result is refused rather than silently clamped —
/// `end_tc` before `at` is a caller mistake, not an intent to make a point
/// marker.
fn resolve_marker_duration(
    at: Tick,
    ticks: Option<i64>,
    seconds: Option<f64>,
    end_tc: Option<&str>,
    frame_rate: Option<FrameRate>,
) -> Result<Tick, ToolResult> {
    let d = if let Some(t) = ticks {
        Tick(t)
    } else if let Some(s) = seconds {
        Tick((s * TICKS_PER_SECOND as f64).round() as i64)
    } else if let Some(tc) = end_tc {
        let end = resolve_tick(None, Some(tc), None, frame_rate)?;
        end - at
    } else {
        Tick::ZERO
    };
    if d.0 < 0 {
        return Err(err_code(
            "TickOutOfRange",
            "marker duration must be >= 0 (0 = point marker)",
        ));
    }
    Ok(d)
}

/// Apply the optional edit fields shared by `set_marker` / `set_clip_marker`
/// onto a cloned marker. Returns the error `ToolResult` for a bad value.
fn apply_marker_edits(
    m: &mut Marker,
    args: &SetMarkerArgs,
    frame_rate: Option<FrameRate>,
) -> Result<(), ToolResult> {
    if args.at_ticks.is_some() || args.at_tc.is_some() || args.at_seconds.is_some() {
        m.at = resolve_tick(
            args.at_ticks,
            args.at_tc.as_deref(),
            args.at_seconds,
            frame_rate,
        )?;
    }
    if args.duration_ticks.is_some() || args.duration_seconds.is_some() || args.end_tc.is_some() {
        m.duration = resolve_marker_duration(
            m.at,
            args.duration_ticks,
            args.duration_seconds,
            args.end_tc.as_deref(),
            frame_rate,
        )?;
    }
    if let Some(name) = args.name.clone() {
        m.name = name;
    }
    if let Some(note) = args.note.clone() {
        m.note = note;
    }
    if let Some(hex) = args.color.as_deref() {
        // "" clears the per-marker override so the category colour shows again.
        m.color = if hex.is_empty() {
            None
        } else {
            match Color::from_hex(hex) {
                Some(c) => Some(c),
                None => {
                    return Err(ToolResult::error(format!(
                        "invalid color {hex:?} — expected #rrggbb or #rrggbbaa"
                    )))
                }
            }
        };
    }
    if args.clear_category {
        m.category = None;
    } else if let Some(cat) = args.category_id {
        m.category = Some(cat);
    }
    Ok(())
}

/// Universal marker editor (26 K-A2). Without this there is no way to give a
/// marker a duration, and `export_per_marker` (K-F2) fans out over exactly the
/// ranged markers nothing could create.
pub async fn set_marker(state: &AppState, args: SetMarkerArgs) -> ToolResult {
    tracing::debug!("tool: set_marker {}", args.marker_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    if let Some(cat) = args.category_id {
        if !args.clear_category && project.marker_category(cat).is_none() {
            return ToolResult::error(format!("marker category {cat} not found"));
        }
    }
    let cmd = match args.clip_id {
        Some(clip_id) => {
            let Some((seq_id, track_id)) = locate_clip(project, clip_id) else {
                return ToolResult::error(format!("clip {clip_id} not found"));
            };
            let Some(clip) = find_clip(project, seq_id, track_id, clip_id) else {
                return ToolResult::error(format!("clip {clip_id} not found"));
            };
            let fr = project.sequences.get(&seq_id).map(|s| s.frame_rate);
            let Some(mut new) = clip
                .markers
                .iter()
                .find(|m| m.id == args.marker_id)
                .cloned()
            else {
                return ToolResult::error(format!(
                    "marker {} not found on clip {clip_id}",
                    args.marker_id
                ));
            };
            if let Err(e) = apply_marker_edits(&mut new, &args, fr) {
                return e;
            }
            ops::set_clip_marker(project, clip_id, new)
        }
        None => {
            let Some(seq_id) = locate_marker(project, args.marker_id) else {
                return ToolResult::error(format!("marker {} not found", args.marker_id));
            };
            let seq = &project.sequences[&seq_id];
            let Some(mut new) = seq.markers.iter().find(|m| m.id == args.marker_id).cloned() else {
                return ToolResult::error(format!("marker {} not found", args.marker_id));
            };
            if let Err(e) = apply_marker_edits(&mut new, &args, Some(seq.frame_rate)) {
                return e;
            }
            ops::set_marker(project, seq_id, new)
        }
    };
    match cmd {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Updated marker")
        }
        Err(e) => map_edit_error(e),
    }
}

// ─── Clip-scoped markers (26 K-A2 / 35 §1.5) ────────────────────────────────

pub async fn add_clip_marker(state: &AppState, args: AddClipMarkerArgs) -> ToolResult {
    tracing::debug!("tool: add_clip_marker {}", args.clip_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, _)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let fr = project.sequences.get(&seq_id).map(|s| s.frame_rate);
    // Clip markers are clip-relative, so `at_tc` is a DURATION into the clip,
    // not a sequence timecode — the same parse, a different origin.
    let at = match resolve_tick(args.at_ticks, args.at_tc.as_deref(), args.at_seconds, fr) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let duration =
        match resolve_marker_duration(at, args.duration_ticks, args.duration_seconds, None, fr) {
            Ok(d) => d,
            Err(e) => return e,
        };
    if let Some(cat) = args.category_id {
        if project.marker_category(cat).is_none() {
            return ToolResult::error(format!("marker category {cat} not found"));
        }
    }
    let mut marker = Marker::clip_scoped(at, args.name.unwrap_or_default());
    marker.duration = duration;
    marker.category = args.category_id;
    if let Some(note) = args.note {
        marker.note = note;
    }
    if let Some(hex) = args.color {
        match Color::from_hex(&hex) {
            Some(c) => marker.color = Some(c),
            None => {
                return ToolResult::error(format!(
                    "invalid color {hex:?} — expected #rrggbb or #rrggbbaa"
                ))
            }
        }
    }
    let marker_id = marker.id;
    match ops::add_clip_marker(project, args.clip_id, marker) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Added clip marker").with_data(json!({ "marker_id": marker_id }))
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn remove_clip_marker(state: &AppState, args: RemoveClipMarkerArgs) -> ToolResult {
    tracing::debug!("tool: remove_clip_marker {}", args.marker_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    match ops::remove_clip_marker(project, args.clip_id, args.marker_id) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Removed clip marker")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn list_clip_markers(state: &AppState, args: ListClipMarkersArgs) -> ToolResult {
    tracing::debug!("tool: list_clip_markers {}", args.clip_id);
    let doc = state.document.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let Some(clip) = find_clip(project, seq_id, track_id, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    // `at` is clip-relative; `sequence_tick` is the timeline position, so a
    // caller never has to re-derive `clip.start + m.at` itself.
    let markers: Vec<serde_json::Value> = clip
        .markers
        .iter()
        .map(|m| {
            let mut v = serde_json::to_value(m).unwrap_or(json!({}));
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "sequence_tick".into(),
                    json!(clip.marker_sequence_tick(m).0),
                );
            }
            v
        })
        .collect();
    ToolResult::text(format!("{} clip marker(s)", clip.markers.len())).with_data(json!({
        "markers": markers,
        "sequence_id": seq_id,
        "clip_start_ticks": clip.start.0,
    }))
}

// ─── Marker categories (26 K-A2 / 35 §1.3) ──────────────────────────────────

pub async fn list_marker_categories(
    state: &AppState,
    _args: ListMarkerCategoriesArgs,
) -> ToolResult {
    tracing::debug!("tool: list_marker_categories");
    let doc = state.document.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    ToolResult::text(format!(
        "{} marker category/categories",
        project.marker_categories.len()
    ))
    .with_data(json!({ "categories": project.marker_categories }))
}

pub async fn add_marker_category(state: &AppState, args: AddMarkerCategoryArgs) -> ToolResult {
    tracing::debug!("tool: add_marker_category {:?}", args.name);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(color) = Color::from_hex(&args.color) else {
        return ToolResult::error(format!(
            "invalid color {:?} — expected #rrggbb or #rrggbbaa",
            args.color
        ));
    };
    let mut category = MarkerCategory::new(args.name, color);
    if let Some(glyph) = args.glyph {
        category.glyph = glyph;
    }
    let category_id = category.id;
    match ops::add_marker_category(project, category) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Added marker category")
                .with_data(json!({ "category_id": category_id }))
        }
        Err(e) => map_edit_error(e),
    }
}

/// Seed the default marker categories as ONE undo unit. Idempotent: a project
/// that already has categories is left alone.
pub async fn seed_marker_categories(
    state: &AppState,
    _args: SeedMarkerCategoriesArgs,
) -> ToolResult {
    tracing::debug!("tool: seed_marker_categories");
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let cmds = ops::seed_marker_categories(project);
    if cmds.is_empty() {
        return ToolResult::text("Marker categories already present — nothing seeded")
            .with_data(json!({ "categories": project.marker_categories }));
    }
    let batch = cmds.into_iter().map(Command::Timeline).collect();
    history.execute_discrete(Command::Batch(batch), &mut doc);
    let categories = doc
        .timeline
        .as_ref()
        .map(|p| p.marker_categories.clone())
        .unwrap_or_default();
    ToolResult::text(format!("Seeded {} marker categories", categories.len()))
        .with_data(json!({ "categories": categories }))
}

pub async fn update_marker_category(
    state: &AppState,
    args: UpdateMarkerCategoryArgs,
) -> ToolResult {
    tracing::debug!("tool: update_marker_category {}", args.category_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(existing) = project.marker_category(args.category_id) else {
        return ToolResult::error(format!("marker category {} not found", args.category_id));
    };
    let mut new = existing.clone();
    if let Some(name) = args.name {
        new.name = name;
    }
    if let Some(hex) = args.color {
        match Color::from_hex(&hex) {
            Some(c) => new.color = c,
            None => {
                return ToolResult::error(format!(
                    "invalid color {hex:?} — expected #rrggbb or #rrggbbaa"
                ))
            }
        }
    }
    if let Some(glyph) = args.glyph {
        new.glyph = glyph;
    }
    match ops::set_marker_category(project, new) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Updated marker category")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn remove_marker_category(
    state: &AppState,
    args: RemoveMarkerCategoryArgs,
) -> ToolResult {
    tracing::debug!("tool: remove_marker_category {}", args.category_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let affected = project.markers_in_category(args.category_id).len();
    match ops::remove_marker_category(project, args.category_id, args.reassign_to) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text(format!(
                "Removed marker category ({affected} marker(s) {})",
                match args.reassign_to {
                    Some(_) => "reassigned",
                    None => "cleared",
                }
            ))
            .with_data(json!({ "markers_retargeted": affected }))
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn remove_marker(state: &AppState, args: RemoveMarkerArgs) -> ToolResult {
    tracing::debug!("tool: remove_marker {}", args.marker_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq_id) = locate_marker(project, args.marker_id) else {
        return ToolResult::error(format!("marker {} not found", args.marker_id));
    };
    match ops::remove_marker(project, seq_id, args.marker_id) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Removed marker")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn list_markers(state: &AppState, args: ListMarkersArgs) -> ToolResult {
    tracing::debug!("tool: list_markers {}", args.sequence_id);
    let doc = state.document.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq) = project.sequences.get(&args.sequence_id) else {
        return ToolResult::error(format!("sequence {} not found", args.sequence_id));
    };
    ToolResult::text(format!("{} marker(s)", seq.markers.len()))
        .with_data(json!({ "markers": seq.markers }))
}

// ─── Track (10 §3.3) ────────────────────────────────────────────────────────

pub async fn add_track(state: &AppState, args: AddTrackArgs) -> ToolResult {
    tracing::debug!("tool: add_track {}", args.sequence_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let name = args.name.unwrap_or_else(|| match args.kind {
        photonic_core::timeline::TrackKind::Video => "Video".into(),
        photonic_core::timeline::TrackKind::Audio => "Audio".into(),
        photonic_core::timeline::TrackKind::Text => "Text".into(),
    });
    let track = Track::new(args.kind, name);
    let track_id = track.id;
    match ops::add_track(project, args.sequence_id, track, args.index) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Added track").with_data(json!({ "track_id": track_id }))
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn remove_track(state: &AppState, args: RemoveTrackArgs) -> ToolResult {
    tracing::debug!("tool: remove_track {}", args.track_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq_id) = locate_track(project, args.track_id) else {
        return ToolResult::error(format!("track {} not found", args.track_id));
    };
    match ops::remove_track(project, seq_id, args.track_id) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Removed track")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn set_track_prop(state: &AppState, args: SetTrackPropArgs) -> ToolResult {
    tracing::debug!("tool: set_track_prop {}", args.track_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq_id) = locate_track(project, args.track_id) else {
        return ToolResult::error(format!("track {} not found", args.track_id));
    };
    let seq = project.sequences.get(&seq_id).unwrap();
    let Some(t) = seq.track(args.track_id) else {
        return ToolResult::error(format!("track {} not found", args.track_id));
    };
    let mut new_settings = TrackSettings::of(t);
    if let Some(n) = args.name {
        new_settings.name = n;
    }
    if let Some(e) = args.enabled {
        new_settings.enabled = e;
    }
    if let Some(l) = args.locked {
        new_settings.locked = l;
    }
    if let Some(h) = args.height_px {
        new_settings.height_px = h;
    }
    match ops::set_track_prop(project, seq_id, args.track_id, new_settings) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Updated track")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn reorder_track(state: &AppState, args: ReorderTrackArgs) -> ToolResult {
    tracing::debug!("tool: reorder_track {}", args.track_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq_id) = locate_track(project, args.track_id) else {
        return ToolResult::error(format!("track {} not found", args.track_id));
    };
    match ops::reorder_track(project, seq_id, args.track_id, args.new_index) {
        Ok(cmds) => {
            history.execute_discrete(
                Command::Batch(cmds.into_iter().map(Command::Timeline).collect()),
                &mut doc,
            );
            ToolResult::text("Reordered track")
        }
        Err(e) => map_edit_error(e),
    }
}

// ─── Clip edit ops (10 §3.4) ────────────────────────────────────────────────

fn to_clip_source(project: &TimelineProject, arg: ClipSourceArg) -> Result<ClipSource, ToolResult> {
    match arg {
        ClipSourceArg::Asset { asset_id } => {
            if !project.media.assets.contains_key(&asset_id) {
                return Err(ToolResult::error(format!("asset {asset_id} not found")));
            }
            check_asset_offline(project, asset_id)?;
            Ok(ClipSource::Asset { asset: asset_id })
        }
        ClipSourceArg::Vector { asset_id } => {
            if !project.media.assets.contains_key(&asset_id) {
                return Err(ToolResult::error(format!("asset {asset_id} not found")));
            }
            Ok(ClipSource::Vector { asset: asset_id })
        }
        ClipSourceArg::NestedSequence { sequence_id } => {
            if !project.sequences.contains_key(&sequence_id) {
                return Err(ToolResult::error(format!(
                    "sequence {sequence_id} not found"
                )));
            }
            Ok(ClipSource::NestedSequence {
                sequence: sequence_id,
            })
        }
        ClipSourceArg::SolidColor { color } => {
            let c = Color::from_hex(&color).ok_or_else(|| {
                ToolResult::error(format!(
                    "invalid color {color:?} — expected #rrggbb or #rrggbbaa"
                ))
            })?;
            Ok(ClipSource::SolidColor { color: c })
        }
        ClipSourceArg::Adjustment => Ok(ClipSource::Adjustment),
    }
}

fn check_asset_offline(project: &TimelineProject, asset_id: AssetId) -> Result<(), ToolResult> {
    if let Some(a) = project.media.assets.get(&asset_id) {
        if let photonic_core::timeline::AssetSource::File { path, .. } = &a.source {
            if !path.exists() {
                return Err(err_code(
                    "AssetOffline",
                    format!("asset {asset_id} file is unreachable: {}", path.display()),
                ));
            }
        }
    }
    Ok(())
}

pub async fn insert_clip(state: &AppState, args: InsertClipArgs) -> ToolResult {
    tracing::debug!("tool: insert_clip on track {}", args.track_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq_id) = locate_track(project, args.track_id) else {
        return ToolResult::error(format!("track {} not found", args.track_id));
    };
    let fr = project.sequences.get(&seq_id).unwrap().frame_rate;

    let start = match resolve_tick(
        args.start_ticks,
        args.start_tc.as_deref(),
        args.start_seconds,
        Some(fr),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let has_source_in = args.source_in_ticks.is_some()
        || args.source_in_tc.is_some()
        || args.source_in_seconds.is_some();
    let source_in = if has_source_in {
        match resolve_tick(
            args.source_in_ticks,
            args.source_in_tc.as_deref(),
            args.source_in_seconds,
            Some(fr),
        ) {
            Ok(t) => t,
            Err(e) => return e,
        }
    } else {
        Tick::ZERO
    };
    if args.duration_ticks <= 0 {
        return err_code("TickOutOfRange", "duration_ticks must be > 0");
    }
    let source = match to_clip_source(project, args.source) {
        Ok(s) => s,
        Err(e) => return e,
    };

    let mut clip = Clip::new(source, start, Tick(args.duration_ticks));
    clip.source_in = source_in;
    if let Some(name) = args.name {
        clip.name = name;
    }
    let clip_id = clip.id;

    match ops::insert_clip(project, seq_id, args.track_id, clip) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Inserted clip").with_data(json!({ "clip_id": clip_id }))
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn move_clip(state: &AppState, args: MoveClipArgs) -> ToolResult {
    tracing::debug!("tool: move_clip {}", args.clip_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let fr = project.sequences.get(&seq_id).unwrap().frame_rate;
    let new_start = match resolve_tick(
        args.new_start_ticks,
        args.new_start_tc.as_deref(),
        args.new_start_seconds,
        Some(fr),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    match ops::move_linked_clip(
        project,
        seq_id,
        track_id,
        args.clip_id,
        new_start,
        args.new_track_id,
    ) {
        Ok(cmds) if cmds.is_empty() => ToolResult::text("No clips moved"),
        Ok(cmds) => {
            history.execute_discrete(batch_or_single(cmds), &mut doc);
            ToolResult::text("Moved linked clip unit")
        }
        Err(e) => map_edit_error(e),
    }
}

/// Shift several clips by one shared delta in a single undo step (04 §2.6,
/// 210 §5) — the MCP arm of the GUI's multi-select body move, per 19 G-21's
/// standing requirement that landed editing verbs keep an MCP trail.
///
/// Link partners are folded into the same moving set rather than expanded per
/// clip: a partner that is also named in `clip_ids` must not move twice, and
/// `ops::move_clips` needs the whole set to tell a collision the move is
/// vacating from a real obstruction.
pub async fn move_clips(state: &AppState, args: MoveClipsArgs) -> ToolResult {
    tracing::debug!("tool: move_clips {} clips", args.clip_ids.len());
    if args.clip_ids.is_empty() {
        return ToolResult::error("clip_ids must not be empty");
    }
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };

    let mut seq_id = None;
    let mut moving: Vec<(TrackId, ClipId)> = Vec::new();
    for clip_id in &args.clip_ids {
        let Some((s, track_id)) = locate_clip(project, *clip_id) else {
            return ToolResult::error(format!("clip {clip_id} not found"));
        };
        // One sequence per call: a delta means nothing across sequences that
        // may not even share a frame rate.
        match seq_id {
            None => seq_id = Some(s),
            Some(existing) if existing != s => {
                return ToolResult::error("all clips must be in the same sequence");
            }
            Some(_) => {}
        }
        if !moving.contains(&(track_id, *clip_id)) {
            moving.push((track_id, *clip_id));
        }
        for partner in link_partners(project, s, track_id, *clip_id) {
            if !moving.contains(&partner) {
                moving.push(partner);
            }
        }
    }
    let seq_id = seq_id.expect("clip_ids is non-empty and every id resolved");

    let moving = match ops::linked_moving_set(project, seq_id, &moving) {
        Ok(moving) => moving,
        Err(error) => return map_edit_error(error),
    };
    match ops::move_clips(
        project,
        seq_id,
        &moving,
        Tick(args.delta_ticks),
        args.track_delta,
    ) {
        Ok(cmds) if cmds.is_empty() => ToolResult::text("No clips moved (zero delta)"),
        Ok(cmds) => {
            let n = cmds.len();
            history.execute_discrete(batch_or_single(cmds), &mut doc);
            ToolResult::text(format!("Moved {n} clips"))
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn trim_clip(state: &AppState, args: TrimClipArgs) -> ToolResult {
    tracing::debug!("tool: trim_clip {}", args.clip_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let fr = project.sequences.get(&seq_id).unwrap().frame_rate;
    let new_tick = match resolve_tick(
        args.new_ticks,
        args.new_tc.as_deref(),
        args.new_seconds,
        Some(fr),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let Some(clip) = find_clip(project, seq_id, track_id, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let new_timing = match args.edge {
        ClipEdge::In => ClipTiming {
            start: new_tick,
            duration: clip.end() - new_tick,
            source_in: clip.source_in + (new_tick - clip.start),
        },
        ClipEdge::Out => ClipTiming {
            start: clip.start,
            duration: new_tick - clip.start,
            source_in: clip.source_in,
        },
    };
    match ops::trim_clip(project, seq_id, track_id, args.clip_id, new_timing) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Trimmed clip")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn split_clip(state: &AppState, args: SplitClipArgs) -> ToolResult {
    tracing::debug!("tool: split_clip {}", args.clip_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let fr = project.sequences.get(&seq_id).unwrap().frame_rate;
    let at = match resolve_tick(
        args.at_ticks,
        args.at_tc.as_deref(),
        args.at_seconds,
        Some(fr),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    match ops::split_clip(project, seq_id, track_id, args.clip_id, at) {
        Ok(cmd) => {
            let new_clip_id = match &cmd {
                TimelineCmd::SplitClip { new_clip_id, .. } => Some(*new_clip_id),
                _ => None,
            };
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Split clip").with_data(json!({ "new_clip_id": new_clip_id }))
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn remove_clip(state: &AppState, args: RemoveClipArgs) -> ToolResult {
    tracing::debug!("tool: remove_clip {}", args.clip_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    if args.ripple {
        // Core `ripple_delete` already expands to sync-locked siblings (14 §M-9).
        match ops::ripple_delete(project, seq_id, track_id, args.clip_id) {
            Ok(cmds) => {
                history.execute_discrete(
                    Command::Batch(cmds.into_iter().map(Command::Timeline).collect()),
                    &mut doc,
                );
                ToolResult::text("Removed clip (rippled)")
            }
            Err(e) => map_edit_error(e),
        }
    } else {
        match ops::remove_clip(project, seq_id, track_id, args.clip_id) {
            Ok(cmd) => {
                // Fan the delete across the link group (14 §M-2) — deleting
                // one half of a linked pair takes the other half with it, in
                // the same undo step. Ripple-delete above uses sync-lock
                // expansion instead of link-group fan-out.
                let mut cmds = vec![cmd];
                cmds.extend(expand_link_group_delete(
                    project,
                    seq_id,
                    track_id,
                    args.clip_id,
                ));
                history.execute_discrete(batch_or_single(cmds), &mut doc);
                ToolResult::text("Removed clip")
            }
            Err(e) => map_edit_error(e),
        }
    }
}

pub async fn roll_edit(state: &AppState, args: RollEditArgs) -> ToolResult {
    tracing::debug!("tool: roll_edit {} / {}", args.clip_id_a, args.clip_id_b);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_a)) = locate_clip(project, args.clip_id_a) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id_a));
    };
    let Some((_, track_b)) = locate_clip(project, args.clip_id_b) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id_b));
    };
    if track_a != track_b {
        return ToolResult::error("roll_edit requires both clips to be on the same track");
    }
    match ops::roll_edit(
        project,
        seq_id,
        track_a,
        args.clip_id_a,
        args.clip_id_b,
        Tick(args.delta_ticks),
    ) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Rolled edit")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn slip_clip(state: &AppState, args: SlipClipArgs) -> ToolResult {
    tracing::debug!("tool: slip_clip {}", args.clip_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let Some(clip) = find_clip(project, seq_id, track_id, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let new_source_in = clip.source_in + Tick(args.delta_ticks);
    match ops::slip_clip(project, seq_id, track_id, args.clip_id, new_source_in) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Slipped clip")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn slide_clip(state: &AppState, args: SlideClipArgs) -> ToolResult {
    tracing::debug!("tool: slide_clip {}", args.clip_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    match ops::slide_clip(
        project,
        seq_id,
        track_id,
        args.clip_id,
        Tick(args.delta_ticks),
    ) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Slid clip")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn ripple_edit(state: &AppState, args: RippleEditArgs) -> ToolResult {
    tracing::debug!("tool: ripple_edit {}", args.clip_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let Some(clip) = find_clip(project, seq_id, track_id, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    // `edge` uses the same in/out vocabulary as trim_clip: in = the clip's
    // in-point (`ops::ClipEdge::Start`), out = its out-point (`::End`).
    let (core_edge, current_boundary) = match args.edge {
        ClipEdge::In => (photonic_core::timeline::ClipEdge::Start, clip.start),
        ClipEdge::Out => (photonic_core::timeline::ClipEdge::End, clip.end()),
    };
    let new_boundary = current_boundary + Tick(args.delta_ticks);
    match ops::ripple_trim(
        project,
        seq_id,
        track_id,
        args.clip_id,
        core_edge,
        new_boundary,
    ) {
        Ok(cmds) => {
            // Core already expands to sync-locked siblings (14 §M-9).
            history.execute_discrete(
                Command::Batch(cmds.into_iter().map(Command::Timeline).collect()),
                &mut doc,
            );
            ToolResult::text("Rippled edit")
        }
        Err(e) => map_edit_error(e),
    }
}

// ─── 3/4-point editing (16 §2, CAP-019 MCP parity) ─────────────────────────

pub async fn insert_edit(state: &AppState, args: InsertEditArgs) -> ToolResult {
    tracing::debug!("tool: insert_edit on track {}", args.track_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq_id) = locate_track(project, args.track_id) else {
        return ToolResult::error(format!("track {} not found", args.track_id));
    };
    let fr = project.sequences.get(&seq_id).unwrap().frame_rate;

    let at = match resolve_tick(
        args.at_ticks,
        args.at_tc.as_deref(),
        args.at_seconds,
        Some(fr),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let has_source_in = args.source_in_ticks.is_some()
        || args.source_in_tc.is_some()
        || args.source_in_seconds.is_some();
    let source_in = if has_source_in {
        match resolve_tick(
            args.source_in_ticks,
            args.source_in_tc.as_deref(),
            args.source_in_seconds,
            Some(fr),
        ) {
            Ok(t) => t,
            Err(e) => return e,
        }
    } else {
        Tick::ZERO
    };
    if args.duration_ticks <= 0 {
        return err_code("TickOutOfRange", "duration_ticks must be > 0");
    }
    let source = match to_clip_source(project, args.source) {
        Ok(s) => s,
        Err(e) => return e,
    };

    let mut clip = Clip::new(source, at, Tick(args.duration_ticks));
    clip.source_in = source_in;
    if let Some(name) = args.name {
        clip.name = name;
    }
    let clip_id = clip.id;

    match ops::insert_edit(project, seq_id, args.track_id, at, clip) {
        Ok(cmds) => {
            history.execute_discrete(batch_or_single(cmds), &mut doc);
            ToolResult::text("Inserted edit (rippled)").with_data(json!({ "clip_id": clip_id }))
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn overwrite_edit(state: &AppState, args: OverwriteEditArgs) -> ToolResult {
    tracing::debug!("tool: overwrite_edit on track {}", args.track_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq_id) = locate_track(project, args.track_id) else {
        return ToolResult::error(format!("track {} not found", args.track_id));
    };
    let fr = project.sequences.get(&seq_id).unwrap().frame_rate;

    let at = match resolve_tick(
        args.at_ticks,
        args.at_tc.as_deref(),
        args.at_seconds,
        Some(fr),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let has_source_in = args.source_in_ticks.is_some()
        || args.source_in_tc.is_some()
        || args.source_in_seconds.is_some();
    let source_in = if has_source_in {
        match resolve_tick(
            args.source_in_ticks,
            args.source_in_tc.as_deref(),
            args.source_in_seconds,
            Some(fr),
        ) {
            Ok(t) => t,
            Err(e) => return e,
        }
    } else {
        Tick::ZERO
    };
    if args.duration_ticks <= 0 {
        return err_code("TickOutOfRange", "duration_ticks must be > 0");
    }
    let source = match to_clip_source(project, args.source) {
        Ok(s) => s,
        Err(e) => return e,
    };

    let mut clip = Clip::new(source, at, Tick(args.duration_ticks));
    clip.source_in = source_in;
    if let Some(name) = args.name {
        clip.name = name;
    }
    let clip_id = clip.id;

    match ops::overwrite_edit(project, seq_id, args.track_id, at, clip) {
        Ok(cmds) => {
            history.execute_discrete(batch_or_single(cmds), &mut doc);
            ToolResult::text("Overwrote edit").with_data(json!({ "clip_id": clip_id }))
        }
        Err(e) => map_edit_error(e),
    }
}

/// Shared by `lift_edit`/`extract_edit`: resolve a [`WorkRangeArg`] to
/// `(Tick, Tick)` against `fr` — both bounds required (a missing one surfaces
/// `resolve_tick`'s standard error).
fn resolve_range(range: &WorkRangeArg, fr: FrameRate) -> Result<(Tick, Tick), ToolResult> {
    let start = resolve_tick(
        range.start_ticks,
        range.start_tc.as_deref(),
        range.start_seconds,
        Some(fr),
    )?;
    let end = resolve_tick(
        range.end_ticks,
        range.end_tc.as_deref(),
        range.end_seconds,
        Some(fr),
    )?;
    Ok((start, end))
}

pub async fn lift_edit(state: &AppState, args: LiftEditArgs) -> ToolResult {
    tracing::debug!("tool: lift_edit on track {}", args.track_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq_id) = locate_track(project, args.track_id) else {
        return ToolResult::error(format!("track {} not found", args.track_id));
    };
    let fr = project.sequences.get(&seq_id).unwrap().frame_rate;
    let range = match resolve_range(&args.range, fr) {
        Ok(r) => r,
        Err(e) => return e,
    };
    match ops::lift_edit(project, seq_id, args.track_id, range) {
        Ok(cmds) => {
            if cmds.is_empty() {
                return ToolResult::text("No clips in range — nothing to lift");
            }
            history.execute_discrete(batch_or_single(cmds), &mut doc);
            ToolResult::text("Lifted range")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn extract_edit(state: &AppState, args: ExtractEditArgs) -> ToolResult {
    tracing::debug!("tool: extract_edit on track {}", args.track_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq_id) = locate_track(project, args.track_id) else {
        return ToolResult::error(format!("track {} not found", args.track_id));
    };
    let fr = project.sequences.get(&seq_id).unwrap().frame_rate;
    let range = match resolve_range(&args.range, fr) {
        Ok(r) => r,
        Err(e) => return e,
    };
    match ops::extract_edit(project, seq_id, args.track_id, range) {
        Ok(cmds) => {
            if cmds.is_empty() {
                return ToolResult::text("No clips in range — nothing to extract");
            }
            history.execute_discrete(batch_or_single(cmds), &mut doc);
            ToolResult::text("Extracted range (rippled)")
        }
        Err(e) => map_edit_error(e),
    }
}

// ─── NLE parity round-2 (17-nle-parity-round2.md, G21 CAP-019 MCP parity) ──
//
// `add_edit_all_tracks`/`close_gap` mirror `photonic_gui::app::timeline::
// ops_bridge`'s `split_all_tracks`/`close_gap_plan`/`close_gap_changes`/
// `close_gaps_at_playhead` field-for-field. MCP has no dependency on the GUI
// crate (see the link-group note above `ops::move_linked_clip`) so this is a
// parallel implementation over the same core primitives (`ops::split_clip`,
// `TimelineCmd::RippleEdit` built directly — the same pattern `ops::
// ripple_trim`/`extract_edit` already use internally).

/// Every unlocked track's clip the tick `at` is strictly inside (`start < at
/// < end`), in track order — the split-worthy targets for Add Edit to All
/// Tracks (G-1). An edge/gap yields none for that track.
fn split_targets(seq: &Sequence, at: Tick) -> Vec<(TrackId, ClipId)> {
    let mut out = Vec::new();
    for t in seq.tracks() {
        if t.locked {
            continue;
        }
        for c in &t.clips {
            if c.start < at && at < c.end() {
                out.push((t.id, c.id));
            }
        }
    }
    out
}

/// Plan closing the gap that contains `at` among a track's sorted `clips`:
/// `(first_shifted_start, gap_width)` — every clip with `start >=
/// first_shifted_start` shifts LEFT by `gap_width`. `None` when `at` is
/// inside a clip, in trailing empty space, or on a flush boundary (no gap).
fn close_gap_plan(clips: &[Clip], at: Tick) -> Option<(Tick, Tick)> {
    let i = clips.iter().position(|c| c.start > at)?;
    let prev_end = if i == 0 {
        Tick::ZERO
    } else {
        clips[i - 1].end()
    };
    if at < prev_end {
        return None; // inside the previous clip — not a gap
    }
    let gap = clips[i].start - prev_end;
    if gap.0 <= 0 {
        return None; // clips are already flush
    }
    Some((clips[i].start, gap))
}

/// The `RippleEdit` change list that closes the gap containing `at` on
/// `track`. Empty when there is no gap there.
fn close_gap_changes(track: &Track, at: Tick) -> Vec<(ClipId, ClipTiming, ClipTiming)> {
    let Some((from, gap)) = close_gap_plan(&track.clips, at) else {
        return Vec::new();
    };
    track
        .clips
        .iter()
        .filter(|c| c.start >= from)
        .map(|c| {
            let old = ClipTiming::of(c);
            (
                c.id,
                old,
                ClipTiming {
                    start: c.start - gap,
                    ..old
                },
            )
        })
        .collect()
}

/// **Replace With Clip** (G-5, Premiere): swap `clip_id`'s source in place —
/// `start`/`duration`/effects/transitions/grade untouched
/// (`ops::replace_clip_source`, a `set_clip_prop` whole-clip diff, one undo
/// step). A shorter new source is held to the slot (the engine samples from
/// `new_source_in` for the slot's length).
pub async fn replace_clip_source(state: &AppState, args: ReplaceClipSourceArgs) -> ToolResult {
    tracing::debug!("tool: replace_clip_source {}", args.clip_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let fr = project.sequences.get(&seq_id).unwrap().frame_rate;
    let new_source = match to_clip_source(project, args.new_source) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let has_source_in = args.new_source_in_ticks.is_some()
        || args.new_source_in_tc.is_some()
        || args.new_source_in_seconds.is_some();
    let new_source_in = if has_source_in {
        match resolve_tick(
            args.new_source_in_ticks,
            args.new_source_in_tc.as_deref(),
            args.new_source_in_seconds,
            Some(fr),
        ) {
            Ok(t) => Some(t),
            Err(e) => return e,
        }
    } else {
        None
    };
    match ops::replace_clip_source(
        project,
        seq_id,
        track_id,
        args.clip_id,
        new_source,
        new_source_in,
    ) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Replaced clip source").with_data(json!({ "clip_id": args.clip_id }))
        }
        Err(e) => map_edit_error(e),
    }
}

/// **Add Edit to All Tracks** (G-1, Premiere Ctrl+Shift+K): split every
/// unlocked track's clip under `at` in one undo step. Mirrors the GUI's
/// forgiving fan-out — a track whose split fails (shouldn't happen for a
/// `split_targets` candidate) is silently skipped rather than aborting the
/// whole batch.
pub async fn add_edit_all_tracks(state: &AppState, args: AddEditAllTracksArgs) -> ToolResult {
    tracing::debug!("tool: add_edit_all_tracks on sequence {}", args.sequence_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq) = project.sequences.get(&args.sequence_id) else {
        return ToolResult::error(format!("sequence {} not found", args.sequence_id));
    };
    let at = match resolve_tick(
        args.at_ticks,
        args.at_tc.as_deref(),
        args.at_seconds,
        Some(seq.frame_rate),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let targets = split_targets(seq, at);
    if targets.is_empty() {
        return ToolResult::text("No clips under the given tick — nothing to split")
            .with_data(json!({ "split_count": 0, "new_clip_ids": [] }));
    }
    let mut cmds = Vec::new();
    let mut new_clip_ids = Vec::new();
    for (track, clip) in targets {
        if let Ok(cmd) = ops::split_clip(project, args.sequence_id, track, clip, at) {
            if let TimelineCmd::SplitClip { new_clip_id, .. } = &cmd {
                new_clip_ids.push(*new_clip_id);
            }
            cmds.push(cmd);
        }
    }
    let n = cmds.len();
    if n == 0 {
        return ToolResult::text("No clips under the given tick — nothing to split")
            .with_data(json!({ "split_count": 0, "new_clip_ids": [] }));
    }
    history.execute_discrete(batch_or_single(cmds), &mut doc);
    ToolResult::text(format!("Split {n} clip(s) across all tracks"))
        .with_data(json!({ "split_count": n, "new_clip_ids": new_clip_ids }))
}

/// **Close Gap** (G-1): close the gap containing `at` — on just `track_id`
/// when supplied, or on every unlocked track in the sequence when omitted —
/// as ONE undo step either way. A no-op (no history entry, `tracks_changed:
/// 0`) when there is nothing to close.
pub async fn close_gap(state: &AppState, args: CloseGapArgs) -> ToolResult {
    tracing::debug!("tool: close_gap on sequence {}", args.sequence_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq) = project.sequences.get(&args.sequence_id) else {
        return ToolResult::error(format!("sequence {} not found", args.sequence_id));
    };
    let at = match resolve_tick(
        args.at_ticks,
        args.at_tc.as_deref(),
        args.at_seconds,
        Some(seq.frame_rate),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };

    if let Some(track_id) = args.track_id {
        let Some(t) = seq.track(track_id) else {
            return ToolResult::error(format!("track {track_id} not found"));
        };
        if t.locked {
            return ToolResult::text("Track is locked — nothing to close")
                .with_data(json!({ "tracks_changed": 0 }));
        }
        let changes = close_gap_changes(t, at);
        if changes.is_empty() {
            return ToolResult::text("No gap at the given tick — nothing to close")
                .with_data(json!({ "tracks_changed": 0 }));
        }
        history.execute_discrete(
            Command::Timeline(TimelineCmd::RippleEdit {
                seq: args.sequence_id,
                track: track_id,
                changes,
            }),
            &mut doc,
        );
        ToolResult::text("Closed gap").with_data(json!({ "tracks_changed": 1 }))
    } else {
        let mut cmds = Vec::new();
        for t in seq.tracks() {
            if t.locked {
                continue;
            }
            let changes = close_gap_changes(t, at);
            if !changes.is_empty() {
                cmds.push(TimelineCmd::RippleEdit {
                    seq: args.sequence_id,
                    track: t.id,
                    changes,
                });
            }
        }
        if cmds.is_empty() {
            return ToolResult::text(
                "No gap at the given tick on any unlocked track — nothing to close",
            )
            .with_data(json!({ "tracks_changed": 0 }));
        }
        let n = cmds.len();
        history.execute_discrete(batch_or_single(cmds), &mut doc);
        ToolResult::text(format!("Closed gap on {n} track(s)"))
            .with_data(json!({ "tracks_changed": n }))
    }
}

fn resolve_amount_ticks(amount_ticks: Option<i64>, amount_seconds: Option<f64>) -> Tick {
    if let Some(t) = amount_ticks {
        return Tick(t);
    }
    let secs = amount_seconds.unwrap_or(1.0);
    Tick((secs * photonic_core::timeline::TICKS_PER_SECOND as f64).round() as i64)
}

/// K-A3 Insert Space: open `amount` at `at` across unlocked tracks (one undo).
pub async fn insert_space(state: &AppState, args: InsertSpaceArgs) -> ToolResult {
    tracing::debug!("tool: insert_space on sequence {}", args.sequence_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq) = project.sequences.get(&args.sequence_id) else {
        return ToolResult::error(format!("sequence {} not found", args.sequence_id));
    };
    let at = match resolve_tick(
        args.at_ticks,
        args.at_tc.as_deref(),
        args.at_seconds,
        Some(seq.frame_rate),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let amount = resolve_amount_ticks(args.amount_ticks, args.amount_seconds);
    if amount.0 <= 0 {
        return ToolResult::error("amount must be > 0");
    }
    match ops::insert_space(project, args.sequence_id, at, amount) {
        Ok(cmds) if cmds.is_empty() => ToolResult::text("Nothing after the point — no shift")
            .with_data(json!({ "commands": 0 })),
        Ok(cmds) => {
            let n = cmds.len();
            history.execute_discrete(batch_or_single(cmds), &mut doc);
            ToolResult::text(format!("Inserted space ({n} command(s))"))
                .with_data(json!({ "commands": n, "amount_ticks": amount.0 }))
        }
        Err(e) => map_edit_error(e),
    }
}

/// K-A3 Remove Space: close up to `amount` of pure gap at `at` (one undo).
pub async fn remove_space(state: &AppState, args: RemoveSpaceArgs) -> ToolResult {
    tracing::debug!("tool: remove_space on sequence {}", args.sequence_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq) = project.sequences.get(&args.sequence_id) else {
        return ToolResult::error(format!("sequence {} not found", args.sequence_id));
    };
    let at = match resolve_tick(
        args.at_ticks,
        args.at_tc.as_deref(),
        args.at_seconds,
        Some(seq.frame_rate),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let amount = resolve_amount_ticks(args.amount_ticks, args.amount_seconds);
    if amount.0 <= 0 {
        return ToolResult::error("amount must be > 0");
    }
    match ops::remove_space(project, args.sequence_id, at, amount) {
        Ok(cmds) if cmds.is_empty() => {
            ToolResult::text("No space to remove").with_data(json!({ "commands": 0 }))
        }
        Ok(cmds) => {
            let n = cmds.len();
            history.execute_discrete(batch_or_single(cmds), &mut doc);
            ToolResult::text(format!("Removed space ({n} command(s))"))
                .with_data(json!({ "commands": n, "amount_ticks": amount.0 }))
        }
        Err(e) => map_edit_error(e),
    }
}

/// K-A3 Remove All Spaces After: pack unlocked tracks from `at` onward.
pub async fn remove_all_spaces_after(state: &AppState, args: SpaceAfterArgs) -> ToolResult {
    tracing::debug!(
        "tool: remove_all_spaces_after on sequence {}",
        args.sequence_id
    );
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq) = project.sequences.get(&args.sequence_id) else {
        return ToolResult::error(format!("sequence {} not found", args.sequence_id));
    };
    let at = match resolve_tick(
        args.at_ticks,
        args.at_tc.as_deref(),
        args.at_seconds,
        Some(seq.frame_rate),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    match ops::remove_all_spaces_after(project, args.sequence_id, at) {
        Ok(cmds) if cmds.is_empty() => {
            ToolResult::text("Already packed — nothing to do").with_data(json!({ "commands": 0 }))
        }
        Ok(cmds) => {
            let n = cmds.len();
            history.execute_discrete(batch_or_single(cmds), &mut doc);
            ToolResult::text(format!("Packed spaces after point ({n} track edit(s))"))
                .with_data(json!({ "commands": n }))
        }
        Err(e) => map_edit_error(e),
    }
}

/// K-A3 Remove All Clips After: delete clips with `start >= at` on unlocked tracks.
pub async fn remove_clips_after(state: &AppState, args: SpaceAfterArgs) -> ToolResult {
    tracing::debug!("tool: remove_clips_after on sequence {}", args.sequence_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq) = project.sequences.get(&args.sequence_id) else {
        return ToolResult::error(format!("sequence {} not found", args.sequence_id));
    };
    let at = match resolve_tick(
        args.at_ticks,
        args.at_tc.as_deref(),
        args.at_seconds,
        Some(seq.frame_rate),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    match ops::remove_clips_after(project, args.sequence_id, at) {
        Ok(cmds) if cmds.is_empty() => {
            ToolResult::text("No clips after the point").with_data(json!({ "removed": 0 }))
        }
        Ok(cmds) => {
            let n = cmds.len();
            history.execute_discrete(batch_or_single(cmds), &mut doc);
            ToolResult::text(format!("Removed {n} clip(s) after the point"))
                .with_data(json!({ "removed": n }))
        }
        Err(e) => map_edit_error(e),
    }
}

/// **Match Frame** (G-3, Premiere F): from `clip_id`, compute the source-media
/// tick that lines up with timeline position `at` (`source_in +
/// speed.source_delta(at - clip.start)`, exact-rational — mirrors the GUI's
/// `timeline_match_frame`). Read-only: no mutation, no undo step. `at` must
/// fall within `[clip.start, clip.end())`.
pub async fn match_frame(state: &AppState, args: MatchFrameArgs) -> ToolResult {
    tracing::debug!("tool: match_frame {}", args.clip_id);
    let doc = state.document.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let Some(clip) = find_clip(project, seq_id, track_id, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let fr = project.sequences.get(&seq_id).unwrap().frame_rate;
    let at = match resolve_tick(
        args.at_ticks,
        args.at_tc.as_deref(),
        args.at_seconds,
        Some(fr),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    if at < clip.start || at >= clip.end() {
        return err_code(
            "TickOutOfRange",
            "at must fall within the clip's timeline span [start, end)",
        );
    }
    let matched = clip.source_in + clip.speed.source_delta(at - clip.start);
    ToolResult::text(format!("Matched source tick {}", matched.0)).with_data(json!({
        "source_tick": matched.0,
        "asset_id": clip.source.asset(),
        "clip_name": clip.name,
    }))
}

/// **Adjustment-layer clip** (G-7): create a no-media `ClipSource::Adjustment`
/// clip spanning `[start, start+duration)` on `track_id`
/// (`ops::add_adjustment_clip`) — its effect stack/grade composites over
/// every lower track beneath its span (engine side, not this tool).
pub async fn insert_adjustment_clip(
    state: &AppState,
    args: InsertAdjustmentClipArgs,
) -> ToolResult {
    tracing::debug!("tool: insert_adjustment_clip on track {}", args.track_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq_id) = locate_track(project, args.track_id) else {
        return ToolResult::error(format!("track {} not found", args.track_id));
    };
    let fr = project.sequences.get(&seq_id).unwrap().frame_rate;
    let start = match resolve_tick(
        args.start_ticks,
        args.start_tc.as_deref(),
        args.start_seconds,
        Some(fr),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    if args.duration_ticks <= 0 {
        return err_code("TickOutOfRange", "duration_ticks must be > 0");
    }
    match ops::add_adjustment_clip(
        project,
        seq_id,
        args.track_id,
        start,
        Tick(args.duration_ticks),
    ) {
        Ok(cmd) => {
            let clip_id = match &cmd {
                TimelineCmd::InsertClip { clip, .. } => Some(clip.id),
                _ => None,
            };
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Inserted adjustment clip").with_data(json!({ "clip_id": clip_id }))
        }
        Err(e) => map_edit_error(e),
    }
}

/// **Title/text clip** (G-12): create a `ClipSource::Text` title/graphics
/// clip spanning `[start, start+duration)` on `track_id`
/// (`ops::add_text_clip`) — `style` patches `CaptionStyle::default()` via the
/// same partial-style vocabulary `set_caption_style` uses.
pub async fn insert_text_clip(state: &AppState, args: InsertTextClipArgs) -> ToolResult {
    tracing::debug!("tool: insert_text_clip on track {}", args.track_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq_id) = locate_track(project, args.track_id) else {
        return ToolResult::error(format!("track {} not found", args.track_id));
    };
    let fr = project.sequences.get(&seq_id).unwrap().frame_rate;
    let start = match resolve_tick(
        args.start_ticks,
        args.start_tc.as_deref(),
        args.start_seconds,
        Some(fr),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    if args.duration_ticks <= 0 {
        return err_code("TickOutOfRange", "duration_ticks must be > 0");
    }
    let style = match args.style {
        Some(patch) => match merge_caption_style(&CaptionStyle::default(), &patch) {
            Ok(s) => s,
            Err(e) => return e,
        },
        None => CaptionStyle::default(),
    };
    let content = TextClipContent {
        text: args.text,
        style,
    };
    match ops::add_text_clip(
        project,
        seq_id,
        args.track_id,
        start,
        Tick(args.duration_ticks),
        content,
    ) {
        Ok(cmd) => {
            let clip_id = match &cmd {
                TimelineCmd::InsertClip { clip, .. } => Some(clip.id),
                _ => None,
            };
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Inserted text clip").with_data(json!({ "clip_id": clip_id }))
        }
        Err(e) => map_edit_error(e),
    }
}

// ─── Clip properties (10 §3.5) ──────────────────────────────────────────────

pub async fn set_clip_prop(state: &AppState, args: SetClipPropArgs) -> ToolResult {
    tracing::debug!("tool: set_clip_prop {}", args.clip_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let Some(clip) = find_clip(project, seq_id, track_id, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let mut new_clip = clip.clone();
    if let Some(name) = args.name {
        new_clip.name = name;
    }
    if let Some(t) = args.transform {
        new_clip.transform.base = t;
    }
    if let Some(r) = args.reframe {
        match r.transform {
            Some(t) => {
                new_clip.reframe.insert(r.format_index, t);
            }
            None => {
                new_clip.reframe.remove(&r.format_index);
            }
        }
    }
    if let Some(en) = args.enabled {
        new_clip.enabled = en;
    }
    if let Some(label) = args.color_label {
        new_clip.color_label = label;
    }
    match ops::set_clip_prop(project, seq_id, track_id, new_clip) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Updated clip")
        }
        Err(e) => map_edit_error(e),
    }
}

// ─── Clip organization: color label & linking (14 §M-1/M-2, CAP-019 MCP
// parity) ────────────────────────────────────────────────────────────────

pub async fn link_clips(state: &AppState, args: LinkClipsArgs) -> ToolResult {
    tracing::debug!("tool: link_clips {} / {}", args.clip_id_a, args.clip_id_b);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_a, track_a)) = locate_clip(project, args.clip_id_a) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id_a));
    };
    let Some((seq_b, track_b)) = locate_clip(project, args.clip_id_b) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id_b));
    };
    if seq_a != seq_b {
        return ToolResult::error("link_clips requires both clips to be in the same sequence");
    }
    match ops::link_clips(
        project,
        seq_a,
        track_a,
        args.clip_id_a,
        track_b,
        args.clip_id_b,
    ) {
        Ok(cmds) => {
            history.execute_discrete(batch_or_single(cmds), &mut doc);
            ToolResult::text("Linked clips")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn unlink_clips(state: &AppState, args: UnlinkClipsArgs) -> ToolResult {
    tracing::debug!("tool: unlink_clips {}", args.clip_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    match ops::unlink_clip(project, seq_id, track_id, args.clip_id) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Unlinked clip")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn set_clip_speed(state: &AppState, args: SetClipSpeedArgs) -> ToolResult {
    tracing::debug!("tool: set_clip_speed {}", args.clip_id);
    if args.ratio.is_some() == args.keys.is_some() {
        return ToolResult::error(
            "supply exactly one of ratio (constant speed) or keys (keyframed ramp, G-11)",
        );
    }
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let Some(clip) = find_clip(project, seq_id, track_id, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let fr = project.sequences.get(&seq_id).unwrap().frame_rate;
    let speed = if let Some(ratio) = args.ratio {
        if ratio.den == 0 {
            return ToolResult::error("ratio.den must be > 0");
        }
        SpeedMap::Constant(Ratio::new(ratio.num, ratio.den))
    } else {
        let keys = args.keys.unwrap();
        if keys.is_empty() {
            return ToolResult::error("keys must have at least one control point");
        }
        let mut resolved = Vec::with_capacity(keys.len());
        for k in keys {
            if k.ratio.den == 0 {
                return ToolResult::error("ratio.den must be > 0");
            }
            let at = match resolve_tick(k.at_ticks, k.at_tc.as_deref(), k.at_seconds, Some(fr)) {
                Ok(t) => t,
                Err(e) => return e,
            };
            resolved.push(SpeedKey::eased(
                at,
                Ratio::new(k.ratio.num, k.ratio.den),
                k.interp.unwrap_or(photonic_core::timeline::Interp::Hold),
            ));
        }
        SpeedMap::Keyframed { keys: resolved }
    };
    match ops::set_speed_map(project, seq_id, track_id, clip.id, speed) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Updated clip speed")
        }
        Err(e) => map_edit_error(e),
    }
}

/// K-B14 Freeze frame: hold the source frame at clip-relative `at_*` for the
/// clip's whole duration via zero-rate `SpeedMap` (one undo step).
pub async fn freeze_frame(state: &AppState, args: FreezeFrameArgs) -> ToolResult {
    tracing::debug!("tool: freeze_frame {}", args.clip_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let Some(fr) = project.sequences.get(&seq_id).map(|s| s.frame_rate) else {
        return ToolResult::error(format!("sequence for clip {} not found", args.clip_id));
    };
    let at = if args.at_ticks.is_none() && args.at_tc.is_none() && args.at_seconds.is_none() {
        Tick::ZERO
    } else {
        match resolve_tick(
            args.at_ticks,
            args.at_tc.as_deref(),
            args.at_seconds,
            Some(fr),
        ) {
            Ok(t) => t,
            Err(e) => return e,
        }
    };
    match ops::freeze_frame(project, seq_id, track_id, args.clip_id, at) {
        Ok(Some(cmd)) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Froze clip at source frame")
        }
        Ok(None) => ToolResult::text("Clip already frozen at that frame"),
        Err(e) => map_edit_error(e),
    }
}

pub async fn set_transition(state: &AppState, args: SetTransitionArgs) -> ToolResult {
    tracing::debug!("tool: set_transition {}", args.clip_id);
    if let Some(t) = &args.transition {
        if t.duration_ticks <= 0 {
            return err_code("TickOutOfRange", "transition duration_ticks must be > 0");
        }
    }
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let Some(clip) = find_clip(project, seq_id, track_id, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let mut new_clip = clip.clone();
    let transition = args.transition.map(|t| Transition {
        kind: t.kind,
        duration: Tick(t.duration_ticks),
        params: t.params,
    });
    match args.edge {
        ClipEdge::In => new_clip.transition_in = transition,
        ClipEdge::Out => new_clip.transition_out = transition,
    }
    match ops::set_clip_prop(project, seq_id, track_id, new_clip) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Updated transition")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn list_clips(state: &AppState, args: ListClipsArgs) -> ToolResult {
    tracing::debug!("tool: list_clips");
    let doc = state.document.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::text("No timeline project yet").with_data(json!({ "clips": [] }));
    };
    let range = match (args.range_start_ticks, args.range_end_ticks) {
        (Some(s), Some(e)) => Some((Tick(s), Tick(e))),
        _ => None,
    };
    let mut out = Vec::new();
    for (sid, seq) in &project.sequences {
        if let Some(f) = args.sequence_id {
            if f != *sid {
                continue;
            }
        }
        for t in seq.video_tracks.iter().chain(seq.audio_tracks.iter()) {
            if let Some(f) = args.track_id {
                if f != t.id {
                    continue;
                }
            }
            for c in &t.clips {
                if let Some((rs, re)) = range {
                    if !(c.start < re && rs < c.end()) {
                        continue;
                    }
                }
                out.push(json!({
                    "clip_id": c.id,
                    "name": c.name,
                    "sequence_id": sid,
                    "track_id": t.id,
                    "start_ticks": c.start.0,
                    "duration_ticks": c.duration.0,
                    "enabled": c.enabled,
                }));
            }
        }
    }
    ToolResult::text(format!("{} clip(s)", out.len())).with_data(json!({ "clips": out }))
}

pub async fn get_clip(state: &AppState, args: GetClipArgs) -> ToolResult {
    tracing::debug!("tool: get_clip {}", args.clip_id);
    let doc = state.document.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let Some(clip) = find_clip(project, seq_id, track_id, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    ToolResult::text(format!("Clip \"{}\"", clip.name)).with_data(json!({
        "sequence_id": seq_id,
        "track_id": track_id,
        "clip": clip,
    }))
}

// ─── Effects (10 §3.6) ───────────────────────────────────────────────────────

pub async fn add_effect(state: &AppState, args: AddEffectArgs) -> ToolResult {
    tracing::debug!("tool: add_effect {}", args.clip_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let effect = ClipEffect::new(args.kind);
    match ops::add_effect(project, seq_id, track_id, args.clip_id, effect, args.index) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Added effect")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn remove_effect(state: &AppState, args: RemoveEffectArgs) -> ToolResult {
    tracing::debug!("tool: remove_effect {}", args.clip_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    match ops::remove_effect(project, seq_id, track_id, args.clip_id, args.effect_index) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Removed effect")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn reorder_effects(state: &AppState, args: ReorderEffectsArgs) -> ToolResult {
    tracing::debug!("tool: reorder_effects {}", args.clip_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    match ops::reorder_effects(project, seq_id, track_id, args.clip_id, args.new_order) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Reordered effects")
        }
        Err(e) => map_edit_error(e),
    }
}

/// Snake-case label for a [`ParamKind`], used in refusal messages.
fn param_kind_label(kind: photonic_core::timeline::ParamKind) -> &'static str {
    use photonic_core::timeline::ParamKind;
    match kind {
        ParamKind::Float => "float",
        ParamKind::Vec2 => "vec2",
        ParamKind::Color => "color",
        ParamKind::Bool => "bool",
        ParamKind::Enum(_) => "enum",
        ParamKind::Path => "path",
    }
}

/// True when `value`'s discriminant is the one a [`ParamKind`] accepts. `Path`
/// has no [`PropValue`](photonic_core::timeline::PropValue) counterpart, so it is
/// never satisfiable (no v1 effect declares a `Path` param).
fn param_kind_accepts(
    kind: photonic_core::timeline::ParamKind,
    value: &photonic_core::timeline::PropValue,
) -> bool {
    use photonic_core::timeline::{ParamKind, PropValue};
    matches!(
        (kind, value),
        (ParamKind::Float, PropValue::Float(_))
            | (ParamKind::Vec2, PropValue::Vec2(_))
            | (ParamKind::Color, PropValue::Color(_))
            | (ParamKind::Bool, PropValue::Bool(_))
            | (ParamKind::Enum(_), PropValue::Enum(_))
    )
}

/// Project one [`ParamSpec`](photonic_core::timeline::ParamSpec) to the JSON
/// shape `list_effect_kinds` emits.
fn param_spec_json(p: &photonic_core::timeline::ParamSpec) -> serde_json::Value {
    json!({
        "path": p.path,
        "kind": param_kind_label(p.kind),
        "default": p.default,
        "range": p.range,
        "animatable": p.animatable,
        "ui": format!("{:?}", p.ui),
        "group": p.group,
        "display": {
            "factor": p.display.factor,
            "offset": p.display.offset,
            "suffix": p.display.suffix,
            "decimals": p.display.decimals,
        },
    })
}

/// Write one param (or the literal path `"enabled"`) into a stacked effect,
/// with the manifest-driven validation of spec 30 §2.7: resolve the effect's
/// manifest, look up the `ParamSpec` by path, and refuse — never clamp — on an
/// unknown path, a value-kind mismatch, or an out-of-range value. A freshly
/// added effect carries `EffectId::EMPTY` until save, so fall back to its
/// kind's stable id; an inert / unknown-id effect has no manifest and refuses
/// every param write.
///
/// Shared by `set_effect_param` (clip stacks) and `effect_stack`
/// (track/master/asset stacks) so the two surfaces cannot drift apart on which
/// writes are legal.
fn write_effect_param(
    effect: &mut photonic_core::timeline::ClipEffect,
    path: &str,
    value: photonic_core::timeline::PropValue,
) -> Result<(), String> {
    if path == "enabled" {
        return match value {
            photonic_core::timeline::PropValue::Bool(b) => {
                effect.enabled = b;
                Ok(())
            }
            _ => Err("path \"enabled\" requires a bool value".to_string()),
        };
    }
    use photonic_core::timeline::effect_manifest;
    let effect_id = if effect.id.is_empty() {
        effect.kind.effect_id()
    } else {
        effect.id.clone()
    };
    let Some(manifest) = effect_manifest::manifest(effect_id.clone()) else {
        return Err(format!(
            "effect id {:?} has no manifest in this build; param writes are refused",
            effect_id.as_str()
        ));
    };
    let Some(spec) = manifest.params.iter().find(|p| p.path == path) else {
        return Err(format!(
            "unknown param path {:?} for effect {:?}",
            path,
            effect_id.as_str()
        ));
    };
    if !param_kind_accepts(spec.kind, &value) {
        return Err(format!(
            "param {:?} expects a {} value, got {:?}",
            path,
            param_kind_label(spec.kind),
            value.kind()
        ));
    }
    if let (Some((lo, hi)), photonic_core::timeline::PropValue::Float(v)) = (spec.range, value) {
        if !(lo..=hi).contains(&v) {
            return Err(format!(
                "param {path:?} value {v} is outside range {lo}..={hi} (refused, not clamped)"
            ));
        }
    }
    effect.params.base.set(path, value);
    Ok(())
}

pub async fn set_effect_param(state: &AppState, args: SetEffectParamArgs) -> ToolResult {
    tracing::debug!(
        "tool: set_effect_param {} [{}]",
        args.clip_id,
        args.effect_index
    );
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let Some(clip) = find_clip(project, seq_id, track_id, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let mut new_clip = clip.clone();
    let Some(effect) = new_clip.effects.get_mut(args.effect_index) else {
        return ToolResult::error(format!("effect index {} out of range", args.effect_index));
    };
    if let Err(e) = write_effect_param(effect, args.path.as_str(), args.value) {
        return ToolResult::error(e);
    }
    match ops::set_clip_prop(project, seq_id, track_id, new_clip) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Updated effect param")
        }
        Err(e) => map_edit_error(e),
    }
}

/// K-B3 set/clear effect zone on a clip stack entry (one undo step).
pub async fn set_effect_zone(state: &AppState, args: SetEffectZoneArgs) -> ToolResult {
    tracing::debug!(
        "tool: set_effect_zone {} [{}]",
        args.clip_id,
        args.effect_index
    );
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    if locate_clip(project, args.clip_id).is_none() {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    }
    let zone = if args.clear {
        None
    } else {
        match (args.start_ticks, args.end_ticks) {
            (Some(a), Some(b)) => Some((Tick(a), Tick(b))),
            (None, None) => {
                return ToolResult::error(
                    "supply start_ticks and end_ticks, or clear=true to remove the zone",
                );
            }
            _ => {
                return ToolResult::error("both start_ticks and end_ticks are required for a zone");
            }
        }
    };
    match ops::set_effect_zone(
        project,
        VfxOwner::Clip(args.clip_id),
        args.effect_index,
        zone,
    ) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text(if zone.is_some() {
                "Set effect zone"
            } else {
                "Cleared effect zone"
            })
        }
        Err(e) => map_edit_error(e),
    }
}

// ─── Scoped effect stacks — track / master / asset (26 §10 K-B1/K-B2) ────────

/// Resolve one [`VfxOwner`] from the four addressing fields `effect_stack`
/// publishes, or the refusal to send back. `master` defaults to the active
/// sequence, mirroring the audio master bus's "active sequence" rule (09 §10)
/// while still allowing an explicit `sequence_id`.
///
/// Shared with the K-B4 preset verbs (`effect_preset_save`/`_apply`) so the two
/// surfaces cannot drift on what a scope means or on which id each one needs.
fn resolve_owner_fields(
    project: &TimelineProject,
    scope: EffectScopeArg,
    clip_id: Option<ClipId>,
    track_id: Option<TrackId>,
    sequence_id: Option<SequenceId>,
    asset_id: Option<AssetId>,
) -> Result<VfxOwner, ToolResult> {
    match scope {
        EffectScopeArg::Clip => {
            let Some(clip) = clip_id else {
                return Err(ToolResult::error("scope=clip requires clip_id"));
            };
            if locate_clip(project, clip).is_none() {
                return Err(ToolResult::error(format!("clip {clip} not found")));
            }
            Ok(VfxOwner::Clip(clip))
        }
        EffectScopeArg::Track => {
            let Some(track) = track_id else {
                return Err(ToolResult::error("scope=track requires track_id"));
            };
            if locate_track(project, track).is_none() {
                return Err(ToolResult::error(format!("track {track} not found")));
            }
            Ok(VfxOwner::Track(track))
        }
        EffectScopeArg::Master => {
            let seq = match sequence_id.or(project.active_sequence) {
                Some(s) => s,
                None => {
                    return Err(ToolResult::error(
                        "scope=master requires sequence_id (no active sequence)",
                    ))
                }
            };
            if !project.sequences.contains_key(&seq) {
                return Err(ToolResult::error(format!("sequence {seq} not found")));
            }
            Ok(VfxOwner::Master(seq))
        }
        EffectScopeArg::Asset => {
            let Some(asset) = asset_id else {
                return Err(ToolResult::error("scope=asset requires asset_id"));
            };
            if !project.media.assets.contains_key(&asset) {
                return Err(ToolResult::error(format!("asset {asset} not found")));
            }
            Ok(VfxOwner::Asset(asset))
        }
    }
}

/// The [`VfxOwner`] an `effect_stack` call names.
fn resolve_vfx_owner(
    project: &TimelineProject,
    args: &EffectStackArgs,
) -> Result<VfxOwner, ToolResult> {
    resolve_owner_fields(
        project,
        args.scope,
        args.clip_id,
        args.track_id,
        args.sequence_id,
        args.asset_id,
    )
}

/// JSON projection of one stacked effect, the shape `effect_stack op=list`
/// returns and the shape an agent needs to address the next call by index.
fn stacked_effect_json(i: usize, e: &photonic_core::timeline::ClipEffect) -> serde_json::Value {
    let params: Vec<_> = e
        .params
        .base
        .entries
        .iter()
        .map(|(path, v)| json!({ "path": path.as_str(), "value": v }))
        .collect();
    json!({
        "index": i,
        "id": e.id.as_str(),
        "kind": e.kind,
        "enabled": e.enabled,
        "inert": e.inert,
        "params": params,
    })
}

/// The one automatable verb for the four video effect stacks (26 §10
/// K-B1/K-B2). `add`/`remove`/`reorder`/`set_param`/`set_grade` are each a
/// single undoable step; `list` is read-only.
pub async fn effect_stack(state: &AppState, args: EffectStackArgs) -> ToolResult {
    tracing::debug!("tool: effect_stack {:?} {:?}", args.scope, args.op);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let owner = match resolve_vfx_owner(project, &args) {
        Ok(o) => o,
        Err(e) => return e,
    };
    let stack = match ops::effect_stack(project, owner) {
        Ok(s) => s,
        Err(e) => return map_edit_error(e),
    };

    let cmd = match args.op {
        EffectStackOp::List => {
            let effects: Vec<_> = stack
                .iter()
                .enumerate()
                .map(|(i, e)| stacked_effect_json(i, e))
                .collect();
            let graded = ops::scope_grade(project, owner)
                .ok()
                .flatten()
                .map(|g| serde_json::to_value(g).unwrap_or(serde_json::Value::Null));
            return ToolResult::text(format!("{} effect(s)", effects.len())).with_data(json!({
                "scope": format!("{:?}", args.scope).to_lowercase(),
                "effects": effects,
                "grade": graded,
            }));
        }
        EffectStackOp::Add => {
            // Prefer the stable manifest id (the full K-B16 catalogue); fall
            // back to the seven legacy kinds so the older vocabulary still
            // works.
            let effect = match (&args.effect_id, args.kind) {
                (Some(id), _) => {
                    let eid = photonic_core::timeline::EffectId::new(id.clone());
                    match photonic_core::timeline::ClipEffect::from_manifest(eid) {
                        Some(e) => e,
                        None => {
                            return ToolResult::error(format!(
                                "unknown effect_id {id:?} — see list_effect_kinds"
                            ))
                        }
                    }
                }
                (None, Some(kind)) => photonic_core::timeline::ClipEffect::new(kind),
                (None, None) => {
                    return ToolResult::error("op=add requires effect_id (or legacy kind)")
                }
            };
            match ops::add_effect_scoped(project, owner, effect, args.index) {
                Ok(c) => c,
                Err(e) => return map_edit_error(e),
            }
        }
        EffectStackOp::Remove => {
            let Some(index) = args.index else {
                return ToolResult::error("op=remove requires index");
            };
            match ops::remove_effect_scoped(project, owner, index) {
                Ok(c) => c,
                Err(e) => return map_edit_error(e),
            }
        }
        EffectStackOp::Reorder => {
            let Some(new_order) = args.new_order.clone() else {
                return ToolResult::error("op=reorder requires new_order");
            };
            match ops::reorder_effects_scoped(project, owner, new_order) {
                Ok(c) => c,
                Err(e) => return map_edit_error(e),
            }
        }
        EffectStackOp::SetParam => {
            let (Some(index), Some(path), Some(value)) =
                (args.index, args.path.clone(), args.value)
            else {
                return ToolResult::error("op=set_param requires index, path and value");
            };
            let Some(effect) = stack.get(index) else {
                return ToolResult::error(format!("effect index {index} out of range"));
            };
            let mut new_effect = effect.clone();
            if let Err(e) = write_effect_param(&mut new_effect, path.as_str(), value) {
                return ToolResult::error(e);
            }
            match ops::set_effect_scoped(project, owner, index, new_effect) {
                Ok(c) => c,
                Err(e) => return map_edit_error(e),
            }
        }
        EffectStackOp::SetGrade => {
            let new_grade: Option<Grade> = match args.grade.clone() {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => match serde_json::from_value::<Grade>(v) {
                    Ok(g) => Some(g),
                    Err(e) => {
                        return ToolResult::error(format!(
                            "invalid grade object: {e} — see 07 §1 for the Grade serde shape"
                        ))
                    }
                },
            };
            match ops::set_grade_scoped(project, owner, new_grade) {
                Ok(c) => c,
                Err(e) => return map_edit_error(e),
            }
        }
    };
    let label = cmd.description();
    history.execute_discrete(Command::Timeline(cmd), &mut doc);
    ToolResult::text(label)
}

// ─── Effect presets, custom stacks and favourites (26 §10 K-B4) ──────────────
//
// The library is USER state in `<config>/Photonic/effect_presets.json`, not
// document state (see `photonic_core::timeline::effect_preset`'s module docs,
// which follow proposal 206). That single decision fixes the whole shape of
// this surface:
//
// * `effect_preset_apply` is the ONLY verb here that touches `CommandHistory`,
//   and it produces exactly ONE `Command::Batch` however many effects (or, for
//   `scope=clip`, however many clips) it lands on.
// * `_list`, `_save`, `_delete`, `_rename` and the two favourite verbs read and
//   write a config file and deliberately create no undo entry — the same
//   contract `save_export_preset`/`delete_export_preset` already ship, down to
//   refusing a built-in name with `NotSupportedV1`.

/// Test seam: when set, the preset verbs read and write this path instead of
/// the resolved config file.
#[cfg(test)]
static TEST_LIBRARY_PATH: StdMutex<Option<std::path::PathBuf>> = StdMutex::new(None);

/// `<config>/Photonic/effect_presets.json`.
#[cfg(not(test))]
fn preset_library_path() -> Result<std::path::PathBuf, ToolResult> {
    effect_preset::library_path().ok_or_else(|| {
        ToolResult::error(
            "could not resolve the app config directory — the effect preset library has nowhere \
             to live",
        )
    })
}

/// Under `cargo test` the developer's real library is NEVER touched: an
/// un-overridden path resolves to a per-process temp file that no test creates,
/// so even a test that forgets [`tests::TestLibrary`] cannot read, rewrite or
/// quarantine `<config>/Photonic/effect_presets.json`.
#[cfg(test)]
fn preset_library_path() -> Result<std::path::PathBuf, ToolResult> {
    if let Some(p) = TEST_LIBRARY_PATH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        return Ok(p);
    }
    Ok(std::env::temp_dir().join(format!(
        "photonic-mcp-unset-preset-library-{}.json",
        std::process::id()
    )))
}

fn load_preset_library() -> Result<effect_preset::LibraryLoad, ToolResult> {
    let path = preset_library_path()?;
    effect_preset::load_library_from(&path)
        .map_err(|e| ToolResult::error(format!("could not read the effect preset library: {e}")))
}

fn save_preset_library(library: &effect_preset::EffectPresetLibrary) -> Result<(), ToolResult> {
    let path = preset_library_path()?;
    effect_preset::save_library_to(&path, library)
        .map_err(|e| ToolResult::error(format!("could not persist the effect preset library: {e}")))
}

/// A built-in name is read-only, reported with the same `NotSupportedV1` shape
/// `save_export_preset` uses; everything else is a plain refusal.
fn map_preset_error(e: effect_preset::EffectPresetError) -> ToolResult {
    match e {
        effect_preset::EffectPresetError::BuiltInName(_) => {
            err_code("NotSupportedV1", e.to_string())
        }
        _ => ToolResult::error(e.to_string()),
    }
}

/// One-line human summary — what applying this preset would add.
fn preset_summary(p: &effect_preset::EffectPreset) -> String {
    let stack = if p.effects.is_empty() {
        "no effects".to_string()
    } else {
        p.effects
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>()
            .join(" → ")
    };
    if p.grade.is_some() {
        format!("{stack} + grade")
    } else {
        stack
    }
}

/// JSON projection of one catalogue entry. `unresolvable_effect_ids` is the
/// honest answer to "will this preset arrive whole on this build?" — entries
/// this build has no manifest for still apply, inert-and-preserved (39 §2.2),
/// they simply do not render.
fn preset_json(p: &effect_preset::EffectPreset) -> Value {
    json!({
        "name": p.name,
        "built_in": effect_preset::is_built_in(&p.name),
        "effect_ids": p.effects.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        "effect_count": p.effects.len(),
        "has_grade": p.grade.is_some(),
        "parameter_preset_for": p.parameter_preset_for().map(|id| id.as_str()),
        "unresolvable_effect_ids": p.inert_ids(),
        "summary": preset_summary(p),
    })
}

pub async fn effect_preset_list(_state: &AppState, _args: EffectPresetListArgs) -> ToolResult {
    tracing::debug!("tool: effect_preset_list");
    let load = match load_preset_library() {
        Ok(l) => l,
        Err(e) => return e,
    };
    let presets: Vec<Value> = load.library.catalogue().iter().map(preset_json).collect();
    let built_ins = presets
        .iter()
        .filter(|p| p["built_in"] == json!(true))
        .count();
    ToolResult::text(format!(
        "{} effect preset(s) ({built_ins} built-in)",
        presets.len()
    ))
    .with_data(json!({
        "presets": presets,
        "library_path": preset_library_path().ok().map(|p| p.display().to_string()),
        // Set when the stored file would not parse and was moved aside rather
        // than overwritten (206 §4.2 rule 5) — the user lost nothing, but they
        // deserve to be told once.
        "quarantined": load.quarantined.map(|p| p.display().to_string()),
    }))
}

pub async fn effect_preset_save(state: &AppState, args: EffectPresetSaveArgs) -> ToolResult {
    tracing::debug!("tool: effect_preset_save {:?} {:?}", args.name, args.scope);
    // Deliberately no history lock: this writes a config file, never the
    // document, and must not produce an undo entry.
    let (stack, grade) = {
        let doc = state.document.lock().await;
        let Some(project) = doc.timeline.as_ref() else {
            return ToolResult::error("no timeline project");
        };
        let owner = match resolve_owner_fields(
            project,
            args.scope,
            args.clip_id,
            args.track_id,
            args.sequence_id,
            args.asset_id,
        ) {
            Ok(o) => o,
            Err(e) => return e,
        };
        let stack = match ops::effect_stack(project, owner) {
            Ok(s) => s.to_vec(),
            Err(e) => return map_edit_error(e),
        };
        let grade = match ops::scope_grade(project, owner) {
            Ok(g) => g.cloned(),
            Err(e) => return map_edit_error(e),
        };
        (stack, grade)
    };
    let preset = effect_preset::EffectPreset::new(args.name.clone(), stack, grade);
    let summary = preset_json(&preset);
    let summary_line = preset_summary(&preset);
    let mut load = match load_preset_library() {
        Ok(l) => l,
        Err(e) => return e,
    };
    let replaced = load.library.presets.iter().any(|p| p.name == args.name);
    if let Err(e) = load.library.upsert(preset) {
        return map_preset_error(e);
    }
    if let Err(e) = save_preset_library(&load.library) {
        return e;
    }
    ToolResult::text(format!(
        "{} effect preset {:?} — {} (no undo step: this is app config, not the document)",
        if replaced { "updated" } else { "saved" },
        args.name,
        summary_line,
    ))
    .with_data(json!({ "preset": summary, "replaced": replaced }))
}

pub async fn effect_preset_apply(state: &AppState, args: EffectPresetApplyArgs) -> ToolResult {
    tracing::debug!("tool: effect_preset_apply {:?} {:?}", args.name, args.scope);
    // Read the library before taking the document lock — file IO under the
    // document lock would be pure contention.
    let load = match load_preset_library() {
        Ok(l) => l,
        Err(e) => return e,
    };
    let Some(preset) = load.library.get(&args.name) else {
        return ToolResult::error(format!(
            "no effect preset named {:?} — see effect_preset_list",
            args.name
        ));
    };
    if preset.effects.is_empty() && preset.grade.is_none() {
        return ToolResult::error(format!(
            "preset {:?} is empty — nothing to apply",
            args.name
        ));
    }
    // Taking `clip_ids` and silently ignoring `clip_id` would apply to a
    // different set of clips than the caller wrote down.
    if args.clip_ids.is_some() && args.clip_id.is_some() {
        return ToolResult::error("give clip_id or clip_ids, not both");
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };

    // Resolve EVERY target before building any command: an unknown id refuses
    // the whole call rather than half-applying, the same no-partial rule
    // `paste_attributes` follows.
    let owners: Vec<VfxOwner> = match (args.scope, args.clip_ids.as_ref()) {
        (EffectScopeArg::Clip, Some(ids)) => {
            if ids.is_empty() {
                return ToolResult::error("clip_ids must not be empty");
            }
            let mut owners = Vec::with_capacity(ids.len());
            for &clip in ids {
                // A repeat id would apply the preset twice to one stack while
                // reporting one target; drop the duplicate instead.
                if owners.contains(&VfxOwner::Clip(clip)) {
                    continue;
                }
                match resolve_owner_fields(
                    project,
                    EffectScopeArg::Clip,
                    Some(clip),
                    None,
                    None,
                    None,
                ) {
                    Ok(o) => owners.push(o),
                    Err(e) => return e,
                }
            }
            owners
        }
        _ => {
            if args.clip_ids.is_some() {
                return ToolResult::error("clip_ids is only valid with scope=clip");
            }
            match resolve_owner_fields(
                project,
                args.scope,
                args.clip_id,
                args.track_id,
                args.sequence_id,
                args.asset_id,
            ) {
                Ok(o) => vec![o],
                Err(e) => return e,
            }
        }
    };

    let mut cmds = Vec::new();
    for owner in &owners {
        match effect_preset::apply_commands(project, *owner, &preset) {
            Ok(mut c) => cmds.append(&mut c),
            Err(e) => return map_edit_error(e),
        }
    }
    if cmds.is_empty() {
        return ToolResult::error(format!("preset {:?} produced no edit", args.name));
    }
    let steps = cmds.len();
    // ONE `Command::Batch`, whatever the preset's size — a one-effect preset is
    // a one-member batch rather than a different carrier, so the undo count
    // never depends on how big the preset is.
    history.execute_discrete(
        Command::Batch(cmds.into_iter().map(Command::Timeline).collect()),
        &mut doc,
    );
    let unresolvable = preset.inert_ids();
    ToolResult::text(format!(
        "applied preset {:?} to {} scope(s) as ONE undo step",
        args.name,
        owners.len()
    ))
    .with_data(json!({
        "name": preset.name,
        "targets": owners.len(),
        "commands": steps,
        "unresolvable_effect_ids": unresolvable,
    }))
}

pub async fn effect_preset_delete(_state: &AppState, args: EffectPresetDeleteArgs) -> ToolResult {
    tracing::debug!("tool: effect_preset_delete {:?}", args.name);
    let mut load = match load_preset_library() {
        Ok(l) => l,
        Err(e) => return e,
    };
    if let Err(e) = load.library.remove(&args.name) {
        return map_preset_error(e);
    }
    if let Err(e) = save_preset_library(&load.library) {
        return e;
    }
    ToolResult::text(format!(
        "deleted effect preset {:?} (no undo step: this is app config, not the document)",
        args.name
    ))
}

pub async fn effect_preset_rename(_state: &AppState, args: EffectPresetRenameArgs) -> ToolResult {
    tracing::debug!(
        "tool: effect_preset_rename {:?} -> {:?}",
        args.from,
        args.to
    );
    let mut load = match load_preset_library() {
        Ok(l) => l,
        Err(e) => return e,
    };
    if let Err(e) = load.library.rename(&args.from, &args.to) {
        return map_preset_error(e);
    }
    if let Err(e) = save_preset_library(&load.library) {
        return e;
    }
    ToolResult::text(format!(
        "renamed effect preset {:?} to {:?} (no undo step: this is app config, not the document)",
        args.from, args.to
    ))
}

/// Is `id` an effect this build actually has a manifest for?
fn effect_id_is_known(id: &str) -> bool {
    photonic_core::timeline::manifest(photonic_core::timeline::EffectId::new(id.to_string()))
        .is_some()
}

pub async fn effect_favourite_list(
    _state: &AppState,
    _args: EffectFavouriteListArgs,
) -> ToolResult {
    tracing::debug!("tool: effect_favourite_list");
    let load = match load_preset_library() {
        Ok(l) => l,
        Err(e) => return e,
    };
    let favourites: Vec<Value> = load
        .library
        .favourites
        .iter()
        .map(|id| {
            let m = photonic_core::timeline::manifest(photonic_core::timeline::EffectId::new(
                id.clone(),
            ));
            json!({
                "id": id,
                // An id this build has no manifest for stays in the user's
                // ordering untouched (39 §2.2) and is simply not offered.
                "available": m.is_some(),
                "name": m.map(|m| m.name),
            })
        })
        .collect();
    ToolResult::text(format!("{} favourite effect(s)", favourites.len()))
        .with_data(json!({ "favourites": favourites }))
}

pub async fn effect_favourite_set(_state: &AppState, args: EffectFavouriteSetArgs) -> ToolResult {
    tracing::debug!(
        "tool: effect_favourite_set {:?} {}",
        args.id,
        args.favourite
    );
    // Starring an id this build cannot resolve is a typo, not a portability
    // story — refuse it. UNstarring one must still work, so a user who opened
    // their library on a build with more effects can prune it here.
    if args.favourite && !effect_id_is_known(&args.id) {
        return ToolResult::error(format!(
            "unknown effect id {:?} — see list_effect_kinds",
            args.id
        ));
    }
    let mut load = match load_preset_library() {
        Ok(l) => l,
        Err(e) => return e,
    };
    if load.library.is_favourite(&args.id) == args.favourite {
        return ToolResult::text(format!(
            "effect {:?} is already {}",
            args.id,
            if args.favourite {
                "a favourite"
            } else {
                "not a favourite"
            }
        ));
    }
    load.library.toggle_favourite(&args.id);
    if let Err(e) = save_preset_library(&load.library) {
        return e;
    }
    ToolResult::text(format!(
        "{} effect {:?} (no undo step: this is app config, not the document)",
        if args.favourite {
            "favourited"
        } else {
            "un-favourited"
        },
        args.id
    ))
}

// ─── Paste Attributes (26 §10 K-B15) ─────────────────────────────────────────

/// **Paste Attributes**: stamp one clip's look — effect stack, grade,
/// transform, clip audio — onto N already-existing clips as ONE undo step.
/// Deliberately distinct from the clip clipboard: no clip is created, moved,
/// retimed or re-sourced. See `ops::paste_clip_attributes` for the exact
/// carried/excluded field list and why each call was made.
pub async fn paste_attributes(state: &AppState, args: PasteAttributesArgs) -> ToolResult {
    tracing::debug!(
        "tool: paste_attributes {} -> {} target(s)",
        args.source_clip_id,
        args.target_clip_ids.len()
    );
    if args.target_clip_ids.is_empty() {
        return ToolResult::error("target_clip_ids must not be empty");
    }
    let sel = match args.attributes.as_deref() {
        None => ops::AttrSelector::ALL,
        Some([]) => {
            return ToolResult::error(
                "attributes must not be an empty array — omit it to paste all four \
                 (effects, grade, transform, audio)",
            )
        }
        Some(list) => {
            let mut s = ops::AttrSelector {
                effects: false,
                grade: false,
                transform: false,
                audio: false,
            };
            for a in list {
                match a {
                    ClipAttributeArg::Effects => s.effects = true,
                    ClipAttributeArg::Grade => s.grade = true,
                    ClipAttributeArg::Transform => s.transform = true,
                    ClipAttributeArg::Audio => s.audio = true,
                }
            }
            s
        }
    };

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let attrs = match ops::clip_attributes(project, args.source_clip_id) {
        Ok(a) => a,
        Err(e) => return map_edit_error(e),
    };
    let cmds = match ops::paste_clip_attributes(project, &attrs, &args.target_clip_ids, sel) {
        Ok(c) => c,
        Err(e) => return map_edit_error(e),
    };

    // Which targets actually changed — the op drops no-op targets, so report
    // them explicitly rather than letting an agent infer success from silence.
    let updated: Vec<String> = cmds
        .iter()
        .filter_map(|c| match c {
            photonic_core::timeline::TimelineCmd::SetClipProp { new, .. } => {
                Some(new.id.to_string())
            }
            _ => None,
        })
        .collect();
    let skipped: Vec<String> = args
        .target_clip_ids
        .iter()
        .map(|c| c.to_string())
        .filter(|c| !updated.contains(c))
        .collect();

    let mut families: Vec<&str> = Vec::new();
    if sel.effects {
        families.push("effects");
    }
    if sel.grade {
        families.push("grade");
    }
    if sel.transform {
        families.push("transform");
    }
    if sel.audio {
        families.push("audio");
    }

    let n = cmds.len();
    if n > 0 {
        // ONE user verb = ONE undo unit, even across a multi-selection. Safe as
        // a plain Batch because none of the pasted fields is read by
        // `Sequence::validate()`, which `TimelineCmd::apply` debug-asserts
        // after every batch member (see the op's module comment).
        history.execute_discrete(
            Command::Batch(cmds.into_iter().map(Command::Timeline).collect()),
            &mut doc,
        );
    }
    ToolResult::text(format!(
        "Pasted attributes onto {n} clip(s) ({} unchanged)",
        skipped.len()
    ))
    .with_data(json!({
        "source_clip_id": args.source_clip_id.to_string(),
        "attributes": families,
        "updated": updated,
        "skipped": skipped,
    }))
}

pub async fn list_effect_kinds(_state: &AppState, _args: ListEffectKindsArgs) -> ToolResult {
    tracing::debug!("tool: list_effect_kinds");
    // Registry introspection driven entirely by the effect manifest catalogue
    // (spec 30 §2.7) — no second hand-maintained copy of the effect list. Each
    // entry carries the manifest's identity plus its full per-param spec so an
    // agent can discover params (and their ranges) without guessing.
    use photonic_core::timeline::effect_manifest;
    let out: Vec<_> = effect_manifest::manifests()
        .iter()
        .map(|m| {
            let params: Vec<_> = m.params.iter().map(param_spec_json).collect();
            json!({
                // Legacy EffectKind tag, retained one format version for existing
                // consumers; `null` for a non-legacy (future) manifest.
                "kind": m.id.legacy_kind(),
                "id": m.id.as_str(),
                "version": m.version,
                "name": m.name,
                "category": format!("{:?}", m.category),
                "arity": m.arity,
                "params": params,
            })
        })
        .collect();
    ToolResult::text(format!("{} effect(s)", out.len())).with_data(json!({ "effect_kinds": out }))
}

// ─── Keyframes (10 §3.7) ─────────────────────────────────────────────────────

fn to_anim_target(arg: &AnimTargetArg) -> AnimTarget {
    match arg {
        AnimTargetArg::ClipTransform { clip_id } => AnimTarget::ClipTransform { clip: *clip_id },
        AnimTargetArg::ClipEffect {
            clip_id,
            effect_index,
        } => AnimTarget::ClipEffect {
            clip: *clip_id,
            effect_index: *effect_index,
        },
    }
}

/// Read-only resolution of a target's owning clip and property-track lane.
fn read_target<'a>(
    project: &'a TimelineProject,
    target: &AnimTargetArg,
) -> Result<(&'a Clip, &'a [photonic_core::timeline::PropertyTrack]), ToolResult> {
    let clip_id = target.clip_id();
    let Some((seq_id, track_id)) = locate_clip(project, clip_id) else {
        return Err(ToolResult::error(format!("clip {clip_id} not found")));
    };
    let clip = find_clip(project, seq_id, track_id, clip_id)
        .ok_or_else(|| ToolResult::error(format!("clip {clip_id} not found")))?;
    match target {
        AnimTargetArg::ClipTransform { .. } => Ok((clip, clip.transform.tracks.as_slice())),
        AnimTargetArg::ClipEffect { effect_index, .. } => {
            let Some(effect) = clip.effects.get(*effect_index) else {
                return Err(ToolResult::error(format!(
                    "effect index {effect_index} out of range"
                )));
            };
            Ok((clip, effect.params.tracks.as_slice()))
        }
    }
}

fn target_frame_rate(project: &TimelineProject, clip_id: ClipId) -> Option<FrameRate> {
    let (seq_id, _) = locate_clip(project, clip_id)?;
    project.sequences.get(&seq_id).map(|s| s.frame_rate)
}

pub async fn set_keyframe(state: &AppState, args: SetKeyframeArgs) -> ToolResult {
    tracing::debug!("tool: set_keyframe {}", args.target.clip_id());
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    if let Err(e) = read_target(project, &args.target) {
        return e;
    }
    let fr = target_frame_rate(project, args.target.clip_id());
    let at = match resolve_tick(args.at_ticks, args.at_tc.as_deref(), args.at_seconds, fr) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let target = to_anim_target(&args.target);
    let kf = Keyframe::new(at, args.value, args.interp);
    let cmd = ops::set_keyframe(project, target, PropPath::new(args.path), kf);
    history.execute_discrete(Command::Timeline(cmd), &mut doc);
    ToolResult::text("Set keyframe")
}

pub async fn remove_keyframe(state: &AppState, args: RemoveKeyframeArgs) -> ToolResult {
    tracing::debug!("tool: remove_keyframe {}", args.target.clip_id());
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    if let Err(e) = read_target(project, &args.target) {
        return e;
    }
    let fr = target_frame_rate(project, args.target.clip_id());
    let at = match resolve_tick(args.at_ticks, args.at_tc.as_deref(), args.at_seconds, fr) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let target = to_anim_target(&args.target);
    match ops::remove_keyframe(project, target, PropPath::new(args.path), at) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Removed keyframe")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn batch_set_keyframes(state: &AppState, args: BatchSetKeyframesArgs) -> ToolResult {
    tracing::debug!("tool: batch_set_keyframes ({} ops)", args.ops.len());
    if args.ops.is_empty() {
        return ToolResult::error("ops must not be empty");
    }
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let mut cmds = Vec::with_capacity(args.ops.len());
    for op in &args.ops {
        if let Err(e) = read_target(project, &op.target) {
            return e;
        }
        let fr = target_frame_rate(project, op.target.clip_id());
        let at = match resolve_tick(op.at_ticks, op.at_tc.as_deref(), op.at_seconds, fr) {
            Ok(t) => t,
            Err(e) => return e,
        };
        let target = to_anim_target(&op.target);
        let kf = Keyframe::new(at, op.value, op.interp);
        cmds.push(Command::Timeline(ops::set_keyframe(
            project,
            target,
            PropPath::new(op.path.clone()),
            kf,
        )));
    }
    let n = cmds.len();
    history.execute_discrete(Command::Batch(cmds), &mut doc);
    ToolResult::text(format!("Set {n} keyframe(s)"))
}

pub async fn get_keyframes(state: &AppState, args: GetKeyframesArgs) -> ToolResult {
    tracing::debug!("tool: get_keyframes {}", args.target.clip_id());
    let doc = state.document.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    match read_target(project, &args.target) {
        Ok((_, tracks)) => ToolResult::text(format!("{} property track(s)", tracks.len()))
            .with_data(json!({ "tracks": tracks })),
        Err(e) => e,
    }
}

/// K-B11: snapshot keyframe tracks into a serializable clipboard payload.
pub async fn copy_keyframes(state: &AppState, args: CopyKeyframesArgs) -> ToolResult {
    tracing::debug!("tool: copy_keyframes {}", args.target.clip_id());
    let doc = state.document.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    if let Err(e) = read_target(project, &args.target) {
        return e;
    }
    let target = to_anim_target(&args.target);
    let paths: Option<Vec<PropPath>> = args
        .paths
        .as_ref()
        .map(|ps| ps.iter().map(|p| PropPath::new(p.clone())).collect());
    let path_slice = paths.as_deref();
    match ops::copy_keyframes(project, &target, path_slice) {
        Ok(board) => {
            let n = board.tracks.len();
            let keys: usize = board.tracks.iter().map(|t| t.keyframes.len()).sum();
            ToolResult::text(format!("Copied {n} track(s), {keys} keyframe(s)"))
                .with_data(json!({ "clipboard": board }))
        }
        Err(e) => map_edit_error(e),
    }
}

/// K-B11: paste a keyframe clipboard onto a target (one undo batch).
pub async fn paste_keyframes(state: &AppState, args: PasteKeyframesArgs) -> ToolResult {
    tracing::debug!("tool: paste_keyframes {}", args.target.clip_id());
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    if let Err(e) = read_target(project, &args.target) {
        return e;
    }
    let target = to_anim_target(&args.target);
    let mapping: Vec<(PropPath, PropPath)> = args
        .mapping
        .iter()
        .map(|m| (PropPath::new(m.from.clone()), PropPath::new(m.to.clone())))
        .collect();
    let cmds = if let Some(anchor) = args.reanchor_ticks {
        ops::paste_keyframes_reanchored(project, target, &args.clipboard, &mapping, Tick(anchor))
    } else {
        ops::paste_keyframes(
            project,
            target,
            &args.clipboard,
            &mapping,
            Tick(args.offset_ticks),
        )
    };
    match cmds {
        Ok(cmds) if cmds.is_empty() => ToolResult::text("Nothing to paste"),
        Ok(cmds) => {
            let n = cmds.len();
            history.execute_discrete(
                Command::Batch(cmds.into_iter().map(Command::Timeline).collect()),
                &mut doc,
            );
            ToolResult::text(format!("Pasted {n} keyframe(s)"))
        }
        Err(e) => map_edit_error(e),
    }
}

// ─── Media (P2 subset: import/relink/list/remove) ───────────────────────────

fn guess_asset_kind(path: &std::path::Path) -> Option<AssetKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" => AssetKind::Video,
        "mp3" | "wav" | "aac" | "flac" | "ogg" | "m4a" => AssetKind::Audio,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif" => AssetKind::Image,
        "svg" | "photon" => AssetKind::VectorDoc,
        "cube" => AssetKind::Lut3d,
        _ => return None,
    })
}

/// The content identity written for newly imported media: the engine's `xxh3`
/// head+tail+len digest (`photonic_video::media::content_hash`).
///
/// This is deliberately the *same* function `probe_media` and the GUI import
/// worker call. Before K-C6 this tool wrote the [`legacy_p2_content_hash`]
/// stopgap instead, so an MCP-imported asset's hash was not comparable with a
/// GUI-imported one — and hash-based relink matching across the two surfaces
/// could never fire. Hashes already stored in older documents keep working:
/// [`hash_like`] recomputes whichever algorithm produced the stored value.
fn content_hash(path: &std::path::Path) -> Option<String> {
    photonic_video::media::content_hash(path).ok()
}

/// The P2 stopgap identity (head+tail+len, `DefaultHasher`/SipHash), retained
/// **only** so a `siphash64:`-prefixed hash written by an older build can still
/// be verified against a file today. Never written for new imports.
fn legacy_p2_content_hash(path: &std::path::Path) -> Option<String> {
    use std::hash::{Hash, Hasher};
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    const CHUNK: u64 = 4096;
    let head_len = CHUNK.min(len) as usize;
    let mut head = vec![0u8; head_len];
    f.read_exact(&mut head).ok()?;
    let mut tail = Vec::new();
    if len > CHUNK {
        let tail_len = CHUNK.min(len);
        f.seek(SeekFrom::End(-(tail_len as i64))).ok()?;
        tail = vec![0u8; tail_len as usize];
        f.read_exact(&mut tail).ok()?;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    len.hash(&mut hasher);
    head.hash(&mut hasher);
    tail.hash(&mut hasher);
    Some(format!("siphash64:{:016x}", hasher.finish()))
}

/// Hash `path` with **the same algorithm that produced `stored`**, or `None`
/// when that algorithm is unrecognized.
///
/// This is what `ops::plan_relink` calls to verify a relink candidate. Hashing
/// with a different algorithm than the stored value would report a mismatch for
/// every asset, which would train a user (or an agent) to pass
/// `allow_hash_mismatch` reflexively and defeat the only guard that catches a
/// relink to the wrong take. `None` (→ `RelinkHashCheck::Unknown`) is the honest
/// answer for an identity we cannot reproduce.
fn hash_like(stored: Option<&str>, path: &std::path::Path) -> Option<String> {
    match stored {
        // No recorded identity: hash in the current algorithm so the relink can
        // record one.
        None => content_hash(path),
        Some(s) if s.starts_with("siphash64:") => legacy_p2_content_hash(path),
        // xxh3-64 renders as 16 bare hex chars (photonic-video `content_hash`).
        Some(s) if s.len() == 16 && s.chars().all(|c| c.is_ascii_hexdigit()) => content_hash(path),
        Some(_) => None,
    }
}

/// Walk `root` for files that could be relink candidates.
///
/// Bounded on purpose: a user pointed at `/` should get an answer, not a
/// traversal. Symlinked directories are not followed (a self-referential link
/// would otherwise be an infinite walk), the depth is capped, and the file count
/// is capped — `truncated` is reported so the caller never mistakes a truncated
/// scan for "no candidate exists".
fn scan_relink_candidates(
    root: &std::path::Path,
    recursive: bool,
    hash_files: bool,
) -> (Vec<ops::RelinkCandidate>, bool) {
    const MAX_DEPTH: usize = 8;
    const MAX_FILES: usize = 20_000;
    /// Above this many candidates the scan stops hashing: by-hash *discovery*
    /// (finding a renamed file) then does not fire, while per-entry
    /// verification still does — it only hashes the one file it chose.
    const MAX_HASHED: usize = 4_096;

    let mut out: Vec<ops::RelinkCandidate> = Vec::new();
    let mut truncated = false;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if out.len() >= MAX_FILES {
                truncated = true;
                break;
            }
            let path = entry.path();
            // `metadata()` follows symlinks; `file_type()` does not — use the
            // latter so a directory symlink is never descended into.
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                if recursive && depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                } else if recursive {
                    truncated = true;
                }
            } else if ft.is_file() {
                out.push(ops::RelinkCandidate {
                    path,
                    content_hash: None,
                });
            }
        }
        if out.len() >= MAX_FILES {
            truncated = true;
            break;
        }
    }
    // Deterministic order regardless of directory iteration order.
    out.sort_by(|a, b| a.path.cmp(&b.path));
    if hash_files && out.len() <= MAX_HASHED {
        for c in out.iter_mut() {
            c.content_hash = content_hash(&c.path);
        }
    }
    (out, truncated)
}

/// JSON row for one planned/committed relink.
fn relink_entry_json(e: &ops::RelinkPlanEntry) -> Value {
    json!({
        "asset_id": e.asset,
        "old_path": e.old_path.display().to_string(),
        "new_path": e.new_path.display().to_string(),
        "matched_by": e.matched_by.as_str(),
        "hash": e.hash.as_str(),
        "ambiguous": e.ambiguous,
    })
}

pub async fn import_media(state: &AppState, args: ImportMediaArgs) -> ToolResult {
    tracing::debug!("tool: import_media ({} path(s))", args.paths.len());
    if args.paths.is_empty() {
        return ToolResult::error("paths must not be empty");
    }
    // Validate + build each asset first (no project state needed for this part).
    let mut pending = Vec::new();
    for p in &args.paths {
        let path = std::path::PathBuf::from(p);
        let path =
            match crate::path_guard::check_path(state, &path, photonic_core::PathAccess::Read) {
                Ok(p) => p,
                Err(e) => return e,
            };
        let Some(kind) = guess_asset_kind(&path) else {
            return ToolResult::error(format!(
                "cannot infer media kind for {p:?} — unrecognized extension"
            ));
        };
        if !path.exists() {
            return err_code("AssetOffline", format!("file not found: {p}"));
        }
        let hash = content_hash(&path);
        let mut asset = photonic_core::timeline::MediaAsset::from_file(kind, path.clone());
        asset.content_hash = hash;
        pending.push((p.clone(), asset));
    }

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let mut cmds = Vec::new();
    let needs_project = doc.timeline.is_none();
    if needs_project {
        cmds.push(Command::Timeline(ops::create_project()));
    }

    // Resolve (or create) the target bin by exact name, then file every
    // imported asset under it directly at construction — cheaper and avoids
    // a chicken-and-egg AssignAssetBin-before-AddAsset ordering problem.
    let bin_id = match &args.bin {
        None => None,
        Some(bin_name) => {
            let existing = if needs_project {
                None
            } else {
                doc.timeline
                    .as_ref()
                    .and_then(|p| find_bin_by_name(p, bin_name))
                    .map(|b| b.id)
            };
            Some(existing.unwrap_or_else(|| {
                let cmd = ops::create_bin(bin_name.clone(), None);
                let id = match &cmd {
                    TimelineCmd::AddBin { bin } => bin.id,
                    _ => unreachable!("ops::create_bin always returns AddBin"),
                };
                cmds.push(Command::Timeline(cmd));
                id
            }))
        }
    };

    let mut created = Vec::new();
    for (p, mut asset) in pending {
        asset.bin = bin_id;
        let asset_id = asset.id;
        let kind = asset.kind;
        cmds.push(Command::Timeline(ops::add_asset(asset)));
        created.push(json!({
            "asset_id": asset_id, "path": p, "kind": kind, "probed": false, "bin_id": bin_id
        }));
    }
    history.execute_discrete(Command::Batch(cmds), &mut doc);

    ToolResult::text(format!(
        "Imported {} asset(s) — probing lands in P3 (ffprobe integration)",
        created.len()
    ))
    .with_data(json!({ "assets": created }))
}

/// Repoint one asset at a new file (26 K-C6).
///
/// Two guards were added when the batch flow landed, because a relink to the
/// *wrong bytes* is a data-integrity failure the user does not notice until
/// export:
///
/// * the new path must exist (`AssetOffline` otherwise — relinking to a missing
///   file has never been anything but a typo), and
/// * if the asset carries a `content_hash` and the new file's hash differs, the
///   call is refused with `HashMismatch` unless `allow_hash_mismatch: true`.
///   Accepting the change re-identifies the asset in the same undo step (new
///   hash recorded, stale `probe` dropped — it described the old bytes).
pub async fn relink_media(state: &AppState, args: RelinkMediaArgs) -> ToolResult {
    tracing::debug!("tool: relink_media {}", args.asset_id);
    let new_path =
        match crate::path_guard::check_path(state, &args.new_path, photonic_core::PathAccess::Read)
        {
            Ok(p) => p,
            Err(e) => return e,
        };
    if !new_path.exists() {
        return err_code("AssetOffline", format!("file not found: {}", args.new_path));
    }
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(asset) = project.media.assets.get(&args.asset_id) else {
        return map_edit_error(photonic_core::timeline::ops::EditError::NoAsset(
            args.asset_id,
        ));
    };
    let stored = asset.content_hash.clone();
    let actual = hash_like(stored.as_deref(), &new_path);
    let mismatch = matches!((&stored, &actual), (Some(s), Some(a)) if s != a);
    if mismatch && !args.allow_hash_mismatch {
        return err_code(
            "HashMismatch",
            format!(
                "{} does not hold this asset's bytes (recorded {}, found {}). \
                 Relinking anyway rebinds every clip to different media — pass \
                 allow_hash_mismatch: true to accept it.",
                args.new_path,
                stored.as_deref().unwrap_or("-"),
                actual.as_deref().unwrap_or("-"),
            ),
        );
    }
    let mut cmds = match ops::relink_asset(project, args.asset_id, new_path) {
        Ok(cmd) => vec![Command::Timeline(cmd)],
        Err(e) => return map_edit_error(e),
    };
    if mismatch {
        // Byte change accepted: record the new identity and drop the probe,
        // which described the file we just stopped pointing at.
        if let Ok(meta) = ops::set_asset_meta(project, args.asset_id, None, actual.clone()) {
            cmds.push(Command::Timeline(meta));
        }
    } else if stored.is_none() {
        if let Ok(meta) =
            ops::set_asset_meta(project, args.asset_id, asset.probe.clone(), actual.clone())
        {
            cmds.push(Command::Timeline(meta));
        }
    }
    let one_step = if cmds.len() == 1 {
        cmds.remove(0)
    } else {
        Command::Batch(cmds)
    };
    history.execute_discrete(one_step, &mut doc);
    ToolResult::text(if mismatch {
        "Relinked asset to different bytes — content hash updated, probe cleared (re-run probe_media)"
    } else {
        "Relinked asset"
    })
    .with_data(json!({
        "asset_id": args.asset_id,
        "new_path": args.new_path,
        "hash": match (&stored, &actual) {
            (Some(_), None) => "unknown",
            (None, _) => "unknown",
            _ if mismatch => "mismatch",
            _ => "match",
        },
    }))
}

/// Every offline asset in the pool (26 K-C6) — the inventory a relink flow
/// starts from, and the "project open reports what is missing" surface.
pub async fn find_offline_media(state: &AppState, _args: FindOfflineMediaArgs) -> ToolResult {
    tracing::debug!("tool: find_offline_media");
    let doc = state.document.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::text("No timeline project yet").with_data(json!({ "offline": [] }));
    };
    let offline = ops::offline_assets(project, |p| p.exists());
    let rows: Vec<Value> = offline
        .iter()
        .filter_map(|id| project.media.assets.get(id))
        .map(|a| {
            let path = match &a.source {
                photonic_core::timeline::AssetSource::File { path, .. } => {
                    path.display().to_string()
                }
                _ => String::new(),
            };
            json!({
                "asset_id": a.id,
                "path": path,
                "kind": a.kind,
                "content_hash": a.content_hash,
                "clip_uses": clip_use_count(project, a.id),
            })
        })
        .collect();
    ToolResult::text(format!(
        "{} offline asset(s) of {} in the pool",
        rows.len(),
        project.media.assets.len()
    ))
    .with_data(json!({
        "offline": rows,
        "pool_size": project.media.assets.len(),
    }))
}

/// How many timeline clips reference `asset` — the "how bad is this" number for
/// an offline row. Derived, never stored.
fn clip_use_count(project: &photonic_core::timeline::TimelineProject, asset: AssetId) -> usize {
    project
        .sequences
        .values()
        .flat_map(|s| s.tracks())
        .flat_map(|t| t.clips.iter())
        .filter(|c| c.source.asset() == Some(asset))
        .count()
}

/// Relink every offline asset that a scan of `search_dir` can account for, as
/// ONE undo step (26 K-C6 — the batch is the whole value of the item).
pub async fn relink_media_batch(state: &AppState, args: RelinkMediaBatchArgs) -> ToolResult {
    tracing::debug!("tool: relink_media_batch {}", args.search_dir);
    let root = std::path::PathBuf::from(&args.search_dir);
    if !root.is_dir() {
        return err_code(
            "AssetOffline",
            format!("search_dir is not a directory: {}", args.search_dir),
        );
    }
    let recursive = args.recursive.unwrap_or(true);
    let (candidates, truncated) = scan_relink_candidates(&root, recursive, true);
    let hashed_scan = candidates.iter().any(|c| c.content_hash.is_some());

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };

    // Default target: every offline asset. An explicit list is honoured as-is
    // (an online asset can be re-pointed too — "Replace clip", not "locate").
    let targets: Vec<AssetId> = match &args.asset_ids {
        Some(ids) if !ids.is_empty() => {
            if let Some(missing) = ids.iter().find(|id| !project.media.assets.contains_key(id)) {
                return map_edit_error(photonic_core::timeline::ops::EditError::NoAsset(*missing));
            }
            ids.clone()
        }
        _ => ops::offline_assets(project, |p| p.exists()),
    };
    let plan = ops::plan_relink(project, &targets, &candidates, hash_like);

    let mismatched: Vec<Value> = plan.mismatched().map(relink_entry_json).collect();
    let allow_mismatch = args.allow_hash_mismatch.unwrap_or(false);
    let would: Vec<Value> = plan
        .entries
        .iter()
        .filter(|e| allow_mismatch || e.hash != ops::RelinkHashCheck::Mismatch)
        .map(relink_entry_json)
        .collect();
    let unmatched: Vec<Value> = plan
        .unmatched
        .iter()
        .filter_map(|id| project.media.assets.get(id))
        .map(|a| {
            json!({
                "asset_id": a.id,
                "path": match &a.source {
                    photonic_core::timeline::AssetSource::File { path, .. } =>
                        path.display().to_string(),
                    _ => String::new(),
                },
            })
        })
        .collect();

    let data = json!({
        "dry_run": args.dry_run.unwrap_or(false),
        "scanned_files": candidates.len(),
        "scan_truncated": truncated,
        "hashed_scan": hashed_scan,
        "relinked": would,
        "skipped_hash_mismatch": if allow_mismatch { Vec::new() } else { mismatched.clone() },
        "unmatched": unmatched,
    });

    if args.dry_run.unwrap_or(false) {
        return ToolResult::text(format!(
            "Dry run: {} asset(s) would relink, {} blocked by a content-hash mismatch, {} unmatched",
            would.len(),
            if allow_mismatch { 0 } else { mismatched.len() },
            plan.unmatched.len()
        ))
        .with_data(data);
    }

    let cmds = ops::relink_plan_commands(project, &plan.entries, allow_mismatch);
    if cmds.is_empty() {
        return ToolResult::text(format!(
            "Nothing relinked — {} candidate file(s) scanned, {} blocked by a content-hash \
             mismatch, {} offline asset(s) unmatched",
            candidates.len(),
            if allow_mismatch { 0 } else { mismatched.len() },
            plan.unmatched.len()
        ))
        .with_data(data);
    }
    // ONE undo unit for the whole folder move (DoD 4).
    history.execute_discrete(
        Command::Batch(cmds.into_iter().map(Command::Timeline).collect()),
        &mut doc,
    );
    ToolResult::text(format!(
        "Relinked {} asset(s) as one undo step ({} blocked by a content-hash mismatch, {} unmatched)",
        would.len(),
        if allow_mismatch { 0 } else { mismatched.len() },
        plan.unmatched.len()
    ))
    .with_data(data)
}

pub async fn list_media(state: &AppState, args: ListMediaArgs) -> ToolResult {
    tracing::debug!("tool: list_media");
    let doc = state.document.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::text("No timeline project yet").with_data(json!({ "assets": [] }));
    };
    let filter_bin = match &args.bin {
        None => None,
        Some(name) => match find_bin_by_name(project, name) {
            Some(b) => Some(b.id),
            None => {
                return ToolResult::text(format!("no bin named {name:?}"))
                    .with_data(json!({ "assets": [] }))
            }
        },
    };
    let assets: Vec<_> = project
        .media
        .assets
        .values()
        .filter(|a| filter_bin.is_none() || a.bin == filter_bin)
        .map(|a| {
            json!({
                "asset_id": a.id,
                "kind": a.kind,
                "source": a.source,
                "probed": a.probe.is_some(),
                "proxy_status": a.proxy.as_ref().map(|p| p.status),
                "content_hash": a.content_hash,
                "bin_id": a.bin,
            })
        })
        .collect();
    ToolResult::text(format!("{} media asset(s)", assets.len()))
        .with_data(json!({ "assets": assets }))
}

pub async fn remove_asset(state: &AppState, args: RemoveAssetArgs) -> ToolResult {
    tracing::debug!("tool: remove_asset {}", args.asset_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    match ops::remove_asset(project, args.asset_id) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Removed asset")
        }
        Err(e) => map_edit_error(e),
    }
}

/// K-C2: set asset tags, upserting the project TagId registry as needed.
pub async fn set_asset_tags(state: &AppState, args: SetAssetTagsArgs) -> ToolResult {
    tracing::debug!("tool: set_asset_tags {}", args.asset_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    match ops::set_asset_tags_resolved(project, args.asset_id, args.tags) {
        Ok(cmds) if cmds.is_empty() => ToolResult::text("Tags unchanged"),
        Ok(cmds) => {
            let n = cmds.len();
            history.execute_discrete(
                Command::Batch(cmds.into_iter().map(Command::Timeline).collect()),
                &mut doc,
            );
            ToolResult::text(format!("Set tags ({n} command(s))"))
        }
        Err(e) => map_edit_error(e),
    }
}

/// K-A8: create a subclip pool entry (zone view of parent media).
pub async fn create_subclip(state: &AppState, args: CreateSubclipArgs) -> ToolResult {
    tracing::debug!("tool: create_subclip parent={}", args.parent_asset_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let tps = TICKS_PER_SECOND as f64;
    let rin = args
        .in_ticks
        .or_else(|| args.in_seconds.map(|s| (s * tps).round() as i64));
    let rout = args
        .out_ticks
        .or_else(|| args.out_seconds.map(|s| (s * tps).round() as i64));
    let (Some(a), Some(b)) = (rin, rout) else {
        return ToolResult::error("supply in_ticks/out_ticks (or in_seconds/out_seconds)");
    };
    match ops::create_subclip(project, args.parent_asset_id, (Tick(a), Tick(b)), args.name) {
        Ok((cmd, id)) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Created subclip").with_data(json!({
                "asset_id": id,
                "parent_asset_id": args.parent_asset_id,
                "in_ticks": a,
                "out_ticks": b,
            }))
        }
        Err(e) => map_edit_error(e),
    }
}

// ─── Media bins (added for the P2 top-up — not in the original §3.1 table) ──

pub async fn create_bin(state: &AppState, args: CreateBinArgs) -> ToolResult {
    tracing::debug!("tool: create_bin {}", args.name);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let needs_project = doc.timeline.is_none();
    if let Some(parent) = args.parent {
        let exists = doc
            .timeline
            .as_ref()
            .map(|p| p.media.bins.iter().any(|b| b.id == parent))
            .unwrap_or(false);
        if !exists {
            return ToolResult::error(format!("bin {parent} not found"));
        }
    }
    let mut cmds = Vec::new();
    if needs_project {
        cmds.push(Command::Timeline(ops::create_project()));
    }
    let cmd = ops::create_bin(args.name, args.parent);
    let bin_id = match &cmd {
        TimelineCmd::AddBin { bin } => bin.id,
        _ => unreachable!("ops::create_bin always returns AddBin"),
    };
    cmds.push(Command::Timeline(cmd));
    history.execute_discrete(Command::Batch(cmds), &mut doc);
    ToolResult::text("Created bin").with_data(json!({ "bin_id": bin_id }))
}

pub async fn remove_bin(state: &AppState, args: RemoveBinArgs) -> ToolResult {
    tracing::debug!("tool: remove_bin {}", args.bin_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    match ops::remove_bin(project, args.bin_id) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Removed bin")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn set_asset_bin(state: &AppState, args: SetAssetBinArgs) -> ToolResult {
    tracing::debug!("tool: set_asset_bin {}", args.asset_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    match ops::assign_asset_bin(project, args.asset_id, args.bin_id) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Updated asset bin")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn list_bins(state: &AppState, _args: ListBinsArgs) -> ToolResult {
    tracing::debug!("tool: list_bins");
    let doc = state.document.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::text("No timeline project yet").with_data(json!({ "bins": [] }));
    };
    let bins: Vec<_> = project
        .media
        .bins
        .iter()
        .map(|b| json!({ "bin_id": b.id, "name": b.name, "parent": b.parent }))
        .collect();
    ToolResult::text(format!("{} bin(s)", bins.len())).with_data(json!({ "bins": bins }))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

// ═════════════════════════════════════════════════════════════════════════════
// P3 engine slice (10-mcp-tools.md): playback (§3.13), render_frame_at
// (§3.14/§4), engine-backed media ops (§3.1 probe/proxy/transcode), export +
// the async-job tools (§3.15, §6). All engine access goes through the lazy
// [`EngineBridge`] in `handlers/video_jobs.rs` — see that module's docs for
// the shadow-document snapshot bridge and the no-GPU degradation story.
// ═════════════════════════════════════════════════════════════════════════════

/// The engine bridge, or the structured no-GPU degradation error (10 §7:
/// "fails with a clear error rather than blocking the rest of the surface").
/// `EngineUnavailable` extends the §8 taxonomy — §8 has no code for a missing
/// GPU adapter because §2 assumed a GUI-shared `GpuContext`.
pub(crate) fn engine_bridge(state: &AppState) -> Result<&EngineBridge, ToolResult> {
    state.video_engine.bridge().ok_or_else(|| {
        err_code(
            "EngineUnavailable",
            "no GPU adapter available — video-engine-backed tools \
             (playback/render_frame_at/export_sequence) are unavailable on this machine",
        )
    })
}

/// Frame rate + formats + active-format index of a sequence, read from the
/// REAL document (readonly borrow — design rule 5: never locks `history`).
async fn sequence_render_info(
    state: &AppState,
    seq: SequenceId,
) -> Result<
    (
        FrameRate,
        Vec<photonic_core::timeline::SequenceFormat>,
        usize,
    ),
    ToolResult,
> {
    let doc = state.document.lock().await;
    let project = doc
        .timeline
        .as_ref()
        .ok_or_else(|| ToolResult::error("no timeline project"))?;
    let s = project
        .sequences
        .get(&seq)
        .ok_or_else(|| ToolResult::error(format!("sequence {seq} not found")))?;
    Ok((s.frame_rate, s.formats.clone(), s.active_format))
}

/// The status payload every transport tool returns (`EngineStatus`, 02 §1).
fn engine_status_json(status: &photonic_video::EngineStatus) -> serde_json::Value {
    json!({
        "playhead_ticks": status.playhead.0,
        "playing": status.playing,
        "dropped_frames": status.dropped,
        "cache": {
            "hits": status.cache.hits,
            "misses": status.cache.misses,
            "resident_entries": status.cache.resident_entries,
            "resident_bytes": status.cache.resident_bytes,
        },
        "source_audition":status.source_audition.as_ref().map(|source|json!({"asset_id":source.asset,"start_ticks":source.range.0.0,"end_ticks":source.range.1.0,"playhead_ticks":source.playhead.0,"playing":source.playing,"has_audio":source.has_audio})),
        "source_audition_error":status.source_audition_error,
        "memory": {
            "decoded_ring_bytes":status.memory.decoded_ring_bytes,"decoded_ring_budget_bytes":status.memory.decoded_ring_budget_bytes,
            "upload_bytes":status.memory.upload_bytes,"still_bytes":status.memory.still_bytes,"vector_bytes":status.memory.vector_bytes,
            "graph_bytes":status.memory.graph_bytes,"gpu_cache_bytes":status.memory.gpu_cache_bytes,"gpu_cache_budget_bytes":status.memory.gpu_cache_budget_bytes,
            "raster_jobs":status.memory.raster_jobs,"raster_job_byte_limit":status.memory.raster_job_byte_limit,"pressure":status.memory.pressure,
            "scope":"managed per-session caches; excludes externally retained textures, renderer scratch, FFmpeg and audio internals"
        },
        "readiness": {"requested":status.readiness.requested,"ready":status.readiness.ready,
            "pending_source_builds":status.readiness.pending_source_builds,"pending_rasters":status.readiness.pending_rasters,
            "capacity_limited":status.readiness.capacity_limited,"failed_sources":status.readiness.failed_sources},
        "command_admission":{"pending":status.command_admission.pending,"coalesced":status.command_admission.coalesced,"rejected":status.command_admission.rejected},
        "audio_xruns": status.audio_xruns,
        "doc_revision": status.doc_revision,
        "snapshot_generation": status.snapshot_generation,
        "frames_published": status.frames_published,
        "evaluations": status.evaluations,
        "evaluation_misses": status.evaluation_misses,
        "last_evaluate_micros": status.last_evaluate_micros,
        "buffering": status.buffering,
        "preview_quality": format!("{:?}", status.preview_quality).to_ascii_lowercase(),
        "active_sequence": status.active_sequence,
        "last_error": status.last_error.as_ref().map(|d| json!({
            "code": d.code.as_str(),
            "severity": format!("{:?}", d.severity),
            "message": d.message,
            "consequence": d.consequence,
        })),
    })
}

/// Poll the published status until `pred` holds or `timeout` elapses;
/// returns the last observed status either way (confirmation is best-effort
/// — the engine applies commands within one 2 ms loop tick, but a saturated
/// box may lag; callers report the snapshot rather than fail).
async fn wait_status(
    bridge: &EngineBridge,
    timeout: Duration,
    pred: impl Fn(&photonic_video::EngineStatus) -> bool,
) -> std::sync::Arc<photonic_video::EngineStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        let s = bridge.session().status();
        if pred(&s) || Instant::now() >= deadline {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Settle a transport command (seek/step) against the engine the way
/// `render_frame_at` does, so the returned playhead reflects the command just
/// dispatched — never the tick one engine loop behind.
///
/// The old transport predicates read the *published* playhead directly:
/// `seek` accepted `playhead >= t` and `step` accepted `playhead != before`.
/// Both can be satisfied by the **stale pre-command** status, since the engine
/// sets the clock and re-presents on a later loop iteration than the one that
/// published the status the tool first observes. A backward seek is the sharp
/// case — the old (larger) playhead is already `>= t`, so `seek F45` returned
/// the stale `F150` immediately, and a following `get_engine_status` echoed it.
///
/// Instead we wait for a *fresh* frame (newer than `prev`, pointer-compared) at
/// the exact target frame-start `frame_tick` — a stale pre-command frame has
/// the wrong tick and can't satisfy it — then, for a paused command, confirm
/// the published status caught up to `expect` (its store trails the frame store
/// by under one engine loop). On timeout (no media, or a boundary clamp that
/// produces no distinct frame) it reports the latest status unconditionally.
async fn settle_transport(
    bridge: &EngineBridge,
    prev: Option<std::sync::Arc<photonic_video::EngineFrame>>,
    frame_tick: Tick,
    seq: SequenceId,
    expect: Option<Tick>,
    timeout: Duration,
) -> std::sync::Arc<photonic_video::EngineStatus> {
    let deadline = Instant::now() + timeout;
    let produced = bridge
        .wait_fresh_frame(prev, timeout, |f| f.time == frame_tick && f.sequence == seq)
        .await
        .is_some();
    if produced {
        if let Some(exp) = expect {
            let remaining = deadline.saturating_duration_since(Instant::now());
            return wait_status(bridge, remaining, |s| s.playhead == exp).await;
        }
    }
    bridge.session().status()
}

// ─── Playback (10 §3.13) ─────────────────────────────────────────────────────
//
// These mutate **engine/session state only** — never the document, never
// `history` (01 §11: playhead is session state). Dispatch classifies them
// `ToolOutput::readonly` so no checkpoint is scheduled; §3.13's `mutating*`
// is about side-effect semantics, not the checkpoint machinery.

pub async fn play(state: &AppState, args: PlayArgs) -> ToolResult {
    tracing::debug!("tool: play");
    let bridge = match engine_bridge(state) {
        Ok(b) => b,
        Err(e) => return e,
    };
    if let Some(seq) = args.sequence_id {
        if let Err(e) = sequence_render_info(state, seq).await {
            return e;
        }
    }
    let _transport = bridge.lock_transport().await;
    bridge.sync(state).await;
    if !bridge.wait_engine_synced(Duration::from_secs(10)).await {
        return err_code(
            "EngineBusy",
            "Engine has not synchronized the document snapshot",
        )
        .with_data(json!({"retryable":true,"snapshot_generation":bridge.snapshot_generation()}));
    }
    if let Some(seq) = args.sequence_id {
        bridge.session().send(EngineCmd::SetActiveSequence(seq));
    }
    if !bridge.session().send(EngineCmd::Play) {
        return ToolResult::error("engine session has shut down");
    }
    // Headless boxes with no audio device play on the soft clock (02 §4) —
    // the engine's lazy cpal open falls back internally, so `play` succeeds
    // rather than raising AudioDeviceUnavailable (10 §7's degraded row).
    let status = wait_status(bridge, Duration::from_secs(2), |s| s.playing).await;
    if !status.playing {
        return err_code("TransportNotConfirmed", "Playback did not start within 2s")
            .with_data(engine_status_json(&status));
    }
    ToolResult::text("playback started").with_data(engine_status_json(&status))
}

pub async fn pause(state: &AppState, _args: PauseArgs) -> ToolResult {
    tracing::debug!("tool: pause");
    let bridge = match engine_bridge(state) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let _transport = bridge.lock_transport().await;
    if !bridge.session().send(EngineCmd::Pause) {
        return ToolResult::error("engine session has shut down");
    }
    let status = wait_status(bridge, Duration::from_secs(2), |s| !s.playing).await;
    ToolResult::text(format!("paused at tick {}", status.playhead.0))
        .with_data(engine_status_json(&status))
}

pub async fn seek(state: &AppState, args: SeekArgs) -> ToolResult {
    tracing::debug!("tool: seek");
    let bridge = match engine_bridge(state) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let (fr, _, _) = match sequence_render_info(state, args.sequence_id).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let t = match resolve_tick(
        args.at_ticks,
        args.at_tc.as_deref(),
        args.at_seconds,
        Some(fr),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    if t.0 < 0 {
        return err_code("TickOutOfRange", "seek target must be >= 0")
            .with_data(json!({ "min_ticks": 0 }));
    }
    let _transport = bridge.lock_transport().await;
    bridge.sync(state).await;
    if !bridge.wait_engine_synced(Duration::from_secs(10)).await {
        return err_code(
            "EngineBusy",
            "Engine has not synchronized the document snapshot",
        )
        .with_data(json!({"retryable":true,"snapshot_generation":bridge.snapshot_generation()}));
    }
    // The engine presents exact frame-start ticks (02 §4); snap the target so
    // the fresh-frame wait matches what will actually be published.
    let snapped = fr.frame_start(fr.frame_at(t));
    let was_playing = bridge.session().status().playing;
    let prev = bridge.session().latest_frame();
    bridge
        .session()
        .send(EngineCmd::SetActiveSequence(args.sequence_id));
    if !bridge.session().send(EngineCmd::Seek(t)) {
        return ToolResult::error("engine session has shut down");
    }
    // Settle against the produced frame (see `settle_transport`): a backward
    // seek used to report the stale pre-seek playhead because it was already
    // `>= t`. Paused ⇒ confirm the clock landed exactly on `t`; playing ⇒ the
    // clock keeps advancing, so the fresh target frame alone proves the seek.
    let status = settle_transport(
        bridge,
        prev,
        snapped,
        args.sequence_id,
        (!was_playing).then_some(t),
        Duration::from_secs(5),
    )
    .await;
    if !was_playing && (status.playhead != t || status.active_sequence != Some(args.sequence_id)) {
        return err_code(
            "TransportNotConfirmed",
            "Engine did not reach the requested seek target",
        )
        .with_data(engine_status_json(&status));
    }
    ToolResult::text(format!("seeked to tick {}", t.0)).with_data(engine_status_json(&status))
}

pub async fn step(state: &AppState, args: StepArgs) -> ToolResult {
    tracing::debug!("tool: step {}", args.frames);
    let bridge = match engine_bridge(state) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let _transport = bridge.lock_transport().await;
    bridge.sync(state).await;
    if !bridge.wait_engine_synced(Duration::from_secs(10)).await {
        return err_code(
            "EngineBusy",
            "Engine has not synchronized the document snapshot",
        )
        .with_data(json!({"retryable":true,"snapshot_generation":bridge.snapshot_generation()}));
    }
    // Step is relative, so reading a stale `before` would compound into a
    // stale result (the finding's "step responses lagged"). Sample the current
    // status, compute the exact frame the engine will snap to (mirrors
    // `PlaybackController::step`), then wait for that frame + its published
    // playhead via the same fresh-frame discipline `render_frame_at` uses.
    let before_status = bridge.session().status();
    let before = before_status.playhead;
    let prev = bridge.session().latest_frame();
    if !bridge.session().send(EngineCmd::Step(args.frames)) {
        return ToolResult::error("engine session has shut down");
    }
    let status = match before_status.active_sequence {
        // Step always pauses (02 §4); `expected` is frame-aligned, so we can
        // confirm the exact landed tick (including a clamp at frame 0, where
        // the forced re-present of the same frame satisfies the wait at once).
        Some(seq) => match sequence_render_info(state, seq).await {
            Ok((fr, _, _)) => {
                let target_frame = (fr.frame_at(before) + args.frames as i64).max(0);
                let expected = fr.frame_start(target_frame);
                settle_transport(
                    bridge,
                    prev,
                    expected,
                    seq,
                    Some(expected),
                    Duration::from_secs(5),
                )
                .await
            }
            // Sequence vanished from the real doc mid-call — best-effort pause.
            Err(_) => wait_status(bridge, Duration::from_secs(2), |s| !s.playing).await,
        },
        // No active sequence to present against — just confirm the pause.
        None => wait_status(bridge, Duration::from_secs(2), |s| !s.playing).await,
    };
    ToolResult::text(format!(
        "stepped {} frame(s) — playhead at tick {} (paused)",
        args.frames, status.playhead.0
    ))
    .with_data(engine_status_json(&status))
}

pub async fn set_loop_range(state: &AppState, args: SetLoopRangeArgs) -> ToolResult {
    tracing::debug!("tool: set_loop_range");
    let bridge = match engine_bridge(state) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let (fr, _, _) = match sequence_render_info(state, args.sequence_id).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let range = match &args.range {
        None => None,
        Some(r) => {
            let start = match resolve_tick(
                r.start_ticks,
                r.start_tc.as_deref(),
                r.start_seconds,
                Some(fr),
            ) {
                Ok(t) => t,
                Err(e) => return e,
            };
            let end = match resolve_tick(r.end_ticks, r.end_tc.as_deref(), r.end_seconds, Some(fr))
            {
                Ok(t) => t,
                Err(e) => return e,
            };
            if end <= start {
                return err_code("TickOutOfRange", "loop end must be after loop start");
            }
            Some((start, end))
        }
    };
    let _transport = bridge.lock_transport().await;
    bridge.sync(state).await;
    if !bridge.wait_engine_synced(Duration::from_secs(10)).await {
        return err_code(
            "EngineBusy",
            "Engine has not synchronized the document snapshot",
        )
        .with_data(json!({"retryable":true,"snapshot_generation":bridge.snapshot_generation()}));
    }
    bridge
        .session()
        .send(EngineCmd::SetActiveSequence(args.sequence_id));
    if !bridge.session().send(EngineCmd::SetLoop(range)) {
        return ToolResult::error("engine session has shut down");
    }
    match range {
        Some((s, e)) => ToolResult::text(format!("loop range set to [{}, {})", s.0, e.0))
            .with_data(json!({ "start_ticks": s.0, "end_ticks": e.0 })),
        None => ToolResult::text("loop range cleared"),
    }
}

pub async fn set_proxy_mode(state: &AppState, args: SetProxyModeArgs) -> ToolResult {
    tracing::debug!("tool: set_proxy_mode");
    let bridge = match engine_bridge(state) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let mode = match args.mode {
        ProxyModeArg::Auto => ProxyMode::Auto,
        ProxyModeArg::ForceProxy => ProxyMode::ForceProxy,
        ProxyModeArg::ForceOriginal => ProxyMode::ForceOriginal,
    };
    let _transport = bridge.lock_transport().await;
    bridge.set_proxy_mode(mode);
    ToolResult::text(format!("proxy mode set to {mode:?}")).with_data(json!({
        "mode": format!("{mode:?}"),
        "note": "Auto and ForceProxy decode generated proxies where present (see generate_proxies), \
                 falling back to originals otherwise; ForceOriginal always decodes originals. \
                 Proxies are never required for correctness (CAP-014)."
    }))
}

pub async fn get_engine_status(state: &AppState, _args: GetEngineStatusArgs) -> ToolResult {
    tracing::debug!("tool: get_engine_status");
    let bridge = match engine_bridge(state) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let _transport = bridge.lock_transport().await;
    bridge.sync(state).await;
    let synced = bridge.wait_engine_synced(Duration::from_secs(2)).await;
    let status = bridge.session().status();
    let mut data = engine_status_json(&status);
    data["snapshot_synced"] = json!(synced);
    ToolResult::text(format!(
        "playhead {} — {}",
        status.playhead.0,
        if status.playing { "playing" } else { "paused" }
    ))
    .with_data(data)
}

// ─── render_frame_at (10 §3.14 / §4) ─────────────────────────────────────────

/// Restore temporary render settings on success, error, or future cancellation.
struct RenderSettingsGuard<'a> {
    session: &'a EngineSession,
    proxy_mode: ProxyMode,
    preview_quality: PreviewQuality,
    scope_tap: Option<ScopeTapPoint>,
}

impl Drop for RenderSettingsGuard<'_> {
    fn drop(&mut self) {
        self.session.send(EngineCmd::SetProxyMode(self.proxy_mode));
        self.session
            .send(EngineCmd::SetPreviewQuality(self.preview_quality));
        if let Some(tap) = self.scope_tap {
            self.session.send(EngineCmd::SetScopeTap(tap));
        }
    }
}

pub async fn render_frame_at(state: &AppState, args: RenderFrameAtArgs) -> ToolResult {
    tracing::debug!("tool: render_frame_at");
    let bridge = match engine_bridge(state) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let expected_revision = state.history.lock().await.revision();
    let seq_id = args.sequence_id;
    let (fr, formats, active_format) = match sequence_render_info(state, seq_id).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if formats.is_empty() {
        return err_code("NoFormat", "Sequence has no render format");
    }
    if let Some(fi) = args.format_index {
        if fi >= formats.len() {
            return ToolResult::error(format!(
                "format_index {fi} out of range — sequence has {} format(s)",
                formats.len()
            ));
        }
    }
    let format_index = args
        .format_index
        .unwrap_or(active_format)
        .min(formats.len().saturating_sub(1));
    let (w, h) = (
        formats[format_index].width.max(1),
        formats[format_index].height.max(1),
    );
    let t = match resolve_tick(
        args.at_ticks,
        args.at_tc.as_deref(),
        args.at_seconds,
        Some(fr),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    if t.0 < 0 {
        return err_code("TickOutOfRange", "render tick must be >= 0")
            .with_data(json!({ "min_ticks": 0 }));
    }
    // The engine presents exact frame-start ticks (02 §4) — snap down.
    let snapped = fr.frame_start(fr.frame_at(t));
    let scale = args.scale.unwrap_or(1.0);
    if !(scale > 0.0 && scale <= 1.0) {
        return ToolResult::error("scale must be in (0, 1]");
    }
    let output_format = args.output_format.unwrap_or_default();
    if u64::from(w) * u64::from(h) > 16_777_216 {
        return err_code("FrameTooLarge", "Frame inspection is limited to 16 million source pixels; use a smaller sequence format");
    }

    let started = Instant::now();
    let _transport = bridge.lock_transport().await;

    bridge
        .sync_with_format(state, Some((seq_id, format_index)))
        .await;
    if bridge.shadow_revision() != expected_revision {
        return err_code("RevisionConflict", "Document changed while preparing the frame; retry inspection").with_data(json!({"actual_revision":bridge.shadow_revision(),"expected_revision":expected_revision}));
    }
    if !bridge.wait_engine_synced(Duration::from_secs(10)).await {
        return ToolResult::error("engine did not pick up the document snapshot within 10s");
    }

    // Media choice and processing resolution are independent. Full must use
    // originals AND the full processing canvas, even on a Draft session.
    let _settings = RenderSettingsGuard {
        session: bridge.session(),
        proxy_mode: bridge.proxy_mode(),
        preview_quality: bridge.session().requested_preview_quality(),
        scope_tap: Some(bridge.session().status().scope_tap),
    };
    let (call_mode, call_quality) = match args.quality {
        RenderQualityArg::Preview => (ProxyMode::ForceProxy, PreviewQuality::Draft),
        RenderQualityArg::Full => (ProxyMode::ForceOriginal, PreviewQuality::Full),
    };
    let prev = bridge.session().latest_frame();
    let request_id = bridge.next_inspection_id();
    if !bridge.session().send(EngineCmd::InspectFrame {
        request_id,
        sequence: seq_id,
        time: snapped,
        proxy_mode: call_mode,
        quality: call_quality,
        scope_tap: ScopeTapPoint::Program,
    }) {
        return err_code("EngineBusy", "Engine command queue is full or closed")
            .with_data(json!({"retryable":true}));
    }

    let frame = bridge
        .wait_fresh_frame(prev, Duration::from_secs(30), |f| {
            f.time == snapped
                && f.sequence == seq_id
                && f.preview_asset.is_none()
                && f.logical_size == (w, h)
                && f.doc_revision == bridge.shadow_revision()
                && f.snapshot_generation == bridge.snapshot_generation()
                && f.inspection_request_id == Some(request_id)
                && f.preview_quality == call_quality
                && f.proxy_mode == call_mode
        })
        .await;
    let result = match frame {
        Some(frame) => {
            // Read the LOGICAL region only — EngineFrame textures are padded
            // to the texture pool's 64 px bucket (see photonic-video
            // session.rs::pad_to_pool_bucket); content sits top-left at the
            // sequence-format size.
            let output_size = if output_format == RenderOutputFormatArg::Png {
                (
                    ((w as f64 * scale).round() as u32).clamp(1, w),
                    ((h as f64 * scale).round() as u32).clamp(1, h),
                )
            } else {
                (w, h)
            };
            let readback = std::sync::Arc::clone(&bridge.readback);
            let gpu = bridge.engine().gpu().clone();
            let texture = std::sync::Arc::clone(&frame.texture);
            let mut result = match tokio::task::spawn_blocking(move || {
                let pixels = readback
                    .lock()
                    .map_err(|_| "readback state poisoned".to_string())?
                    .read(&gpu, &texture, (w, h), output_size)?;
                Ok::<_, String>(build_render_result(
                    pixels,
                    output_size.0,
                    output_size.1,
                    if output_format == RenderOutputFormatArg::Png {
                        1.0
                    } else {
                        scale
                    },
                    output_format,
                    snapped,
                    started.elapsed(),
                ))
            })
            .await
            {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => return err_code("ReadbackFailed", error),
                Err(error) => return err_code("ReadbackFailed", error.to_string()),
            };
            if result.is_error != Some(true) {
                let mut data = result.structured_content.take().unwrap_or_default();
                data["sequence_id"] = json!(seq_id);
                data["revision"] = json!(frame.doc_revision);
                data["snapshot_generation"] = json!(frame.snapshot_generation);
                data["inspection_request_id"] = json!(request_id);
                data["format_index"] = json!(format_index);
                data["quality"] = json!(match args.quality {
                    RenderQualityArg::Full => "full",
                    RenderQualityArg::Preview => "preview",
                });
                data["processing_quality"] =
                    json!(format!("{:?}", frame.preview_quality).to_ascii_lowercase());
                data["source_width"] = json!(w);
                data["source_height"] = json!(h);
                data["gpu_downscaled"] = json!(output_size != (w, h));
                result = result.with_data(data);
            }
            result
        }
        None => ToolResult::error(
            "engine did not produce the requested frame within 30s — cold-seek decode \
             cost can dominate (see tool description)",
        ),
    };
    result
}

/// Deterministic box downscale on linear premultiplied f32 pixels (fixed
/// iteration order ⇒ byte-stable output for the raw path).
fn box_downscale(src: &[[f32; 4]], w: u32, h: u32, ow: u32, oh: u32) -> Vec<[f32; 4]> {
    let mut out = Vec::with_capacity((ow * oh) as usize);
    for oy in 0..oh {
        let y0 = (oy as u64 * h as u64 / oh as u64) as u32;
        let y1 = (((oy as u64 + 1) * h as u64).div_ceil(oh as u64) as u32).clamp(y0 + 1, h);
        for ox in 0..ow {
            let x0 = (ox as u64 * w as u64 / ow as u64) as u32;
            let x1 = (((ox as u64 + 1) * w as u64).div_ceil(ow as u64) as u32).clamp(x0 + 1, w);
            let mut acc = [0f64; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = src[(y * w + x) as usize];
                    for (a, c) in acc.iter_mut().zip(p.iter()) {
                        *a += *c as f64;
                    }
                }
            }
            let n = ((y1 - y0) as f64) * ((x1 - x0) as f64);
            out.push([
                (acc[0] / n) as f32,
                (acc[1] / n) as f32,
                (acc[2] / n) as f32,
                (acc[3] / n) as f32,
            ]);
        }
    }
    out
}

/// IEEE 754 binary32 → binary16 bits, round-to-nearest-even. Values read back
/// from an `Rgba16Float` texture are exactly representable, so the raw path
/// round-trips the GPU's own bits.
fn f32_to_f16_bits(f: f32) -> u16 {
    let bits = f.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let frac = bits & 0x007f_ffff;
    if exp == 0xff {
        // Inf / NaN (quietened).
        return sign | 0x7c00 | if frac != 0 { 0x0200 } else { 0 };
    }
    let exp = exp - 127 + 15;
    if exp >= 0x1f {
        return sign | 0x7c00; // overflow → inf
    }
    if exp <= 0 {
        if exp < -10 {
            return sign; // underflow → signed zero
        }
        // Subnormal half.
        let frac = frac | 0x0080_0000;
        let shift = (14 - exp) as u32;
        let mut sub = (frac >> shift) as u16;
        let rem = frac & ((1u32 << shift) - 1);
        let half = 1u32 << (shift - 1);
        if rem > half || (rem == half && (sub & 1) == 1) {
            sub += 1;
        }
        return sign | sub;
    }
    let mut out = sign | ((exp as u16) << 10) | ((frac >> 13) as u16);
    let rem = frac & 0x1fff;
    if rem > 0x1000 || (rem == 0x1000 && (out & 1) == 1) {
        out += 1; // carry may roll into the exponent — correct RNE behavior
    }
    out
}

fn build_render_result(
    pixels: Vec<[f32; 4]>,
    w: u32,
    h: u32,
    scale: f64,
    output_format: RenderOutputFormatArg,
    tick: Tick,
    elapsed: Duration,
) -> ToolResult {
    let ow = ((w as f64 * scale).round() as u32).clamp(1, w);
    let oh = ((h as f64 * scale).round() as u32).clamp(1, h);
    let pixels = if (ow, oh) == (w, h) {
        pixels
    } else {
        box_downscale(&pixels, w, h, ow, oh)
    };
    let render_ms = elapsed.as_millis() as u64;
    match output_format {
        RenderOutputFormatArg::Png => {
            // Reuse the export path's color math (single source of truth):
            // unpremultiply + linear→sRGB transfer + quantize.
            let flat = pixels.into_flattened();
            let rgba8 = match export_convert::working_frame_to_rgba8(&flat, ow, oh) {
                export_convert::EncodePlanes::Rgba8 { rgba, .. } => rgba,
                _ => return ToolResult::error("internal error: unexpected plane kind"),
            };
            let Some(img) = image::RgbaImage::from_raw(ow, oh, rgba8) else {
                return ToolResult::error("internal error: pixel buffer size mismatch");
            };
            let mut png = Vec::new();
            if let Err(e) =
                img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            {
                return ToolResult::error(format!("PNG encode failed: {e}"));
            }
            ToolResult::text(format!(
                "rendered {ow}x{oh} frame at tick {} ({render_ms} ms)",
                tick.0
            ))
            .with_image(general_purpose::STANDARD.encode(&png))
            .with_data(json!({
                "width": ow, "height": oh, "tick": tick.0,
                "render_ms": render_ms, "output_format": "png",
            }))
        }
        RenderOutputFormatArg::RawRgba16f => {
            let mut bytes = Vec::with_capacity(pixels.len() * 8);
            for px in &pixels {
                for c in px {
                    bytes.extend_from_slice(&f32_to_f16_bits(*c).to_le_bytes());
                }
            }
            ToolResult::text(format!(
                "rendered {ow}x{oh} raw frame at tick {} ({render_ms} ms)",
                tick.0
            ))
            .with_data(json!({
                "width": ow, "height": oh, "tick": tick.0,
                "render_ms": render_ms, "output_format": "raw_rgba16f",
                "encoding": "interleaved RGBA, f16 little-endian, row-major, linear premultiplied (D-09)",
                "data_base64": general_purpose::STANDARD.encode(&bytes),
            }))
        }
    }
}

// ─── Engine-backed media ops (10 §3.1: probe / proxy / transcode) ────────────

/// Resolve an asset to its backing file path, with §8 error mapping.
async fn asset_file_path(
    state: &AppState,
    asset: AssetId,
) -> Result<std::path::PathBuf, ToolResult> {
    let doc = state.document.lock().await;
    let project = doc
        .timeline
        .as_ref()
        .ok_or_else(|| ToolResult::error("no timeline project"))?;
    let a = project
        .media
        .assets
        .get(&asset)
        .ok_or_else(|| ToolResult::error(format!("asset {asset} not found")))?;
    match &a.source {
        photonic_core::timeline::AssetSource::File { path, .. } => Ok(path.clone()),
        _ => Err(ToolResult::error(
            "asset is not file-backed — nothing to probe/transcode",
        )),
    }
}

pub async fn probe_media(state: &AppState, args: ProbeMediaArgs) -> ToolResult {
    tracing::debug!("tool: probe_media {}", args.asset_id);
    let path = match asset_file_path(state, args.asset_id).await {
        Ok(p) => p,
        Err(e) => return e,
    };
    if !path.exists() {
        return err_code(
            "AssetOffline",
            format!("file not found: {}", path.display()),
        );
    }
    let tools = match ffmpeg_locate::locate() {
        Ok(t) => t,
        Err(e) => {
            return err_code(
                "FfmpegUnavailable",
                format!("ffprobe not found ({e}) — set PHOTONIC_FFMPEG_DIR or install ffmpeg"),
            )
        }
    };
    let (job_id, cancel) = match state.video_jobs.lock().expect("job registry poisoned").start("probe_media") {
        Ok(job) => job,
        Err(error) => return ToolResult::error_with_code("JobCapacityExceeded", error.to_string()).with_data(json!({"retryable":true,"max_active_jobs":crate::handlers::video_jobs::MAX_ACTIVE_JOBS})),
    };
    let jobs = std::sync::Arc::clone(&state.video_jobs);
    let document = std::sync::Arc::clone(&state.document);
    let history = std::sync::Arc::clone(&state.history);
    let asset_id = args.asset_id;
    std::thread::spawn(move || {
        set_job_status(
            &jobs,
            job_id,
            JobStatus::Running {
                progress: 0.0,
                message: "probing".into(),
            },
        );
        if cancel.load(Ordering::Relaxed) {
            set_job_status(&jobs, job_id, JobStatus::Cancelled);
            return;
        }
        let probe = match video_probe::probe_asset(&tools, &path) {
            Ok(p) => p,
            Err(e) => {
                set_job_status(
                    &jobs,
                    job_id,
                    JobStatus::Failed {
                        error_code: "ProbeFailed".into(),
                        message: e.to_string(),
                    },
                );
                return;
            }
        };
        // xxh3 head+tail+len — the real relink identity (replaces the P2
        // SipHash stopgap noted at the top of this file).
        let hash = video_probe::content_hash(&path).ok();
        // Commit — design rule 7 lock order: document BEFORE history. This is
        // the job-completion path that mutates outside dispatch_tool_inner's
        // post-mutation hook (design rule 6), so the checkpoint is scheduled
        // here explicitly. `MediaAsset::probe` is engine-derived cache
        // ("Filled by the engine after ffprobe", core media.rs) with no
        // TimelineCmd variant — written directly, not as an undo step; a
        // `SetAssetProbe` command in core would let this use
        // `execute_discrete` (noted seam).
        let updated = {
            let mut doc = document.blocking_lock();
            let updated = match doc
                .timeline
                .as_mut()
                .and_then(|p| p.media.assets.get_mut(&asset_id))
            {
                Some(asset) => {
                    asset.probe = Some(probe.clone());
                    if hash.is_some() {
                        asset.content_hash = hash.clone();
                    }
                    true
                }
                None => false, // asset removed while probing — drop the result
            };
            let mut hist = history.blocking_lock();
            hist.schedule_mcp_checkpoint("probe_media");
            updated
        };
        set_job_status(
            &jobs,
            job_id,
            JobStatus::Done {
                result: json!({
                    "asset_id": asset_id,
                    "updated": updated,
                    "probe": probe,
                    "content_hash": hash,
                }),
            },
        );
    });
    ToolResult::text("probe job started — poll get_job_status")
        .with_data(json!({ "job_id": job_id }))
}

/// Write (or clear) an asset's `MediaAsset::proxy` and schedule an MCP
/// checkpoint. `MediaAsset::proxy` is engine-managed cache (like `probe`) with
/// no `TimelineCmd` variant, so it is written directly — mirroring
/// `probe_media`'s commit (design rule 7 lock order: document before history).
/// Returns whether the asset still existed.
fn set_asset_proxy(
    document: &std::sync::Arc<tokio::sync::Mutex<photonic_core::Document>>,
    history: &std::sync::Arc<tokio::sync::Mutex<photonic_core::history::CommandHistory>>,
    asset_id: AssetId,
    proxy: Option<ProxyRef>,
    checkpoint: &str,
) -> bool {
    let mut doc = document.blocking_lock();
    let updated = match doc
        .timeline
        .as_mut()
        .and_then(|p| p.media.assets.get_mut(&asset_id))
    {
        Some(asset) => {
            asset.proxy = proxy;
            true
        }
        None => false, // asset removed while generating — drop the result
    };
    let mut hist = history.blocking_lock();
    hist.schedule_mcp_checkpoint(checkpoint.to_string());
    updated
}

pub async fn generate_proxies(state: &AppState, args: GenerateProxiesArgs) -> ToolResult {
    tracing::debug!("tool: generate_proxies ({} asset(s))", args.asset_ids.len());
    if args.asset_ids.is_empty() {
        return ToolResult::error("no asset_ids given");
    }
    let tools = match ffmpeg_locate::locate() {
        Ok(t) => t,
        Err(e) => {
            return err_code(
                "FfmpegUnavailable",
                format!("ffmpeg not found ({e}) — set PHOTONIC_FFMPEG_DIR or install ffmpeg"),
            )
        }
    };
    let force = args.force.unwrap_or(false);

    // Resolve each asset to a file-backed video path under the doc lock; carry
    // any already-computed content hash so we can reuse it. Non-video, embedded,
    // and unknown assets are skipped with a reason (never an error — a batch
    // proxies what it can).
    let mut work: Vec<(AssetId, std::path::PathBuf, Option<String>)> = Vec::new();
    let mut skipped: Vec<serde_json::Value> = Vec::new();
    {
        let doc = state.document.lock().await;
        let Some(project) = doc.timeline.as_ref() else {
            return ToolResult::error("no timeline project");
        };
        for id in &args.asset_ids {
            match project.media.assets.get(id) {
                None => skipped.push(json!({ "asset_id": id, "reason": "not found" })),
                Some(a) if a.kind != AssetKind::Video => {
                    skipped.push(json!({ "asset_id": id, "reason": "not a video asset" }))
                }
                // G-15A: never replace a user-attached proxy via generate_proxies.
                Some(a)
                    if a.proxy.as_ref().is_some_and(|p| {
                        p.origin == photonic_core::timeline::ProxyOrigin::Attached
                    }) =>
                {
                    skipped.push(json!({
                        "asset_id": id,
                        "reason": "attached proxy present (detach first)"
                    }))
                }
                Some(a) => match &a.source {
                    photonic_core::timeline::AssetSource::File { path, .. } => {
                        work.push((*id, path.clone(), a.content_hash.clone()))
                    }
                    _ => skipped.push(json!({ "asset_id": id, "reason": "not file-backed" })),
                },
            }
        }
    }
    if work.is_empty() {
        return err_code("NoWorkableAssets", "no file-backed video assets to proxy")
            .with_data(json!({ "skipped": skipped }));
    }

    let (job_id, cancel) = match state.video_jobs.lock().expect("job registry poisoned").start("generate_proxies") {
        Ok(job) => job,
        Err(error) => return ToolResult::error_with_code("JobCapacityExceeded", error.to_string()).with_data(json!({"retryable":true,"max_active_jobs":crate::handlers::video_jobs::MAX_ACTIVE_JOBS})),
    };
    let jobs = std::sync::Arc::clone(&state.video_jobs);
    let document = std::sync::Arc::clone(&state.document);
    let history = std::sync::Arc::clone(&state.history);
    let skipped_ret = skipped.clone();
    let total = work.len();

    std::thread::spawn(move || {
        let cancel_fn = || cancel.load(Ordering::Relaxed);
        let mut results: Vec<serde_json::Value> = Vec::new();
        for (i, (asset_id, input, existing_hash)) in work.into_iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                set_job_status(&jobs, job_id, JobStatus::Cancelled);
                return;
            }
            set_job_status(
                &jobs,
                job_id,
                JobStatus::Running {
                    progress: i as f32 / total as f32,
                    message: format!("proxy {}/{}", i + 1, total),
                },
            );

            if !input.exists() {
                results.push(json!({
                    "asset_id": asset_id, "status": "failed", "error": "source offline"
                }));
                continue;
            }
            // Content hash keys the cache file (survives project moves,
            // rebuildable). Compute it now if import/probe never did.
            let hash = match existing_hash {
                Some(h) => h,
                None => match video_probe::content_hash(&input) {
                    Ok(h) => h,
                    Err(e) => {
                        results.push(json!({
                            "asset_id": asset_id, "status": "failed",
                            "error": format!("content hash: {e}")
                        }));
                        continue;
                    }
                },
            };
            let cache_dir = video_proxy::proxy_cache_dir(None);
            let out = video_proxy::proxy_cache_path(&cache_dir, &hash);

            // Reuse an existing cached proxy unless the caller forces a rebuild.
            if out.is_file() && !force {
                set_asset_proxy(
                    &document,
                    &history,
                    asset_id,
                    Some(ProxyRef::ready_generated(out.clone())),
                    "generate_proxies",
                );
                results.push(json!({
                    "asset_id": asset_id, "status": "ready", "reused": true, "path": out
                }));
                continue;
            }

            // Mark Pending so proxy_status reflects reality mid-flight, then
            // transcode → Ready / Failed.
            set_asset_proxy(
                &document,
                &history,
                asset_id,
                Some(ProxyRef::with_status(out.clone(), ProxyStatus::Pending)),
                "generate_proxies",
            );
            match video_proxy::generate_proxy(&tools, &input, &out, &cancel_fn) {
                Ok(()) => {
                    set_asset_proxy(
                        &document,
                        &history,
                        asset_id,
                        Some(ProxyRef::ready_generated(out.clone())),
                        "generate_proxies",
                    );
                    results.push(json!({
                        "asset_id": asset_id, "status": "ready", "path": out
                    }));
                }
                Err(video_proxy::ProxyError::Cancelled) => {
                    set_asset_proxy(
                        &document,
                        &history,
                        asset_id,
                        Some(ProxyRef::with_status(out.clone(), ProxyStatus::Failed)),
                        "generate_proxies",
                    );
                    set_job_status(&jobs, job_id, JobStatus::Cancelled);
                    return;
                }
                Err(e) => {
                    set_asset_proxy(
                        &document,
                        &history,
                        asset_id,
                        Some(ProxyRef::with_status(out.clone(), ProxyStatus::Failed)),
                        "generate_proxies",
                    );
                    results.push(json!({
                        "asset_id": asset_id, "status": "failed", "error": e.to_string()
                    }));
                }
            }
        }
        set_job_status(
            &jobs,
            job_id,
            JobStatus::Done {
                result: json!({ "proxies": results, "skipped": skipped }),
            },
        );
    });

    ToolResult::text(format!(
        "proxy generation started for {total} asset(s) — poll get_job_status"
    ))
    .with_data(json!({ "job_id": job_id, "skipped": skipped_ret }))
}

pub async fn remove_proxy(state: &AppState, args: RemoveProxyArgs) -> ToolResult {
    tracing::debug!("tool: remove_proxy ({} asset(s))", args.asset_ids.len());
    if args.asset_ids.is_empty() {
        return ToolResult::error("no asset_ids given");
    }
    // Detach the ProxyRef from each asset under the doc lock. Only Generated
    // (cache-owned) paths are collected for delete — Attached user files are
    // never deleted on detach (G-15A).
    let mut assets: Vec<serde_json::Value> = Vec::new();
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let mut mutated = false;
    {
        let mut doc = state.document.lock().await;
        let Some(project) = doc.timeline.as_mut() else {
            return ToolResult::error("no timeline project");
        };
        for id in &args.asset_ids {
            match project.media.assets.get_mut(id) {
                Some(a) => match a.proxy.take() {
                    Some(p) => {
                        mutated = true;
                        let origin = match p.origin {
                            photonic_core::timeline::ProxyOrigin::Generated => "generated",
                            photonic_core::timeline::ProxyOrigin::Attached => "attached",
                        };
                        if p.origin == photonic_core::timeline::ProxyOrigin::Generated {
                            files.push(p.path.clone());
                        }
                        assets.push(json!({
                            "asset_id": id,
                            "removed": true,
                            "path": p.path,
                            "origin": origin,
                        }));
                    }
                    None => assets
                        .push(json!({ "asset_id": id, "removed": false, "reason": "no proxy" })),
                },
                None => {
                    assets.push(json!({ "asset_id": id, "removed": false, "reason": "not found" }))
                }
            }
        }
    }
    // Best-effort delete only cache-owned Generated files.
    let mut files_deleted = 0usize;
    for f in &files {
        if std::fs::remove_file(f).is_ok() {
            files_deleted += 1;
        }
    }
    if mutated {
        let mut hist = state.history.lock().await;
        hist.schedule_mcp_checkpoint("remove_proxy");
    }
    let detached = assets
        .iter()
        .filter(|a| a.get("removed") == Some(&json!(true)))
        .count();
    ToolResult::text(format!(
        "detached {detached} proxy ref(s), deleted {files_deleted} file(s)"
    ))
    .with_data(json!({ "assets": assets, "files_deleted": files_deleted }))
}

/// Attach a user-supplied proxy file to a video asset (G-15A). Never copies
/// the file; validation via ffprobe. Detach never deletes attached files.
pub async fn attach_proxy(state: &AppState, args: AttachProxyArgs) -> ToolResult {
    tracing::debug!("tool: attach_proxy asset={}", args.asset_id);
    let tools = match ffmpeg_locate::locate() {
        Ok(t) => t,
        Err(e) => {
            return err_code(
                "FfmpegUnavailable",
                format!("ffmpeg not found ({e}) — set PHOTONIC_FFMPEG_DIR or install ffmpeg"),
            )
        }
    };
    let allow_mismatch = args.allow_mismatch.unwrap_or(false);
    let proxy_path = std::path::PathBuf::from(&args.path);

    let original = {
        let doc = state.document.lock().await;
        let Some(project) = doc.timeline.as_ref() else {
            return ToolResult::error("no timeline project");
        };
        let Some(asset) = project.media.assets.get(&args.asset_id) else {
            return err_code("NotFound", format!("asset {} not found", args.asset_id));
        };
        if asset.kind != AssetKind::Video {
            return err_code("NotVideo", "attach_proxy requires a video asset");
        }
        match &asset.source {
            photonic_core::timeline::AssetSource::File { path, .. } => path.clone(),
            _ => return err_code("NotFileBacked", "attach_proxy requires a file-backed asset"),
        }
    };

    let validation =
        match video_proxy::validate_attach(&tools, &original, &proxy_path, allow_mismatch) {
            Ok(v) => v,
            Err(e) => {
                return err_code(
                    match &e {
                        video_proxy::AttachError::MissingPath(_) => "MissingPath",
                        video_proxy::AttachError::NotAFile(_) => "NotAFile",
                        video_proxy::AttachError::SameAsOriginal => "SameAsOriginal",
                        video_proxy::AttachError::NotVideo => "NotVideo",
                        video_proxy::AttachError::ProbeFailed(_) => "ProbeFailed",
                        video_proxy::AttachError::DurationMismatch => "DurationMismatch",
                        video_proxy::AttachError::FrameRateMismatch => "FrameRateMismatch",
                    },
                    e.to_string(),
                )
            }
        };

    {
        let mut doc = state.document.lock().await;
        let Some(asset) = doc
            .timeline
            .as_mut()
            .and_then(|p| p.media.assets.get_mut(&args.asset_id))
        else {
            return err_code("NotFound", format!("asset {} not found", args.asset_id));
        };
        asset.proxy = Some(validation.proxy.clone());
    }
    {
        let mut hist = state.history.lock().await;
        hist.schedule_mcp_checkpoint("attach_proxy".to_string());
    }
    ToolResult::text("proxy attached").with_data(json!({
        "asset_id": args.asset_id,
        "path": validation.proxy.path,
        "origin": "attached",
        "warnings": validation.warnings,
    }))
}

/// Detach proxy without deleting user-owned Attached files (G-15A).
pub async fn detach_proxy(state: &AppState, args: DetachProxyArgs) -> ToolResult {
    tracing::debug!("tool: detach_proxy asset={}", args.asset_id);
    let (had, origin, path) = {
        let mut doc = state.document.lock().await;
        let Some(project) = doc.timeline.as_mut() else {
            return ToolResult::error("no timeline project");
        };
        match project.media.assets.get_mut(&args.asset_id) {
            Some(a) => match a.proxy.take() {
                Some(p) => (
                    true,
                    match p.origin {
                        photonic_core::timeline::ProxyOrigin::Generated => "generated",
                        photonic_core::timeline::ProxyOrigin::Attached => "attached",
                    },
                    Some(p.path),
                ),
                None => (false, "none", None),
            },
            None => return err_code("NotFound", format!("asset {} not found", args.asset_id)),
        }
    };
    if had {
        let mut hist = state.history.lock().await;
        hist.schedule_mcp_checkpoint("detach_proxy");
    }
    ToolResult::text(if had {
        "proxy detached (file not deleted)"
    } else {
        "no proxy to detach"
    })
    .with_data(json!({
        "asset_id": args.asset_id,
        "detached": had,
        "origin": origin,
        "path": path,
        "file_deleted": false,
    }))
}

impl TranscodePresetArg {
    /// ffmpeg encode args + output extension for each editing-intermediate
    /// preset (10 §3.1: user-picked codec, distinct from the proxy profile).
    fn ffmpeg_spec(self) -> (&'static [&'static str], &'static str, &'static str) {
        match self {
            TranscodePresetArg::ProresProxy => (
                &[
                    "-c:v",
                    "prores_ks",
                    "-profile:v",
                    "0",
                    "-vendor",
                    "apl0",
                    "-pix_fmt",
                    "yuv422p10le",
                    "-c:a",
                    "pcm_s16le",
                ],
                "mov",
                "prores_proxy",
            ),
            TranscodePresetArg::ProresLt => (
                &[
                    "-c:v",
                    "prores_ks",
                    "-profile:v",
                    "1",
                    "-vendor",
                    "apl0",
                    "-pix_fmt",
                    "yuv422p10le",
                    "-c:a",
                    "pcm_s16le",
                ],
                "mov",
                "prores_lt",
            ),
            TranscodePresetArg::DnxhrLb => (
                &[
                    "-c:v",
                    "dnxhd",
                    "-profile:v",
                    "dnxhr_lb",
                    "-pix_fmt",
                    "yuv422p",
                    "-c:a",
                    "pcm_s16le",
                ],
                "mov",
                "dnxhr_lb",
            ),
            TranscodePresetArg::H264High => (
                &[
                    "-c:v", "libx264", "-preset", "medium", "-crf", "18", "-pix_fmt", "yuv420p",
                    "-c:a", "aac",
                ],
                "mp4",
                "h264_high",
            ),
        }
    }
}

pub async fn transcode_media(state: &AppState, args: TranscodeMediaArgs) -> ToolResult {
    tracing::debug!("tool: transcode_media {}", args.asset_id);
    let input = match asset_file_path(state, args.asset_id).await {
        Ok(p) => p,
        Err(e) => return e,
    };
    if !input.exists() {
        return err_code(
            "AssetOffline",
            format!("file not found: {}", input.display()),
        );
    }
    let tools = match ffmpeg_locate::locate() {
        Ok(t) => t,
        Err(e) => {
            return err_code(
                "FfmpegUnavailable",
                format!("ffmpeg not found ({e}) — set PHOTONIC_FFMPEG_DIR or install ffmpeg"),
            )
        }
    };
    let (enc_args, ext, preset_name) = args.preset.ffmpeg_spec();
    let out_path = match &args.out_path {
        Some(p) => {
            match crate::path_guard::check_path(state, p, photonic_core::PathAccess::Write) {
                Ok(path) => path,
                Err(e) => return e,
            }
        }
        None => {
            let p = input.with_extension(format!("{preset_name}.{ext}"));
            match crate::path_guard::check_path(state, &p, photonic_core::PathAccess::Write) {
                Ok(path) => path,
                Err(e) => return e,
            }
        }
    };
    if out_path == input {
        return ToolResult::error("out_path must differ from the source file");
    }
    let (job_id, cancel) = match state.video_jobs.lock().expect("job registry poisoned").start("transcode_media") {
        Ok(job) => job,
        Err(error) => return ToolResult::error_with_code("JobCapacityExceeded", error.to_string()).with_data(json!({"retryable":true,"max_active_jobs":crate::handlers::video_jobs::MAX_ACTIVE_JOBS})),
    };
    let jobs = std::sync::Arc::clone(&state.video_jobs);
    let out_clone = out_path.clone();
    std::thread::spawn(move || {
        set_job_status(
            &jobs,
            job_id,
            JobStatus::Running {
                progress: 0.0,
                message: format!("transcoding to {preset_name}"),
            },
        );
        let mut cmd = std::process::Command::new(&tools.ffmpeg);
        cmd.arg("-y")
            .args(["-nostdin", "-loglevel", "error"])
            .arg("-i")
            .arg(&input)
            .args(enc_args)
            .arg(&out_clone)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                set_job_status(
                    &jobs,
                    job_id,
                    JobStatus::Failed {
                        error_code: "TranscodeFailed".into(),
                        message: format!("spawn ffmpeg: {e}"),
                    },
                );
                return;
            }
        };
        // Poll for exit / cooperative cancel (kill + clean the partial file).
        let status = loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&out_clone);
                set_job_status(&jobs, job_id, JobStatus::Cancelled);
                return;
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(e) => {
                    set_job_status(
                        &jobs,
                        job_id,
                        JobStatus::Failed {
                            error_code: "TranscodeFailed".into(),
                            message: format!("wait ffmpeg: {e}"),
                        },
                    );
                    return;
                }
            }
        };
        if status.success() {
            set_job_status(
                &jobs,
                job_id,
                JobStatus::Done {
                    result: json!({ "output_path": out_clone, "preset": preset_name }),
                },
            );
        } else {
            let mut stderr = String::new();
            if let Some(mut s) = child.stderr.take() {
                use std::io::Read;
                let _ = s.read_to_string(&mut stderr);
            }
            let tail: String = stderr
                .chars()
                .rev()
                .take(500)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            set_job_status(
                &jobs,
                job_id,
                JobStatus::Failed {
                    error_code: "TranscodeFailed".into(),
                    message: format!("ffmpeg exited with {status}: {tail}"),
                },
            );
        }
    });
    ToolResult::text(format!(
        "transcode job started → {} — poll get_job_status",
        out_path.display()
    ))
    .with_data(json!({ "job_id": job_id, "output_path": out_path }))
}

// ─── Export (10 §3.15) + job tools (10 §6) ───────────────────────────────────

pub(crate) fn find_export_preset(name: &str) -> Option<export_presets::ExportPreset> {
    export_presets::built_in_presets()
        .into_iter()
        .chain(export_presets::load_custom_presets().unwrap_or_default())
        .find(|p| p.name == name)
}

pub async fn export_sequence(state: &AppState, args: ExportSequenceArgs) -> ToolResult {
    super::video_export::export_sequence(state, args).await
}

pub async fn get_job_status(state: &AppState, args: GetJobStatusArgs) -> ToolResult {
    tracing::debug!("tool: get_job_status {}", args.job_id);
    let payload = state
        .video_jobs
        .lock()
        .expect("job registry poisoned")
        .status_json(args.job_id);
    match payload {
        Some(v) => {
            let s = v["status"]["state"].as_str().unwrap_or("?").to_string();
            ToolResult::text(format!("job {} — {s}", args.job_id)).with_data(v)
        }
        None => err_code(
            "JobNotFound",
            format!(
                "job {} unknown or evicted (terminal jobs are retained 10 minutes)",
                args.job_id
            ),
        ),
    }
}

pub async fn cancel_job(state: &AppState, args: CancelJobArgs) -> ToolResult {
    tracing::debug!("tool: cancel_job {}", args.job_id);
    let outcome = state
        .video_jobs
        .lock()
        .expect("job registry poisoned")
        .request_cancel(args.job_id);
    match outcome {
        None => err_code(
            "JobNotFound",
            format!(
                "job {} unknown or evicted (terminal jobs are retained 10 minutes)",
                args.job_id
            ),
        ),
        Some(true) => ToolResult::text(
            "cancellation requested — the worker stops at its next check point \
             (between frames for exports)",
        ),
        Some(false) => ToolResult::text("job already finished — nothing to cancel"),
    }
}

pub async fn list_export_presets(_state: &AppState, _args: ListExportPresetsArgs) -> ToolResult {
    tracing::debug!("tool: list_export_presets");
    let mut out = Vec::new();
    for p in export_presets::built_in_presets() {
        let mut v = serde_json::to_value(&p).unwrap_or_default();
        v["built_in"] = json!(true);
        out.push(v);
    }
    let customs = export_presets::load_custom_presets().unwrap_or_default();
    for p in customs {
        let mut v = serde_json::to_value(&p).unwrap_or_default();
        v["built_in"] = json!(false);
        out.push(v);
    }
    ToolResult::text(format!("{} export preset(s)", out.len())).with_data(json!({ "presets": out }))
}

pub async fn save_export_preset(_state: &AppState, args: SaveExportPresetArgs) -> ToolResult {
    tracing::debug!("tool: save_export_preset {}", args.name);
    let mut preset: export_presets::ExportPreset = match serde_json::from_value(args.preset) {
        Ok(p) => p,
        Err(e) => {
            return ToolResult::error(format!(
                "invalid preset object: {e} — see list_export_presets output for the serde shape"
            ))
        }
    };
    preset.name = args.name.clone();
    if export_presets::built_in_presets()
        .iter()
        .any(|b| b.name == preset.name)
    {
        return err_code(
            "NotSupportedV1",
            format!(
                "{:?} is a built-in preset (read-only) — choose another name",
                preset.name
            ),
        );
    }
    if let Err(e) = export_presets::validate(&preset) {
        return ToolResult::error(format!("preset failed validation: {e}"));
    }
    let mut customs = match export_presets::load_custom_presets() {
        Ok(c) => c,
        Err(e) => return ToolResult::error(format!("could not read custom presets: {e}")),
    };
    let replaced = customs.iter().any(|p| p.name == preset.name);
    customs.retain(|p| p.name != preset.name);
    customs.push(preset);
    if let Err(e) = export_presets::save_custom_presets(&customs) {
        return ToolResult::error(format!("could not persist custom presets: {e}"));
    }
    ToolResult::text(format!(
        "{} custom preset {:?}",
        if replaced { "updated" } else { "saved" },
        args.name
    ))
}

pub async fn delete_export_preset(_state: &AppState, args: DeleteExportPresetArgs) -> ToolResult {
    tracing::debug!("tool: delete_export_preset {}", args.name);
    if export_presets::built_in_presets()
        .iter()
        .any(|b| b.name == args.name)
    {
        return err_code(
            "NotSupportedV1",
            format!(
                "{:?} is a built-in preset (read-only) — it cannot be deleted",
                args.name
            ),
        );
    }
    let mut customs = match export_presets::load_custom_presets() {
        Ok(c) => c,
        Err(e) => return ToolResult::error(format!("could not read custom presets: {e}")),
    };
    let before = customs.len();
    customs.retain(|p| p.name != args.name);
    if customs.len() == before {
        return ToolResult::error(format!("no custom preset named {:?}", args.name));
    }
    if let Err(e) = export_presets::save_custom_presets(&customs) {
        return ToolResult::error(format!("could not persist custom presets: {e}"));
    }
    ToolResult::text(format!("deleted custom preset {:?}", args.name))
}

// ═════════════════════════════════════════════════════════════════════════════
// P4+ slice (10-mcp-tools.md): captions (§3.8), tts (§3.9), grade (§3.10),
// node graph (§3.11), audio (§3.12), title templates (05 §4b). Every mutating
// handler routes through a committed core op / command (design rule 1): a pure
// `ops`/`graph_ops` fn where one exists, otherwise the exact
// `CaptionCmd`/`AudioCmd`/`TtsCmd`/`GraphCmd`/`TimelineCmd` variant it maps to
// (01 §10), executed via `history.execute_discrete` (design rule 4). Lock order
// is document before history (design rule 7).
// ═════════════════════════════════════════════════════════════════════════════

/// Wrap `cmds` so that `target_seq` is the active sequence while they apply,
/// restoring the prior active sequence afterward — needed because a few core
/// `apply` paths (caption-track *creation* via `BulkInsertCues.created_track`,
/// and every master-bus edit) resolve against `TimelineProject::active_sequence`
/// (01 §10). The whole thing is one undo step and leaves the active sequence
/// unchanged. Degrades to a plain batch/single when `target_seq` is already
/// active.
fn with_active_seq(p: &TimelineProject, target_seq: SequenceId, cmds: Vec<TimelineCmd>) -> Command {
    let prev = p.active_sequence;
    if prev == Some(target_seq) || cmds.is_empty() {
        return batch_or_single(cmds);
    }
    let mut wrapped = Vec::with_capacity(cmds.len() + 2);
    wrapped.push(TimelineCmd::SetActiveSequence {
        old: prev,
        new: Some(target_seq),
    });
    wrapped.extend(cmds);
    wrapped.push(TimelineCmd::SetActiveSequence {
        old: Some(target_seq),
        new: prev,
    });
    Command::Batch(wrapped.into_iter().map(Command::Timeline).collect())
}

fn batch_or_single(cmds: Vec<TimelineCmd>) -> Command {
    if cmds.len() == 1 {
        Command::Timeline(cmds.into_iter().next().unwrap())
    } else {
        Command::Batch(cmds.into_iter().map(Command::Timeline).collect())
    }
}

/// Which `(SequenceId, &CaptionTrack)` owns a caption track id.
fn find_caption_track(p: &TimelineProject, track: TrackId) -> Option<(SequenceId, &CaptionTrack)> {
    for (sid, s) in &p.sequences {
        if let Some(t) = s.caption_tracks.iter().find(|t| t.id == track) {
            return Some((*sid, t));
        }
    }
    None
}

/// Which `(SequenceId, TrackId, &CaptionCue)` owns a cue id.
fn find_cue(p: &TimelineProject, cue: CueId) -> Option<(SequenceId, TrackId, &CaptionCue)> {
    for (sid, s) in &p.sequences {
        for ct in &s.caption_tracks {
            if let Some(c) = ct.cues.iter().find(|c| c.id == cue) {
                return Some((*sid, ct.id, c));
            }
        }
    }
    None
}

fn tick_from_seconds_f64(secs: f64) -> Tick {
    Tick((secs * TICKS_PER_SECOND as f64).round() as i64)
}

fn provider_error_code(e: &captions::ProviderError) -> &'static str {
    use captions::ProviderError::*;
    match e {
        Unauthorized => "ProviderAuthError",
        Unavailable => "ProviderAuthError",
        RateLimited => "RateLimited",
        Timeout => "Timeout",
        Cancelled => "Cancelled",
        _ => "ProviderError",
    }
}

// ─── Captions (10 §3.8) ─────────────────────────────────────────────────────

/// Hosted transcription config from environment (D-04). `None` when unset.
fn hosted_transcription_from_env() -> Option<captions::HostedTranscriptionConfig> {
    let base_url = std::env::var("PHOTONIC_TRANSCRIBE_URL").ok()?;
    let auth = std::env::var("PHOTONIC_TRANSCRIBE_TOKEN")
        .ok()
        .map(|t| ("Authorization".to_string(), format!("Bearer {t}")));
    let path = std::env::var("PHOTONIC_TRANSCRIBE_PATH")
        .unwrap_or_else(|_| "/v1/audio/transcriptions".to_string());
    Some(captions::HostedTranscriptionConfig {
        base_url,
        auth_header: auth,
        shape: captions::TranscriptionEndpointShape::OpenAiCompatible { path },
        extra_timeout: Duration::from_secs(0),
    })
}

enum TranscribeChoice {
    Mock(String),
    Hosted(captions::HostedTranscriptionConfig),
}

pub async fn auto_caption(state: &AppState, args: AutoCaptionArgs) -> ToolResult {
    tracing::debug!("tool: auto_caption");
    let provider = args
        .provider
        .clone()
        .unwrap_or_else(|| "hosted".to_string());

    // Resolve target sequence, the absolute placement range (for the mock
    // fixture), the clip's source span + placement offset (for hosted
    // extraction), and the backing audio file.
    let (seq_id, span, source_range, place_offset, audio_path) = {
        let doc = state.document.lock().await;
        let Some(p) = doc.timeline.as_ref() else {
            return ToolResult::error("no timeline project");
        };
        if let Some(clip_id) = args.clip_id {
            let Some((sid, tid)) = locate_clip(p, clip_id) else {
                return ToolResult::error(format!("clip {clip_id} not found"));
            };
            let clip = find_clip(p, sid, tid, clip_id).unwrap();
            let audio = match &clip.source {
                ClipSource::Asset { asset } | ClipSource::Vector { asset } => {
                    p.media.assets.get(asset).and_then(|a| match &a.source {
                        photonic_core::timeline::AssetSource::File { path, .. } => {
                            Some(path.clone())
                        }
                        _ => None,
                    })
                }
                _ => None,
            };
            (
                sid,
                (clip.start, clip.end()),
                Some((clip.source_in, clip.source_in + clip.duration)),
                clip.start,
                audio,
            )
        } else if let Some(sid) = args.sequence_id {
            let Some(s) = p.sequences.get(&sid) else {
                return ToolResult::error(format!("sequence {sid} not found"));
            };
            let end = s
                .video_tracks
                .iter()
                .chain(s.audio_tracks.iter())
                .flat_map(|t| t.clips.iter())
                .map(|c| c.end())
                .max()
                .unwrap_or(Tick(0));
            (sid, (Tick(0), end), None, Tick(0), None)
        } else {
            return ToolResult::error("supply one of sequence_id / clip_id");
        }
    };
    if span.1 <= span.0 {
        return err_code(
            "TickOutOfRange",
            "target range is empty (no content to caption)",
        );
    }

    // Validate provider choice synchronously for a clean start-time error.
    let choice = match provider.as_str() {
        "mock" => {
            let Some(text) = args.mock_transcript.clone() else {
                return err_code(
                    "InvalidRequest",
                    "provider=\"mock\" requires mock_transcript (the deterministic offline transcript)",
                );
            };
            TranscribeChoice::Mock(text)
        }
        "hosted" => {
            let Some(cfg) = hosted_transcription_from_env() else {
                return err_code(
                    "ProviderAuthError",
                    "no hosted transcription provider configured — set PHOTONIC_TRANSCRIBE_URL (+ PHOTONIC_TRANSCRIBE_TOKEN), or use provider=\"mock\" with mock_transcript",
                );
            };
            if source_range.is_none() || audio_path.is_none() {
                return err_code(
                    "InvalidRequest",
                    "hosted transcription needs a file-backed clip audio source — pass clip_id of an imported video/audio asset",
                );
            }
            TranscribeChoice::Hosted(cfg)
        }
        other => {
            return err_code(
                "InvalidRequest",
                format!("unknown provider {other:?} — use \"hosted\" or \"mock\""),
            )
        }
    };

    // Resolve or create the destination caption track.
    let (track_id, created_track): (TrackId, Option<Box<CaptionTrack>>) = {
        let doc = state.document.lock().await;
        let p = doc.timeline.as_ref().unwrap();
        match args.track_id {
            Some(tid) if find_caption_track(p, tid).is_some() => (tid, None),
            Some(_) => return ToolResult::error("track_id is not a caption track"),
            None => {
                let ct = CaptionTrack::new(args.name.clone().unwrap_or_else(|| "Captions".into()));
                (ct.id, Some(Box::new(ct)))
            }
        }
    };

    let (job_id, cancel) = match state.video_jobs.lock().expect("job registry poisoned").start("auto_caption") {
        Ok(job) => job,
        Err(error) => return ToolResult::error_with_code("JobCapacityExceeded", error.to_string()).with_data(json!({"retryable":true,"max_active_jobs":crate::handlers::video_jobs::MAX_ACTIVE_JOBS})),
    };
    let jobs = std::sync::Arc::clone(&state.video_jobs);
    let document = std::sync::Arc::clone(&state.document);
    let history = std::sync::Arc::clone(&state.history);
    let language_hint = args.language_hint.clone();

    std::thread::spawn(move || {
        set_job_status(
            &jobs,
            job_id,
            JobStatus::Running {
                progress: 0.1,
                message: "transcribing".into(),
            },
        );
        let cancel_tok = captions::CancelToken::new();
        if cancel.load(Ordering::Relaxed) {
            set_job_status(&jobs, job_id, JobStatus::Cancelled);
            return;
        }
        let (tx, _rx) = crossbeam_channel::unbounded();
        let (words_result, hosted) = match choice {
            TranscribeChoice::Mock(text) => {
                let prov = captions::MockTranscriptionProvider::fixture(&text, span.0, span.1);
                use captions::TranscriptionProvider;
                (
                    prov.transcribe(
                        captions::TranscriptionRequest {
                            audio_path: std::path::PathBuf::from("mock.wav"),
                            language_hint,
                            model: None,
                        },
                        tx,
                        cancel_tok,
                    ),
                    false,
                )
            }
            TranscribeChoice::Hosted(cfg) => {
                let tools = match ffmpeg_locate::locate() {
                    Ok(t) => t,
                    Err(e) => {
                        set_job_status(
                            &jobs,
                            job_id,
                            JobStatus::Failed {
                                error_code: "FfmpegUnavailable".into(),
                                message: format!("ffmpeg not found ({e})"),
                            },
                        );
                        return;
                    }
                };
                let wav = std::env::temp_dir().join(format!("photonic-mcp-autocap-{job_id}.wav"));
                if let Err(e) = captions::extract::extract_audio_48k_mono(
                    &tools,
                    &audio_path.unwrap(),
                    &wav,
                    source_range,
                ) {
                    set_job_status(
                        &jobs,
                        job_id,
                        JobStatus::Failed {
                            error_code: "ExtractFailed".into(),
                            message: e.to_string(),
                        },
                    );
                    return;
                }
                let prov = captions::HostedTranscriptionProvider::new(cfg);
                use captions::TranscriptionProvider;
                let r = prov.transcribe(
                    captions::TranscriptionRequest {
                        audio_path: wav.clone(),
                        language_hint,
                        model: None,
                    },
                    tx,
                    cancel_tok,
                );
                let _ = std::fs::remove_file(&wav);
                (r, true)
            }
        };

        let mut words = match words_result {
            Ok(r) => r.words,
            Err(e) => {
                set_job_status(
                    &jobs,
                    job_id,
                    JobStatus::Failed {
                        error_code: provider_error_code(&e).into(),
                        message: e.to_string(),
                    },
                );
                return;
            }
        };
        // Hosted adapters return audio-relative ticks (06 §2.2, §3.4: the
        // offset mapping is the caller's job); shift them to the clip's
        // sequence position. The mock fixture is already absolute.
        if hosted {
            for w in &mut words {
                w.start = w.start + place_offset;
                w.end = w.end + place_offset;
            }
        }
        let cues = captions::group_words_into_cues(&words, &captions::GroupingParams::default());
        let cue_count = cues.len();

        let bulk = TimelineCmd::CaptionEdit(CaptionCmd::BulkInsertCues {
            track: track_id,
            cues,
            replace_range: None,
            replaced: Vec::new(),
            created_track,
        });
        {
            let mut doc = document.blocking_lock();
            let cmd = {
                let p = doc.timeline.as_ref();
                match p {
                    Some(p) => with_active_seq(p, seq_id, vec![bulk]),
                    None => {
                        set_job_status(
                            &jobs,
                            job_id,
                            JobStatus::Failed {
                                error_code: "NoProject".into(),
                                message: "timeline project vanished".into(),
                            },
                        );
                        return;
                    }
                }
            };
            let mut hist = history.blocking_lock();
            hist.execute_discrete(cmd, &mut doc);
            hist.schedule_mcp_checkpoint("auto_caption".to_string());
        }
        set_job_status(
            &jobs,
            job_id,
            JobStatus::Done {
                result: json!({ "track_id": track_id, "cue_count": cue_count }),
            },
        );
    });

    ToolResult::text("auto-caption job started — poll get_job_status")
        .with_data(json!({ "job_id": job_id, "track_id": track_id }))
}

pub async fn add_caption_track(state: &AppState, args: AddCaptionTrackArgs) -> ToolResult {
    tracing::debug!("tool: add_caption_track {}", args.sequence_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    if !p.sequences.contains_key(&args.sequence_id) {
        return ToolResult::error(format!("sequence {} not found", args.sequence_id));
    }
    let track = CaptionTrack::new(args.name.unwrap_or_else(|| "Captions".into()));
    let track_id = track.id;
    let bulk = TimelineCmd::CaptionEdit(CaptionCmd::BulkInsertCues {
        track: track_id,
        cues: Vec::new(),
        replace_range: None,
        replaced: Vec::new(),
        created_track: Some(Box::new(track)),
    });
    let cmd = with_active_seq(p, args.sequence_id, vec![bulk]);
    history.execute_discrete(cmd, &mut doc);
    ToolResult::text("Added caption track").with_data(json!({ "track_id": track_id }))
}

pub async fn remove_caption_track(state: &AppState, args: RemoveCaptionTrackArgs) -> ToolResult {
    tracing::debug!("tool: remove_caption_track {}", args.track_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((_, ct)) = find_caption_track(p, args.track_id) else {
        return ToolResult::error(format!("caption track {} not found", args.track_id));
    };
    let inserted_ids: Vec<CueId> = ct.cues.iter().map(|c| c.id).collect();
    // The committed core has no undoable RemoveCaptionTrack: caption tracks are
    // created/removed only as side effects of bulk cue insertion (06 §3.6).
    // `UndoBulkInsert{remove_track}` removes the track structurally; its inverse
    // is `None`, so this participates in history as a checkpoint but is not a
    // fine-grained undo step (documented in the tool description).
    let cmd = TimelineCmd::CaptionEdit(CaptionCmd::UndoBulkInsert {
        track: args.track_id,
        inserted_ids,
        restored: Vec::new(),
        remove_track: true,
    });
    history.execute_discrete(Command::Timeline(cmd), &mut doc);
    ToolResult::text("Removed caption track")
}

pub async fn get_caption_track(state: &AppState, args: GetCaptionTrackArgs) -> ToolResult {
    tracing::debug!("tool: get_caption_track {}", args.track_id);
    let doc = state.document.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, ct)) = find_caption_track(p, args.track_id) else {
        return ToolResult::error(format!("caption track {} not found", args.track_id));
    };
    ToolResult::text(format!(
        "caption track \"{}\" — {} cue(s)",
        ct.name,
        ct.cues.len()
    ))
    .with_data(json!({ "sequence_id": seq_id, "track": ct }))
}

/// Build `CaptionWord`s from explicit per-word timing, or distribute `text`
/// proportionally across `[start, end)` (§2.2 fallback math).
fn build_caption_words(
    words: Option<Vec<CaptionWordArg>>,
    text: Option<String>,
    start: Tick,
    end: Tick,
    fr: FrameRate,
) -> Result<Vec<CaptionWord>, ToolResult> {
    if let Some(ws) = words {
        let mut out = Vec::with_capacity(ws.len());
        for w in ws {
            let s = resolve_tick(
                w.start_ticks,
                w.start_tc.as_deref(),
                w.start_seconds,
                Some(fr),
            )?;
            let e = resolve_tick(w.end_ticks, w.end_tc.as_deref(), w.end_seconds, Some(fr))?;
            out.push(CaptionWord::new(w.text, s, e));
        }
        Ok(out)
    } else if let Some(t) = text {
        Ok(
            captions::proportional::distribute_words_proportionally(&t, start, end)
                .into_iter()
                .map(|tw| CaptionWord::new(tw.text, tw.start, tw.end))
                .collect(),
        )
    } else {
        Err(ToolResult::error(
            "supply `words` (per-word timing) or `text`",
        ))
    }
}

pub async fn set_caption_cue(state: &AppState, args: SetCaptionCueArgs) -> ToolResult {
    tracing::debug!("tool: set_caption_cue {}", args.track_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, ct)) = find_caption_track(p, args.track_id) else {
        return ToolResult::error(format!("caption track {} not found", args.track_id));
    };
    let fr = p.sequences.get(&seq_id).unwrap().frame_rate;
    let has_start =
        args.start_ticks.is_some() || args.start_tc.is_some() || args.start_seconds.is_some();
    let has_end = args.end_ticks.is_some() || args.end_tc.is_some() || args.end_seconds.is_some();

    if let Some(cue_id) = args.cue_id {
        // Edit an existing cue: timing + words in one batch (design rule 4).
        let Some(cue) = ct.cues.iter().find(|c| c.id == cue_id) else {
            return ToolResult::error(format!("cue {cue_id} not found on this track"));
        };
        let cue = cue.clone();
        let mut cmds = Vec::new();
        if has_start || has_end {
            let start = if has_start {
                match resolve_tick(
                    args.start_ticks,
                    args.start_tc.as_deref(),
                    args.start_seconds,
                    Some(fr),
                ) {
                    Ok(t) => t,
                    Err(e) => return e,
                }
            } else {
                cue.start
            };
            let end = if has_end {
                match resolve_tick(
                    args.end_ticks,
                    args.end_tc.as_deref(),
                    args.end_seconds,
                    Some(fr),
                ) {
                    Ok(t) => t,
                    Err(e) => return e,
                }
            } else {
                cue.end
            };
            if end <= start {
                return err_code("TickOutOfRange", "cue end must be after start");
            }
            if (start, end) != (cue.start, cue.end) {
                cmds.push(TimelineCmd::CaptionEdit(CaptionCmd::RetimeCue {
                    track: args.track_id,
                    cue: cue_id,
                    old: (cue.start, cue.end),
                    new: (start, end),
                }));
            }
        }
        if args.words.is_some() || args.text.is_some() {
            let new_words = match build_caption_words(args.words, args.text, cue.start, cue.end, fr)
            {
                Ok(w) => w,
                Err(e) => return e,
            };
            cmds.push(TimelineCmd::CaptionEdit(CaptionCmd::SetCueText {
                track: args.track_id,
                cue: cue_id,
                old_words: cue.words.clone(),
                new_words,
            }));
        }
        if cmds.is_empty() {
            return ToolResult::error("nothing to change — supply timing, words, or text");
        }
        history.execute_discrete(batch_or_single(cmds), &mut doc);
        let note = args
            .position_override
            .map(|_| "position_override is only settable on cue creation in v1 — ignored")
            .unwrap_or("");
        ToolResult::text("Updated caption cue").with_data(json!({ "cue_id": cue_id, "note": note }))
    } else {
        // Create a new cue on the (already-existing) track.
        if !has_start || !has_end {
            return ToolResult::error("creating a cue requires both start and end");
        }
        let start = match resolve_tick(
            args.start_ticks,
            args.start_tc.as_deref(),
            args.start_seconds,
            Some(fr),
        ) {
            Ok(t) => t,
            Err(e) => return e,
        };
        let end = match resolve_tick(
            args.end_ticks,
            args.end_tc.as_deref(),
            args.end_seconds,
            Some(fr),
        ) {
            Ok(t) => t,
            Err(e) => return e,
        };
        if end <= start {
            return err_code("TickOutOfRange", "cue end must be after start");
        }
        let words = match build_caption_words(args.words, args.text, start, end, fr) {
            Ok(w) => w,
            Err(e) => return e,
        };
        let mut cue = CaptionCue::new(start, end, words);
        cue.position_override = args.position_override;
        let cue_id = cue.id;
        let cmd = TimelineCmd::CaptionEdit(CaptionCmd::BulkInsertCues {
            track: args.track_id,
            cues: vec![cue],
            replace_range: None,
            replaced: Vec::new(),
            created_track: None,
        });
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
        ToolResult::text("Added caption cue").with_data(json!({ "cue_id": cue_id }))
    }
}

pub async fn split_caption_cue(state: &AppState, args: SplitCaptionCueArgs) -> ToolResult {
    tracing::debug!("tool: split_caption_cue {}", args.cue_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id, cue)) = find_cue(p, args.cue_id) else {
        return ToolResult::error(format!("cue {} not found", args.cue_id));
    };
    let fr = p.sequences.get(&seq_id).unwrap().frame_rate;
    let at = match resolve_tick(
        args.at_ticks,
        args.at_tc.as_deref(),
        args.at_seconds,
        Some(fr),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    // Split before the first word that starts at/after `at`.
    let idx = cue.words.iter().take_while(|w| w.start < at).count();
    if idx == 0 || idx >= cue.words.len() {
        return err_code(
            "TickOutOfRange",
            "split point must fall strictly between two words of the cue",
        );
    }
    let new_cue_id = CueId::new();
    let cmd = TimelineCmd::CaptionEdit(CaptionCmd::SplitCue {
        track: track_id,
        cue: args.cue_id,
        at_word_index: idx,
        new_cue_id,
    });
    history.execute_discrete(Command::Timeline(cmd), &mut doc);
    ToolResult::text("Split caption cue").with_data(json!({ "new_cue_id": new_cue_id }))
}

pub async fn merge_caption_cues(state: &AppState, args: MergeCaptionCuesArgs) -> ToolResult {
    tracing::debug!(
        "tool: merge_caption_cues {} + {}",
        args.cue_id_a,
        args.cue_id_b
    );
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((_, track_a, cue_a)) = find_cue(p, args.cue_id_a) else {
        return ToolResult::error(format!("cue {} not found", args.cue_id_a));
    };
    let cue_a = cue_a.clone();
    let Some((_, track_b, cue_b)) = find_cue(p, args.cue_id_b) else {
        return ToolResult::error(format!("cue {} not found", args.cue_id_b));
    };
    if track_a != track_b {
        return ToolResult::error("merge requires both cues on the same caption track");
    }
    let cmd = TimelineCmd::CaptionEdit(CaptionCmd::MergeCues {
        track: track_a,
        a: args.cue_id_a,
        b: args.cue_id_b,
        old_a: Box::new(cue_a),
        old_b: Box::new(cue_b.clone()),
    });
    history.execute_discrete(Command::Timeline(cmd), &mut doc);
    ToolResult::text("Merged caption cues")
}

pub async fn set_caption_word(state: &AppState, args: SetCaptionWordArgs) -> ToolResult {
    tracing::debug!(
        "tool: set_caption_word {}[{}]",
        args.cue_id,
        args.word_index
    );
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id, cue)) = find_cue(p, args.cue_id) else {
        return ToolResult::error(format!("cue {} not found", args.cue_id));
    };
    let fr = p.sequences.get(&seq_id).unwrap().frame_rate;
    if args.word_index >= cue.words.len() {
        return ToolResult::error(format!("word index {} out of range", args.word_index));
    }
    let old_words = cue.words.clone();
    let mut new_words = old_words.clone();
    {
        let w = &mut new_words[args.word_index];
        if let Some(t) = args.text {
            w.text = t;
        }
        let has_start =
            args.start_ticks.is_some() || args.start_tc.is_some() || args.start_seconds.is_some();
        let has_end =
            args.end_ticks.is_some() || args.end_tc.is_some() || args.end_seconds.is_some();
        if has_start {
            match resolve_tick(
                args.start_ticks,
                args.start_tc.as_deref(),
                args.start_seconds,
                Some(fr),
            ) {
                Ok(t) => w.start = t,
                Err(e) => return e,
            }
        }
        if has_end {
            match resolve_tick(
                args.end_ticks,
                args.end_tc.as_deref(),
                args.end_seconds,
                Some(fr),
            ) {
                Ok(t) => w.end = t,
                Err(e) => return e,
            }
        }
    }
    let cmd = TimelineCmd::CaptionEdit(CaptionCmd::SetCueText {
        track: track_id,
        cue: args.cue_id,
        old_words,
        new_words,
    });
    history.execute_discrete(Command::Timeline(cmd), &mut doc);
    ToolResult::text("Updated caption word")
}

fn merge_caption_style(
    base: &CaptionStyle,
    arg: &CaptionStyleArg,
) -> Result<CaptionStyle, ToolResult> {
    let mut s = base.clone();
    if let Some(f) = &arg.font_family {
        s.font_family = f.clone();
    }
    if let Some(v) = arg.font_size {
        s.font_size = v;
    }
    if let Some(v) = arg.weight {
        s.weight = v;
    }
    if let Some(hex) = &arg.fill {
        s.fill = Color::from_hex(hex)
            .ok_or_else(|| ToolResult::error(format!("invalid fill color {hex:?}")))?;
    }
    if let Some(pos) = arg.position {
        s.position = pos;
    }
    if let Some(v) = arg.max_width {
        s.max_width = v;
    }
    Ok(s)
}

pub async fn set_caption_style(state: &AppState, args: SetCaptionStyleArgs) -> ToolResult {
    tracing::debug!("tool: set_caption_style");
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    // Resolve the owning track from track_id or the cue.
    let track_id = if let Some(t) = args.track_id {
        t
    } else if let Some(cue) = args.cue_id {
        match find_cue(p, cue) {
            Some((_, t, _)) => t,
            None => return ToolResult::error(format!("cue {cue} not found")),
        }
    } else {
        return ToolResult::error("supply track_id (track scope) or cue_id (cue/word scope)");
    };
    let Some((_, ct)) = find_caption_track(p, track_id) else {
        return ToolResult::error(format!("caption track {track_id} not found"));
    };

    let (target, old, base): (StyleTarget, Option<CaptionStyle>, CaptionStyle) =
        match (args.cue_id, args.word_index) {
            (Some(cue_id), Some(wi)) => {
                let Some(c) = ct.cues.iter().find(|c| c.id == cue_id) else {
                    return ToolResult::error(format!("cue {cue_id} not found on this track"));
                };
                let Some(w) = c.words.get(wi) else {
                    return ToolResult::error(format!("word index {wi} out of range"));
                };
                let base = w
                    .style_override
                    .clone()
                    .or_else(|| c.style_override.clone())
                    .unwrap_or_else(|| ct.style.clone());
                (
                    StyleTarget::Word(cue_id, wi),
                    w.style_override.clone(),
                    base,
                )
            }
            (Some(cue_id), None) => {
                let Some(c) = ct.cues.iter().find(|c| c.id == cue_id) else {
                    return ToolResult::error(format!("cue {cue_id} not found on this track"));
                };
                let base = c.style_override.clone().unwrap_or_else(|| ct.style.clone());
                (StyleTarget::Cue(cue_id), c.style_override.clone(), base)
            }
            (None, _) => (StyleTarget::Track, Some(ct.style.clone()), ct.style.clone()),
        };

    let new: Option<CaptionStyle> = if args.clear && !matches!(target, StyleTarget::Track) {
        None
    } else {
        let arg = args.style.unwrap_or_default();
        match merge_caption_style(&base, &arg) {
            Ok(s) => Some(s),
            Err(e) => return e,
        }
    };

    let cmd = TimelineCmd::CaptionEdit(CaptionCmd::SetStyle {
        track: track_id,
        target,
        old: old.map(Box::new),
        new: new.map(Box::new),
    });
    history.execute_discrete(Command::Timeline(cmd), &mut doc);
    ToolResult::text("Updated caption style")
}

fn caption_format_from(path: &str, explicit: Option<&str>) -> Option<String> {
    if let Some(f) = explicit {
        return Some(f.to_ascii_lowercase());
    }
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

pub async fn import_captions(state: &AppState, args: ImportCaptionsArgs) -> ToolResult {
    tracing::debug!("tool: import_captions {}", args.track_id);
    let cap_path =
        match crate::path_guard::check_path(state, &args.path, photonic_core::PathAccess::Read) {
            Ok(p) => p,
            Err(e) => return e,
        };
    let content = match std::fs::read_to_string(&cap_path) {
        Ok(c) => c,
        Err(e) => return ToolResult::error(format!("could not read {}: {e}", args.path)),
    };
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((_, ct)) = find_caption_track(p, args.track_id) else {
        return ToolResult::error(format!("caption track {} not found", args.track_id));
    };
    let track_name = ct.name.clone();
    let fmt = caption_format_from(&args.path, args.format.as_deref());
    let (cues, notes) = match fmt.as_deref() {
        Some("srt") => match captions::interchange::srt::parse_srt(&content) {
            Ok((c, s)) => (c, s.notes),
            Err(e) => return ToolResult::error(format!("SRT parse error: {e}")),
        },
        Some("vtt") => match captions::interchange::vtt::parse_vtt(&content) {
            Ok((c, s)) => (c, s.notes),
            Err(e) => return ToolResult::error(format!("VTT parse error: {e}")),
        },
        Some("ass") => match captions::interchange::ass::parse_ass(&content, &track_name) {
            Ok(r) => (r.track.cues, r.summary.notes),
            Err(e) => return ToolResult::error(format!("ASS parse error: {e}")),
        },
        other => {
            return ToolResult::error(format!(
                "unknown/unsupported caption format {other:?} — use srt | vtt | ass"
            ))
        }
    };
    let count = cues.len();
    let cmd = TimelineCmd::CaptionEdit(CaptionCmd::BulkInsertCues {
        track: args.track_id,
        cues,
        replace_range: None,
        replaced: Vec::new(),
        created_track: None,
    });
    history.execute_discrete(Command::Timeline(cmd), &mut doc);
    ToolResult::text(format!("Imported {count} caption cue(s)"))
        .with_data(json!({ "cues_imported": count, "notes": notes }))
}

pub async fn export_captions(state: &AppState, args: ExportCaptionsArgs) -> ToolResult {
    tracing::debug!("tool: export_captions {}", args.track_id);
    let doc = state.document.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((_, ct)) = find_caption_track(p, args.track_id) else {
        return ToolResult::error(format!("caption track {} not found", args.track_id));
    };
    let fmt = caption_format_from(&args.path, args.format.as_deref());
    let (text, notes) = match fmt.as_deref() {
        Some("srt") => (captions::interchange::srt::write_srt(&ct.cues), Vec::new()),
        Some("vtt") => (
            captions::interchange::vtt::write_vtt(&ct.cues, true),
            Vec::new(),
        ),
        Some("ass") => {
            let (t, summary) = captions::interchange::ass::write_ass(ct);
            (t, summary.notes)
        }
        other => {
            return ToolResult::error(format!(
                "unknown/unsupported caption format {other:?} — use srt | vtt | ass"
            ))
        }
    };
    let out_path =
        match crate::path_guard::check_path(state, &args.path, photonic_core::PathAccess::Write) {
            Ok(p) => p,
            Err(e) => return e,
        };
    if let Err(e) = std::fs::write(&out_path, text) {
        return ToolResult::error(format!("could not write {}: {e}", out_path.display()));
    }
    ToolResult::text(format!(
        "Exported {} cue(s) to {}",
        ct.cues.len(),
        out_path.display()
    ))
    .with_data(json!({ "path": out_path, "notes": notes }))
}

// ─── TTS (10 §3.9) ───────────────────────────────────────────────────────────

fn hosted_tts_from_env() -> Option<captions::HostedTtsConfig> {
    let base_url = std::env::var("PHOTONIC_TTS_URL").ok()?;
    let auth = std::env::var("PHOTONIC_TTS_TOKEN")
        .ok()
        .map(|t| ("Authorization".to_string(), format!("Bearer {t}")));
    let path =
        std::env::var("PHOTONIC_TTS_PATH").unwrap_or_else(|_| "/v1/audio/speech".to_string());
    let voices_path =
        std::env::var("PHOTONIC_TTS_VOICES_PATH").unwrap_or_else(|_| "/v1/voices".to_string());
    Some(captions::HostedTtsConfig {
        base_url,
        auth_header: auth,
        synthesize_shape: captions::TtsEndpointShape::OpenAiCompatible { path },
        voices_path,
        extra_timeout: Duration::from_secs(0),
    })
}

enum TtsChoice {
    Mock,
    Hosted(captions::HostedTtsConfig),
}

pub async fn generate_voiceover(state: &AppState, args: GenerateVoiceoverArgs) -> ToolResult {
    tracing::debug!("tool: generate_voiceover");
    if args.text.trim().is_empty() {
        return ToolResult::error("text must not be empty");
    }
    let provider = args
        .provider
        .clone()
        .unwrap_or_else(|| "hosted".to_string());
    let (seq_id, fr) = {
        let doc = state.document.lock().await;
        let Some(p) = doc.timeline.as_ref() else {
            return ToolResult::error("no timeline project");
        };
        let Some(seq_id) = locate_track(p, args.track_id) else {
            return ToolResult::error(format!("track {} not found", args.track_id));
        };
        (seq_id, p.sequences.get(&seq_id).unwrap().frame_rate)
    };
    let start = match resolve_tick(
        args.start_ticks,
        args.start_tc.as_deref(),
        args.start_seconds,
        Some(fr),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let choice = match provider.as_str() {
        "mock" => TtsChoice::Mock,
        "hosted" => match hosted_tts_from_env() {
            Some(cfg) => TtsChoice::Hosted(cfg),
            None => {
                return err_code(
                    "ProviderAuthError",
                    "no hosted TTS provider configured — set PHOTONIC_TTS_URL (+ PHOTONIC_TTS_TOKEN), or use provider=\"mock\"",
                )
            }
        },
        other => return err_code("InvalidRequest", format!("unknown provider {other:?}")),
    };
    let voice = args
        .voice
        .clone()
        .unwrap_or_else(|| "mock-voice".to_string());

    let (job_id, cancel) = match state.video_jobs.lock().expect("job registry poisoned").start("generate_voiceover") {
        Ok(job) => job,
        Err(error) => return ToolResult::error_with_code("JobCapacityExceeded", error.to_string()).with_data(json!({"retryable":true,"max_active_jobs":crate::handlers::video_jobs::MAX_ACTIVE_JOBS})),
    };
    let jobs = std::sync::Arc::clone(&state.video_jobs);
    let document = std::sync::Arc::clone(&state.document);
    let history = std::sync::Arc::clone(&state.history);
    let text = args.text.clone();
    let track_id = args.track_id;
    let also_caption = args.also_caption;
    let caption_track_id = args.caption_track_id;

    std::thread::spawn(move || {
        set_job_status(
            &jobs,
            job_id,
            JobStatus::Running {
                progress: 0.2,
                message: "synthesizing".into(),
            },
        );
        if cancel.load(Ordering::Relaxed) {
            set_job_status(&jobs, job_id, JobStatus::Cancelled);
            return;
        }
        let (tx, _rx) = crossbeam_channel::unbounded();
        let req = captions::TtsRequest {
            text: text.clone(),
            voice,
            params: std::collections::HashMap::new(),
        };
        use captions::TtsProvider;
        let result = match choice {
            TtsChoice::Mock => captions::MockTtsProvider::default().synthesize(
                req,
                tx,
                captions::CancelToken::new(),
            ),
            TtsChoice::Hosted(cfg) => captions::HostedTtsProvider::new(cfg).synthesize(
                req,
                tx,
                captions::CancelToken::new(),
            ),
        };
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                set_job_status(
                    &jobs,
                    job_id,
                    JobStatus::Failed {
                        error_code: provider_error_code(&e).into(),
                        message: e.to_string(),
                    },
                );
                return;
            }
        };
        // Persist the synthesized WAV in the proxy/sidecar cache dir and import
        // it as a file-backed audio asset.
        let duration_secs = captions::wav::read_wav_info(&result.audio)
            .map(|i| i.duration_secs())
            .unwrap_or(0.0);
        let dur_ticks = tick_from_seconds_f64(duration_secs).max(Tick(1));
        let cache_dir = video_proxy::proxy_cache_dir(None);
        let _ = std::fs::create_dir_all(&cache_dir);
        let wav_path = cache_dir.join(format!("tts-{job_id}.wav"));
        if let Err(e) = std::fs::write(&wav_path, &result.audio) {
            set_job_status(
                &jobs,
                job_id,
                JobStatus::Failed {
                    error_code: "WriteFailed".into(),
                    message: format!("could not write voiceover audio: {e}"),
                },
            );
            return;
        }
        let asset = photonic_core::timeline::MediaAsset::from_file(AssetKind::Audio, wav_path);
        let asset_id = asset.id;
        let clip = Clip::new(ClipSource::Asset { asset: asset_id }, start, dur_ticks);
        let clip_id = clip.id;

        // Optional word-level captions from the provider's own alignment.
        let caption_cues: Vec<CaptionCue> = if also_caption {
            match &result.word_timings {
                Some(words) => {
                    let shifted: Vec<captions::TranscribedWord> = words
                        .iter()
                        .map(|w| captions::TranscribedWord {
                            text: w.text.clone(),
                            start: w.start + start,
                            end: w.end + start,
                            confidence: w.confidence,
                        })
                        .collect();
                    captions::group_words_into_cues(&shifted, &captions::GroupingParams::default())
                }
                None => Vec::new(),
            }
        } else {
            Vec::new()
        };

        let mut doc = document.blocking_lock();
        let cmd = {
            let Some(p) = doc.timeline.as_ref() else {
                set_job_status(
                    &jobs,
                    job_id,
                    JobStatus::Failed {
                        error_code: "NoProject".into(),
                        message: "timeline project vanished".into(),
                    },
                );
                return;
            };
            let ins = match ops::insert_clip(p, seq_id, track_id, clip) {
                Ok(c) => c,
                Err(e) => {
                    drop(doc);
                    set_job_status(
                        &jobs,
                        job_id,
                        JobStatus::Failed {
                            error_code: "InsertFailed".into(),
                            message: format!("could not place voiceover clip: {e}"),
                        },
                    );
                    return;
                }
            };
            let mut cmds = vec![ops::add_asset(asset), ins];
            let mut cap_track: Option<TrackId> = None;
            if !caption_cues.is_empty() {
                let (ctrack, created) =
                    match caption_track_id.and_then(|t| find_caption_track(p, t).map(|_| t)) {
                        Some(t) => (t, None),
                        None => {
                            let ct = CaptionTrack::new("Voiceover captions");
                            (ct.id, Some(Box::new(ct)))
                        }
                    };
                cap_track = Some(ctrack);
                cmds.push(TimelineCmd::CaptionEdit(CaptionCmd::BulkInsertCues {
                    track: ctrack,
                    cues: caption_cues.clone(),
                    replace_range: None,
                    replaced: Vec::new(),
                    created_track: created,
                }));
            }
            let _ = cap_track;
            with_active_seq(p, seq_id, cmds)
        };
        let mut hist = history.blocking_lock();
        hist.execute_discrete(cmd, &mut doc);
        hist.schedule_mcp_checkpoint("generate_voiceover".to_string());
        drop(hist);
        drop(doc);
        set_job_status(
            &jobs,
            job_id,
            JobStatus::Done {
                result: json!({
                    "asset_id": asset_id,
                    "clip_id": clip_id,
                    "duration_ticks": dur_ticks.0,
                    "captioned": !caption_cues.is_empty(),
                }),
            },
        );
    });

    ToolResult::text("voiceover job started — poll get_job_status")
        .with_data(json!({ "job_id": job_id }))
}

// ─── Grade (10 §3.10) ────────────────────────────────────────────────────────

pub async fn set_grade(state: &AppState, args: SetGradeArgs) -> ToolResult {
    tracing::debug!("tool: set_grade {}", args.clip_id);
    let new_grade: Option<Grade> = match args.grade {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => match serde_json::from_value::<Grade>(v) {
            Ok(g) => Some(g),
            Err(e) => {
                return ToolResult::error(format!(
                "invalid grade object: {e} — see get_clip output / 07 §1 for the Grade serde shape"
            ))
            }
        },
    };
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(p, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    match ops::set_grade(p, seq_id, track_id, args.clip_id, new_grade) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Updated grade")
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn apply_lut(state: &AppState, args: ApplyLutArgs) -> ToolResult {
    tracing::debug!("tool: apply_lut {}", args.clip_id);
    // Validate + resolve the LUT file up front (outside the doc lock).
    let lut_asset: Option<(AssetId, TimelineCmd)> = match &args.lut_path {
        None => None,
        Some(path) => {
            let pb =
                match crate::path_guard::check_path(state, path, photonic_core::PathAccess::Read) {
                    Ok(p) => p,
                    Err(e) => return e,
                };
            if !pb.exists() {
                return err_code("AssetOffline", format!("LUT file not found: {path}"));
            }
            match std::fs::read_to_string(&pb) {
                Ok(src) => {
                    if let Err(e) = photonic_render::parse_cube(&src) {
                        return ToolResult::error(format!("invalid .cube LUT: {e:?}"));
                    }
                }
                Err(e) => return ToolResult::error(format!("could not read LUT: {e}")),
            }
            let asset = photonic_core::timeline::MediaAsset::from_file(AssetKind::Lut3d, pb);
            let id = asset.id;
            Some((id, ops::add_asset(asset)))
        }
    };

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(p, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let clip = find_clip(p, seq_id, track_id, args.clip_id).unwrap();
    let mut grade = clip.grade.clone().unwrap_or_default();
    // Drop any existing LUT op(s) — a clip carries at most one LUT in this tool.
    grade.ops.retain(|o| o.kind != GradeOpKind::Lut3d);
    let mut cmds = Vec::new();
    if let Some((asset_id, add_cmd)) = &lut_asset {
        cmds.push(add_cmd.clone());
        let op = GradeOp::new(
            GradeOpKind::Lut3d,
            GradeOpParams::Lut3d {
                asset: *asset_id,
                intensity: args.intensity.unwrap_or(1.0).clamp(0.0, 1.0),
                interp: photonic_core::timeline::LutInterp::Trilinear,
            },
        );
        grade.ops.push(op);
    }
    let new_grade = if grade.ops.is_empty() {
        None
    } else {
        Some(grade)
    };
    match ops::set_grade(p, seq_id, track_id, args.clip_id, new_grade) {
        Ok(set_cmd) => {
            cmds.push(set_cmd);
            history.execute_discrete(batch_or_single(cmds), &mut doc);
            if lut_asset.is_some() {
                ToolResult::text("Applied LUT to clip grade")
            } else {
                ToolResult::text("Removed LUT from clip grade")
            }
        }
        Err(e) => map_edit_error(e),
    }
}

pub async fn copy_grade(state: &AppState, args: CopyGradeArgs) -> ToolResult {
    tracing::debug!(
        "tool: copy_grade {} -> {} target(s)",
        args.source_clip_id,
        args.target_clip_ids.len()
    );
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((s_seq, s_track)) = locate_clip(p, args.source_clip_id) else {
        return ToolResult::error(format!("source clip {} not found", args.source_clip_id));
    };
    let grade = find_clip(p, s_seq, s_track, args.source_clip_id).and_then(|c| c.grade.clone());
    let mut cmds = Vec::new();
    let mut applied = 0usize;
    for target in &args.target_clip_ids {
        let Some((seq_id, track_id)) = locate_clip(p, *target) else {
            return ToolResult::error(format!("target clip {target} not found"));
        };
        match ops::set_grade(p, seq_id, track_id, *target, grade.clone()) {
            Ok(cmd) => {
                cmds.push(Command::Timeline(cmd));
                applied += 1;
            }
            Err(e) => return map_edit_error(e),
        }
    }
    if cmds.is_empty() {
        return ToolResult::error("no target clips given");
    }
    history.execute_discrete(Command::Batch(cmds), &mut doc);
    ToolResult::text(format!("Copied grade to {applied} clip(s)"))
}

fn grade_presets_path() -> Option<std::path::PathBuf> {
    export_presets::config_dir().map(|d| d.join("grade_presets.json"))
}

fn load_grade_presets() -> std::collections::BTreeMap<String, Grade> {
    let Some(path) = grade_presets_path() else {
        return Default::default();
    };
    std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_grade_presets(map: &std::collections::BTreeMap<String, Grade>) -> Result<(), String> {
    let Some(path) = grade_presets_path() else {
        return Err("no config directory available".into());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(map).map_err(|e| e.to_string())?;
    std::fs::write(&path, bytes).map_err(|e| e.to_string())
}

pub async fn grade_preset(state: &AppState, args: GradePresetArgs) -> ToolResult {
    tracing::debug!("tool: grade_preset");
    match args.op {
        GradePresetOp::List => {
            let names: Vec<String> = load_grade_presets().into_keys().collect();
            ToolResult::text(format!("{} grade preset(s)", names.len()))
                .with_data(json!({ "presets": names }))
        }
        GradePresetOp::Save => {
            let (Some(clip_id), Some(name)) = (args.clip_id, args.name.clone()) else {
                return ToolResult::error("save requires clip_id and name");
            };
            let doc = state.document.lock().await;
            let Some(p) = doc.timeline.as_ref() else {
                return ToolResult::error("no timeline project");
            };
            let Some((seq_id, track_id)) = locate_clip(p, clip_id) else {
                return ToolResult::error(format!("clip {clip_id} not found"));
            };
            let Some(grade) = find_clip(p, seq_id, track_id, clip_id).and_then(|c| c.grade.clone())
            else {
                return ToolResult::error("clip has no grade to save");
            };
            drop(doc);
            let mut map = load_grade_presets();
            map.insert(name.clone(), grade);
            if let Err(e) = save_grade_presets(&map) {
                return ToolResult::error(format!("could not save preset: {e}"));
            }
            ToolResult::text(format!("Saved grade preset {name:?}"))
        }
        GradePresetOp::Apply => {
            let (Some(clip_id), Some(name)) = (args.clip_id, args.name.clone()) else {
                return ToolResult::error("apply requires clip_id and name");
            };
            let map = load_grade_presets();
            let Some(grade) = map.get(&name).cloned() else {
                return ToolResult::error(format!("no grade preset named {name:?}"));
            };
            let mut doc = state.document.lock().await;
            let mut history = state.history.lock().await;
            let Some(p) = doc.timeline.as_ref() else {
                return ToolResult::error("no timeline project");
            };
            let Some((seq_id, track_id)) = locate_clip(p, clip_id) else {
                return ToolResult::error(format!("clip {clip_id} not found"));
            };
            match ops::set_grade(p, seq_id, track_id, clip_id, Some(grade)) {
                Ok(cmd) => {
                    history.execute_discrete(Command::Timeline(cmd), &mut doc);
                    ToolResult::text(format!("Applied grade preset {name:?}"))
                }
                Err(e) => map_edit_error(e),
            }
        }
    }
}

pub async fn get_scopes(state: &AppState, args: GetScopesArgs) -> ToolResult {
    tracing::debug!("tool: get_scopes {}", args.clip_id);
    // Resolve the clip's owning sequence + tick.
    let (seq_id, fr) = {
        let doc = state.document.lock().await;
        let Some(p) = doc.timeline.as_ref() else {
            return ToolResult::error("no timeline project");
        };
        let Some((seq_id, _)) = locate_clip(p, args.clip_id) else {
            return ToolResult::error(format!("clip {} not found", args.clip_id));
        };
        (seq_id, p.sequences.get(&seq_id).unwrap().frame_rate)
    };
    let t = match resolve_tick(
        args.at_ticks,
        args.at_tc.as_deref(),
        args.at_seconds,
        Some(fr),
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    // K-E2: the scopes tap is a per-clip readback point, not the program frame.
    // `render_scope_tap_pixels` asks the engine for the clip's post-`Grade`
    // texture and reports which point it actually got, so an agent grading a
    // clip under a caption track or a second video track measures the signal it
    // is adjusting (03 §3.6 as amended by 27 A-7).
    let want = match args.tap {
        ScopeTap::Clip => ScopeTapPoint::Clip(args.clip_id),
        ScopeTap::Program => ScopeTapPoint::Program,
    };
    let (pixels, w, h, got) =
        match render_scope_tap_pixels(state, seq_id, t, args.format_index, want).await {
            Ok(v) => v,
            Err(e) => return e,
        };
    let flat: Vec<f32> = pixels.iter().flat_map(|p| p.iter().copied()).collect();
    let scopes = photonic_render::scopes::scopes_from_pixels_cpu(&flat, w, h);
    let tap_label = match got {
        ScopeTapPoint::Clip(_) => "clip",
        ScopeTapPoint::Program => "program",
    };
    let fell_back = want != got;
    let mut data = scopes_json(&scopes, t);
    if let Some(obj) = data.as_object_mut() {
        obj.insert("tap".into(), json!(tap_label));
        obj.insert("width".into(), json!(w));
        obj.insert("height".into(), json!(h));
        if fell_back {
            obj.insert(
                "tap_fallback_reason".into(),
                json!(
                    "the requested clip is not rendered at this tick (off its span, \
                       a disabled track, or zero opacity) — scoped the program instead"
                ),
            );
        }
    }
    let note = if fell_back {
        " (fell back from the clip tap — the clip is not in this frame)"
    } else {
        ""
    };
    ToolResult::text(format!(
        "scopes for {w}x{h} {tap_label} tap at tick {}{note}",
        t.0
    ))
    .with_data(data)
}

/// Compact, agent-consumable scope payload (07 §5): full histograms plus a
/// down-sampled luma waveform (per-column brightest populated bin) and a 32×32
/// vectorscope count grid — "data, not an image" (10 §3.10).
fn scopes_json(s: &photonic_render::scopes::Scopes, t: Tick) -> serde_json::Value {
    let h = &s.histogram;
    // Waveform: for up to 256 sampled columns, the brightest populated bin.
    let cols = s.waveform.width.max(1);
    let step = cols.div_ceil(256).max(1);
    let mut luma_peaks = Vec::new();
    let mut x = 0;
    while x < cols {
        let mut top = 0usize;
        for bin in (0..s.waveform.bins).rev() {
            if s.waveform.count(x, bin) > 0 {
                top = bin;
                break;
            }
        }
        luma_peaks.push(top as u32);
        x += step;
    }
    // Vectorscope: down-sample the 256×256 grid to 32×32 sums.
    let vs_size = s.vectorscope.size.max(1);
    const GRID: usize = 32;
    let mut vs = vec![0u32; GRID * GRID];
    for cr in 0..vs_size {
        for cb in 0..vs_size {
            let c = s.vectorscope.count(cb, cr);
            if c > 0 {
                let gr = cr * GRID / vs_size;
                let gb = cb * GRID / vs_size;
                vs[gr * GRID + gb] += c;
            }
        }
    }
    json!({
        "tick": t.0,
        "histogram": {
            "bins": h.luma.len(),
            "luma": h.luma.to_vec(), "red": h.red.to_vec(),
            "green": h.green.to_vec(), "blue": h.blue.to_vec(),
        },
        "waveform": {
            "columns": luma_peaks.len(),
            "bins": s.waveform.bins,
            "luma_peaks": luma_peaks,
        },
        "vectorscope": { "grid": GRID, "counts": vs },
        "note": "linear working-space samples (D-09); waveform luma_peaks are 0..255 bin indices, one per sampled column",
    })
}

/// K-E2: render one frame headlessly and read back the **scope tap** —
/// `want` resolved against that frame's graph, which is `ScopeTapPoint::Program`
/// whenever the requested clip is not in the frame (13 §10.2). Returns the
/// pixels, their logical size (the pooled texture is bucket-padded, so the
/// frame's own logical dims are used, never the texture's) and the point that
/// actually produced them.
///
/// This costs no second render: the tap names a node the same evaluation
/// already produced (see `Evaluator::evaluate_with_tap`). What it does cost is
/// the same seek-and-wait transaction `render_frame_at` pays, so it stays behind
/// the transport lock like every other engine read.
async fn render_scope_tap_pixels(
    state: &AppState,
    seq_id: SequenceId,
    t: Tick,
    format_index: Option<usize>,
    want: ScopeTapPoint,
) -> Result<(Vec<[f32; 4]>, u32, u32, ScopeTapPoint), ToolResult> {
    let bridge = engine_bridge(state)?;
    let (fr, formats, active_format) = sequence_render_info(state, seq_id).await?;
    if let Some(fi) = format_index {
        if fi >= formats.len() {
            return Err(ToolResult::error(format!(
                "format_index {fi} out of range — sequence has {} format(s)",
                formats.len()
            )));
        }
    }
    let fi = format_index
        .unwrap_or(active_format)
        .min(formats.len().saturating_sub(1));
    if t.0 < 0 {
        return Err(err_code("TickOutOfRange", "tick must be >= 0"));
    }
    let snapped = fr.frame_start(fr.frame_at(t));

    let _transport = bridge.lock_transport().await;
    bridge.sync_with_format(state, Some((seq_id, fi))).await;
    if !bridge.wait_engine_synced(Duration::from_secs(10)).await {
        return Err(ToolResult::error(
            "engine did not pick up the document snapshot within 10s",
        ));
    }
    let _settings = RenderSettingsGuard {
        session: bridge.session(),
        proxy_mode: bridge.proxy_mode(),
        preview_quality: bridge.session().requested_preview_quality(),
        scope_tap: Some(bridge.session().status().scope_tap),
    };
    let prev = bridge.session().latest_frame();
    let request_id = bridge.next_inspection_id();
    if !bridge.session().send(EngineCmd::InspectFrame {
        request_id,
        sequence: seq_id,
        time: snapped,
        proxy_mode: ProxyMode::ForceOriginal,
        quality: PreviewQuality::Full,
        scope_tap: want,
    }) {
        return Err(
            err_code("EngineBusy", "Engine command queue is full or closed")
                .with_data(json!({"retryable":true})),
        );
    }
    let frame = bridge
        .wait_fresh_frame(prev, Duration::from_secs(30), |f| {
            f.time == snapped
                && f.sequence == seq_id
                && f.preview_quality == PreviewQuality::Full
                && f.snapshot_generation == bridge.snapshot_generation()
                && f.inspection_request_id == Some(request_id)
                && f.proxy_mode == ProxyMode::ForceOriginal
        })
        .await;
    let Some(frame) = frame else {
        return Err(ToolResult::error(
            "engine did not produce the requested frame within 30s",
        ));
    };
    let Some(tap) = frame.scope_tap.as_ref() else {
        return Err(err_code(
            "NoScopeSignal",
            "the sequence renders nothing at this tick — there is no signal to scope",
        ));
    };
    Ok((
        read_texture_rgba16f(bridge.engine().gpu(), &tap.texture, tap.width, tap.height),
        tap.width,
        tap.height,
        frame.scope_tap_point,
    ))
}

// ─── Node graph (10 §3.11) ───────────────────────────────────────────────────

pub async fn create_clip_composition(
    state: &AppState,
    args: CreateClipCompositionArgs,
) -> ToolResult {
    tracing::debug!("tool: create_clip_composition {}", args.clip_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(p, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    if args.detach {
        return match ops::detach_clip_composition(p, seq_id, track_id, args.clip_id) {
            Ok(cmd) => {
                history.execute_discrete(Command::Timeline(cmd), &mut doc);
                ToolResult::text("Detached clip composition")
            }
            Err(e) => map_edit_error(e),
        };
    }
    if let Some(src) = args.graph_id {
        if !p.graphs.contains_key(&src) {
            return err_code(
                "GraphTypeMismatch",
                format!("graph {src} not found to paste"),
            );
        }
        return match ops::paste_clip_composition(p, src, seq_id, track_id, args.clip_id) {
            Ok(cmds) => {
                let new_id = cmds.iter().find_map(|c| match c {
                    TimelineCmd::AddGraph { graph } => Some(graph.id),
                    _ => None,
                });
                history.execute_discrete(
                    Command::Batch(cmds.into_iter().map(Command::Timeline).collect()),
                    &mut doc,
                );
                ToolResult::text("Pasted clip composition").with_data(json!({ "graph_id": new_id }))
            }
            Err(e) => map_edit_error(e),
        };
    }
    match ops::create_clip_composition(p, seq_id, track_id, args.clip_id) {
        Ok(cmds) => {
            let new_id = cmds.iter().find_map(|c| match c {
                TimelineCmd::AddGraph { graph } => Some(graph.id),
                _ => None,
            });
            history.execute_discrete(
                Command::Batch(cmds.into_iter().map(Command::Timeline).collect()),
                &mut doc,
            );
            ToolResult::text("Created clip composition").with_data(json!({ "graph_id": new_id }))
        }
        Err(e) => map_edit_error(e),
    }
}

fn map_graph_error(e: EditError) -> ToolResult {
    match e {
        EditError::WouldCreateCycle => err_code(
            "CycleDetected",
            "edge would create a cycle in the node graph",
        ),
        EditError::NoGraph(id) => err_code(
            "GraphTypeMismatch",
            format!("graph {id} or a referenced node not found"),
        ),
        other => map_edit_error(other),
    }
}

pub async fn add_graph_node(state: &AppState, args: AddGraphNodeArgs) -> ToolResult {
    tracing::debug!("tool: add_graph_node {}", args.graph_id);
    let op: GraphOp = match serde_json::from_value(args.op) {
        Ok(o) => o,
        Err(e) => {
            return ToolResult::error(format!(
                "invalid graph op: {e} — e.g. {{\"op\":\"blur\"}} (08 §2)"
            ))
        }
    };
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    if !p.graphs.contains_key(&args.graph_id) {
        return err_code(
            "GraphTypeMismatch",
            format!("graph {} not found", args.graph_id),
        );
    }
    let node = GraphNode::new(op);
    let node_id = node.id;
    let pos = args
        .pos
        .map(|[x, y]| NodePos { x, y })
        .unwrap_or(NodePos { x: 0.0, y: 0.0 });
    let cmd = graph_ops::add_node(args.graph_id, node, pos);
    history.execute_discrete(Command::Timeline(cmd), &mut doc);
    ToolResult::text("Added graph node").with_data(json!({ "node_id": node_id }))
}

pub async fn remove_graph_node(state: &AppState, args: RemoveGraphNodeArgs) -> ToolResult {
    tracing::debug!("tool: remove_graph_node {}", args.node_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    match graph_ops::remove_node(p, args.graph_id, args.node_id) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Removed graph node")
        }
        Err(e) => map_graph_error(e),
    }
}

pub async fn add_graph_edge(state: &AppState, args: AddGraphEdgeArgs) -> ToolResult {
    tracing::debug!("tool: add_graph_edge {}", args.graph_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let from = (args.from.node_id, OutPort(args.from.port.unwrap_or(0)));
    let to = (args.to.node_id, InPort(args.to.port.unwrap_or(0)));
    match graph_ops::add_edge(p, args.graph_id, from, to) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Added graph edge")
        }
        Err(e) => map_graph_error(e),
    }
}

pub async fn remove_graph_edge(state: &AppState, args: RemoveGraphEdgeArgs) -> ToolResult {
    tracing::debug!("tool: remove_graph_edge {}", args.graph_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(g) = p.graphs.get(&args.graph_id) else {
        return err_code(
            "GraphTypeMismatch",
            format!("graph {} not found", args.graph_id),
        );
    };
    let Some(edge) = g.edges.get(args.edge_index).copied() else {
        return ToolResult::error(format!(
            "edge_index {} out of range — graph has {} edge(s)",
            args.edge_index,
            g.edges.len()
        ));
    };
    let cmd = graph_ops::remove_edge(args.graph_id, edge);
    history.execute_discrete(Command::Timeline(cmd), &mut doc);
    ToolResult::text("Removed graph edge")
}

pub async fn set_graph_node_param(state: &AppState, args: SetGraphNodeParamArgs) -> ToolResult {
    tracing::debug!("tool: set_graph_node_param {}", args.node_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(g) = p.graphs.get(&args.graph_id) else {
        return err_code(
            "GraphTypeMismatch",
            format!("graph {} not found", args.graph_id),
        );
    };
    let Some(node) = g.nodes.get(&args.node_id) else {
        return err_code(
            "GraphTypeMismatch",
            format!("node {} not found", args.node_id),
        );
    };
    let mut new_params: GraphNodeParams = node.params.base.clone();
    new_params.0.set(args.path.as_str(), args.value);
    match graph_ops::set_node_param(p, args.graph_id, args.node_id, new_params) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Updated graph node param")
        }
        Err(e) => map_graph_error(e),
    }
}

pub async fn set_project_graph(state: &AppState, args: SetProjectGraphArgs) -> ToolResult {
    tracing::debug!("tool: set_project_graph");
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    if args.clear {
        let cmd = TimelineCmd::SetProjectGraph {
            old: p.project_graph,
            new: None,
        };
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
        return ToolResult::text("Cleared project graph");
    }
    if let Some(gid) = args.graph_id {
        if !p.graphs.contains_key(&gid) {
            return err_code("GraphTypeMismatch", format!("graph {gid} not found"));
        }
        let cmd = TimelineCmd::SetProjectGraph {
            old: p.project_graph,
            new: Some(gid),
        };
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
        return ToolResult::text("Set project graph").with_data(json!({ "graph_id": gid }));
    }
    // Create a fresh empty project graph.
    let cmds = ops::set_project_graph(p, None);
    let new_id = cmds.iter().find_map(|c| match c {
        TimelineCmd::AddGraph { graph } => Some(graph.id),
        _ => None,
    });
    history.execute_discrete(
        Command::Batch(cmds.into_iter().map(Command::Timeline).collect()),
        &mut doc,
    );
    ToolResult::text("Created project graph").with_data(json!({ "graph_id": new_id }))
}

pub async fn get_graph(state: &AppState, args: GetGraphArgs) -> ToolResult {
    tracing::debug!("tool: get_graph {}", args.graph_id);
    let doc = state.document.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(g) = p.graphs.get(&args.graph_id) else {
        return err_code(
            "GraphTypeMismatch",
            format!("graph {} not found", args.graph_id),
        );
    };
    let has_output = g.nodes.values().any(|n| matches!(n.op, GraphOp::Output));
    let has_cycle = g.has_cycle();
    let mut diagnostics = Vec::new();
    if has_cycle {
        diagnostics.push("graph contains a cycle".to_string());
    }
    if !has_output {
        diagnostics.push("graph has no Output node".to_string());
    }
    ToolResult::text(format!(
        "graph \"{}\" — {} node(s), {} edge(s){}",
        g.name,
        g.nodes.len(),
        g.edges.len(),
        if diagnostics.is_empty() {
            ""
        } else {
            " (has diagnostics)"
        }
    ))
    .with_data(json!({
        "graph": g,
        "diagnostics": diagnostics,
        "compiles": diagnostics.is_empty(),
    }))
}

// ─── Audio (10 §3.12) ────────────────────────────────────────────────────────

pub async fn set_clip_audio(state: &AppState, args: SetClipAudioArgs) -> ToolResult {
    tracing::debug!("tool: set_clip_audio {}", args.clip_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(p, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    // AudioCmd set-prop ops no-op when the clip has no ClipAudio container yet
    // (there is no committed "create ClipAudio" command); ensure it exists so
    // the value edits below take effect. The zero-value default init is
    // observationally equivalent to absent, so undoing the value edits is exact.
    let need_init = find_clip(p, seq_id, track_id, args.clip_id)
        .map(|c| c.audio.is_none())
        .unwrap_or(false);
    if need_init {
        if let Some(c) = doc
            .timeline
            .as_mut()
            .and_then(|p| p.sequences.get_mut(&seq_id))
            .and_then(|s| {
                s.video_tracks
                    .iter_mut()
                    .chain(s.audio_tracks.iter_mut())
                    .find(|t| t.id == track_id)
            })
            .and_then(|t| t.clips.iter_mut().find(|c| c.id == args.clip_id))
        {
            c.audio = Some(ClipAudio::new());
        }
    }
    let p = doc.timeline.as_ref().unwrap();
    let clip = find_clip(p, seq_id, track_id, args.clip_id).unwrap();
    let audio = clip.audio.clone().unwrap_or_default();
    let mut cmds: Vec<TimelineCmd> = Vec::new();
    if let Some(gain) = args.gain_db {
        let old = audio.params.base;
        let new = ClipAudioParams { gain_db: gain };
        if new != old {
            cmds.push(TimelineCmd::AudioEdit(AudioCmd::SetClipAudioProp {
                clip: args.clip_id,
                old,
                new,
            }));
        }
    }
    let shape = args.fade_shape.unwrap_or(FadeShape::EqualPower);
    if let Some(ft) = args.fade_in_ticks {
        let new = (ft > 0).then_some(AudioFade {
            duration: Tick(ft),
            shape,
        });
        cmds.push(TimelineCmd::AudioEdit(AudioCmd::SetClipFade {
            clip: args.clip_id,
            edge: FadeEdge::In,
            old: audio.fade_in,
            new,
        }));
    }
    if let Some(ft) = args.fade_out_ticks {
        let new = (ft > 0).then_some(AudioFade {
            duration: Tick(ft),
            shape,
        });
        cmds.push(TimelineCmd::AudioEdit(AudioCmd::SetClipFade {
            clip: args.clip_id,
            edge: FadeEdge::Out,
            old: audio.fade_out,
            new,
        }));
    }
    if let Some(cm) = args.channel_map {
        if cm != audio.channel_map {
            cmds.push(TimelineCmd::AudioEdit(AudioCmd::SetChannelMap {
                clip: args.clip_id,
                old: audio.channel_map,
                new: cm,
            }));
        }
    }
    if cmds.is_empty() && !need_init {
        return ToolResult::error(
            "nothing to change — supply gain_db / fade_*_ticks / channel_map",
        );
    }
    if need_init {
        let mut hist2 = history;
        hist2.schedule_mcp_checkpoint("set_clip_audio");
        if !cmds.is_empty() {
            hist2.execute_discrete(batch_or_single(cmds), &mut doc);
        }
    } else {
        history.execute_discrete(batch_or_single(cmds), &mut doc);
    }
    ToolResult::text("Updated clip audio")
}

pub async fn set_track_audio(state: &AppState, args: SetTrackAudioArgs) -> ToolResult {
    tracing::debug!("tool: set_track_audio {}", args.track_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq_id) = locate_track(p, args.track_id) else {
        return ToolResult::error(format!("track {} not found", args.track_id));
    };
    let need_init = p
        .sequences
        .get(&seq_id)
        .and_then(|s| s.track(args.track_id))
        .map(|t| t.audio.is_none())
        .unwrap_or(false);
    if need_init {
        if let Some(t) = doc
            .timeline
            .as_mut()
            .and_then(|p| p.sequences.get_mut(&seq_id))
            .and_then(|s| {
                s.video_tracks
                    .iter_mut()
                    .chain(s.audio_tracks.iter_mut())
                    .find(|t| t.id == args.track_id)
            })
        {
            t.audio = Some(TrackAudio::new());
        }
    }
    let p = doc.timeline.as_ref().unwrap();
    let audio = p
        .sequences
        .get(&seq_id)
        .and_then(|s| s.track(args.track_id))
        .and_then(|t| t.audio.clone())
        .unwrap_or_default();
    let mut cmds: Vec<TimelineCmd> = Vec::new();
    if args.volume_db.is_some() || args.pan.is_some() {
        let old = audio.params.base;
        let new = TrackAudioParams {
            volume_db: args.volume_db.unwrap_or(old.volume_db),
            pan: args.pan.unwrap_or(old.pan),
        };
        if new != old {
            cmds.push(TimelineCmd::AudioEdit(AudioCmd::SetTrackAudioProp {
                track: args.track_id,
                old,
                new,
            }));
        }
    }
    if args.muted.is_some() || args.solo.is_some() {
        let old = (audio.mute, audio.solo);
        let new = (
            args.muted.unwrap_or(audio.mute),
            args.solo.unwrap_or(audio.solo),
        );
        if new != old {
            cmds.push(TimelineCmd::AudioEdit(AudioCmd::SetTrackMuteSolo {
                track: args.track_id,
                old,
                new,
            }));
        }
    }
    if need_init {
        history.schedule_mcp_checkpoint("set_track_audio");
    }
    if !cmds.is_empty() {
        history.execute_discrete(batch_or_single(cmds), &mut doc);
    } else if !need_init {
        return ToolResult::error("nothing to change — supply volume_db / pan / muted / solo");
    }
    ToolResult::text("Updated track audio")
}

pub async fn audio_fx(state: &AppState, args: AudioFxArgs) -> ToolResult {
    tracing::debug!("tool: audio_fx {}", args.track_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq_id) = locate_track(p, args.track_id) else {
        return ToolResult::error(format!("track {} not found", args.track_id));
    };
    let need_init = p
        .sequences
        .get(&seq_id)
        .and_then(|s| s.track(args.track_id))
        .map(|t| t.audio.is_none())
        .unwrap_or(false);
    if need_init {
        if let Some(t) = doc
            .timeline
            .as_mut()
            .and_then(|p| p.sequences.get_mut(&seq_id))
            .and_then(|s| {
                s.video_tracks
                    .iter_mut()
                    .chain(s.audio_tracks.iter_mut())
                    .find(|t| t.id == args.track_id)
            })
        {
            t.audio = Some(TrackAudio::new());
        }
        history.schedule_mcp_checkpoint("audio_fx");
    }
    let owner = FxOwner::Track(args.track_id);
    let p = doc.timeline.as_ref().unwrap();
    let chain_len = p
        .sequences
        .get(&seq_id)
        .and_then(|s| s.track(args.track_id))
        .and_then(|t| t.audio.as_ref())
        .map(|a| a.fx_chain.len())
        .unwrap_or(0);
    let cmd = match args.op {
        AudioFxOp::Add => {
            let Some(kind) = args.kind else {
                return ToolResult::error(
                    "audio_fx op=add requires kind (eq|compressor|limiter|gate)",
                );
            };
            let index = args.index.unwrap_or(chain_len).min(chain_len);
            TimelineCmd::AudioEdit(AudioCmd::AddAudioFx {
                owner,
                index,
                unit: AudioFxUnit::new(kind),
            })
        }
        AudioFxOp::Remove => {
            let Some(index) = args.index else {
                return ToolResult::error("audio_fx op=remove requires index");
            };
            let unit = p
                .sequences
                .get(&seq_id)
                .and_then(|s| s.track(args.track_id))
                .and_then(|t| t.audio.as_ref())
                .and_then(|a| a.fx_chain.get(index))
                .cloned();
            let Some(unit) = unit else {
                return ToolResult::error(format!("fx index {index} out of range"));
            };
            TimelineCmd::AudioEdit(AudioCmd::RemoveAudioFx { owner, index, unit })
        }
        AudioFxOp::Reorder => {
            let Some(new_order) = args.new_order.clone() else {
                return ToolResult::error("audio_fx op=reorder requires new_order");
            };
            if new_order.len() != chain_len {
                return ToolResult::error("new_order length must match the fx chain length");
            }
            let old_order: Vec<usize> = (0..chain_len).collect();
            TimelineCmd::AudioEdit(AudioCmd::ReorderAudioFx {
                owner,
                old_order,
                new_order,
            })
        }
    };
    history.execute_discrete(Command::Timeline(cmd), &mut doc);
    ToolResult::text("Updated audio fx chain")
}

pub async fn set_master_bus(state: &AppState, args: SetMasterBusArgs) -> ToolResult {
    tracing::debug!("tool: set_master_bus {}", args.sequence_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some(seq) = p.sequences.get(&args.sequence_id) else {
        return ToolResult::error(format!("sequence {} not found", args.sequence_id));
    };
    let master = seq.audio_master.clone();
    let mut cmds: Vec<TimelineCmd> = Vec::new();
    if let Some(v) = args.volume_db {
        let old = master.params.base;
        let new = MasterBusParams { volume_db: v };
        if new != old {
            cmds.push(TimelineCmd::AudioEdit(AudioCmd::SetMasterBusProp {
                old,
                new,
            }));
        }
    }
    if let Some(l) = &args.loudness {
        let new = match l.to_ascii_lowercase().as_str() {
            "streaming" => Some(LoudnessTarget::streaming()),
            "broadcast" => Some(LoudnessTarget::broadcast()),
            "none" => None,
            other => {
                return ToolResult::error(format!(
                    "unknown loudness target {other:?} — use streaming | broadcast | none"
                ))
            }
        };
        if new != master.loudness_target {
            cmds.push(TimelineCmd::AudioEdit(AudioCmd::SetLoudnessTarget {
                old: master.loudness_target,
                new,
            }));
        }
    }
    if cmds.is_empty() {
        return ToolResult::error("nothing to change — supply volume_db and/or loudness");
    }
    // Master-bus edits resolve against the active sequence (01 §10 apply);
    // apply with the target sequence active, restoring the prior one.
    let cmd = with_active_seq(p, args.sequence_id, cmds);
    history.execute_discrete(cmd, &mut doc);
    ToolResult::text("Updated master bus")
}

pub async fn get_audio_meters(state: &AppState, args: GetAudioMetersArgs) -> ToolResult {
    tracing::debug!("tool: get_audio_meters {}", args.sequence_id);
    let doc = state.document.lock().await;
    let Some(p) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    if !p.sequences.contains_key(&args.sequence_id) {
        return ToolResult::error(format!("sequence {} not found", args.sequence_id));
    }
    // G-4: when the engine is playing with a live feeder, `EngineStatus.master_level`
    // carries the real mixer output meter. Headless MCP without a device still
    // reports structured unavailability (not fabricated values).
    drop(doc);
    let Some(bridge) = state.video_engine.bridge() else {
        return err_code(
            "EngineUnavailable",
            "no GPU adapter — audio meters require a live engine session",
        );
    };
    let status = bridge.session().status();
    match status.master_level {
        Some(m) => ToolResult::text("master meter").with_data(json!({
            "sequence_id": args.sequence_id,
            "peak": m.peak,
            "rms": m.rms,
            "graph_latency_samples": status.graph_latency_samples,
            "source": "mixer_output",
        })),
        None => err_code(
            "NotSupportedV1",
            "live audio meters require interactive playback (no feeder running — press play, or no audio device)",
        ),
    }
}

pub async fn get_waveform(state: &AppState, args: GetWaveformArgs) -> ToolResult {
    tracing::debug!("tool: get_waveform");
    let (path, hash): (std::path::PathBuf, Option<String>) = {
        let doc = state.document.lock().await;
        let Some(p) = doc.timeline.as_ref() else {
            return ToolResult::error("no timeline project");
        };
        let asset_id = if let Some(a) = args.asset_id {
            a
        } else if let Some(clip_id) = args.clip_id {
            let Some((s, t)) = locate_clip(p, clip_id) else {
                return ToolResult::error(format!("clip {clip_id} not found"));
            };
            match find_clip(p, s, t, clip_id).map(|c| &c.source) {
                Some(ClipSource::Asset { asset }) | Some(ClipSource::Vector { asset }) => *asset,
                _ => return ToolResult::error("clip has no media asset for a waveform"),
            }
        } else {
            return ToolResult::error("supply asset_id or clip_id");
        };
        let Some(a) = p.media.assets.get(&asset_id) else {
            return ToolResult::error(format!("asset {asset_id} not found"));
        };
        match &a.source {
            photonic_core::timeline::AssetSource::File { path, .. } => {
                (path.clone(), a.content_hash.clone())
            }
            _ => return ToolResult::error("asset is not file-backed"),
        }
    };
    if !path.exists() {
        return err_code(
            "AssetOffline",
            format!("file not found: {}", path.display()),
        );
    }
    let resolution = args.resolution.unwrap_or(512).clamp(1, 8192);
    let cache_dir = video_proxy::proxy_cache_dir(None);

    // Prefer a cached pyramid; otherwise decode + build one and cache it.
    let hash = match hash {
        Some(h) => h,
        None => match video_probe::content_hash(&path) {
            Ok(h) => h,
            Err(e) => return ToolResult::error(format!("content hash failed: {e}")),
        },
    };
    let pyramid = match photonic_video::audio::waveform::load_from_dir(&cache_dir, &hash) {
        Ok(Some(p)) => p,
        _ => {
            let tools = match ffmpeg_locate::locate() {
                Ok(t) => t,
                Err(e) => {
                    return err_code(
                        "FfmpegUnavailable",
                        format!("ffmpeg not found ({e}); cannot decode audio for a waveform"),
                    )
                }
            };
            let mut src = match photonic_video::playback::pcm::FfmpegPcmSource::spawn(
                &tools,
                &path,
                Tick(0),
                48_000,
            ) {
                Ok(s) => s,
                Err(e) => return ToolResult::error(format!("audio decode spawn failed: {e}")),
            };
            let pyr = photonic_video::audio::waveform::build_pyramid(&mut src, hash.clone());
            let _ = photonic_video::audio::waveform::save_to_dir(&pyr, &cache_dir);
            pyr
        }
    };

    // Summarize the coarsest level down to `resolution` buckets/channel.
    let Some(level) = pyramid.levels.last() else {
        return ToolResult::text("waveform is empty").with_data(json!({
            "channels": pyramid.channels, "buckets": [],
        }));
    };
    let channels: Vec<serde_json::Value> = level
        .channels
        .iter()
        .map(|buckets| {
            let n = buckets.len();
            if n <= resolution {
                buckets
                    .iter()
                    .map(|b| json!([b.min, b.max, b.rms]))
                    .collect::<Vec<_>>()
            } else {
                (0..resolution)
                    .map(|i| {
                        let b = &buckets[i * n / resolution];
                        json!([b.min, b.max, b.rms])
                    })
                    .collect::<Vec<_>>()
            }
        })
        .map(serde_json::Value::Array)
        .collect();
    ToolResult::text(format!(
        "waveform: {} channel(s), {} source frame(s)",
        pyramid.channels, pyramid.total_frames
    ))
    .with_data(json!({
        "channels": pyramid.channels,
        "source_sample_rate": pyramid.source_sample_rate,
        "total_frames": pyramid.total_frames,
        "bucket_format": "[min, max, rms]",
        "resolution": resolution,
        "waveform": channels,
    }))
}

// ─── Title templates (05 §4b) ────────────────────────────────────────────────

pub async fn list_title_templates(_state: &AppState, _args: ListTitleTemplatesArgs) -> ToolResult {
    tracing::debug!("tool: list_title_templates");
    // The shipped vector title-template library (05 §4b, ~8–10 built-ins) is a
    // P6 deliverable not yet committed to this repo; no registry exists to read.
    ToolResult::text("no title templates available (the shipped library lands in P6)")
        .with_data(json!({ "templates": [] }))
}

pub async fn insert_title_template(
    _state: &AppState,
    _args: InsertTitleTemplateArgs,
) -> ToolResult {
    tracing::debug!("tool: insert_title_template");
    err_code(
        "NotSupportedV1",
        "the vector title-template library is not shipped in this build (05 §4b, P6) — nothing to insert",
    )
}

// ── D-12 gyro stabilization (22 §6.5) ───────────────────────────────────────

/// Bind a gyro/IMU sidecar to a clip.
///
/// Parses eagerly so a bad file is rejected here, with a located error, rather
/// than surfacing later as a failed analysis (22 §6.6: hard-fail, never guess).
/// The response reports what the file actually contains — sample count, rate,
/// and whether it carries accelerometer data — because "horizon lock does
/// nothing" is otherwise a mystery.
///
/// Device serials, GPS and precise timestamps are never echoed (23 §9.4).
pub async fn import_motion_metadata(
    state: &AppState,
    args: ImportMotionMetadataArgs,
) -> ToolResult {
    use photonic_core::timeline::{
        LensProfileRef, MotionBinding, MotionSourceRef, StabilizationSpec,
    };

    tracing::debug!("tool: import_motion_metadata {}", args.clip_id);
    // 28: every filesystem read goes through the path policy.
    let path =
        match crate::path_guard::check_path(state, &args.path, photonic_core::PathAccess::Read) {
            Ok(p) => p,
            Err(denied) => return denied,
        };
    let series = match photonic_video::media::parse_motion(&path) {
        Ok(s) => s,
        Err(e) => return ToolResult::error(format!("motion metadata rejected: {e}")),
    };

    let lens = match args.lens_profile_path.as_deref() {
        Some(p) => {
            let lp = match crate::path_guard::check_path(state, p, photonic_core::PathAccess::Read)
            {
                Ok(v) => v,
                Err(denied) => return denied,
            };
            // Validate now, for the same reason as the motion file.
            if let Err(e) = photonic_video::graph::stabilize::LensProfile::from_path(&lp) {
                return ToolResult::error(format!("lens profile rejected: {e}"));
            }
            LensProfileRef::UserFile {
                path: lp,
                rel_path: None,
            }
        }
        None => LensProfileRef::RotationOnly,
    };

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let Some(clip) = find_clip(project, seq_id, track_id, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };

    let mut spec = StabilizationSpec::new(MotionBinding {
        source: MotionSourceRef::Sidecar {
            path,
            rel_path: None,
            format: series.format,
        },
        sync: Default::default(),
        lens,
    });
    if let Some(v) = args.smoothness {
        spec.smoothness = v;
    }
    if let Some(v) = args.horizon_lock {
        spec.horizon_lock = v;
    }
    if let Err(e) = spec.validate() {
        return ToolResult::error(format!("invalid stabilization recipe: {e:?}"));
    }

    let mut new_clip = clip.clone();
    new_clip.stabilization = Some(spec);
    match ops::set_clip_prop(project, seq_id, track_id, new_clip) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text(format!(
                "Bound motion metadata: {} samples{}{}. Run analyze_stabilization next.",
                series.samples.len(),
                series
                    .sample_rate_hz()
                    .map(|hz| format!(", {hz:.0} Hz"))
                    .unwrap_or_default(),
                if series.has_accel() {
                    ", with accelerometer"
                } else {
                    ", gyro only (horizon lock will have no effect)"
                },
            ))
        }
        Err(e) => map_edit_error(e),
    }
}

/// Update an existing stabilization recipe. Only supplied fields change.
pub async fn set_stabilization(state: &AppState, args: SetStabilizationArgs) -> ToolResult {
    use photonic_core::timeline::StabilizationCropMode;

    tracing::debug!("tool: set_stabilization {}", args.clip_id);
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let Some(clip) = find_clip(project, seq_id, track_id, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let Some(mut spec) = clip.stabilization.clone() else {
        return ToolResult::error("clip has no motion metadata; call import_motion_metadata first");
    };

    if let Some(v) = args.smoothness {
        spec.smoothness = v;
    }
    if let Some(v) = args.horizon_lock {
        spec.horizon_lock = v;
    }
    if let Some(v) = args.max_zoom {
        spec.max_zoom = v;
    }
    if let Some(m) = args.crop_mode.as_deref() {
        spec.crop_mode = match m {
            "static_safe" => StabilizationCropMode::StaticSafe,
            "dynamic" => StabilizationCropMode::Dynamic,
            "transparent_edges" => StabilizationCropMode::TransparentEdges,
            other => {
                return ToolResult::error(format!(
                "unknown crop_mode {other:?}; expected static_safe, dynamic or transparent_edges"
            ))
            }
        };
    }
    // Changing the recipe invalidates any prior analysis: the corrections were
    // computed for the old settings.
    if args.smoothness.is_some()
        || args.horizon_lock.is_some()
        || args.max_zoom.is_some()
        || args.crop_mode.is_some()
    {
        spec.analysis_key = None;
    }
    // Reject rather than clamp (22 §6.6).
    if let Err(e) = spec.validate() {
        return ToolResult::error(format!("invalid stabilization recipe: {e:?}"));
    }

    let mut new_clip = clip.clone();
    new_clip.stabilization = Some(spec);
    match ops::set_clip_prop(project, seq_id, track_id, new_clip) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), &mut doc);
            ToolResult::text("Updated stabilization recipe; re-run analyze_stabilization")
        }
        Err(e) => map_edit_error(e),
    }
}

/// Run the analysis and report its diagnostics.
///
/// Synchronous rather than a background job: the integration is a linear pass
/// over the motion series and completes in well under a second for any
/// realistic clip, so a job handle would add a polling round-trip and buy
/// nothing. If a corpus ever proves otherwise this moves onto `JobRegistry`
/// without changing the tool's contract.
pub async fn analyze_stabilization(state: &AppState, args: AnalyzeStabilizationArgs) -> ToolResult {
    use photonic_video::graph::stabilize::{analyze_clip, geometry_for_clip};

    tracing::debug!("tool: analyze_stabilization {}", args.clip_id);
    let doc = state.document.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let Some(clip) = find_clip(project, seq_id, track_id, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let Some(spec) = clip.stabilization.clone() else {
        return ToolResult::error("clip has no motion metadata; call import_motion_metadata first");
    };
    let Some(seq) = project.sequences.get(&seq_id) else {
        return ToolResult::error("sequence not found");
    };
    let format = seq.format();
    let rate = seq.frame_rate;
    let fps = rate.num as f64 / rate.den.max(1) as f64;
    let geom = geometry_for_clip(format.width as f64, format.height as f64, fps, clip);

    match analyze_clip(&spec, geom, |p| p.to_path_buf()) {
        Ok(a) => {
            let d = &a.diagnostics;
            let mut out = format!(
                "Analyzed {} frames at {:.3} fps.\n\
                 Gyro bias: {}\n\
                 Sample rate: {}\n\
                 Clock: {:.1} ppm drift over {} anchor(s)\n\
                 Max required zoom: {:.3}x",
                a.frames.len(),
                a.fps,
                if d.bias.estimated {
                    format!(
                        "[{:.5}, {:.5}, {:.5}] rad/s",
                        d.bias.bias_rad_s[0], d.bias.bias_rad_s[1], d.bias.bias_rad_s[2]
                    )
                } else {
                    "not estimated (no sufficiently still span)".into()
                },
                d.sample_rate_hz
                    .map(|hz| format!("{hz:.0} Hz"))
                    .unwrap_or_else(|| "unknown".into()),
                d.clock_drift_ppm,
                d.anchors_used,
                d.max_required_zoom,
            );
            if let Some((first, last)) = d.infeasible_range {
                out.push_str(&format!(
                    "\nWARNING: frames {first}-{last} cannot be covered within max_zoom \
                     {:.2}x; they will show exposed edges.",
                    spec.max_zoom
                ));
            }
            if d.horizon_lock_unavailable {
                out.push_str(
                    "\nWARNING: horizon lock is set but the motion source carries no \
                     accelerometer data, so it has no effect.",
                );
            }
            if let Some(c) = d.mean_gravity_confidence {
                out.push_str(&format!("\nMean gravity confidence: {c:.2}"));
            }
            ToolResult::text(out)
        }
        Err(e) => ToolResult::error(format!("analysis failed: {e}")),
    }
}

/// Report a clip's stabilization state without running anything.
pub async fn get_stabilization_status(
    state: &AppState,
    args: GetStabilizationStatusArgs,
) -> ToolResult {
    use photonic_core::timeline::{LensProfileRef, MotionSourceRef};

    let doc = state.document.lock().await;
    let Some(project) = doc.timeline.as_ref() else {
        return ToolResult::error("no timeline project");
    };
    let Some((seq_id, track_id)) = locate_clip(project, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let Some(clip) = find_clip(project, seq_id, track_id, args.clip_id) else {
        return ToolResult::error(format!("clip {} not found", args.clip_id));
    };
    let Some(spec) = clip.stabilization.as_ref() else {
        return ToolResult::text("not stabilized");
    };
    // Only the file *name* is reported, never the full path: 23 §9.4 keeps
    // location-bearing detail out of default tool output.
    let source = match &spec.binding.source {
        MotionSourceRef::Embedded { .. } => "embedded".to_string(),
        MotionSourceRef::Sidecar { path, format, .. } => format!(
            "{} ({format:?})",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "<unnamed>".into())
        ),
    };
    let lens = match &spec.binding.lens {
        LensProfileRef::RotationOnly => "rotation only (uncalibrated)".to_string(),
        LensProfileRef::UserFile { path, .. } => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "<unnamed>".into()),
        LensProfileRef::Bundled { id } => format!("bundled {id}"),
    };
    ToolResult::text(format!(
        "source: {source}\nlens: {lens}\nsmoothness: {:.2}\nhorizon_lock: {:.2}\n\
         crop_mode: {:?}\nmax_zoom: {:.2}\nanalyzed: {}\nsync anchors: {}",
        spec.smoothness,
        spec.horizon_lock,
        spec.crop_mode,
        spec.max_zoom,
        spec.analysis_key.is_some(),
        spec.binding.sync.anchors.len(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::McpServerConfig;
    use photonic_core::{AuditLog, Document};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::Mutex;

    // ── K-E2 `get_scopes` tap argument ──────────────────────────────────────

    /// Omitting `tap` must select the **clip** readback point — the whole point
    /// of K-E2 is that the default stops being "the program frame". A default
    /// that silently flipped back to `program` would be invisible in every other
    /// test, so it is pinned here.
    #[test]
    fn get_scopes_defaults_to_the_per_clip_tap() {
        let args: GetScopesArgs =
            serde_json::from_value(json!({ "clip_id": ClipId::new().to_string() }))
                .expect("minimal args parse");
        assert_eq!(args.tap, ScopeTap::Clip);
    }

    /// Every value the published schema advertises for `tap` must actually
    /// deserialize, and nothing else may. The accepted set is READ FROM THE
    /// SCHEMA rather than written out again here, so the two cannot drift.
    #[test]
    fn get_scopes_tap_enum_matches_the_published_schema() {
        let schema = crate::schema_gen::tool_list();
        let tool = schema
            .as_array()
            .expect("tool list")
            .iter()
            .find(|t| t["name"] == "get_scopes")
            .expect("get_scopes is published");
        let values = tool["inputSchema"]["properties"]["tap"]["enum"]
            .as_array()
            .expect("tap advertises an enum")
            .clone();
        assert!(!values.is_empty());
        for v in &values {
            let args: GetScopesArgs = serde_json::from_value(json!({
                "clip_id": ClipId::new().to_string(),
                "tap": v,
            }))
            .unwrap_or_else(|e| panic!("schema advertises tap={v} but it fails to parse: {e}"));
            let round = match args.tap {
                ScopeTap::Clip => "clip",
                ScopeTap::Program => "program",
            };
            assert_eq!(Value::from(round), *v, "tap value round-trips");
        }
        // Sensitivity: an un-advertised value must be rejected, or the loop above
        // would prove nothing about the enum being closed.
        assert!(serde_json::from_value::<GetScopesArgs>(json!({
            "clip_id": ClipId::new().to_string(),
            "tap": "monitor",
        }))
        .is_err());
    }

    fn test_state() -> AppState {
        let (tx, _rx) = std::sync::mpsc::channel();
        AppState {
            document: Arc::new(Mutex::new(Document::new("t", 100.0, 100.0))),
            history: Arc::new(Mutex::new(photonic_core::history::CommandHistory::new(100))),
            capture_tx: Arc::new(StdMutex::new(tx)),
            config: McpServerConfig::default(),
            path_policy: photonic_core::PathPolicy::test_default(),
            audit_log: Arc::new(StdMutex::new(AuditLog::new())),
            clipboard_ring: Arc::new(crate::handlers::clipboard::new_clipboard_ring()),
            video_engine: Arc::new(crate::handlers::video_jobs::VideoEngineHandle::new()),
            video_jobs: Arc::new(StdMutex::new(
                crate::handlers::video_jobs::JobRegistry::new(),
            )),
            document_path: Arc::new(StdMutex::new(None)),
        }
    }

    /// Dispatch through the real JSON-RPC tool-call path (args deserialize +
    /// `dispatch_tool_inner` match arm + handler + audit log) — the closest
    /// in-process equivalent to an agent's `tools/call`.
    async fn call(state: &AppState, name: &str, args: Value) -> ToolResult {
        crate::dispatch::dispatch_tool(state, name, args)
            .await
            .unwrap_or_else(|e| panic!("dispatch({name}) failed: {e}"))
    }

    /// The JSON payload attached via `ToolResult::with_data` (second content item).
    fn data(r: &ToolResult) -> Value {
        match r.content.get(1) {
            Some(ContentItem::Text { text }) => serde_json::from_str(text).unwrap_or(Value::Null),
            _ => Value::Null,
        }
    }

    async fn create_track(state: &AppState, seq_id: &Value, kind: &str) -> Value {
        let r = call(
            state,
            "add_track",
            json!({ "sequence_id": seq_id, "kind": kind }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "add_track: {r:?}");
        data(&r)["track_id"].clone()
    }

    async fn create_seq_and_track(state: &AppState, kind: &str) -> (Value, Value) {
        let r = call(
            state,
            "create_sequence",
            json!({
                "name": "Test Seq", "frame_rate": {"num": 30, "den": 1},
                "formats": [{"name": "16:9", "width": 1920, "height": 1080}]
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "create_sequence: {r:?}");
        let seq_id = data(&r)["sequence_id"].clone();
        let track_id = create_track(state, &seq_id, kind).await;
        (seq_id, track_id)
    }

    async fn insert_solid_clip(state: &AppState, track_id: &Value, start: i64, dur: i64) -> Value {
        let r = call(
            state,
            "insert_clip",
            json!({
                "track_id": track_id, "start_ticks": start, "duration_ticks": dur,
                "source": {"kind": "solid_color", "color": "#00ff00"}
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "insert_clip: {r:?}");
        data(&r)["clip_id"].clone()
    }

    // ── effect_stack: track / master / asset scopes (26 §10 K-B1/K-B2) ───────

    /// Every scope round-trips through the real dispatch path: add → list →
    /// reorder → set_param → remove, each one undo step, and undo restores the
    /// exact prior stack.
    #[tokio::test]
    async fn effect_stack_edits_every_scope_and_undoes() {
        let state = test_state();
        let (seq_id, track_id) = create_seq_and_track(&state, "video").await;
        let clip_id = insert_solid_clip(&state, &track_id, 0, 300).await;

        let tmp =
            std::env::temp_dir().join(format!("photonic_mcp_fxstack_{}.mp4", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, b"fx stack test bytes").unwrap();
        let r = call(
            &state,
            "import_media",
            json!({ "paths": [tmp.to_string_lossy()] }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "import_media: {r:?}");
        let asset_id = data(&r)["assets"][0]["asset_id"].clone();
        let _ = std::fs::remove_file(&tmp);

        let scopes = [
            json!({ "scope": "clip", "clip_id": clip_id }),
            json!({ "scope": "track", "track_id": track_id }),
            json!({ "scope": "master", "sequence_id": seq_id }),
            json!({ "scope": "asset", "asset_id": asset_id }),
        ];

        for base in scopes {
            let with = |extra: Value| {
                let mut v = base.clone();
                let (Value::Object(o), Value::Object(e)) = (&mut v, extra) else {
                    unreachable!()
                };
                o.extend(e);
                v
            };

            for kind in ["blur", "sharpen", "invert"] {
                let r = call(
                    &state,
                    "effect_stack",
                    with(json!({ "op": "add", "kind": kind })),
                )
                .await;
                assert_ne!(r.is_error, Some(true), "add {base}: {r:?}");
            }
            let r = call(&state, "effect_stack", with(json!({ "op": "list" }))).await;
            let effects = data(&r)["effects"].as_array().cloned().unwrap_or_default();
            assert_eq!(effects.len(), 3, "list {base}: {r:?}");
            assert_eq!(effects[0]["kind"], json!("blur"));
            assert_eq!(effects[2]["kind"], json!("invert"));

            // Reorder, then undo it — the exact prior order must come back.
            let r = call(
                &state,
                "effect_stack",
                with(json!({ "op": "reorder", "new_order": [2, 0, 1] })),
            )
            .await;
            assert_ne!(r.is_error, Some(true), "reorder {base}: {r:?}");
            let r = call(&state, "effect_stack", with(json!({ "op": "list" }))).await;
            let kinds: Vec<Value> = data(&r)["effects"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["kind"].clone())
                .collect();
            assert_eq!(
                kinds,
                vec![json!("invert"), json!("blur"), json!("sharpen")]
            );

            let r = call(&state, "undo", json!({})).await;
            assert_ne!(r.is_error, Some(true), "undo: {r:?}");
            let r = call(&state, "effect_stack", with(json!({ "op": "list" }))).await;
            let kinds: Vec<Value> = data(&r)["effects"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["kind"].clone())
                .collect();
            assert_eq!(
                kinds,
                vec![json!("blur"), json!("sharpen"), json!("invert")],
                "undo must restore the exact prior order, {base}"
            );

            // set_param: toggle `enabled`, then a real manifest param.
            let r = call(
                &state,
                "effect_stack",
                with(json!({
                    "op": "set_param", "index": 0,
                    "path": "enabled", "value": { "t": "bool", "v": false }
                })),
            )
            .await;
            assert_ne!(r.is_error, Some(true), "set_param enabled {base}: {r:?}");
            let r = call(&state, "effect_stack", with(json!({ "op": "list" }))).await;
            assert_eq!(data(&r)["effects"][0]["enabled"], json!(false));

            // remove the middle entry
            let r = call(
                &state,
                "effect_stack",
                with(json!({ "op": "remove", "index": 1 })),
            )
            .await;
            assert_ne!(r.is_error, Some(true), "remove {base}: {r:?}");
            let r = call(&state, "effect_stack", with(json!({ "op": "list" }))).await;
            let kinds: Vec<Value> = data(&r)["effects"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["kind"].clone())
                .collect();
            assert_eq!(kinds, vec![json!("blur"), json!("invert")]);

            // set_grade at this scope, then clear it.
            let r = call(
                &state,
                "effect_stack",
                with(json!({ "op": "set_grade", "grade": { "ops": [] } })),
            )
            .await;
            assert_ne!(r.is_error, Some(true), "set_grade {base}: {r:?}");
            let r = call(&state, "effect_stack", with(json!({ "op": "list" }))).await;
            assert!(!data(&r)["grade"].is_null(), "grade should be set {base}");
            let r = call(
                &state,
                "effect_stack",
                with(json!({ "op": "set_grade", "grade": null })),
            )
            .await;
            assert_ne!(r.is_error, Some(true), "clear grade {base}: {r:?}");
            let r = call(&state, "effect_stack", with(json!({ "op": "list" }))).await;
            assert!(
                data(&r)["grade"].is_null(),
                "grade should be cleared {base}"
            );
        }
    }

    /// The four stacks are independent: a track-scoped add never lands on the
    /// clip that sits on that track (the bug this scope enum exists to prevent).
    #[tokio::test]
    async fn effect_stack_scopes_do_not_leak_into_each_other() {
        let state = test_state();
        let (seq_id, track_id) = create_seq_and_track(&state, "video").await;
        let clip_id = insert_solid_clip(&state, &track_id, 0, 300).await;

        let r = call(
            &state,
            "effect_stack",
            json!({ "scope": "track", "op": "add", "track_id": track_id, "kind": "blur" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "{r:?}");

        let r = call(&state, "get_clip", json!({ "clip_id": clip_id })).await;
        let clip_effects = data(&r)["effects"].as_array().cloned().unwrap_or_default();
        assert!(
            clip_effects.is_empty(),
            "a track effect must not land on the clip: {clip_effects:?}"
        );
        let r = call(
            &state,
            "effect_stack",
            json!({ "scope": "master", "op": "list", "sequence_id": seq_id }),
        )
        .await;
        assert_eq!(data(&r)["effects"].as_array().unwrap().len(), 0);
    }

    /// Refusals: a missing owner id, an unknown owner, a bad index and a bad
    /// param value are all reported, and none of them mutates the document.
    #[tokio::test]
    async fn effect_stack_refuses_bad_requests() {
        let state = test_state();
        let (_seq_id, track_id) = create_seq_and_track(&state, "video").await;

        for (args, why) in [
            (
                json!({ "scope": "track", "op": "add", "kind": "blur" }),
                "no track_id",
            ),
            (
                json!({ "scope": "asset", "op": "list", "asset_id": "00000000-0000-0000-0000-000000000000" }),
                "unknown asset",
            ),
            (
                json!({ "scope": "track", "op": "remove", "track_id": track_id, "index": 0 }),
                "index out of range on an empty stack",
            ),
            (
                json!({ "scope": "track", "op": "reorder", "track_id": track_id }),
                "reorder without new_order",
            ),
            (
                json!({ "scope": "track", "op": "add", "track_id": track_id }),
                "add without effect_id or kind",
            ),
        ] {
            let r = call(&state, "effect_stack", args.clone()).await;
            assert_eq!(r.is_error, Some(true), "should refuse ({why}): {r:?}");
        }

        // A param outside its manifest range is refused, not clamped.
        let r = call(
            &state,
            "effect_stack",
            json!({ "scope": "track", "op": "add", "track_id": track_id, "effect_id": "blur.gaussian" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "add by manifest id: {r:?}");
        let r = call(
            &state,
            "effect_stack",
            json!({
                "scope": "track", "op": "set_param", "track_id": track_id, "index": 0,
                "path": "params.radius", "value": { "t": "float", "v": 9999.0 }
            }),
        )
        .await;
        assert_eq!(
            r.is_error,
            Some(true),
            "out-of-range param must refuse: {r:?}"
        );
        let r = call(
            &state,
            "effect_stack",
            json!({
                "scope": "track", "op": "set_param", "track_id": track_id, "index": 0,
                "path": "params.nope", "value": { "t": "float", "v": 1.0 }
            }),
        )
        .await;
        assert_eq!(
            r.is_error,
            Some(true),
            "unknown param path must refuse: {r:?}"
        );
    }

    // ── Effect presets / custom stacks / favourites (26 §10 K-B4) ───────────

    /// Serializes the tests that install a preset-library override, because
    /// that override is process-global.
    static PRESET_LIBRARY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Points the K-B4 verbs at a throwaway library file for one test.
    ///
    /// Lock poisoning is deliberately ignored on both mutexes: a test that
    /// panics must fail on its own assertion, not convert every later preset
    /// test into a lock-poison error that hides the real failure.
    struct TestLibrary {
        _lock: std::sync::MutexGuard<'static, ()>,
        dir: std::path::PathBuf,
    }

    impl TestLibrary {
        fn new() -> Self {
            let lock = PRESET_LIBRARY_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let dir =
                std::env::temp_dir().join(format!("photonic-mcp-presets-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("temp preset dir");
            let path = dir.join("effect_presets.json");
            *TEST_LIBRARY_PATH.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
            TestLibrary { _lock: lock, dir }
        }

        fn path(&self) -> std::path::PathBuf {
            self.dir.join("effect_presets.json")
        }
    }

    impl Drop for TestLibrary {
        fn drop(&mut self) {
            *TEST_LIBRARY_PATH.lock().unwrap_or_else(|e| e.into_inner()) = None;
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn undo_depth(state: &AppState) -> usize {
        state.history.lock().await.undo_depth()
    }

    /// The stack ids currently on a clip, via the real `effect_stack op=list`.
    async fn stack_ids(state: &AppState, clip: &Value) -> Vec<String> {
        let r = call(
            state,
            "effect_stack",
            json!({ "scope": "clip", "op": "list", "clip_id": clip }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "effect_stack list: {r:?}");
        data(&r)["effects"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|e| e["id"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// The built-in catalogue is offered as-is, and the shape each entry
    /// publishes is the shape an agent needs to choose one.
    #[tokio::test]
    async fn effect_preset_list_offers_the_built_in_catalogue() {
        let _lib = TestLibrary::new();
        let state = test_state();
        let r = call(&state, "effect_preset_list", json!({})).await;
        assert_ne!(r.is_error, Some(true), "{r:?}");
        let presets = data(&r)["presets"].as_array().cloned().unwrap_or_default();

        // Expectation READ FROM THE CATALOGUE, never a literal count.
        let expected = effect_preset::built_in_presets();
        assert!(!expected.is_empty(), "this build ships built-in presets");
        assert_eq!(
            presets.len(),
            expected.len(),
            "empty library ⇒ built-ins only"
        );
        for (got, want) in presets.iter().zip(&expected) {
            assert_eq!(got["name"], json!(want.name));
            assert_eq!(got["built_in"], json!(true));
            assert_eq!(got["effect_count"], json!(want.effects.len()));
            assert!(
                got["unresolvable_effect_ids"]
                    .as_array()
                    .expect("unresolvable_effect_ids is an array")
                    .is_empty(),
                "a built-in must resolve on the build that ships it: {got}"
            );
        }
        // `parameter_preset_for` has to discriminate, or it is decoration: it
        // is set for a single-effect preset and absent for a multi-effect one.
        let single = presets
            .iter()
            .find(|p| p["effect_count"] == json!(1))
            .expect("a one-effect built-in");
        assert!(single["parameter_preset_for"].is_string(), "{single}");
        let multi = presets
            .iter()
            .find(|p| p["effect_count"].as_u64().unwrap_or(0) > 1)
            .expect("a multi-effect built-in");
        assert!(multi["parameter_preset_for"].is_null(), "{multi}");
    }

    /// THE CRUX of K-B4's MCP half: saving captures the scope's stack *and*
    /// grade into a config file with no undo entry, and applying it lands as
    /// exactly ONE undo step that a single `undo` fully reverses.
    #[tokio::test]
    async fn effect_preset_save_captures_a_scope_and_apply_is_one_undo_step() {
        let lib = TestLibrary::new();
        let state = test_state();
        let (_seq, track) = create_seq_and_track(&state, "video").await;
        let src = insert_solid_clip(&state, &track, 0, 300).await;
        let dst = insert_solid_clip(&state, &track, 400, 300).await;

        for id in ["blur.gaussian", "stylize.vignette"] {
            let r = call(
                &state,
                "effect_stack",
                json!({ "scope": "clip", "op": "add", "clip_id": src, "effect_id": id }),
            )
            .await;
            assert_ne!(r.is_error, Some(true), "add {id}: {r:?}");
        }
        let r = call(
            &state,
            "effect_stack",
            json!({
                "scope": "clip", "op": "set_grade", "clip_id": src,
                "grade": { "ops": [], "bypass": true }
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_grade: {r:?}");

        // Saving is a config write: the undo depth must not move.
        let before_save = undo_depth(&state).await;
        let r = call(
            &state,
            "effect_preset_save",
            json!({ "name": "My Look", "scope": "clip", "clip_id": src }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "save: {r:?}");
        assert_eq!(
            undo_depth(&state).await,
            before_save,
            "saving a preset must NOT create an undo entry"
        );
        assert!(lib.path().exists(), "the library file was written");
        assert_eq!(
            data(&r)["preset"]["effect_ids"],
            json!(["blur.gaussian", "stylize.vignette"])
        );
        assert_eq!(data(&r)["preset"]["has_grade"], json!(true));
        assert_eq!(data(&r)["preset"]["built_in"], json!(false));

        // It shows up in the catalogue after the built-ins.
        let r = call(&state, "effect_preset_list", json!({})).await;
        let presets = data(&r)["presets"].as_array().cloned().unwrap_or_default();
        assert_eq!(presets.len(), effect_preset::built_in_presets().len() + 1);
        assert_eq!(presets.last().unwrap()["name"], json!("My Look"));

        // Apply to the untouched clip.
        assert!(stack_ids(&state, &dst).await.is_empty(), "dst starts clean");
        let before_apply = undo_depth(&state).await;
        let r = call(
            &state,
            "effect_preset_apply",
            json!({ "name": "My Look", "scope": "clip", "clip_id": dst }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "apply: {r:?}");

        // Sensitivity: prove it really applied, in the preset's own order, and
        // that the grade came with it — before claiming undo restored anything.
        assert_eq!(
            stack_ids(&state, &dst).await,
            vec!["blur.gaussian".to_string(), "stylize.vignette".to_string()],
            "the preset's stack is appended in its own order"
        );
        let r_list = call(
            &state,
            "effect_stack",
            json!({ "scope": "clip", "op": "list", "clip_id": dst }),
        )
        .await;
        assert_eq!(
            data(&r_list)["grade"]["bypass"],
            json!(true),
            "grade applied"
        );
        assert_eq!(
            undo_depth(&state).await,
            before_apply + 1,
            "a 2-effect + grade apply is ONE undo step, not three"
        );

        let r = call(&state, "undo", json!({})).await;
        assert_ne!(r.is_error, Some(true), "undo: {r:?}");
        assert!(
            stack_ids(&state, &dst).await.is_empty(),
            "one undo reverses the whole apply"
        );
        let r_list = call(
            &state,
            "effect_stack",
            json!({ "scope": "clip", "op": "list", "clip_id": dst }),
        )
        .await;
        assert!(
            data(&r_list)["grade"].is_null(),
            "one undo also restores the grade slot"
        );
    }

    /// A multi-clip apply is still exactly ONE `Command::Batch`: one `undo`
    /// restores every target, not just the last one.
    #[tokio::test]
    async fn effect_preset_apply_to_many_clips_is_one_batch() {
        let _lib = TestLibrary::new();
        let state = test_state();
        let (seq, v1) = create_seq_and_track(&state, "video").await;
        let v2 = create_track(&state, &seq, "video").await;
        let a = insert_solid_clip(&state, &v1, 0, 200).await;
        let b = insert_solid_clip(&state, &v1, 400, 200).await;
        let c = insert_solid_clip(&state, &v2, 0, 200).await;

        // "Soft Focus" is a shipped single-effect built-in — no save needed.
        let name = effect_preset::built_in_presets()
            .into_iter()
            .find(|p| p.effects.len() == 1)
            .expect("a one-effect built-in")
            .name;

        let before = undo_depth(&state).await;
        let r = call(
            &state,
            "effect_preset_apply",
            json!({ "name": name, "scope": "clip", "clip_ids": [a, b, c] }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "apply: {r:?}");
        assert_eq!(data(&r)["targets"], json!(3));
        for clip in [&a, &b, &c] {
            assert_eq!(stack_ids(&state, clip).await.len(), 1, "clip {clip} got it");
        }
        assert_eq!(
            undo_depth(&state).await,
            before + 1,
            "three clips, ONE step"
        );

        call(&state, "undo", json!({})).await;
        for clip in [&a, &b, &c] {
            assert!(
                stack_ids(&state, clip).await.is_empty(),
                "one undo reverses clip {clip} too"
            );
        }

        // A repeated id is one target, not two stacked applies …
        let r = call(
            &state,
            "effect_preset_apply",
            json!({ "name": name, "scope": "clip", "clip_ids": [a, a] }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "{r:?}");
        assert_eq!(data(&r)["targets"], json!(1));
        assert_eq!(
            stack_ids(&state, &a).await.len(),
            1,
            "applied once, not twice"
        );

        // … and the two spellings of "which clips" may not disagree.
        let r = call(
            &state,
            "effect_preset_apply",
            json!({ "name": name, "scope": "clip", "clip_id": b, "clip_ids": [c] }),
        )
        .await;
        assert_eq!(r.is_error, Some(true), "clip_id + clip_ids: {r:?}");
    }

    /// No partial apply: one unknown target refuses the whole call and leaves
    /// the good clips untouched.
    #[tokio::test]
    async fn effect_preset_apply_refuses_whole_on_an_unknown_target() {
        let _lib = TestLibrary::new();
        let state = test_state();
        let (_seq, track) = create_seq_and_track(&state, "video").await;
        let good = insert_solid_clip(&state, &track, 0, 200).await;
        let name = effect_preset::built_in_presets()
            .first()
            .expect("a built-in")
            .name
            .clone();
        let ghost = json!(ClipId::new().to_string());

        let before = undo_depth(&state).await;
        let r = call(
            &state,
            "effect_preset_apply",
            json!({ "name": name, "scope": "clip", "clip_ids": [good, ghost] }),
        )
        .await;
        assert_eq!(r.is_error, Some(true), "unknown target must refuse: {r:?}");
        assert!(
            stack_ids(&state, &good).await.is_empty(),
            "the resolvable clip must be left alone"
        );
        assert_eq!(undo_depth(&state).await, before, "a refusal is not an edit");

        // Sensitivity: the same call with only the good id succeeds, so the
        // refusal above is about the ghost id and not about the arguments.
        let r = call(
            &state,
            "effect_preset_apply",
            json!({ "name": name, "scope": "clip", "clip_ids": [good] }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "{r:?}");
        assert!(!stack_ids(&state, &good).await.is_empty());
    }

    /// Built-ins are read-only on every management verb, reported with the
    /// same `NotSupportedV1` code `save_export_preset` uses.
    #[tokio::test]
    async fn built_in_presets_refuse_save_delete_and_rename() {
        let _lib = TestLibrary::new();
        let state = test_state();
        let (_seq, track) = create_seq_and_track(&state, "video").await;
        let clip = insert_solid_clip(&state, &track, 0, 200).await;
        let r = call(
            &state,
            "effect_stack",
            json!({ "scope": "clip", "op": "add", "clip_id": clip, "effect_id": "blur.gaussian" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "{r:?}");
        let built_in = effect_preset::built_in_presets()
            .first()
            .expect("a built-in")
            .name
            .clone();

        for (tool, args) in [
            (
                "effect_preset_save",
                json!({ "name": built_in, "scope": "clip", "clip_id": clip }),
            ),
            ("effect_preset_delete", json!({ "name": built_in })),
            (
                "effect_preset_rename",
                json!({ "from": built_in, "to": "Anything" }),
            ),
        ] {
            let r = call(&state, tool, args).await;
            assert_eq!(r.is_error, Some(true), "{tool} on a built-in: {r:?}");
            assert_eq!(
                data(&r)["error_code"],
                json!("NotSupportedV1"),
                "{tool} must refuse a built-in with NotSupportedV1: {r:?}"
            );
        }
        // Renaming a user preset ONTO a built-in name is refused too.
        let r = call(
            &state,
            "effect_preset_save",
            json!({ "name": "Mine", "scope": "clip", "clip_id": clip }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "{r:?}");
        let r = call(
            &state,
            "effect_preset_rename",
            json!({ "from": "Mine", "to": built_in }),
        )
        .await;
        assert_eq!(r.is_error, Some(true), "rename onto a built-in: {r:?}");
        assert_eq!(data(&r)["error_code"], json!("NotSupportedV1"));

        // Sensitivity: the identical verbs DO work on the user preset, so the
        // refusals above are about built-in-ness and not about the plumbing.
        let r = call(
            &state,
            "effect_preset_rename",
            json!({ "from": "Mine", "to": "Ours" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "rename a user preset: {r:?}");
        let r = call(&state, "effect_preset_delete", json!({ "name": "Ours" })).await;
        assert_ne!(r.is_error, Some(true), "delete a user preset: {r:?}");
        let r = call(&state, "effect_preset_list", json!({})).await;
        assert_eq!(
            data(&r)["presets"].as_array().map(|a| a.len()),
            Some(effect_preset::built_in_presets().len()),
            "the user preset is gone, the built-ins are not"
        );
    }

    /// Managing the library is NEVER an undo unit — the one rule that
    /// separates "app config" from "document state" here. Only apply moves the
    /// history, and the same run proves it does.
    #[tokio::test]
    async fn preset_management_never_touches_the_history() {
        let _lib = TestLibrary::new();
        let state = test_state();
        let (_seq, track) = create_seq_and_track(&state, "video").await;
        let clip = insert_solid_clip(&state, &track, 0, 200).await;
        let r = call(
            &state,
            "effect_stack",
            json!({ "scope": "clip", "op": "add", "clip_id": clip, "effect_id": "blur.gaussian" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "{r:?}");

        let base = undo_depth(&state).await;
        for (tool, args) in [
            ("effect_preset_list", json!({})),
            (
                "effect_preset_save",
                json!({ "name": "Look A", "scope": "clip", "clip_id": clip }),
            ),
            (
                "effect_preset_rename",
                json!({ "from": "Look A", "to": "Look B" }),
            ),
            (
                "effect_favourite_set",
                json!({ "id": "blur.gaussian", "favourite": true }),
            ),
            ("effect_favourite_list", json!({})),
            ("effect_preset_delete", json!({ "name": "Look B" })),
        ] {
            let r = call(&state, tool, args).await;
            assert_ne!(r.is_error, Some(true), "{tool}: {r:?}");
            assert_eq!(
                undo_depth(&state).await,
                base,
                "{tool} must not create an undo entry"
            );
        }

        // Sensitivity: the check above would pass on a broken `undo_depth`, so
        // prove the counter moves for the one verb that IS a document edit.
        let name = effect_preset::built_in_presets()
            .first()
            .expect("a built-in")
            .name
            .clone();
        let r = call(
            &state,
            "effect_preset_apply",
            json!({ "name": name, "scope": "clip", "clip_id": clip }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "{r:?}");
        assert_eq!(undo_depth(&state).await, base + 1, "apply IS one undo step");
    }

    /// Favourites round-trip, report availability honestly, refuse starring an
    /// id this build cannot resolve, and still allow un-starring one.
    #[tokio::test]
    async fn effect_favourites_round_trip_and_refuse_unknown_ids() {
        let lib = TestLibrary::new();
        let state = test_state();

        // Seed a library holding one id this build has no manifest for — the
        // cross-build case 39 §2.2 describes, unreachable through the API.
        let mut seeded = effect_preset::EffectPresetLibrary::new();
        seeded.favourites.push("ghost.effect".to_string());
        effect_preset::save_library_to(&lib.path(), &seeded).expect("seed library");

        let r = call(&state, "effect_favourite_list", json!({})).await;
        let favs = data(&r)["favourites"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(favs.len(), 1);
        assert_eq!(favs[0]["id"], json!("ghost.effect"));
        assert_eq!(
            favs[0]["available"],
            json!(false),
            "an id with no manifest is kept but reported unavailable"
        );

        // Starring an unknown id is a typo and is refused …
        let r = call(
            &state,
            "effect_favourite_set",
            json!({ "id": "no.such.effect", "favourite": true }),
        )
        .await;
        assert_eq!(r.is_error, Some(true), "{r:?}");
        // … while UN-starring the stale one still works.
        let r = call(
            &state,
            "effect_favourite_set",
            json!({ "id": "ghost.effect", "favourite": false }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "{r:?}");

        // A real id round-trips, and the list reflects it both ways.
        let r = call(
            &state,
            "effect_favourite_set",
            json!({ "id": "blur.gaussian", "favourite": true }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "{r:?}");
        let r = call(&state, "effect_favourite_list", json!({})).await;
        let favs = data(&r)["favourites"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(favs.len(), 1, "the stale id is gone, the real one is in");
        assert_eq!(favs[0]["id"], json!("blur.gaussian"));
        assert_eq!(favs[0]["available"], json!(true));
        assert!(favs[0]["name"].is_string(), "the manifest name is surfaced");

        // Idempotent, and it survives a reload from disk (it is a file, not a
        // process-lifetime cache).
        let r = call(
            &state,
            "effect_favourite_set",
            json!({ "id": "blur.gaussian", "favourite": true }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "setting the same state succeeds");
        let reloaded = effect_preset::load_library_from(&lib.path()).expect("reload");
        assert_eq!(
            reloaded.library.favourites,
            vec!["blur.gaussian".to_string()]
        );

        let r = call(
            &state,
            "effect_favourite_set",
            json!({ "id": "blur.gaussian", "favourite": false }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "{r:?}");
        let r = call(&state, "effect_favourite_list", json!({})).await;
        assert!(data(&r)["favourites"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(false));
    }

    /// A preset written by a build with more effects than this one still
    /// applies — the unknown entry lands inert-and-preserved (39 §2.2), and
    /// both the list and the apply say so instead of silently dropping it.
    #[tokio::test]
    async fn a_preset_naming_an_unknown_effect_still_applies_inert() {
        let lib = TestLibrary::new();
        let state = test_state();
        let (_seq, track) = create_seq_and_track(&state, "video").await;
        let clip = insert_solid_clip(&state, &track, 0, 200).await;

        // Hand-author a library holding one real and one future effect.
        let real = ClipEffect::from_manifest(photonic_core::timeline::EffectId::new_static(
            "blur.gaussian",
        ))
        .expect("blur.gaussian ships in this build");
        let mut future = real.clone();
        future.id = photonic_core::timeline::EffectId::new("from.the.future".to_string());
        let mut seeded = effect_preset::EffectPresetLibrary::new();
        seeded
            .upsert(effect_preset::EffectPreset::new(
                "Time Traveller",
                vec![real, future],
                None,
            ))
            .expect("seed preset");
        effect_preset::save_library_to(&lib.path(), &seeded).expect("seed library");

        let r = call(&state, "effect_preset_list", json!({})).await;
        let listed = data(&r)["presets"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .find(|p| p["name"] == json!("Time Traveller"))
            .expect("the seeded preset is listed");
        assert_eq!(
            listed["unresolvable_effect_ids"],
            json!(["from.the.future"]),
            "the unknown id is reported, not hidden: {listed}"
        );
        assert_eq!(listed["effect_count"], json!(2), "and it is not dropped");

        let r = call(
            &state,
            "effect_preset_apply",
            json!({ "name": "Time Traveller", "scope": "clip", "clip_id": clip }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "apply: {r:?}");
        assert_eq!(
            data(&r)["unresolvable_effect_ids"],
            json!(["from.the.future"])
        );
        assert_eq!(
            stack_ids(&state, &clip).await,
            vec!["blur.gaussian".to_string(), "from.the.future".to_string()],
            "both entries land, in the preset's order"
        );
    }

    /// Every scope `effect_stack` addresses can be saved and re-applied, with
    /// the same argument spelling — one vocabulary, not two.
    #[tokio::test]
    async fn effect_presets_cover_every_effect_stack_scope() {
        let _lib = TestLibrary::new();
        let state = test_state();
        let (seq, track) = create_seq_and_track(&state, "video").await;
        let clip = insert_solid_clip(&state, &track, 0, 200).await;

        for (scope, addr) in [
            ("clip", json!({ "clip_id": clip })),
            ("track", json!({ "track_id": track })),
            ("master", json!({ "sequence_id": seq })),
        ] {
            let mut add = json!({ "scope": scope, "op": "add", "effect_id": "blur.gaussian" });
            let mut save = json!({ "scope": scope, "name": format!("{scope} look") });
            let mut apply = json!({ "scope": scope, "name": format!("{scope} look") });
            for target in [&mut add, &mut save, &mut apply] {
                for (k, v) in addr.as_object().expect("addressing fields") {
                    target[k] = v.clone();
                }
            }
            let r = call(&state, "effect_stack", add).await;
            assert_ne!(r.is_error, Some(true), "{scope} add: {r:?}");
            let r = call(&state, "effect_preset_save", save).await;
            assert_ne!(r.is_error, Some(true), "{scope} save: {r:?}");
            assert_eq!(data(&r)["preset"]["effect_ids"], json!(["blur.gaussian"]));
            let before = undo_depth(&state).await;
            let r = call(&state, "effect_preset_apply", apply).await;
            assert_ne!(r.is_error, Some(true), "{scope} apply: {r:?}");
            assert_eq!(
                undo_depth(&state).await,
                before + 1,
                "{scope} apply is one undo step"
            );
        }

        // Sensitivity: the scope's own required id is genuinely required.
        let r = call(
            &state,
            "effect_preset_save",
            json!({ "scope": "track", "name": "no id" }),
        )
        .await;
        assert_eq!(
            r.is_error,
            Some(true),
            "scope=track without track_id: {r:?}"
        );
    }

    /// Saving a scope with nothing on it is refused: an empty preset could
    /// only ever be a no-op, and silence there reads as success.
    #[tokio::test]
    async fn saving_an_empty_scope_is_refused() {
        let lib = TestLibrary::new();
        let state = test_state();
        let (_seq, track) = create_seq_and_track(&state, "video").await;
        let clip = insert_solid_clip(&state, &track, 0, 200).await;

        let r = call(
            &state,
            "effect_preset_save",
            json!({ "name": "Nothing", "scope": "clip", "clip_id": clip }),
        )
        .await;
        assert_eq!(r.is_error, Some(true), "empty scope: {r:?}");
        assert!(
            !lib.path().exists(),
            "a refused save must not create the library file"
        );

        // Sensitivity: one effect is enough to make the identical call succeed.
        let r = call(
            &state,
            "effect_stack",
            json!({ "scope": "clip", "op": "add", "clip_id": clip, "effect_id": "blur.gaussian" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "{r:?}");
        let r = call(
            &state,
            "effect_preset_save",
            json!({ "name": "Nothing", "scope": "clip", "clip_id": clip }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "{r:?}");
    }

    // ── paste_attributes (26 §10 K-B15) ─────────────────────────────────────

    /// A source clip dressed with two effects and a grade, plus three targets
    /// spread over two tracks (one of them in the same track as the source) at
    /// deliberately different start/duration/source_in.
    async fn paste_attr_state() -> (AppState, Value, Vec<Value>) {
        let state = test_state();
        let (seq_id, v1) = create_seq_and_track(&state, "video").await;
        let v2 = create_track(&state, &seq_id, "video").await;

        let src = insert_solid_clip(&state, &v1, 0, 300).await;
        let t1 = insert_solid_clip(&state, &v1, 400, 200).await;
        let t2 = insert_solid_clip(&state, &v2, 0, 100).await;
        let t3 = insert_solid_clip(&state, &v2, 700, 500).await;

        for kind in ["blur", "sharpen"] {
            let r = call(
                &state,
                "effect_stack",
                json!({ "scope": "clip", "op": "add", "clip_id": src, "kind": kind }),
            )
            .await;
            assert_ne!(r.is_error, Some(true), "add {kind}: {r:?}");
        }
        let r = call(
            &state,
            "effect_stack",
            json!({
                "scope": "clip", "op": "set_grade", "clip_id": src,
                "grade": { "ops": [], "bypass": true }
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_grade: {r:?}");
        (state, src, vec![t1, t2, t3])
    }

    async fn clip_json(state: &AppState, clip: &Value) -> Value {
        let r = call(state, "get_clip", json!({ "clip_id": clip })).await;
        assert_ne!(r.is_error, Some(true), "get_clip: {r:?}");
        data(&r)["clip"].clone()
    }

    /// THE CRUX: pasting onto three clips is ONE undo step, and one `undo`
    /// restores all three — not the first, not the last.
    #[tokio::test]
    async fn paste_attributes_is_one_undo_step_across_a_multi_selection() {
        let (state, src, targets) = paste_attr_state().await;
        for t in &targets {
            let c = clip_json(&state, t).await;
            assert_eq!(c["effects"].as_array().map(|a| a.len()).unwrap_or(0), 0);
        }

        let r = call(
            &state,
            "paste_attributes",
            json!({ "source_clip_id": src, "target_clip_ids": targets }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "paste_attributes: {r:?}");
        assert_eq!(data(&r)["updated"].as_array().unwrap().len(), 3);
        assert!(data(&r)["skipped"].as_array().unwrap().is_empty());

        for t in &targets {
            let c = clip_json(&state, t).await;
            assert_eq!(
                c["effects"].as_array().unwrap().len(),
                2,
                "target {t} did not receive the stack"
            );
            assert_eq!(c["grade"]["bypass"], json!(true), "target {t} grade");
        }

        // ONE undo must take all three back.
        let r = call(&state, "undo", json!({})).await;
        assert_ne!(r.is_error, Some(true), "undo: {r:?}");
        for t in &targets {
            let c = clip_json(&state, t).await;
            assert_eq!(
                c["effects"].as_array().map(|a| a.len()).unwrap_or(0),
                0,
                "a single undo must clear target {t} too"
            );
            assert!(c["grade"].is_null(), "target {t} grade must be gone");
        }
        // The source is untouched throughout.
        let s = clip_json(&state, &src).await;
        assert_eq!(s["effects"].as_array().unwrap().len(), 2);
    }

    /// A paste that silently moved or retimed a clip would be a bug. Nothing
    /// about position, length, trim, speed or source may change.
    #[tokio::test]
    async fn paste_attributes_never_moves_or_retimes_a_clip() {
        let (state, src, targets) = paste_attr_state().await;
        let mut before = Vec::new();
        for t in &targets {
            before.push(clip_json(&state, t).await);
        }
        let source = clip_json(&state, &src).await;
        // Fixture sanity: the source's timing really does differ, so the
        // "unchanged" assertions below can fail.
        for b in &before {
            assert_ne!(
                (&b["start"], &b["duration"]),
                (&source["start"], &source["duration"])
            );
        }

        let r = call(
            &state,
            "paste_attributes",
            json!({ "source_clip_id": src, "target_clip_ids": targets }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "paste_attributes: {r:?}");

        for (i, t) in targets.iter().enumerate() {
            let after = clip_json(&state, t).await;
            for field in ["id", "start", "duration", "source", "source_in", "speed"] {
                assert_eq!(
                    after[field], before[i][field],
                    "paste changed {field} on target {t}"
                );
            }
        }
    }

    /// `attributes` narrows the paste; an unlisted family leaves the target's
    /// own value untouched rather than resetting it.
    #[tokio::test]
    async fn paste_attributes_narrows_by_family() {
        let (state, src, targets) = paste_attr_state().await;
        let t = &targets[0];

        let r = call(
            &state,
            "paste_attributes",
            json!({
                "source_clip_id": src, "target_clip_ids": [t],
                "attributes": ["effects"]
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "paste effects-only: {r:?}");
        assert_eq!(data(&r)["attributes"], json!(["effects"]));
        let c = clip_json(&state, t).await;
        assert_eq!(c["effects"].as_array().unwrap().len(), 2);
        assert!(
            c["grade"].is_null(),
            "grade was not requested and must not have been pasted"
        );

        // Re-pasting the same family is now a no-op — no empty undo step.
        let r = call(
            &state,
            "paste_attributes",
            json!({
                "source_clip_id": src, "target_clip_ids": [t],
                "attributes": ["effects"]
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true));
        assert!(data(&r)["updated"].as_array().unwrap().is_empty());
        assert_eq!(data(&r)["skipped"].as_array().unwrap().len(), 1);

        // Now the grade, which must not disturb the already-pasted stack.
        let r = call(
            &state,
            "paste_attributes",
            json!({
                "source_clip_id": src, "target_clip_ids": [t],
                "attributes": ["grade"]
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "paste grade-only: {r:?}");
        let c = clip_json(&state, t).await;
        assert_eq!(c["grade"]["bypass"], json!(true));
        assert_eq!(c["effects"].as_array().unwrap().len(), 2);
    }

    /// Every refusal path, including the one that matters most: an unknown
    /// target aborts the WHOLE paste instead of landing on the others.
    #[tokio::test]
    async fn paste_attributes_refuses_bad_requests() {
        let (state, src, targets) = paste_attr_state().await;
        let ghost = uuid::Uuid::new_v4().to_string();

        for (args, why) in [
            (
                json!({ "source_clip_id": src, "target_clip_ids": [] }),
                "empty target list",
            ),
            (
                json!({ "source_clip_id": src, "target_clip_ids": [&targets[0]], "attributes": [] }),
                "empty attribute list",
            ),
            (
                json!({ "source_clip_id": ghost, "target_clip_ids": [&targets[0]] }),
                "unknown source",
            ),
            (
                json!({ "source_clip_id": src, "target_clip_ids": [&targets[0], ghost] }),
                "unknown target",
            ),
        ] {
            let r = call(&state, "paste_attributes", args).await;
            assert_eq!(r.is_error, Some(true), "{why} must refuse: {r:?}");
        }

        // …and the refused "unknown target" call left NO partial paste behind.
        let c = clip_json(&state, &targets[0]).await;
        assert_eq!(
            c["effects"].as_array().map(|a| a.len()).unwrap_or(0),
            0,
            "a refused paste must not have landed on the resolvable target"
        );
    }

    // ── Unit test: resolve_tick precedence (design rule 3) ──────────────────

    #[test]
    fn resolve_tick_precedence_and_missing_context() {
        let fr = FrameRate::FPS_30;
        // ticks beats tc and seconds.
        assert_eq!(
            resolve_tick(Some(100), Some("00:00:01:00"), Some(5.0), Some(fr)).unwrap(),
            Tick(100)
        );
        // tc beats seconds.
        assert_eq!(
            resolve_tick(None, Some("00:00:01:00"), Some(5.0), Some(fr)).unwrap(),
            Tick::from_seconds(1)
        );
        // seconds is the last fallback.
        assert_eq!(
            resolve_tick(None, None, Some(2.0), Some(fr)).unwrap(),
            Tick::from_seconds(2)
        );
        // tc with no sequence context -> MissingSequenceContext.
        let err = resolve_tick(None, Some("00:00:01:00"), None, None).unwrap_err();
        assert_eq!(err.is_error, Some(true));
        assert_eq!(data(&err)["error_code"], json!("MissingSequenceContext"));
        // nothing supplied -> plain error.
        assert!(resolve_tick(None, None, None, Some(fr)).is_err());
    }

    // ── Unit test: parse_timecode is frame-grid exact for NTSC (spec 10 §1.3) ──

    #[test]
    fn parse_timecode_ntsc_lands_on_frame_grid() {
        let fr = FrameRate::FPS_29_97; // 30000/1001
        let tpf = fr.ticks_per_frame().0;
        // 00:01:00:00 @ 29.97 is frame 1800 (60 s * 30 nominal fps), NOT 60
        // wall-clock seconds. The old wall-clock parse landed ~1.8 frames early.
        let t = parse_timecode("00:01:00:00", fr).unwrap();
        assert_eq!(t, Tick(1800 * tpf), "00:01:00:00 must be frame 1800");
        assert_eq!(t, Tick(42_378_336_000), "spec-mandated exact tick value");
        assert_eq!(fr.frame_at(t), 1800);
        // A wall-clock-seconds parse would have given 60 * TICKS_PER_SECOND,
        // which is a different (off-grid) value.
        assert_ne!(t, Tick::from_seconds(60));

        // Frame field carries within the nominal second: 00:00:00:29 = frame 29.
        assert_eq!(parse_timecode("00:00:00:29", fr).unwrap(), Tick(29 * tpf));
        // K-A12: `;` is real SMPTE drop-frame — :00/:01 of non-10th minutes are
        // invalid labels; 00:01:00;02 is the first legal label after the drop.
        assert!(
            parse_timecode("00:01:00;00", fr).is_none(),
            "DF drops frames 0–1 at mm=01"
        );
        let df = parse_timecode("00:01:00;02", fr).unwrap();
        // raw (1*60*30+2) − 2 dropped = frame 1800
        assert_eq!(df, Tick(1800 * tpf));
    }

    #[test]
    fn parse_timecode_integer_rate_matches_wall_clock() {
        // For exact integer rates a whole-second timecode coincides with
        // wall-clock seconds (1 s = 30 frames @ 30 fps).
        let fr = FrameRate::FPS_30;
        assert_eq!(
            parse_timecode("00:00:01:00", fr).unwrap(),
            Tick::from_seconds(1)
        );
        assert_eq!(
            parse_timecode("01:00:00:00", fr).unwrap(),
            Tick::from_seconds(3600)
        );
    }

    #[test]
    fn parse_timecode_rejects_out_of_range_and_malformed() {
        let fr = FrameRate::FPS_30;
        // Frame field must be < nominal_fps (30).
        assert!(parse_timecode("00:00:00:30", fr).is_none());
        // Minutes / seconds must be < 60.
        assert!(parse_timecode("00:60:00:00", fr).is_none());
        assert!(parse_timecode("00:00:60:00", fr).is_none());
        // Negative components are rejected.
        assert!(parse_timecode("-1:00:00:00", fr).is_none());
        assert!(parse_timecode("00:-1:00:00", fr).is_none());
        assert!(parse_timecode("00:00:00:-1", fr).is_none());
        // Structurally malformed: missing frame field / wrong part count.
        assert!(parse_timecode("00:01:00", fr).is_none());
        assert!(parse_timecode("00:00:00:00:00", fr).is_none());
        assert!(parse_timecode("garbage", fr).is_none());
        assert!(parse_timecode("aa:bb:cc:dd", fr).is_none());
        // Boundary that IS valid: last frame of the second.
        assert!(parse_timecode("00:00:00:29", fr).is_some());
    }

    #[test]
    fn parse_timecode_every_valid_tc_is_frame_aligned() {
        // Property: for a spread of rates and components, any timecode the parser
        // accepts lands exactly on a frame boundary (snap is a no-op).
        for fr in [
            FrameRate::FPS_24,
            FrameRate::FPS_25,
            FrameRate::FPS_30,
            FrameRate::FPS_60,
            FrameRate::FPS_29_97,
            FrameRate::FPS_23_976,
        ] {
            let den = fr.den.max(1) as i64;
            let nominal = ((fr.num as i64) + den / 2) / den;
            for &(h, m, s) in &[(0, 0, 0), (0, 1, 0), (1, 23, 45), (2, 59, 59), (10, 0, 30)] {
                for ff in [0, 1, nominal / 2, nominal - 1] {
                    let tc = format!("{h:02}:{m:02}:{s:02}:{ff:02}");
                    let t = parse_timecode(&tc, fr)
                        .unwrap_or_else(|| panic!("{tc} @ {fr:?} should parse"));
                    assert_eq!(fr.snap(t), t, "{tc} @ {fr:?} not frame-aligned");
                    assert_eq!(
                        fr.frame_start(fr.frame_at(t)),
                        t,
                        "{tc} @ {fr:?} not on its own frame boundary"
                    );
                }
            }
        }
    }

    // ── Tool family: sequence → track → clip → split → list (per work order) ─

    #[tokio::test]
    async fn family_sequence_track_clip_split_list() {
        let state = test_state();
        let (_, track_id) = create_seq_and_track(&state, "video").await;

        let clip_id = insert_solid_clip(&state, &track_id, 0, 1000).await;

        let r = call(
            &state,
            "split_clip",
            json!({ "clip_id": clip_id, "at_ticks": 500 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "split_clip: {r:?}");
        let new_clip_id = data(&r)["new_clip_id"].clone();
        assert!(!new_clip_id.is_null());

        let r = call(&state, "list_clips", json!({})).await;
        assert_ne!(r.is_error, Some(true), "list_clips: {r:?}");
        let clips = data(&r)["clips"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            clips.len(),
            2,
            "expected 2 clips after split, got {clips:?}"
        );
    }

    // ── Tool family: track lifecycle ─────────────────────────────────────────

    #[tokio::test]
    async fn family_track_lifecycle() {
        let state = test_state();
        let (seq_id, track_id) = create_seq_and_track(&state, "audio").await;

        let r = call(
            &state,
            "set_track_prop",
            json!({ "track_id": track_id, "name": "Music", "locked": true }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_track_prop: {r:?}");

        let track_id2 = create_track(&state, &seq_id, "audio").await;
        let r = call(
            &state,
            "reorder_track",
            json!({ "track_id": track_id2, "new_index": 0 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "reorder_track: {r:?}");

        let r = call(&state, "remove_track", json!({ "track_id": track_id })).await;
        assert_ne!(r.is_error, Some(true), "remove_track: {r:?}");
    }

    // ── Tool family: clip edit ops ────────────────────────────────────────────

    #[tokio::test]
    async fn family_clip_edit_ops() {
        let state = test_state();
        let (seq_id, track_id) = create_seq_and_track(&state, "video").await;

        let clip_a = insert_solid_clip(&state, &track_id, 0, 1000).await;
        let clip_b = insert_solid_clip(&state, &track_id, 1000, 1000).await;

        let r = call(
            &state,
            "roll_edit",
            json!({ "clip_id_a": clip_a, "clip_id_b": clip_b, "delta_ticks": 100 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "roll_edit: {r:?}");

        let r = call(
            &state,
            "move_clip",
            json!({ "clip_id": clip_a, "new_start_ticks": 5000 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "move_clip: {r:?}");

        // Cross-track move.
        let other_track = create_track(&state, &seq_id, "video").await;
        let r = call(
            &state,
            "move_clip",
            json!({ "clip_id": clip_a, "new_start_ticks": 6000, "new_track_id": other_track }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "cross-track move_clip: {r:?}");
        let r = call(&state, "list_clips", json!({ "track_id": other_track })).await;
        let clips_on_dest = data(&r)["clips"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            clips_on_dest.len(),
            1,
            "clip should have landed on the destination track: {clips_on_dest:?}"
        );

        let r = call(
            &state,
            "trim_clip",
            json!({ "clip_id": clip_b, "edge": "out", "new_ticks": 1500 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "trim_clip: {r:?}");

        let r = call(
            &state,
            "slip_clip",
            json!({ "clip_id": clip_b, "delta_ticks": 10 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "slip_clip: {r:?}");

        // Fresh isolated pair for slide_clip so it doesn't interact with clip_a/clip_b's post-roll state.
        let track2 = create_track(&state, &seq_id, "video").await;
        let clip_c = insert_solid_clip(&state, &track2, 0, 1000).await;
        let clip_d = insert_solid_clip(&state, &track2, 1000, 1000).await;
        let r = call(
            &state,
            "slide_clip",
            json!({ "clip_id": clip_d, "delta_ticks": 50 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "slide_clip: {r:?}");

        let r = call(
            &state,
            "remove_clip",
            json!({ "clip_id": clip_c, "ripple": true }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "remove_clip ripple: {r:?}");
    }

    #[tokio::test]
    async fn insert_clip_rejects_overlap() {
        let state = test_state();
        let (_, track_id) = create_seq_and_track(&state, "video").await;
        let _ = insert_solid_clip(&state, &track_id, 0, 1000).await;
        let r = call(
            &state,
            "insert_clip",
            json!({
                "track_id": track_id, "start_ticks": 500, "duration_ticks": 1000,
                "source": {"kind": "solid_color", "color": "#0000ff"}
            }),
        )
        .await;
        assert_eq!(r.is_error, Some(true), "overlapping insert should fail");
    }

    #[tokio::test]
    async fn ripple_edit_trims_and_shifts_later_clips() {
        let state = test_state();
        let (_, track_id) = create_seq_and_track(&state, "video").await;
        let clip_a = insert_solid_clip(&state, &track_id, 0, 1000).await;
        let clip_b = insert_solid_clip(&state, &track_id, 1000, 1000).await;

        // Trim clip_a's out-point later by 200 ticks; clip_b should shift later
        // by 200 to close the resulting gap — one undo step.
        let r = call(
            &state,
            "ripple_edit",
            json!({ "clip_id": clip_a, "edge": "out", "delta_ticks": 200 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "ripple_edit: {r:?}");

        let r = call(&state, "get_clip", json!({ "clip_id": clip_a })).await;
        assert_eq!(data(&r)["clip"]["duration"], json!(1200));

        let r = call(&state, "get_clip", json!({ "clip_id": clip_b })).await;
        assert_eq!(
            data(&r)["clip"]["start"],
            json!(1200),
            "clip_b should shift later by 200 to close the gap"
        );

        let r = call(&state, "undo", json!({})).await;
        assert_ne!(r.is_error, Some(true), "undo: {r:?}");
        let r = call(&state, "get_clip", json!({ "clip_id": clip_a })).await;
        assert_eq!(
            data(&r)["clip"]["duration"],
            json!(1000),
            "ripple_edit should undo as one step"
        );
        let r = call(&state, "get_clip", json!({ "clip_id": clip_b })).await;
        assert_eq!(data(&r)["clip"]["start"], json!(1000));
    }

    // ── Link-group fan-out (INTEG-MCP-LINK): move_clip / remove_clip must
    // expand across `ops::clips_in_link_group` the same way the GUI's
    // `ops_bridge` does, so an agent-driven edit doesn't strand a linked
    // partner. Trim deliberately does NOT propagate (matches the GUI).

    #[tokio::test]
    async fn move_clip_carries_linked_partner_in_one_undo_step() {
        let state = test_state();
        let (seq_id, v_track) = create_seq_and_track(&state, "video").await;
        let a_track = create_track(&state, &seq_id, "audio").await;
        let vclip = insert_solid_clip(&state, &v_track, 0, 1000).await;
        let aclip = insert_solid_clip(&state, &a_track, 0, 1000).await;

        let r = call(
            &state,
            "link_clips",
            json!({ "clip_id_a": vclip, "clip_id_b": aclip }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "link_clips: {r:?}");

        // Move only the video clip via MCP — the linked audio partner must
        // ride along on its own track by the identical delta (gate: "move
        // video left audio at 0" must NOT happen).
        let r = call(
            &state,
            "move_clip",
            json!({ "clip_id": vclip, "new_start_ticks": 500 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "move_clip: {r:?}");

        let r = call(&state, "get_clip", json!({ "clip_id": vclip })).await;
        assert_eq!(data(&r)["clip"]["start"], json!(500));
        let r = call(&state, "get_clip", json!({ "clip_id": aclip })).await;
        assert_eq!(
            data(&r)["clip"]["start"],
            json!(500),
            "linked audio partner must move with the video clip"
        );

        // One undo restores BOTH — proves the fan-out landed as a single
        // undo step, not two.
        let r = call(&state, "undo", json!({})).await;
        assert_ne!(r.is_error, Some(true), "undo: {r:?}");
        let r = call(&state, "get_clip", json!({ "clip_id": vclip })).await;
        assert_eq!(data(&r)["clip"]["start"], json!(0));
        let r = call(&state, "get_clip", json!({ "clip_id": aclip })).await;
        assert_eq!(
            data(&r)["clip"]["start"],
            json!(0),
            "single undo must restore both halves of the link group"
        );
    }

    #[tokio::test]
    async fn cross_track_move_clip_carries_linked_partner_on_its_own_track() {
        let state = test_state();
        let (seq_id, v_track) = create_seq_and_track(&state, "video").await;
        let v2_track = create_track(&state, &seq_id, "video").await;
        let a_track = create_track(&state, &seq_id, "audio").await;
        let vclip = insert_solid_clip(&state, &v_track, 0, 1000).await;
        let aclip = insert_solid_clip(&state, &a_track, 0, 1000).await;

        let r = call(
            &state,
            "link_clips",
            json!({ "clip_id_a": vclip, "clip_id_b": aclip }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "link_clips: {r:?}");

        let r = call(
            &state,
            "move_clip",
            json!({ "clip_id": vclip, "new_start_ticks": 200, "new_track_id": v2_track }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "cross-track move_clip: {r:?}");

        // Primary clip landed on the destination track at the new start.
        let r = call(&state, "list_clips", json!({ "track_id": v2_track })).await;
        let clips_on_dest = data(&r)["clips"].as_array().cloned().unwrap_or_default();
        assert_eq!(clips_on_dest.len(), 1, "clip should have moved to V2");

        // Linked audio partner stayed on ITS OWN track (never reassigned)
        // but shifted by the same delta.
        let r = call(&state, "get_clip", json!({ "clip_id": aclip })).await;
        assert_eq!(
            data(&r)["clip"]["start"],
            json!(200),
            "linked audio partner should shift on its own track, not follow to V2"
        );
        let r = call(&state, "list_clips", json!({ "track_id": a_track })).await;
        let clips_on_audio = data(&r)["clips"].as_array().cloned().unwrap_or_default();
        assert_eq!(clips_on_audio.len(), 1, "audio partner must stay on A1");
    }

    // ── move_clips: multi-select body move (04 §2.6, 210 §5, 19 G-21) ──────

    /// A contiguous run shifts as a body in one undo step. Contiguity is the
    /// point: three separate `move_clip` calls would each refuse on the
    /// neighbour they are about to vacate.
    #[tokio::test]
    async fn move_clips_shifts_a_contiguous_run_in_one_undo_step() {
        let state = test_state();
        let (_, track_id) = create_seq_and_track(&state, "video").await;
        let a = insert_solid_clip(&state, &track_id, 100, 100).await;
        let b = insert_solid_clip(&state, &track_id, 200, 100).await;
        let c = insert_solid_clip(&state, &track_id, 300, 100).await;

        let r = call(
            &state,
            "move_clips",
            json!({ "clip_ids": [a, b, c], "delta_ticks": 50 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "move_clips: {r:?}");

        let starts = |v: &Value| -> Vec<i64> {
            let mut s: Vec<i64> = v["clips"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|c| c["start_ticks"].as_i64().unwrap())
                .collect();
            s.sort_unstable();
            s
        };
        let r = call(&state, "list_clips", json!({ "track_id": track_id })).await;
        assert_eq!(starts(&data(&r)), vec![150, 250, 350]);

        // One undo returns the whole body.
        let r = call(&state, "undo", json!({})).await;
        assert_ne!(r.is_error, Some(true), "undo: {r:?}");
        let r = call(&state, "list_clips", json!({ "track_id": track_id })).await;
        assert_eq!(
            starts(&data(&r)),
            vec![100, 200, 300],
            "one undo must revert the whole multi-move"
        );
    }

    /// Refused whole, not in part: a blocked move leaves every clip put.
    #[tokio::test]
    async fn move_clips_blocked_move_shifts_nothing() {
        let state = test_state();
        let (_, track_id) = create_seq_and_track(&state, "video").await;
        let a = insert_solid_clip(&state, &track_id, 100, 100).await;
        let b = insert_solid_clip(&state, &track_id, 200, 100).await;
        // Stationary obstacle just past the run.
        let _blocker = insert_solid_clip(&state, &track_id, 320, 50).await;

        let r = call(
            &state,
            "move_clips",
            json!({ "clip_ids": [a, b], "delta_ticks": 50 }),
        )
        .await;
        assert_eq!(r.is_error, Some(true), "blocked move should fail: {r:?}");

        let r = call(&state, "list_clips", json!({ "track_id": track_id })).await;
        let mut s: Vec<i64> = data(&r)["clips"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|c| c["start_ticks"].as_i64().unwrap())
            .collect();
        s.sort_unstable();
        assert_eq!(s, vec![100, 200, 320], "nothing may move on a refusal");
    }

    /// Naming a linked pair explicitly must not shift the partner twice.
    #[tokio::test]
    async fn move_clips_moves_a_selected_link_partner_exactly_once() {
        let state = test_state();
        let (seq_id, v_track) = create_seq_and_track(&state, "video").await;
        let a_track = create_track(&state, &seq_id, "audio").await;
        let vclip = insert_solid_clip(&state, &v_track, 100, 100).await;
        let aclip = insert_solid_clip(&state, &a_track, 100, 100).await;
        let r = call(
            &state,
            "link_clips",
            json!({ "clip_id_a": vclip.clone(), "clip_id_b": aclip.clone() }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "link_clips: {r:?}");

        let r = call(
            &state,
            "move_clips",
            json!({ "clip_ids": [vclip.clone(), aclip.clone()], "delta_ticks": 40 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "move_clips: {r:?}");

        let r = call(&state, "get_clip", json!({ "clip_id": vclip })).await;
        assert_eq!(data(&r)["clip"]["start"], json!(140));
        let r = call(&state, "get_clip", json!({ "clip_id": aclip })).await;
        assert_eq!(
            data(&r)["clip"]["start"],
            json!(140),
            "a selected partner moves once, not once per mention"
        );
    }

    #[tokio::test]
    async fn remove_clip_carries_linked_partner_in_one_undo_step() {
        let state = test_state();
        let (seq_id, v_track) = create_seq_and_track(&state, "video").await;
        let a_track = create_track(&state, &seq_id, "audio").await;
        let vclip = insert_solid_clip(&state, &v_track, 0, 1000).await;
        let aclip = insert_solid_clip(&state, &a_track, 0, 1000).await;

        let r = call(
            &state,
            "link_clips",
            json!({ "clip_id_a": vclip, "clip_id_b": aclip }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "link_clips: {r:?}");

        let r = call(&state, "remove_clip", json!({ "clip_id": vclip })).await;
        assert_ne!(r.is_error, Some(true), "remove_clip: {r:?}");

        let r = call(&state, "list_clips", json!({})).await;
        let clips = data(&r)["clips"].as_array().cloned().unwrap_or_default();
        assert!(
            clips.is_empty(),
            "both linked clips should be gone: {clips:?}"
        );

        // One undo restores BOTH.
        let r = call(&state, "undo", json!({})).await;
        assert_ne!(r.is_error, Some(true), "undo: {r:?}");
        let r = call(&state, "list_clips", json!({})).await;
        let clips = data(&r)["clips"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            clips.len(),
            2,
            "single undo must restore both halves of the link group: {clips:?}"
        );
    }

    #[tokio::test]
    async fn trim_clip_does_not_propagate_to_linked_partner() {
        let state = test_state();
        let (seq_id, v_track) = create_seq_and_track(&state, "video").await;
        let a_track = create_track(&state, &seq_id, "audio").await;
        let vclip = insert_solid_clip(&state, &v_track, 0, 1000).await;
        let aclip = insert_solid_clip(&state, &a_track, 0, 1000).await;

        let r = call(
            &state,
            "link_clips",
            json!({ "clip_id_a": vclip, "clip_id_b": aclip }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "link_clips: {r:?}");

        let r = call(
            &state,
            "trim_clip",
            json!({ "clip_id": vclip, "edge": "out", "new_ticks": 700 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "trim_clip: {r:?}");

        let r = call(&state, "get_clip", json!({ "clip_id": vclip })).await;
        assert_eq!(data(&r)["clip"]["duration"], json!(700));
        // Trim intentionally does NOT propagate — matches the GUI ("trim is
        // independent, move is linked", 14 §M-2). Linked partner's own
        // in/out points stay put.
        let r = call(&state, "get_clip", json!({ "clip_id": aclip })).await;
        assert_eq!(
            data(&r)["clip"]["duration"],
            json!(1000),
            "trim must NOT propagate to a linked partner"
        );
    }

    #[tokio::test]
    async fn move_clip_of_an_unlinked_clip_leaves_other_clips_untouched() {
        let state = test_state();
        let (seq_id, v_track) = create_seq_and_track(&state, "video").await;
        let a_track = create_track(&state, &seq_id, "audio").await;
        let vclip = insert_solid_clip(&state, &v_track, 0, 1000).await;
        let aclip = insert_solid_clip(&state, &a_track, 0, 1000).await;
        // No link this time.

        let r = call(
            &state,
            "move_clip",
            json!({ "clip_id": vclip, "new_start_ticks": 300 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "move_clip: {r:?}");

        let r = call(&state, "get_clip", json!({ "clip_id": aclip })).await;
        assert_eq!(data(&r)["clip"]["start"], json!(0));
    }

    #[tokio::test]
    async fn cross_track_move_round_trips_via_undo() {
        let state = test_state();
        let (seq_id, track_a) = create_seq_and_track(&state, "video").await;
        let track_b = create_track(&state, &seq_id, "video").await;
        let clip_id = insert_solid_clip(&state, &track_a, 0, 1000).await;

        let r = call(
            &state,
            "move_clip",
            json!({ "clip_id": clip_id, "new_start_ticks": 2000, "new_track_id": track_b }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "move_clip cross-track: {r:?}");

        let r = call(&state, "list_clips", json!({ "track_id": track_b })).await;
        assert_eq!(
            data(&r)["clips"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .len(),
            1
        );
        let r = call(&state, "list_clips", json!({ "track_id": track_a })).await;
        assert_eq!(
            data(&r)["clips"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .len(),
            0
        );

        let r = call(&state, "undo", json!({})).await;
        assert_ne!(r.is_error, Some(true), "undo: {r:?}");

        let r = call(&state, "list_clips", json!({ "track_id": track_a })).await;
        let clips_a = data(&r)["clips"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            clips_a.len(),
            1,
            "clip should be back on track_a after undo"
        );
        assert_eq!(clips_a[0]["start_ticks"], json!(0));
        let r = call(&state, "list_clips", json!({ "track_id": track_b })).await;
        assert_eq!(
            data(&r)["clips"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .len(),
            0
        );
    }

    // ── Tool family: markers + work range ─────────────────────────────────────

    #[tokio::test]
    async fn family_markers_and_work_range() {
        let state = test_state();
        let (seq_id, _) = create_seq_and_track(&state, "video").await;

        let r = call(
            &state,
            "add_marker",
            json!({ "sequence_id": seq_id, "at_ticks": 100, "name": "Beat 1", "color": "#ff0000" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "add_marker: {r:?}");
        let marker_id = data(&r)["marker_id"].clone();

        let r = call(
            &state,
            "add_marker",
            json!({ "sequence_id": seq_id, "at_ticks": 500, "name": "Beat 2" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "add_marker 2: {r:?}");

        let r = call(&state, "list_markers", json!({ "sequence_id": seq_id })).await;
        assert_ne!(r.is_error, Some(true), "list_markers: {r:?}");
        let markers = data(&r)["markers"].as_array().cloned().unwrap_or_default();
        assert_eq!(markers.len(), 2, "{markers:?}");

        let r = call(&state, "remove_marker", json!({ "marker_id": marker_id })).await;
        assert_ne!(r.is_error, Some(true), "remove_marker: {r:?}");
        let r = call(&state, "list_markers", json!({ "sequence_id": seq_id })).await;
        assert_eq!(
            data(&r)["markers"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .len(),
            1
        );

        let r = call(
            &state,
            "set_work_range",
            json!({ "sequence_id": seq_id, "range": {"start_ticks": 0, "end_ticks": 1000} }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_work_range: {r:?}");

        let r = call(&state, "set_work_range", json!({ "sequence_id": seq_id })).await;
        assert_ne!(r.is_error, Some(true), "set_work_range clear: {r:?}");
    }

    // ── Tool family: marker depth (26 K-A2) ──────────────────────────────────

    /// The category registry is writable end-to-end, and deleting a category
    /// retargets its markers in the SAME undo step.
    #[tokio::test]
    async fn family_marker_categories() {
        let state = test_state();
        let (seq_id, _) = create_seq_and_track(&state, "video").await;

        // Empty until something seeds it.
        let r = call(&state, "list_marker_categories", json!({})).await;
        assert_ne!(r.is_error, Some(true), "list_marker_categories: {r:?}");
        assert_eq!(
            data(&r)["categories"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .len(),
            0,
            "a fresh project has no categories"
        );

        // Counted against the seed itself, not a literal: proposal 210 added a
        // sixth default ("Bookmarks") and a pinned `5` went red on a change the
        // test has no opinion about. The seed's *length* is not what this test
        // is protecting — idempotency and one-undo-per-batch are.
        let seeded = photonic_core::timeline::MarkerCategory::default_seed().len();
        let r = call(&state, "seed_marker_categories", json!({})).await;
        assert_ne!(r.is_error, Some(true), "seed_marker_categories: {r:?}");
        let cats = data(&r)["categories"].as_array().cloned().unwrap();
        assert_eq!(cats.len(), seeded, "{cats:?}");
        let cut = cats[1]["id"].clone();
        let note = cats[2]["id"].clone();

        // Seeding twice does not duplicate.
        let _ = call(&state, "seed_marker_categories", json!({})).await;
        let r = call(&state, "list_marker_categories", json!({})).await;
        assert_eq!(
            data(&r)["categories"].as_array().unwrap().len(),
            seeded,
            "seeding is idempotent"
        );

        // Seeding is ONE undo unit — undo empties the registry in one step.
        let r = call(&state, "undo", json!({})).await;
        assert_ne!(r.is_error, Some(true), "undo: {r:?}");
        let r = call(&state, "list_marker_categories", json!({})).await;
        assert_eq!(
            data(&r)["categories"].as_array().unwrap().len(),
            0,
            "one undo must revert the whole seed batch"
        );
        let r = call(&state, "redo", json!({})).await;
        assert_ne!(r.is_error, Some(true), "redo: {r:?}");
        let r = call(&state, "list_marker_categories", json!({})).await;
        assert_eq!(data(&r)["categories"].as_array().unwrap().len(), seeded);

        // Rename/recolour/re-glyph in place.
        let r = call(
            &state,
            "update_marker_category",
            json!({ "category_id": cut, "name": "Hard cut", "color": "#112233", "glyph": "square" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "update_marker_category: {r:?}");
        let r = call(&state, "list_marker_categories", json!({})).await;
        let cats = data(&r)["categories"].as_array().cloned().unwrap();
        assert_eq!(cats[1]["name"], json!("Hard cut"));
        assert_eq!(cats[1]["glyph"], json!("square"));
        assert_eq!(cats[1]["id"], cut, "the id must survive a rename");

        // A custom category.
        let r = call(
            &state,
            "add_marker_category",
            json!({ "name": "VFX", "color": "#8800ff", "glyph": "flag" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "add_marker_category: {r:?}");
        let vfx = data(&r)["category_id"].clone();

        // Put a marker on "Hard cut", then delete that category with a reassign.
        let r = call(
            &state,
            "add_marker",
            json!({ "sequence_id": seq_id, "at_ticks": 100, "name": "m", "category_id": cut }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "add_marker with category: {r:?}");
        let marker_id = data(&r)["marker_id"].clone();
        let r = call(&state, "list_markers", json!({ "sequence_id": seq_id })).await;
        assert_eq!(
            data(&r)["markers"][0]["category"],
            cut,
            "fixture is non-vacuous: the marker really is on the doomed category"
        );

        let r = call(
            &state,
            "remove_marker_category",
            json!({ "category_id": cut, "reassign_to": note }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "remove_marker_category: {r:?}");
        assert_eq!(data(&r)["markers_retargeted"], json!(1));
        let r = call(&state, "list_markers", json!({ "sequence_id": seq_id })).await;
        assert_eq!(
            data(&r)["markers"][0]["category"],
            note,
            "the marker must be reassigned, not left dangling"
        );

        // ONE undo restores both the category and the marker's reference.
        let r = call(&state, "undo", json!({})).await;
        assert_ne!(r.is_error, Some(true), "undo: {r:?}");
        let r = call(&state, "list_markers", json!({ "sequence_id": seq_id })).await;
        assert_eq!(
            data(&r)["markers"][0]["category"],
            cut,
            "undo must put the marker back on the deleted category"
        );
        let r = call(&state, "list_marker_categories", json!({})).await;
        let cats = data(&r)["categories"].as_array().cloned().unwrap();
        assert_eq!(cats[1]["id"], cut, "…at its original display position");

        // Reassigning to the category being deleted is refused.
        let r = call(
            &state,
            "remove_marker_category",
            json!({ "category_id": cut, "reassign_to": cut }),
        )
        .await;
        assert_eq!(r.is_error, Some(true), "self-reassign must fail: {r:?}");
        // As is pointing a marker at a category that does not exist.
        let r = call(
            &state,
            "set_marker",
            json!({ "marker_id": marker_id, "category_id": uuid::Uuid::new_v4().to_string() }),
        )
        .await;
        assert_eq!(r.is_error, Some(true), "unknown category must fail: {r:?}");
        let _ = vfx;
    }

    /// `set_marker` is what makes a RANGED marker reachable — the unit
    /// `export_per_marker` (K-F2) fans out over.
    #[tokio::test]
    async fn set_marker_edits_every_field_including_the_range() {
        let state = test_state();
        let (seq_id, _) = create_seq_and_track(&state, "video").await;

        let r = call(
            &state,
            "add_marker",
            json!({ "sequence_id": seq_id, "at_ticks": 100, "name": "point" }),
        )
        .await;
        let marker_id = data(&r)["marker_id"].clone();
        let r = call(&state, "list_markers", json!({ "sequence_id": seq_id })).await;
        assert_eq!(
            data(&r)["markers"][0]["duration"],
            json!(0),
            "a plain add_marker is a POINT marker — the thing K-F2 skips"
        );

        let r = call(
            &state,
            "set_marker",
            json!({
                "marker_id": marker_id, "at_ticks": 200, "duration_ticks": 900,
                "name": "chapter", "note": "review", "color": "#00ff00"
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_marker: {r:?}");
        let r = call(&state, "list_markers", json!({ "sequence_id": seq_id })).await;
        let m = &data(&r)["markers"][0];
        assert_eq!(m["at"], json!(200));
        assert_eq!(m["duration"], json!(900), "now a RANGED marker");
        assert_eq!(m["name"], json!("chapter"));
        assert_eq!(m["note"], json!("review"));

        // Clearing the colour override falls back to the category colour.
        let r = call(
            &state,
            "set_marker",
            json!({ "marker_id": marker_id, "color": "" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "clear color: {r:?}");
        let r = call(&state, "list_markers", json!({ "sequence_id": seq_id })).await;
        assert!(data(&r)["markers"][0]["color"].is_null());

        // Ranged markers can also be created directly, by duration or by end.
        let r = call(
            &state,
            "add_marker",
            json!({ "sequence_id": seq_id, "at_seconds": 2.0, "duration_seconds": 1.0, "name": "b" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "ranged add_marker: {r:?}");
        let r = call(
            &state,
            "add_marker",
            json!({ "sequence_id": seq_id, "at_tc": "00:00:10:00", "end_tc": "00:00:12:00", "name": "c" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "end_tc add_marker: {r:?}");
        let r = call(&state, "list_markers", json!({ "sequence_id": seq_id })).await;
        let ranged: Vec<_> = data(&r)["markers"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["duration"].as_i64().unwrap_or(0) > 0)
            .cloned()
            .collect();
        assert_eq!(ranged.len(), 3, "three ranged markers: {ranged:?}");

        // An end before the start is refused rather than clamped to a point.
        let r = call(
            &state,
            "add_marker",
            json!({ "sequence_id": seq_id, "at_tc": "00:00:10:00", "end_tc": "00:00:09:00" }),
        )
        .await;
        assert_eq!(r.is_error, Some(true), "backwards range must fail: {r:?}");

        // One set_marker = ONE undo step.
        let r = call(&state, "undo", json!({})).await;
        assert_ne!(r.is_error, Some(true), "undo: {r:?}");
    }

    /// Clip markers have a writer, are clip-relative, and travel with the clip.
    #[tokio::test]
    async fn family_clip_markers() {
        let state = test_state();
        let (_, track_id) = create_seq_and_track(&state, "video").await;
        let clip = insert_solid_clip(&state, &track_id, 1000, 1000).await;

        let r = call(
            &state,
            "add_clip_marker",
            json!({ "clip_id": clip, "at_ticks": 250, "name": "beat", "duration_ticks": 100 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "add_clip_marker: {r:?}");
        let marker_id = data(&r)["marker_id"].clone();

        let r = call(&state, "list_clip_markers", json!({ "clip_id": clip })).await;
        assert_ne!(r.is_error, Some(true), "list_clip_markers: {r:?}");
        let m = &data(&r)["markers"][0];
        assert_eq!(m["at"], json!(250), "`at` is CLIP-relative");
        assert_eq!(
            m["sequence_tick"],
            json!(1250),
            "…and the timeline position is clip.start + at"
        );
        assert_eq!(
            m["anchor"],
            json!("content"),
            "clip markers are Content-anchored"
        );

        // Edited through the same universal editor, with clip_id as the scope.
        let r = call(
            &state,
            "set_marker",
            json!({ "marker_id": marker_id, "clip_id": clip, "name": "renamed", "at_ticks": 300 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_marker on clip: {r:?}");
        let r = call(&state, "list_clip_markers", json!({ "clip_id": clip })).await;
        assert_eq!(data(&r)["markers"][0]["name"], json!("renamed"));
        assert_eq!(data(&r)["markers"][0]["at"], json!(300));

        // A position outside the clip is refused — it would never be drawn.
        let r = call(
            &state,
            "add_clip_marker",
            json!({ "clip_id": clip, "at_ticks": 5000 }),
        )
        .await;
        assert_eq!(
            r.is_error,
            Some(true),
            "out-of-clip marker must fail: {r:?}"
        );

        // A sequence-scoped set_marker must NOT find a clip marker.
        let r = call(&state, "set_marker", json!({ "marker_id": marker_id })).await;
        assert_eq!(
            r.is_error,
            Some(true),
            "clip markers are not reachable without clip_id: {r:?}"
        );

        let r = call(
            &state,
            "remove_clip_marker",
            json!({ "clip_id": clip, "marker_id": marker_id }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "remove_clip_marker: {r:?}");
        let r = call(&state, "list_clip_markers", json!({ "clip_id": clip })).await;
        assert_eq!(data(&r)["markers"].as_array().unwrap().len(), 0);

        // ...and one undo brings it back.
        let r = call(&state, "undo", json!({})).await;
        assert_ne!(r.is_error, Some(true), "undo: {r:?}");
        let r = call(&state, "list_clip_markers", json!({ "clip_id": clip })).await;
        assert_eq!(data(&r)["markers"].as_array().unwrap().len(), 1);
    }

    // ── Tool family: media bins ────────────────────────────────────────────────

    #[tokio::test]
    async fn family_media_bins() {
        let state = test_state();
        let tmp = std::env::temp_dir().join(format!(
            "photonic_mcp_bin_test_{}.mp4",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&tmp, b"bin test bytes").unwrap();

        // import_media auto-creates the bin by name.
        let r = call(
            &state,
            "import_media",
            json!({ "paths": [tmp.to_string_lossy()], "bin": "Interviews" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "import_media: {r:?}");
        let assets = data(&r)["assets"].as_array().cloned().unwrap_or_default();
        assert_eq!(assets.len(), 1);
        let asset_id = assets[0]["asset_id"].clone();
        assert!(
            !assets[0]["bin_id"].is_null(),
            "asset should carry a bin_id"
        );

        let r = call(&state, "list_bins", json!({})).await;
        assert_ne!(r.is_error, Some(true), "list_bins: {r:?}");
        let bins = data(&r)["bins"].as_array().cloned().unwrap_or_default();
        assert_eq!(bins.len(), 1);
        assert_eq!(bins[0]["name"], json!("Interviews"));
        let interviews_bin_id = bins[0]["bin_id"].clone();

        let r = call(&state, "list_media", json!({ "bin": "Interviews" })).await;
        assert_ne!(r.is_error, Some(true), "list_media filter: {r:?}");
        assert_eq!(
            data(&r)["assets"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .len(),
            1
        );

        let r = call(&state, "list_media", json!({ "bin": "Nonexistent" })).await;
        assert_ne!(
            r.is_error,
            Some(true),
            "list_media filter (no match): {r:?}"
        );
        assert_eq!(
            data(&r)["assets"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .len(),
            0
        );

        let r = call(
            &state,
            "set_asset_bin",
            json!({ "asset_id": asset_id, "bin_id": null }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_asset_bin clear: {r:?}");
        let r = call(&state, "list_media", json!({ "bin": "Interviews" })).await;
        assert_eq!(
            data(&r)["assets"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .len(),
            0
        );

        let r = call(&state, "create_bin", json!({ "name": "B-Roll" })).await;
        assert_ne!(r.is_error, Some(true), "create_bin: {r:?}");
        let broll_bin_id = data(&r)["bin_id"].clone();
        let r = call(
            &state,
            "set_asset_bin",
            json!({ "asset_id": asset_id, "bin_id": broll_bin_id }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_asset_bin: {r:?}");

        let r = call(&state, "remove_bin", json!({ "bin_id": interviews_bin_id })).await;
        assert_ne!(r.is_error, Some(true), "remove_bin: {r:?}");

        let _ = std::fs::remove_file(&tmp);
    }

    // ── Tool family: clip properties ─────────────────────────────────────────

    #[tokio::test]
    async fn family_clip_properties() {
        let state = test_state();
        let (_, track_id) = create_seq_and_track(&state, "video").await;
        let clip_id = insert_solid_clip(&state, &track_id, 0, 1000).await;

        let r = call(
            &state,
            "set_clip_prop",
            json!({
                "clip_id": clip_id, "name": "Renamed", "enabled": false,
                "transform": {"x": 10.0, "y": 20.0, "scale_x": 1.0, "scale_y": 1.0, "rotation": 0.0, "anchor_x": 0.0, "anchor_y": 0.0, "opacity": 0.5}
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_clip_prop: {r:?}");

        let r = call(
            &state,
            "set_clip_speed",
            json!({ "clip_id": clip_id, "ratio": {"num": 2, "den": 1} }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_clip_speed: {r:?}");

        let r = call(
            &state,
            "set_transition",
            json!({
                "clip_id": clip_id, "edge": "in",
                "transition": {"kind": "cross_dissolve", "duration_ticks": 200}
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_transition: {r:?}");

        let r = call(&state, "get_clip", json!({ "clip_id": clip_id })).await;
        assert_ne!(r.is_error, Some(true), "get_clip: {r:?}");
        let clip_json = data(&r)["clip"].clone();
        assert_eq!(clip_json["name"], json!("Renamed"));
        assert_eq!(clip_json["enabled"], json!(false));
    }

    // ── Tool family: effects ──────────────────────────────────────────────────

    #[tokio::test]
    async fn family_effects() {
        let state = test_state();
        let (_, track_id) = create_seq_and_track(&state, "video").await;
        let clip_id = insert_solid_clip(&state, &track_id, 0, 1000).await;

        let r = call(
            &state,
            "add_effect",
            json!({ "clip_id": clip_id, "kind": "blur" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "add_effect(blur): {r:?}");
        let r = call(
            &state,
            "add_effect",
            json!({ "clip_id": clip_id, "kind": "glow" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "add_effect(glow): {r:?}");

        let r = call(
            &state,
            "set_effect_param",
            json!({
                "clip_id": clip_id, "effect_index": 0, "path": "params.radius",
                "value": {"t": "float", "v": 12.0}
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_effect_param: {r:?}");

        let r = call(
            &state,
            "reorder_effects",
            json!({ "clip_id": clip_id, "new_order": [1, 0] }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "reorder_effects: {r:?}");

        let r = call(
            &state,
            "remove_effect",
            json!({ "clip_id": clip_id, "effect_index": 0 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "remove_effect: {r:?}");

        let r = call(&state, "list_effect_kinds", json!({})).await;
        assert_ne!(r.is_error, Some(true), "list_effect_kinds: {r:?}");
        let kinds = data(&r)["effect_kinds"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        // The manifest catalogue is the single source of truth (30 §2.7), so
        // assert against it rather than a literal — a hardcoded count went stale
        // the moment K-B16 bridged the raster kernels (7 → 44) and left this
        // test red on the branch.
        let catalogue = photonic_core::timeline::effect_manifest::manifests();
        assert_eq!(kinds.len(), catalogue.len());
        assert!(
            kinds.len() >= 7,
            "catalogue must not shrink below the 7 originally authored manifests"
        );
    }

    /// Set up a clip carrying a single Gaussian-blur effect at index 0.
    async fn clip_with_blur(state: &AppState) -> Value {
        let (_, track_id) = create_seq_and_track(state, "video").await;
        let clip_id = insert_solid_clip(state, &track_id, 0, 1000).await;
        let r = call(
            state,
            "add_effect",
            json!({ "clip_id": clip_id, "kind": "blur" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "add_effect(blur): {r:?}");
        clip_id
    }

    #[tokio::test]
    async fn set_effect_param_refuses_unknown_path() {
        let state = test_state();
        let clip_id = clip_with_blur(&state).await;
        let r = call(
            &state,
            "set_effect_param",
            json!({
                "clip_id": clip_id, "effect_index": 0, "path": "params.does_not_exist",
                "value": {"t": "float", "v": 1.0}
            }),
        )
        .await;
        assert_eq!(
            r.is_error,
            Some(true),
            "unknown path must be refused: {r:?}"
        );
    }

    #[tokio::test]
    async fn set_effect_param_refuses_out_of_range() {
        let state = test_state();
        let clip_id = clip_with_blur(&state).await;
        // blur.gaussian params.radius range is 0..500; 9999 is out of range and
        // must be refused (never clamped).
        let r = call(
            &state,
            "set_effect_param",
            json!({
                "clip_id": clip_id, "effect_index": 0, "path": "params.radius",
                "value": {"t": "float", "v": 9999.0}
            }),
        )
        .await;
        assert_eq!(
            r.is_error,
            Some(true),
            "out-of-range must be refused: {r:?}"
        );
    }

    #[tokio::test]
    async fn set_effect_param_refuses_kind_mismatch() {
        let state = test_state();
        let clip_id = clip_with_blur(&state).await;
        // params.radius is a Float param; a Bool value is a kind mismatch.
        let r = call(
            &state,
            "set_effect_param",
            json!({
                "clip_id": clip_id, "effect_index": 0, "path": "params.radius",
                "value": {"t": "bool", "v": true}
            }),
        )
        .await;
        assert_eq!(
            r.is_error,
            Some(true),
            "kind mismatch must be refused: {r:?}"
        );
    }

    #[tokio::test]
    async fn list_effect_kinds_covers_every_manifest() {
        let state = test_state();
        let r = call(&state, "list_effect_kinds", json!({})).await;
        assert_ne!(r.is_error, Some(true), "list_effect_kinds: {r:?}");
        let kinds = data(&r)["effect_kinds"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let manifests = photonic_core::timeline::manifests();
        assert_eq!(
            kinds.len(),
            manifests.len(),
            "list_effect_kinds count must match manifests()"
        );
        let got_ids: Vec<&str> = kinds.iter().map(|k| k["id"].as_str().unwrap()).collect();
        let want_ids: Vec<&str> = manifests.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(got_ids, want_ids, "ids (and order) must match manifests()");
    }

    // ── Tool family: keyframes ─────────────────────────────────────────────

    #[tokio::test]
    async fn family_keyframes() {
        let state = test_state();
        let (_, track_id) = create_seq_and_track(&state, "video").await;
        let clip_id = insert_solid_clip(&state, &track_id, 0, 1000).await;

        let r = call(
            &state,
            "set_keyframe",
            json!({
                "target": "clip_transform", "clip_id": clip_id, "path": "transform.x",
                "at_ticks": 0, "value": {"t": "float", "v": 0.0}, "interp": {"kind": "linear"}
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_keyframe: {r:?}");

        let r = call(
            &state,
            "batch_set_keyframes",
            json!({
                "ops": [
                    {"target": "clip_transform", "clip_id": clip_id, "path": "transform.x", "at_ticks": 500, "value": {"t": "float", "v": 50.0}, "interp": {"kind": "linear"}},
                    {"target": "clip_transform", "clip_id": clip_id, "path": "transform.x", "at_ticks": 1000, "value": {"t": "float", "v": 100.0}, "interp": {"kind": "hold"}}
                ]
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "batch_set_keyframes: {r:?}");

        let r = call(
            &state,
            "get_keyframes",
            json!({ "target": "clip_transform", "clip_id": clip_id }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "get_keyframes: {r:?}");
        let tracks = data(&r)["tracks"].as_array().cloned().unwrap_or_default();
        assert_eq!(tracks.len(), 1);
        let kfs = tracks[0]["keyframes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(kfs.len(), 3, "expected 3 keyframes, got {kfs:?}");

        let r = call(
            &state,
            "remove_keyframe",
            json!({ "target": "clip_transform", "clip_id": clip_id, "path": "transform.x", "at_ticks": 500 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "remove_keyframe: {r:?}");
    }

    // ── Tool family: media ─────────────────────────────────────────────────

    #[tokio::test]
    async fn family_media() {
        let state = test_state();
        let tmp =
            std::env::temp_dir().join(format!("photonic_mcp_test_{}.mp4", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, b"fake mp4 bytes for content-hash test").unwrap();

        let r = call(
            &state,
            "import_media",
            json!({ "paths": [tmp.to_string_lossy()] }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "import_media: {r:?}");
        let assets = data(&r)["assets"].as_array().cloned().unwrap_or_default();
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0]["probed"], json!(false));
        let asset_id = assets[0]["asset_id"].clone();

        let r = call(&state, "list_media", json!({})).await;
        assert_ne!(r.is_error, Some(true), "list_media: {r:?}");
        let list = data(&r)["assets"].as_array().cloned().unwrap_or_default();
        assert_eq!(list.len(), 1);
        // K-C6: import writes the engine's xxh3 identity (16 bare hex chars),
        // not the old `siphash64:` stopgap — so a hash written here is
        // comparable with one written by the GUI import worker or probe_media.
        let hash = list[0]["content_hash"].as_str().unwrap_or_default();
        assert_eq!(hash.len(), 16, "xxh3-64 renders as 16 hex chars: {hash:?}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "{hash:?}");

        let tmp2 =
            std::env::temp_dir().join(format!("photonic_mcp_test_{}.mp4", uuid::Uuid::new_v4()));
        std::fs::write(&tmp2, b"other bytes").unwrap();
        // Different bytes → refused, because a relink to the wrong take is
        // invisible until export (K-C6).
        let r = call(
            &state,
            "relink_media",
            json!({ "asset_id": asset_id, "new_path": tmp2.to_string_lossy() }),
        )
        .await;
        assert_eq!(r.is_error, Some(true), "expected a HashMismatch: {r:?}");
        assert_eq!(data(&r)["error_code"], json!("HashMismatch"));
        let r = call(
            &state,
            "relink_media",
            json!({
                "asset_id": asset_id, "new_path": tmp2.to_string_lossy(),
                "allow_hash_mismatch": true
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "relink_media: {r:?}");
        assert_eq!(data(&r)["hash"], json!("mismatch"));

        let r = call(&state, "remove_asset", json!({ "asset_id": asset_id })).await;
        assert_ne!(r.is_error, Some(true), "remove_asset: {r:?}");

        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&tmp2);
    }

    #[tokio::test]
    async fn import_media_missing_file_is_asset_offline() {
        let state = test_state();
        let r = call(
            &state,
            "import_media",
            json!({ "paths": ["/nonexistent/path/does-not-exist.mp4"] }),
        )
        .await;
        assert_eq!(r.is_error, Some(true));
        assert_eq!(data(&r)["error_code"], json!("AssetOffline"));
    }

    // ── K-C6: offline inventory + batch relink ───────────────────────────────

    /// A scratch dir under the system temp root, removed by the caller.
    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("photonic_kc6_{tag}_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Import `names` from `dir` and return their asset ids in order.
    async fn import_from(state: &AppState, dir: &std::path::Path, names: &[&str]) -> Vec<Value> {
        let paths: Vec<String> = names
            .iter()
            .map(|n| dir.join(n).to_string_lossy().into_owned())
            .collect();
        let r = call(state, "import_media", json!({ "paths": paths })).await;
        assert_ne!(r.is_error, Some(true), "import_media: {r:?}");
        data(&r)["assets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["asset_id"].clone())
            .collect()
    }

    async fn asset_path(state: &AppState, asset_id: &Value) -> String {
        let r = call(state, "list_media", json!({})).await;
        data(&r)["assets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| &a["asset_id"] == asset_id)
            .map(|a| a["source"]["path"].as_str().unwrap_or_default().to_string())
            .unwrap_or_default()
    }

    /// The moved-folder case: three clips go offline together, a dry run
    /// previews the rewrite, the real call commits it as ONE undo step.
    #[tokio::test]
    async fn relink_media_batch_moves_a_whole_folder_as_one_undo_step() {
        let state = test_state();
        let old = scratch_dir("old");
        let new = scratch_dir("new");
        let names = ["a.mp4", "b.mp4", "c.mp4"];
        for (i, n) in names.iter().enumerate() {
            std::fs::write(old.join(n), format!("clip bytes {i}").as_bytes()).unwrap();
        }
        let ids = import_from(&state, &old, &names).await;

        // Nothing is offline yet — the inventory tool must say so, or the test
        // below could pass against a pool that was broken from the start.
        let r = call(&state, "find_offline_media", json!({})).await;
        assert_eq!(data(&r)["offline"].as_array().unwrap().len(), 0);

        // Simulate the folder move: same names, same bytes, new directory.
        for n in names.iter() {
            std::fs::rename(old.join(n), new.join(n)).unwrap();
        }
        let r = call(&state, "find_offline_media", json!({})).await;
        assert_eq!(
            data(&r)["offline"].as_array().unwrap().len(),
            3,
            "moving the folder must take every asset offline: {r:?}"
        );

        // Dry run: full plan, zero document change.
        let r = call(
            &state,
            "relink_media_batch",
            json!({ "search_dir": new.to_string_lossy(), "dry_run": true }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "dry run: {r:?}");
        let d = data(&r);
        assert_eq!(d["relinked"].as_array().unwrap().len(), 3);
        assert!(d["unmatched"].as_array().unwrap().is_empty());
        assert!(d["relinked"][0]["hash"] == json!("match"));
        assert_eq!(
            asset_path(&state, &ids[0]).await,
            old.join("a.mp4").to_string_lossy(),
            "a dry run must not move anything"
        );

        // Commit.
        let r = call(
            &state,
            "relink_media_batch",
            json!({ "search_dir": new.to_string_lossy() }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "relink_media_batch: {r:?}");
        assert_eq!(data(&r)["relinked"].as_array().unwrap().len(), 3);
        for (i, n) in names.iter().enumerate() {
            assert_eq!(
                asset_path(&state, &ids[i]).await,
                new.join(n).to_string_lossy()
            );
        }

        // DoD 4: ONE user verb, ONE undo unit — a single undo restores all three.
        let r = call(&state, "undo", json!({})).await;
        assert_ne!(r.is_error, Some(true), "undo: {r:?}");
        for (i, n) in names.iter().enumerate() {
            assert_eq!(
                asset_path(&state, &ids[i]).await,
                old.join(n).to_string_lossy(),
                "one undo must restore every relinked asset"
            );
        }

        let _ = std::fs::remove_dir_all(&old);
        let _ = std::fs::remove_dir_all(&new);
    }

    /// A same-named file holding *different* bytes is the wrong-take trap. It
    /// must be reported and left offline until the user says otherwise — and a
    /// renamed file with the *right* bytes must still be found in the same scan.
    #[tokio::test]
    async fn relink_media_batch_refuses_a_wrong_take_but_finds_a_renamed_one() {
        let state = test_state();
        let old = scratch_dir("old");
        let new = scratch_dir("new");
        std::fs::write(old.join("a.mp4"), b"the real take, all of it").unwrap();
        std::fs::write(old.join("b.mp4"), b"second clip bytes").unwrap();
        let ids = import_from(&state, &old, &["a.mp4", "b.mp4"]).await;

        std::fs::remove_file(old.join("a.mp4")).unwrap();
        std::fs::remove_file(old.join("b.mp4")).unwrap();
        // Same name, different bytes → must NOT be bound silently.
        std::fs::write(new.join("a.mp4"), b"a DIFFERENT take entirely!").unwrap();
        // Right bytes under a new name → must still be found, by hash.
        std::fs::write(new.join("b_final.mp4"), b"second clip bytes").unwrap();

        let r = call(
            &state,
            "relink_media_batch",
            json!({ "search_dir": new.to_string_lossy() }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "relink_media_batch: {r:?}");
        let d = data(&r);
        assert!(
            d["hashed_scan"] == json!(true),
            "scan must have hashed: {d}"
        );
        let relinked = d["relinked"].as_array().unwrap();
        assert_eq!(relinked.len(), 1, "only the verified match commits: {d}");
        assert_eq!(relinked[0]["asset_id"], ids[1]);
        assert_eq!(relinked[0]["matched_by"], json!("content_hash"));
        let skipped = d["skipped_hash_mismatch"].as_array().unwrap();
        assert_eq!(skipped.len(), 1, "the byte change must be named: {d}");
        assert_eq!(skipped[0]["asset_id"], ids[0]);
        assert_eq!(skipped[0]["matched_by"], json!("exact_name"));

        assert_eq!(
            asset_path(&state, &ids[0]).await,
            old.join("a.mp4").to_string_lossy(),
            "the mismatched asset must stay offline, not silently rebind"
        );
        assert_eq!(
            asset_path(&state, &ids[1]).await,
            new.join("b_final.mp4").to_string_lossy()
        );

        // With consent it binds — and re-identifies the asset so the pool no
        // longer claims bytes that are not there.
        let r = call(
            &state,
            "relink_media_batch",
            json!({ "search_dir": new.to_string_lossy(), "allow_hash_mismatch": true }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "relink_media_batch: {r:?}");
        assert_eq!(
            asset_path(&state, &ids[0]).await,
            new.join("a.mp4").to_string_lossy()
        );
        let r = call(&state, "list_media", json!({})).await;
        let row = data(&r)["assets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["asset_id"] == ids[0])
            .cloned()
            .unwrap();
        let on_disk = photonic_video::media::content_hash(&new.join("a.mp4")).unwrap();
        assert_eq!(
            row["content_hash"].as_str().unwrap(),
            on_disk,
            "an accepted byte change must re-identify the asset"
        );

        let _ = std::fs::remove_dir_all(&old);
        let _ = std::fs::remove_dir_all(&new);
    }

    /// Nothing to relink is a clean, informative no-op — not an error, and not
    /// a spurious undo step.
    #[tokio::test]
    async fn relink_media_batch_reports_unmatched_assets() {
        let state = test_state();
        let old = scratch_dir("old");
        let empty = scratch_dir("empty");
        std::fs::write(old.join("a.mp4"), b"bytes").unwrap();
        let ids = import_from(&state, &old, &["a.mp4"]).await;
        std::fs::remove_file(old.join("a.mp4")).unwrap();

        let r = call(
            &state,
            "relink_media_batch",
            json!({ "search_dir": empty.to_string_lossy() }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "{r:?}");
        let d = data(&r);
        assert!(d["relinked"].as_array().unwrap().is_empty());
        assert_eq!(d["unmatched"].as_array().unwrap().len(), 1);
        assert_eq!(d["unmatched"][0]["asset_id"], ids[0]);

        // A non-directory search root is rejected rather than silently scanning
        // nothing.
        let r = call(
            &state,
            "relink_media_batch",
            json!({ "search_dir": old.join("a.mp4").to_string_lossy() }),
        )
        .await;
        assert_eq!(r.is_error, Some(true));
        assert_eq!(data(&r)["error_code"], json!("AssetOffline"));

        let _ = std::fs::remove_dir_all(&old);
        let _ = std::fs::remove_dir_all(&empty);
    }

    // ── delete_sequence dangling-reference guard ─────────────────────────────

    #[tokio::test]
    async fn delete_sequence_blocks_dangling_nested_reference() {
        let state = test_state();
        let (inner_seq, _) = create_seq_and_track(&state, "video").await;
        let (_, outer_track) = create_seq_and_track(&state, "video").await;

        let r = call(
            &state,
            "insert_clip",
            json!({
                "track_id": outer_track, "start_ticks": 0, "duration_ticks": 1000,
                "source": {"kind": "nested_sequence", "sequence_id": inner_seq}
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "insert nested_sequence clip: {r:?}");

        let r = call(
            &state,
            "delete_sequence",
            json!({ "sequence_id": inner_seq }),
        )
        .await;
        assert_eq!(
            r.is_error,
            Some(true),
            "delete of a referenced sequence should be blocked"
        );
        assert_eq!(data(&r)["error_code"], json!("CycleDetected"));
    }

    // ── Undo round-trip through the shared CommandHistory ───────────────────

    #[tokio::test]
    async fn undo_reverts_a_timeline_edit() {
        let state = test_state();
        let r = call(
            &state,
            "create_sequence",
            json!({
                "name": "UndoSeq", "frame_rate": {"num": 30, "den": 1},
                "formats": [{"name": "16:9", "width": 1920, "height": 1080}]
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "create_sequence: {r:?}");
        {
            let doc = state.document.lock().await;
            assert!(doc.timeline.is_some());
            assert_eq!(doc.timeline.as_ref().unwrap().sequences.len(), 1);
        }

        let r = call(&state, "undo", json!({})).await;
        assert_ne!(r.is_error, Some(true), "undo: {r:?}");
        {
            let doc = state.document.lock().await;
            // create_sequence batches [CreateProject, AddSequence] as ONE undo
            // step (design rule 4) when the project didn't exist yet — one
            // undo call reverts both, back to no timeline project at all.
            assert!(
                doc.timeline.is_none(),
                "expected timeline project to be undone"
            );
        }
    }

    // ── Schema/args/dispatch consistency (10 §9.3) ───────────────────────────

    const VIDEO_TOOL_NAMES: &[&str] = &[
        "create_sequence",
        "delete_sequence",
        "list_sequences",
        "set_active_sequence",
        "set_sequence_format",
        "set_active_format",
        "set_work_range",
        "add_marker",
        "remove_marker",
        "list_markers",
        // Marker depth (26 K-A2)
        "set_marker",
        "add_clip_marker",
        "remove_clip_marker",
        "list_clip_markers",
        "list_marker_categories",
        "add_marker_category",
        "seed_marker_categories",
        "update_marker_category",
        "remove_marker_category",
        "add_track",
        "remove_track",
        "set_track_prop",
        "reorder_track",
        "insert_clip",
        "move_clip",
        "trim_clip",
        "split_clip",
        "remove_clip",
        "roll_edit",
        "slip_clip",
        "slide_clip",
        "ripple_edit",
        // 3/4-point editing (16 §2, CAP-019 MCP parity)
        "insert_edit",
        "overwrite_edit",
        "lift_edit",
        "extract_edit",
        // NLE parity round-2 (17-nle-parity-round2.md, G21 CAP-019 MCP parity)
        "replace_clip_source",
        "add_edit_all_tracks",
        "close_gap",
        "match_frame",
        "insert_adjustment_clip",
        "insert_text_clip",
        "set_clip_prop",
        "set_clip_speed",
        "set_transition",
        // Clip organization: linking (14 §M-2, CAP-019 MCP parity)
        "link_clips",
        "unlink_clips",
        "list_clips",
        "get_clip",
        "add_effect",
        "remove_effect",
        "reorder_effects",
        "set_effect_param",
        "list_effect_kinds",
        // Scoped effect stacks (26 §10 K-B1/K-B2) and Paste Attributes (K-B15)
        "effect_stack",
        "paste_attributes",
        // Effect presets / custom stacks / favourites (26 §10 K-B4)
        "effect_preset_list",
        "effect_preset_save",
        "effect_preset_apply",
        "effect_preset_delete",
        "effect_preset_rename",
        "effect_favourite_list",
        "effect_favourite_set",
        "set_keyframe",
        "remove_keyframe",
        "batch_set_keyframes",
        "get_keyframes",
        "import_media",
        "relink_media",
        // Offline media recovery (26 K-C6)
        "find_offline_media",
        "relink_media_batch",
        "list_media",
        "remove_asset",
        "create_bin",
        "remove_bin",
        "set_asset_bin",
        "list_bins",
        // P3 engine slice
        "play",
        "pause",
        "seek",
        "step",
        "set_loop_range",
        "set_proxy_mode",
        "get_engine_status",
        "render_frame_at",
        "probe_media",
        "generate_proxies",
        "remove_proxy",
        "transcode_media",
        "export_sequence",
        "get_job_status",
        "cancel_job",
        "list_export_presets",
        "save_export_preset",
        "delete_export_preset",
        // P4+ slice: captions / tts / grade / graph / audio / titles
        "auto_caption",
        "add_caption_track",
        "remove_caption_track",
        "get_caption_track",
        "set_caption_cue",
        "split_caption_cue",
        "merge_caption_cues",
        "set_caption_word",
        "set_caption_style",
        "import_captions",
        "export_captions",
        "generate_voiceover",
        "set_grade",
        "apply_lut",
        "copy_grade",
        "grade_preset",
        "get_scopes",
        "create_clip_composition",
        "add_graph_node",
        "remove_graph_node",
        "add_graph_edge",
        "remove_graph_edge",
        "set_graph_node_param",
        "set_project_graph",
        "get_graph",
        "set_clip_audio",
        "set_track_audio",
        "audio_fx",
        "set_master_bus",
        "get_audio_meters",
        "get_waveform",
        "list_title_templates",
        "insert_title_template",
    ];

    #[test]
    fn every_video_tool_is_in_the_schema() {
        let schema = crate::schema_gen::tool_list();
        let names: Vec<String> = schema
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        for tool in VIDEO_TOOL_NAMES {
            assert!(
                names.contains(&tool.to_string()),
                "tool {tool} missing from schema_gen::tool_list()"
            );
        }
    }

    #[tokio::test]
    async fn every_video_tool_has_a_dispatch_arm() {
        let state = test_state();
        for tool in VIDEO_TOOL_NAMES {
            let result = crate::dispatch::dispatch_tool(&state, tool, json!({})).await;
            if let Err(msg) = result {
                assert!(
                    !msg.starts_with("Unknown tool"),
                    "{tool}: no dispatch_tool_inner arm found ({msg})"
                );
            }
        }
    }

    // ─── P3 engine slice tests (10 §9 hooks 4/5 + §7 headless matrix) ────────

    /// GPU-gate: engine-backed tests skip (adapter-skip convention) when the
    /// lazy bridge can't get an adapter. Detected via the structured
    /// `EngineUnavailable` error the tools themselves return.
    async fn engine_available(state: &AppState) -> bool {
        let r = call(state, "get_engine_status", json!({})).await;
        if r.is_error == Some(true) {
            eprintln!("no GPU adapter — skipping engine-backed MCP test");
            return false;
        }
        true
    }

    /// 10 §9 hook 4: registry/cancel/GC wiring through the REAL tool
    /// dispatch, against a fake job — no ffmpeg, no engine, no GPU.
    #[tokio::test]
    async fn job_tools_lifecycle_with_a_fake_job() {
        use crate::handlers::video_jobs::JobStatus;
        let state = test_state();

        // Unknown id → JobNotFound (structured, 10 §8).
        let r = call(
            &state,
            "get_job_status",
            json!({ "job_id": "00000000-0000-0000-0000-000000000000" }),
        )
        .await;
        assert_eq!(r.is_error, Some(true));
        assert_eq!(data(&r)["error_code"], "JobNotFound");

        let (job_id, cancel) = state.video_jobs.lock().unwrap().start("fake").unwrap();
        let jid = json!({ "job_id": job_id });

        let r = call(&state, "get_job_status", jid.clone()).await;
        assert_ne!(r.is_error, Some(true));
        assert_eq!(data(&r)["status"]["state"], "queued");

        state.video_jobs.lock().unwrap().set_status(
            job_id,
            JobStatus::Running {
                progress: 0.25,
                message: "working".into(),
            },
        );
        let r = call(&state, "get_job_status", jid.clone()).await;
        assert_eq!(data(&r)["status"]["state"], "running");
        assert_eq!(data(&r)["status"]["progress"], 0.25);

        // cancel_job flags the worker's AtomicBool cooperatively.
        let r = call(&state, "cancel_job", jid.clone()).await;
        assert_ne!(r.is_error, Some(true));
        assert!(cancel.load(std::sync::atomic::Ordering::Relaxed));

        state.video_jobs.lock().unwrap().set_status(
            job_id,
            JobStatus::Done {
                result: json!({ "ok": true }),
            },
        );
        let r = call(&state, "get_job_status", jid.clone()).await;
        assert_eq!(data(&r)["status"]["state"], "done");
        assert_eq!(data(&r)["status"]["result"]["ok"], true);

        // Cancelling a finished job is a no-op, not an error.
        let r = call(&state, "cancel_job", jid.clone()).await;
        assert_ne!(r.is_error, Some(true));

        // GC with zero retention evicts the terminal job → JobNotFound.
        state
            .video_jobs
            .lock()
            .unwrap()
            .gc(std::time::Duration::from_secs(0));
        let r = call(&state, "get_job_status", jid).await;
        assert_eq!(r.is_error, Some(true));
        assert_eq!(data(&r)["error_code"], "JobNotFound");
    }

    /// 10 §7: the playback surface works headless — on a box with no audio
    /// device the engine plays on the soft clock; none of the transport tools
    /// error. (GPU-gated; on a no-GPU box the gate itself asserts the clean
    /// `EngineUnavailable` degradation.)
    #[tokio::test]
    async fn playback_tools_degrade_cleanly_headless() {
        let state = test_state();
        let (seq_id, track_id) = create_seq_and_track(&state, "video").await;
        insert_solid_clip(&state, &track_id, 0, TICKS_PER_SECOND * 2).await;
        if !engine_available(&state).await {
            return;
        }

        let r = call(&state, "play", json!({ "sequence_id": seq_id })).await;
        assert_ne!(r.is_error, Some(true), "play: {r:?}");
        assert_eq!(
            data(&r)["playing"],
            true,
            "headless play must run on the soft clock, not fail: {r:?}"
        );

        let r = call(&state, "pause", json!({})).await;
        assert_ne!(r.is_error, Some(true), "pause: {r:?}");
        assert_eq!(data(&r)["playing"], false);

        // Paused seek is a pure clock set — playhead confirms exactly. The
        // target sits far past any soft-clock drift the play/pause window
        // above could have accumulated (seeking past content is legal).
        let tpf = TICKS_PER_SECOND / 30;
        let target = 300 * tpf;
        let r = call(
            &state,
            "seek",
            json!({ "sequence_id": seq_id, "at_ticks": target }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "seek: {r:?}");
        assert_eq!(data(&r)["playhead_ticks"], target);

        // Step +1 pauses (already paused) and lands on the next frame start.
        let r = call(&state, "step", json!({ "frames": 1 })).await;
        assert_ne!(r.is_error, Some(true), "step: {r:?}");
        assert_eq!(data(&r)["playing"], false);
        assert_eq!(data(&r)["playhead_ticks"], target + tpf);

        let r = call(
            &state,
            "set_loop_range",
            json!({
                "sequence_id": seq_id,
                "range": { "start_ticks": 0, "end_ticks": TICKS_PER_SECOND }
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_loop_range: {r:?}");

        let r = call(
            &state,
            "set_proxy_mode",
            json!({ "mode": "force_original" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_proxy_mode: {r:?}");

        let r = call(&state, "get_engine_status", json!({})).await;
        assert_ne!(r.is_error, Some(true), "get_engine_status: {r:?}");
        assert_eq!(data(&r)["active_sequence"], seq_id);
        assert_eq!(data(&r)["snapshot_synced"], true);
    }

    /// STALE-PLAYHEAD gate: a paused *backward* seek must report the tick it
    /// landed on, not the (larger) playhead one command behind. Before the
    /// fresh-frame settle, `seek`'s `playhead >= t` predicate returned the
    /// stale pre-seek tick immediately (seek F45 reporting F150), and an
    /// immediately-following `get_engine_status` echoed it. (GPU-gated; on a
    /// no-GPU box the gate asserts the clean `EngineUnavailable` degradation.)
    #[tokio::test]
    async fn seek_then_status_reports_the_seek_target_not_one_behind() {
        let state = test_state();
        let (seq_id, track_id) = create_seq_and_track(&state, "video").await;
        insert_solid_clip(&state, &track_id, 0, TICKS_PER_SECOND * 20).await;
        if !engine_available(&state).await {
            return;
        }
        let tpf = TICKS_PER_SECOND / 30;

        // Seek far forward first so the published playhead is large.
        let forward = 150 * tpf;
        let r = call(
            &state,
            "seek",
            json!({ "sequence_id": seq_id, "at_ticks": forward }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "forward seek: {r:?}");
        assert_eq!(data(&r)["playhead_ticks"], forward);

        // Now seek backward. The old `>=` predicate would report `forward`
        // (150) rather than the tick this command actually lands on (45).
        let backward = 45 * tpf;
        let r = call(
            &state,
            "seek",
            json!({ "sequence_id": seq_id, "at_ticks": backward }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "backward seek: {r:?}");
        assert_eq!(
            data(&r)["playhead_ticks"],
            backward,
            "backward seek must report the tick it landed on, not one behind"
        );

        // An immediately-following status read must agree — not lag a command.
        let r = call(&state, "get_engine_status", json!({})).await;
        assert_ne!(r.is_error, Some(true), "get_engine_status: {r:?}");
        assert_eq!(
            data(&r)["playhead_ticks"],
            backward,
            "get_engine_status must reflect the last seek, not a stale playhead"
        );
    }

    /// 10 §9 hook 5: two `render_frame_at` calls with the same args and
    /// `output_format: raw_rgba16f` are byte-identical (the evaluator's pure-
    /// function property) — plus a pixel probe and the png/scale smoke.
    ///
    /// Spawned on a large-stack thread: macOS CI Metal + full-quality eval
    /// overflowed the default tokio worker stack (`stack overflow, aborting`).
    #[tokio::test]
    async fn full_render_resolves_one_pixel_detail_after_preview() {
        let state = test_state();
        if !engine_available(&state).await {
            assert!(
                std::env::var_os("PHOTONIC_REQUIRE_GPU").is_none(),
                "GPU required"
            );
            return;
        }
        let path =
            std::env::temp_dir().join(format!("photonic-detail-{}.png", uuid::Uuid::new_v4()));
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _fixture = Fixture(path.clone());
        image::RgbaImage::from_fn(1920, 64, |x, _| {
            let c = if x % 2 == 0 { 0 } else { 255 };
            image::Rgba([c, c, c, 255])
        })
        .save(&path)
        .unwrap();
        let mut project = TimelineProject::new();
        let asset = project
            .media
            .insert(photonic_core::timeline::MediaAsset::from_file(
                AssetKind::Image,
                path,
            ));
        let mut seq = Sequence::new("detail", FrameRate::FPS_30, 1920, 64);
        let sequence_id = seq.id;
        let mut track = Track::new(photonic_core::timeline::TrackKind::Video, "V1");
        track.clips.push(Clip::new(
            ClipSource::Asset { asset },
            Tick(0),
            Tick(TICKS_PER_SECOND),
        ));
        seq.video_tracks.push(track);
        project.insert_sequence(seq);
        project.active_sequence = Some(sequence_id);
        state.document.lock().await.timeline = Some(project);
        state.history.lock().await.reset();
        let preview = call(&state, "render_frame_at", json!({
            "sequence_id": sequence_id, "at_ticks": 0, "quality": "preview", "output_format": "raw_rgba16f"
        })).await;
        assert_ne!(preview.is_error, Some(true));
        let full = call(&state, "render_frame_at", json!({
            "sequence_id": sequence_id, "at_ticks": 0, "quality": "full", "output_format": "raw_rgba16f"
        })).await;
        assert_ne!(full.is_error, Some(true));
        let payload = data(&full);
        let bytes = general_purpose::STANDARD
            .decode(payload["data_base64"].as_str().unwrap())
            .unwrap();
        let red = |x: usize| u16::from_le_bytes([bytes[x * 8], bytes[x * 8 + 1]]);
        assert!(
            red(200) < 0x1400,
            "Full must preserve the black source pixel"
        );
        assert!(
            red(201) > 0x3bf0,
            "Full must preserve the white source pixel"
        );
    }

    #[test]
    fn render_frame_at_is_deterministic() {
        std::thread::Builder::new()
            .name("render_frame_at_is_deterministic".into())
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("tokio runtime");
                rt.block_on(render_frame_at_is_deterministic_inner());
            })
            .expect("spawn large-stack test thread")
            .join()
            .expect("render_frame_at_is_deterministic panicked");
    }

    async fn render_frame_at_is_deterministic_inner() {
        let state = test_state();
        // Small format keeps the raw payload cheap (320*180*8 ≈ 460 KB).
        let r = call(
            &state,
            "create_sequence",
            json!({
                "name": "Render Seq", "frame_rate": {"num": 30, "den": 1},
                "formats": [{"name": "16:9", "width": 320, "height": 180}]
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "create_sequence: {r:?}");
        let seq_id = data(&r)["sequence_id"].clone();
        let track_id = create_track(&state, &seq_id, "video").await;
        insert_solid_clip(&state, &track_id, 0, TICKS_PER_SECOND * 2).await;
        if !engine_available(&state).await {
            return;
        }

        let args = json!({
            "sequence_id": seq_id,
            "at_seconds": 0.5,
            "quality": "full",
            "output_format": "raw_rgba16f",
        });
        let r1 = call(&state, "render_frame_at", args.clone()).await;
        assert_ne!(r1.is_error, Some(true), "render 1: {r1:?}");
        let r2 = call(&state, "render_frame_at", args).await;
        assert_ne!(r2.is_error, Some(true), "render 2: {r2:?}");
        let (d1, d2) = (data(&r1), data(&r2));
        assert_eq!(d1["width"], 320);
        assert_eq!(d1["height"], 180);
        // Exact frame tick: 0.5s @30fps = frame 15.
        assert_eq!(d1["tick"], 15 * (TICKS_PER_SECOND / 30));
        let b1 = d1["data_base64"].as_str().expect("raw payload");
        let b2 = d2["data_base64"].as_str().expect("raw payload");
        assert!(!b1.is_empty());
        assert_eq!(b1, b2, "raw_rgba16f output must be byte-deterministic");

        // Pixel probe: the solid #00ff00 clip must read back green
        // (linear premultiplied f16, interleaved RGBA little-endian).
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b1)
            .expect("base64");
        assert_eq!(bytes.len(), 320 * 180 * 8);
        let px = ((180 / 2) * 320 + 320 / 2) * 8;
        let f16 = |o: usize| {
            let bits = u16::from_le_bytes([bytes[o], bytes[o + 1]]);
            // Positive-normal decode is enough for a 0..1 color probe.
            let exp = ((bits >> 10) & 0x1f) as i32;
            let frac = (bits & 0x3ff) as f32;
            if exp == 0 {
                frac * 2f32.powi(-24)
            } else {
                (1.0 + frac / 1024.0) * 2f32.powi(exp - 15)
            }
        };
        let (r, g, a) = (f16(px), f16(px + 2), f16(px + 6));
        assert!(r < 0.05, "red channel should be ~0, got {r}");
        assert!(g > 0.9, "green channel should be ~1, got {g}");
        assert!(a > 0.99, "alpha should be 1, got {a}");

        // PNG + scale smoke: returns an image content item at scaled dims.
        let r = call(
            &state,
            "render_frame_at",
            json!({
                "sequence_id": seq_id,
                "at_ticks": 0,
                "quality": "preview",
                "scale": 0.25,
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "png render: {r:?}");
        assert!(
            r.content
                .iter()
                .any(|c| matches!(c, ContentItem::Image { .. })),
            "png output must include an image content item"
        );
        // The data payload sits after the image item here (text, image, json).
        let d = match r.content.last() {
            Some(ContentItem::Text { text }) => serde_json::from_str::<Value>(text).unwrap(),
            other => panic!("expected trailing json payload, got {other:?}"),
        };
        assert_eq!(d["width"], 80);
        assert_eq!(d["height"], 45);
    }

    /// Export E2E (GPU + ffmpeg): a 1-second solid-color sequence through
    /// `export_sequence` → poll `get_job_status` to done → ffprobe the output
    /// (dims + duration). Skips without a GPU adapter or ffmpeg toolchain.
    #[tokio::test]
    async fn export_sequence_e2e_solid_color() {
        let state = test_state();
        let r = call(
            &state,
            "create_sequence",
            json!({
                "name": "Export Seq", "frame_rate": {"num": 30, "den": 1},
                "formats": [{"name": "1:1", "width": 128, "height": 128}]
            }),
        )
        .await;
        let seq_id = data(&r)["sequence_id"].clone();
        let track_id = create_track(&state, &seq_id, "video").await;
        insert_solid_clip(&state, &track_id, 0, TICKS_PER_SECOND).await;
        if !engine_available(&state).await {
            return;
        }
        let Some(tools) = photonic_video::media::ffmpeg_locate::locate_for_test() else {
            eprintln!("ffmpeg/ffprobe not found — skipping export E2E test");
            return;
        };

        let out = std::env::temp_dir().join(format!(
            "photonic-mcp-export-e2e-{}.mp4",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&out);
        let r = call(
            &state,
            "export_sequence",
            json!({
                "sequence_id": seq_id,
                "out_path": out.to_string_lossy(),
                "preset": "Web H.264",
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "export_sequence: {r:?}");
        let d = data(&r);
        assert_eq!(d["total_frames"], 30);
        let job_id = d["job_id"].clone();

        // Poll to terminal state (cold engine spin-up + encode ≪ 120 s).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let final_state = loop {
            let r = call(&state, "get_job_status", json!({ "job_id": job_id })).await;
            assert_ne!(r.is_error, Some(true), "get_job_status: {r:?}");
            let d = data(&r);
            let s = d["status"]["state"].as_str().unwrap().to_string();
            if s != "queued" && s != "running" {
                break d;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "export did not finish in time (last: {d})"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        assert_eq!(
            final_state["status"]["state"], "done",
            "export failed: {final_state}"
        );
        assert!(out.exists(), "output file missing: {}", out.display());

        // ffprobe the result: 128x128, ~1 s of video.
        let probe = std::process::Command::new(&tools.ffprobe)
            .args([
                "-v",
                "error",
                "-print_format",
                "json",
                "-show_streams",
                "-show_format",
            ])
            .arg(&out)
            .output()
            .expect("run ffprobe");
        assert!(probe.status.success());
        let meta: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
        let stream = meta["streams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["codec_type"] == "video")
            .expect("video stream");
        assert_eq!(stream["width"], 128);
        assert_eq!(stream["height"], 128);
        let duration: f64 = meta["format"]["duration"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            (duration - 1.0).abs() < 0.2,
            "expected ~1s of video, got {duration}s"
        );
        let _ = std::fs::remove_file(&out);
    }

    /// `remove_proxy` detaches the ProxyRef and deletes the cache file, and
    /// `proxy_status` (via `list_media`) reflects that reality — ready → null.
    /// No ffmpeg needed: proxy *generation* is covered by the engine
    /// integration test in `photonic-video::media::proxy`.
    #[tokio::test]
    async fn remove_proxy_detaches_and_reflects_reality() {
        let state = test_state();
        let src = std::env::temp_dir().join(format!(
            "photonic_mcp_proxy_src_{}.mp4",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&src, b"src bytes").unwrap();
        let proxy_file =
            std::env::temp_dir().join(format!("photonic_mcp_{}.proxy.mp4", uuid::Uuid::new_v4()));
        std::fs::write(&proxy_file, b"proxy bytes").unwrap();

        let r = call(
            &state,
            "import_media",
            json!({ "paths": [src.to_string_lossy()] }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "import_media: {r:?}");
        let asset_id = data(&r)["assets"][0]["asset_id"].clone();

        // Attach a Ready proxy directly (engine-managed cache field, like probe).
        {
            let mut doc = state.document.lock().await;
            let a = doc
                .timeline
                .as_mut()
                .unwrap()
                .media
                .assets
                .values_mut()
                .next()
                .unwrap();
            a.proxy = Some(photonic_core::timeline::ProxyRef::ready_generated(
                proxy_file.clone(),
            ));
        }

        // proxy_status reflects reality: ready.
        let r = call(&state, "list_media", json!({})).await;
        let assets = data(&r)["assets"].as_array().cloned().unwrap_or_default();
        assert_eq!(assets[0]["proxy_status"], json!("ready"));

        // remove_proxy detaches the ref and deletes the file.
        let r = call(&state, "remove_proxy", json!({ "asset_ids": [asset_id] })).await;
        assert_ne!(r.is_error, Some(true), "remove_proxy: {r:?}");
        assert_eq!(data(&r)["files_deleted"], json!(1));
        assert!(!proxy_file.exists(), "proxy file should be deleted");

        // proxy_status now null.
        let r = call(&state, "list_media", json!({})).await;
        let assets = data(&r)["assets"].as_array().cloned().unwrap_or_default();
        assert!(
            assets[0]["proxy_status"].is_null(),
            "proxy_status should be null after removal"
        );

        let _ = std::fs::remove_file(&src);
    }

    // ═══ P4+ slice tests (captions / tts / grade / graph / audio / titles) ═══

    async fn poll_job(state: &AppState, job_id: &Value) -> Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let r = call(state, "get_job_status", json!({ "job_id": job_id })).await;
            let d = data(&r);
            let s = d["status"]["state"].as_str().unwrap_or("?").to_string();
            if s != "queued" && s != "running" {
                return d;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "job did not finish: {d}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    // ── Captions family E2E (10 §9.1) ────────────────────────────────────────
    #[tokio::test]
    async fn family_captions() {
        let state = test_state();
        let (seq_id, _) = create_seq_and_track(&state, "video").await;

        let r = call(
            &state,
            "add_caption_track",
            json!({ "sequence_id": seq_id, "name": "English" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "add_caption_track: {r:?}");
        let track_id = data(&r)["track_id"].clone();

        // Create a cue from plain text (words distributed proportionally).
        let r = call(
            &state,
            "set_caption_cue",
            json!({ "track_id": track_id, "start_ticks": 0, "end_ticks": 1000, "text": "hello world" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_caption_cue create: {r:?}");
        let cue_id = data(&r)["cue_id"].clone();

        let r = call(&state, "get_caption_track", json!({ "track_id": track_id })).await;
        assert_ne!(r.is_error, Some(true), "get_caption_track: {r:?}");
        let cues = data(&r)["track"]["cues"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0]["words"].as_array().unwrap().len(), 2);

        // Edit a word's text.
        let r = call(
            &state,
            "set_caption_word",
            json!({ "cue_id": cue_id, "word_index": 1, "text": "there" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_caption_word: {r:?}");

        // Track-scope style.
        let r = call(
            &state,
            "set_caption_style",
            json!({ "track_id": track_id, "style": { "font_size": 60.0, "fill": "#ffcc00" } }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_caption_style: {r:?}");

        // Split then merge.
        let r = call(
            &state,
            "split_caption_cue",
            json!({ "cue_id": cue_id, "at_ticks": 500 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "split_caption_cue: {r:?}");
        let new_cue_id = data(&r)["new_cue_id"].clone();
        let r = call(&state, "get_caption_track", json!({ "track_id": track_id })).await;
        assert_eq!(data(&r)["track"]["cues"].as_array().unwrap().len(), 2);

        let r = call(
            &state,
            "merge_caption_cues",
            json!({ "cue_id_a": cue_id, "cue_id_b": new_cue_id }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "merge_caption_cues: {r:?}");

        // Export → import round trip through SRT.
        let srt =
            std::env::temp_dir().join(format!("photonic-mcp-cap-{}.srt", uuid::Uuid::new_v4()));
        let r = call(
            &state,
            "export_captions",
            json!({ "track_id": track_id, "path": srt.to_string_lossy() }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "export_captions: {r:?}");
        assert!(srt.exists());

        let r = call(
            &state,
            "add_caption_track",
            json!({ "sequence_id": seq_id, "name": "Imported" }),
        )
        .await;
        let track2 = data(&r)["track_id"].clone();
        let r = call(
            &state,
            "import_captions",
            json!({ "track_id": track2, "path": srt.to_string_lossy() }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "import_captions: {r:?}");
        assert!(data(&r)["cues_imported"].as_u64().unwrap() >= 1);

        let r = call(
            &state,
            "remove_caption_track",
            json!({ "track_id": track2 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "remove_caption_track: {r:?}");

        let _ = std::fs::remove_file(&srt);
    }

    // ── auto_caption via the MockTranscriptionProvider (10 §9.1) ──────────────
    #[tokio::test]
    async fn auto_caption_mock_job() {
        let state = test_state();
        let (seq_id, track_id) = create_seq_and_track(&state, "video").await;
        insert_solid_clip(&state, &track_id, 0, TICKS_PER_SECOND * 4).await;

        let r = call(
            &state,
            "auto_caption",
            json!({
                "sequence_id": seq_id, "provider": "mock",
                "mock_transcript": "Hello there. This is a test transcript.",
                "name": "Auto"
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "auto_caption: {r:?}");
        let job_id = data(&r)["job_id"].clone();
        let cap_track = data(&r)["track_id"].clone();

        let d = poll_job(&state, &job_id).await;
        assert_eq!(d["status"]["state"], "done", "auto_caption failed: {d}");
        assert!(d["status"]["result"]["cue_count"].as_u64().unwrap() >= 1);

        let r = call(
            &state,
            "get_caption_track",
            json!({ "track_id": cap_track }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "get_caption_track: {r:?}");
        let cues = data(&r)["track"]["cues"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(!cues.is_empty(), "expected transcribed cues");
    }

    // ── generate_voiceover via the MockTtsProvider (10 §9.1, §9.4) ────────────
    #[tokio::test]
    async fn generate_voiceover_mock_job() {
        let state = test_state();
        let (_, track_id) = create_seq_and_track(&state, "audio").await;

        let r = call(
            &state,
            "generate_voiceover",
            json!({
                "text": "Welcome to the show", "track_id": track_id,
                "start_ticks": 0, "provider": "mock", "voice": "mock-voice"
            }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "generate_voiceover: {r:?}");
        let job_id = data(&r)["job_id"].clone();

        let d = poll_job(&state, &job_id).await;
        assert_eq!(d["status"]["state"], "done", "voiceover failed: {d}");
        assert!(!d["status"]["result"]["clip_id"].is_null());
        assert!(d["status"]["result"]["duration_ticks"].as_i64().unwrap() > 0);

        let r = call(&state, "list_clips", json!({ "track_id": track_id })).await;
        assert_eq!(
            data(&r)["clips"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .len(),
            1,
            "voiceover clip should be placed"
        );
    }

    // ── Grade family E2E (10 §9.1) ───────────────────────────────────────────
    #[tokio::test]
    async fn family_grade() {
        let state = test_state();
        let (seq_id, track_id) = create_seq_and_track(&state, "video").await;
        let clip_a = insert_solid_clip(&state, &track_id, 0, 1000).await;
        let clip_b = insert_solid_clip(&state, &track_id, 1000, 1000).await;

        // Set a (bypassed) grade, then confirm it round-trips.
        let r = call(
            &state,
            "set_grade",
            json!({ "clip_id": clip_a, "grade": { "ops": [], "bypass": true } }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_grade: {r:?}");
        let r = call(&state, "get_clip", json!({ "clip_id": clip_a })).await;
        assert_eq!(data(&r)["clip"]["grade"]["bypass"], json!(true));

        // Save → list → apply a preset.
        let r = call(
            &state,
            "grade_preset",
            json!({ "op": "save", "clip_id": clip_a, "name": "photonic_mcp_test_preset" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "grade_preset save: {r:?}");
        let r = call(&state, "grade_preset", json!({ "op": "list" })).await;
        assert!(data(&r)["presets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == "photonic_mcp_test_preset"));
        let r = call(
            &state,
            "grade_preset",
            json!({ "op": "apply", "clip_id": clip_b, "name": "photonic_mcp_test_preset" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "grade_preset apply: {r:?}");

        // copy_grade onto b (again) — one batch.
        let r = call(
            &state,
            "copy_grade",
            json!({ "source_clip_id": clip_a, "target_clip_ids": [clip_b] }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "copy_grade: {r:?}");

        // apply_lut: a missing file is AssetOffline; null removes cleanly.
        let r = call(
            &state,
            "apply_lut",
            json!({ "clip_id": clip_a, "lut_path": "/no/such/lut.cube" }),
        )
        .await;
        assert_eq!(data(&r)["error_code"], json!("AssetOffline"));
        let r = call(&state, "apply_lut", json!({ "clip_id": clip_a })).await;
        assert_ne!(r.is_error, Some(true), "apply_lut remove: {r:?}");

        // Clear the grade.
        let r = call(
            &state,
            "set_grade",
            json!({ "clip_id": clip_a, "grade": null }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_grade clear: {r:?}");
        let r = call(&state, "get_clip", json!({ "clip_id": clip_a })).await;
        assert!(data(&r)["clip"]["grade"].is_null());
        let _ = seq_id;
    }

    // ── Node-graph family E2E (10 §9.1) ──────────────────────────────────────
    #[tokio::test]
    async fn family_graph() {
        let state = test_state();
        let (_, track_id) = create_seq_and_track(&state, "video").await;
        let clip_id = insert_solid_clip(&state, &track_id, 0, 1000).await;

        let r = call(
            &state,
            "create_clip_composition",
            json!({ "clip_id": clip_id }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "create_clip_composition: {r:?}");
        let graph_id = data(&r)["graph_id"].clone();

        // Read the seeded ClipIn / Output ids.
        let r = call(&state, "get_graph", json!({ "graph_id": graph_id })).await;
        assert_ne!(r.is_error, Some(true), "get_graph: {r:?}");
        let nodes = data(&r)["graph"]["nodes"].clone();
        let node_by_op = |op: &str| -> Value {
            nodes
                .as_object()
                .unwrap()
                .iter()
                .find(|(_, n)| n["op"]["op"] == op)
                .map(|(id, _)| Value::String(id.clone()))
                .unwrap_or(Value::Null)
        };
        let clip_in = node_by_op("clip_in");
        let output = node_by_op("output");
        assert!(!clip_in.is_null() && !output.is_null());
        assert_eq!(data(&r)["compiles"], json!(true));

        // Add a blur node and connect ClipIn -> blur (legal, acyclic).
        let r = call(
            &state,
            "add_graph_node",
            json!({ "graph_id": graph_id, "op": { "op": "blur" }, "pos": [120.0, 40.0] }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "add_graph_node: {r:?}");
        let blur = data(&r)["node_id"].clone();

        let r = call(
            &state,
            "add_graph_edge",
            json!({ "graph_id": graph_id, "from": { "node_id": clip_in }, "to": { "node_id": blur } }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "add_graph_edge: {r:?}");

        // A back-edge Output -> ClipIn is a cycle → CycleDetected.
        let r = call(
            &state,
            "add_graph_edge",
            json!({ "graph_id": graph_id, "from": { "node_id": output }, "to": { "node_id": clip_in } }),
        )
        .await;
        assert_eq!(data(&r)["error_code"], json!("CycleDetected"));

        // Param edit.
        let r = call(
            &state,
            "set_graph_node_param",
            json!({ "graph_id": graph_id, "node_id": blur, "path": "params.radius", "value": { "t": "float", "v": 8.0 } }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_graph_node_param: {r:?}");

        // Remove the blur node.
        let r = call(
            &state,
            "remove_graph_node",
            json!({ "graph_id": graph_id, "node_id": blur }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "remove_graph_node: {r:?}");

        // Fresh project graph.
        let r = call(&state, "set_project_graph", json!({})).await;
        assert_ne!(r.is_error, Some(true), "set_project_graph: {r:?}");
        assert!(!data(&r)["graph_id"].is_null());
    }

    // ── Audio family E2E (10 §9.1) ───────────────────────────────────────────
    #[tokio::test]
    async fn family_audio() {
        let state = test_state();
        let (seq_id, track_id) = create_seq_and_track(&state, "audio").await;
        let clip_id = insert_solid_clip(&state, &track_id, 0, 2000).await;

        let r = call(
            &state,
            "set_clip_audio",
            json!({ "clip_id": clip_id, "gain_db": -3.0, "fade_in_ticks": 200, "fade_out_ticks": 200 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_clip_audio: {r:?}");

        let r = call(
            &state,
            "set_track_audio",
            json!({ "track_id": track_id, "volume_db": -6.0, "pan": 0.25, "muted": true }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_track_audio: {r:?}");

        let r = call(
            &state,
            "audio_fx",
            json!({ "track_id": track_id, "op": "add", "kind": "compressor" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "audio_fx add: {r:?}");
        let r = call(
            &state,
            "audio_fx",
            json!({ "track_id": track_id, "op": "remove", "index": 0 }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "audio_fx remove: {r:?}");

        let r = call(
            &state,
            "set_master_bus",
            json!({ "sequence_id": seq_id, "volume_db": -1.0, "loudness": "streaming" }),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "set_master_bus: {r:?}");

        // Meters are NotSupportedV1 in the headless bridge.
        let r = call(&state, "get_audio_meters", json!({ "sequence_id": seq_id })).await;
        assert_eq!(data(&r)["error_code"], json!("NotSupportedV1"));

        // A generator clip has no media asset for a waveform.
        let r = call(&state, "get_waveform", json!({ "clip_id": clip_id })).await;
        assert_eq!(r.is_error, Some(true), "solid-color clip has no waveform");
    }

    // ── Title templates: empty catalog + NotSupportedV1 insert (05 §4b) ──────
    #[tokio::test]
    async fn title_templates_are_flagged_p6() {
        let state = test_state();
        let (_, track_id) = create_seq_and_track(&state, "video").await;

        let r = call(&state, "list_title_templates", json!({})).await;
        assert_ne!(r.is_error, Some(true), "list_title_templates: {r:?}");
        assert_eq!(
            data(&r)["templates"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .len(),
            0
        );

        let r = call(
            &state,
            "insert_title_template",
            json!({ "template": "lower_third", "track_id": track_id, "start_ticks": 0 }),
        )
        .await;
        assert_eq!(data(&r)["error_code"], json!("NotSupportedV1"));
    }
}
