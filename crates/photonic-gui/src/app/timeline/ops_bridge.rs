//! The intent → `timeline/ops.rs` → `CommandHistory` bridge (04 §2.3).
//!
//! This is the SOLE place in the timeline GUI that calls `ops::*` and pushes to
//! history. `interact.rs` decides *what the user intends*; every function here
//! turns that intent into a pure `ops::*` call producing a `TimelineCmd`, then
//! routes it through `CommandHistory`. No GUI code mutates `doc.timeline`
//! directly — the one sanctioned exception is [`set_track_height`] (04 §2.3
//! marks `height_px` a persisted-but-non-undoable UI field).
//!
//! Drag gestures commit ONE command on pointer release (via `execute_discrete`,
//! which never folds), so each gesture is exactly one undo step — the same
//! guarantee the coalesce anchor gives streamed edits (01 §10), reached here by
//! previewing a ghost during the drag and committing from the drag-start
//! snapshot on release. Multi-clip ops (roll/ripple/slide) have no `coalesce`
//! arm, so commit-on-release is the only correct choice for them and is applied
//! uniformly.

use photonic_core::document::Document;
use photonic_core::history::{Command, CommandHistory};
use photonic_core::timeline::{
    ops, AssetId, AssetKind, Clip, ClipId, ClipSource, ClipTiming, FrameRate, Marker, MarkerId,
    MediaAsset, Sequence, SequenceFormat, SequenceId, Tick, TimelineCmd, TimelineProject, Track,
    TrackId, TrackKind, TrackSettings, TICKS_PER_SECOND,
};

/// Push one timeline command as a single, non-folding undo step.
fn commit(history: &mut CommandHistory, doc: &mut Document, cmd: TimelineCmd) {
    history.execute_discrete(Command::Timeline(cmd), doc);
}

/// Push several timeline commands as ONE undo step (`Command::Batch`).
fn commit_batch(history: &mut CommandHistory, doc: &mut Document, cmds: Vec<TimelineCmd>) {
    if cmds.is_empty() {
        return;
    }
    let batch = cmds.into_iter().map(Command::Timeline).collect();
    history.execute_discrete(Command::Batch(batch), doc);
}

// ── Aspect ratio / sequence format ──────────────────────────────────────────

/// Switch the sequence to the aspect/frame named `name` (`w`×`h`): activate the
/// existing format if one already matches, otherwise add it and activate it —
/// one undoable step either way. This is the one-click "make this 9:16 / 1:1 /
/// 16:9" control behind the monitor's format bar (CAP-012).
/// CAP-019 GUI-arm surface (29 §3) — driven headlessly by the acceptance-story harness.
pub fn switch_to_aspect(
    history: &mut CommandHistory,
    doc: &mut Document,
    seq_id: SequenceId,
    name: &str,
    w: u32,
    h: u32,
) {
    let Some(project) = doc.timeline.as_ref() else {
        return;
    };
    let Some(seq) = project.sequences.get(&seq_id) else {
        return;
    };
    // Match an existing format by dimensions (name is cosmetic; the aspect is
    // what matters), so repeated clicks just re-activate rather than pile up.
    if let Some(idx) = seq
        .formats
        .iter()
        .position(|f| f.width == w && f.height == h)
    {
        if seq.active_format != idx {
            if let Ok(cmd) = ops::set_active_format(project, seq_id, idx) {
                commit(history, doc, cmd);
            }
        }
        return;
    }
    let new_idx = seq.formats.len();
    let add = ops::add_format(seq_id, SequenceFormat::new(name, w, h));
    // Add then activate as one undo step.
    let activate = TimelineCmd::SetActiveFormat {
        seq: seq_id,
        old: seq.active_format,
        new: new_idx,
    };
    commit_batch(history, doc, vec![add, activate]);
}

/// The built-in quick aspect presets shown on the monitor's format bar
/// (name, width, height). 1080-tall/wide family per 04 §4.1 / CAP-012.
pub(crate) const ASPECT_PRESETS: &[(&str, u32, u32)] = &[
    ("16:9", 1920, 1080),
    ("9:16", 1080, 1920),
    ("1:1", 1080, 1080),
    ("4:5", 1080, 1350),
    ("4:3", 1440, 1080),
    ("21:9", 2560, 1080),
];

// ── Sequence tabs (17 G-17) ─────────────────────────────────────────────────

/// Create a new empty sequence, pin it in `open_tabs`, and activate it (G-17 +).
pub fn create_sequence_tab(
    doc: &mut Document,
    history: &mut CommandHistory,
    frame_rate: FrameRate,
    width: u32,
    height: u32,
    open_tabs: &mut Vec<SequenceId>,
) {
    let Some(project) = doc.timeline.as_ref() else {
        return;
    };
    let n = project.sequences.len() + 1;
    let name = format!("Sequence {n}");
    let seq = Sequence::new(name, frame_rate, width, height);
    let id = seq.id;
    commit(history, doc, ops::add_sequence(seq));
    // Activate the new sequence.
    if let Some(project) = doc.timeline.as_ref() {
        let cmd = ops::set_active_sequence(project, Some(id));
        commit(history, doc, cmd);
    }
    if !open_tabs.contains(&id) {
        open_tabs.push(id);
    }
}

/// Duplicate a sequence into a new tab and activate the copy (G-17 context menu).
pub fn duplicate_sequence_tab(
    doc: &mut Document,
    history: &mut CommandHistory,
    id: SequenceId,
    open_tabs: &mut Vec<SequenceId>,
) {
    let Some(project) = doc.timeline.as_ref() else {
        return;
    };
    let Ok(cmd) = ops::duplicate_sequence(project, id) else {
        return;
    };
    // Peek the new id from the command payload before commit.
    let new_id = match &cmd {
        TimelineCmd::AddSequence { sequence, .. } => sequence.id,
        _ => return,
    };
    commit(history, doc, cmd);
    if let Some(project) = doc.timeline.as_ref() {
        let activate = ops::set_active_sequence(project, Some(new_id));
        commit(history, doc, activate);
    }
    if !open_tabs.contains(&new_id) {
        open_tabs.push(new_id);
    }
}

// ── Project / sequence / track lifecycle ────────────────────────────────────

/// Guarantee a project *and* at least one sequence exist, returning the active
/// sequence id. The first video-mode action creates them lazily and undoably
/// (04 §1.3): `CreateProject` (+ a default 1080p sequence if the project has
/// none) batched into one step.
/// CAP-019 GUI-arm surface (29 §3) — driven headlessly by the acceptance-story harness.
pub fn ensure_project_and_sequence(
    doc: &mut Document,
    history: &mut CommandHistory,
    frame_rate: FrameRate,
) -> Option<SequenceId> {
    // Already have an active sequence → nothing to do.
    if let Some(p) = doc.timeline.as_ref() {
        if let Some(active) = p.active_sequence {
            return Some(active);
        }
        // Project exists but no sequence: add one.
        let seq = Sequence::new("Sequence 1", frame_rate, 1920, 1080);
        let id = seq.id;
        commit(history, doc, ops::add_sequence(seq));
        // `AddSequence` makes it active when none was (see commands.rs).
        return doc
            .timeline
            .as_ref()
            .and_then(|p| p.active_sequence)
            .or(Some(id));
    }

    // No project at all: create project + first sequence in one undo step.
    let create = ops::create_project();
    let seq = Sequence::new("Sequence 1", frame_rate, 1920, 1080);
    let id = seq.id;
    commit_batch(history, doc, vec![create, ops::add_sequence(seq)]);
    doc.timeline
        .as_ref()
        .and_then(|p| p.active_sequence)
        .or(Some(id))
}

/// Append a new track of `kind` to the active sequence.
/// CAP-019 GUI-arm surface (29 §3) — driven headlessly by the acceptance-story harness.
pub fn add_track(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    kind: TrackKind,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    let n = p
        .sequences
        .get(&seq)
        .map(|s| s.tracks_for(kind).len())
        .unwrap_or(0);
    let prefix = match kind {
        TrackKind::Video => "V",
        TrackKind::Audio => "A",
        TrackKind::Text => "T",
    };
    let track = Track::new(kind, format!("{prefix}{}", n + 1));
    if let Ok(cmd) = ops::add_track(p, seq, track, None) {
        commit(history, doc, cmd);
    }
}

pub(crate) fn remove_track(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    if let Ok(cmd) = ops::remove_track(p, seq, track) {
        commit(history, doc, cmd);
    }
}

pub(crate) fn move_track(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    new_index: usize,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    if let Ok(cmds) = ops::reorder_track(p, seq, track, new_index) {
        commit_batch(history, doc, cmds);
    }
}

/// Change one field of a track's settings via `SetTrackProp` (04 §2.6).
fn set_track_settings(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    edit: impl FnOnce(&mut TrackSettings),
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    let Some(t) = p.sequences.get(&seq).and_then(|s| s.track(track)) else {
        return;
    };
    let mut new = TrackSettings::of(t);
    edit(&mut new);
    if let Ok(cmd) = ops::set_track_prop(p, seq, track, new) {
        commit(history, doc, cmd);
    }
}

pub(crate) fn rename_track(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    name: String,
) {
    set_track_settings(doc, history, seq, track, |s| s.name = name);
}

pub(crate) fn toggle_enabled(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
) {
    set_track_settings(doc, history, seq, track, |s| s.enabled = !s.enabled);
}

pub(crate) fn toggle_locked(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
) {
    set_track_settings(doc, history, seq, track, |s| s.locked = !s.locked);
}

/// The one sanctioned direct mutation (04 §2.3): `height_px` is a persisted-but-
/// non-undoable UI field, so a header height-drag writes it in place rather than
/// producing a command.
pub(crate) fn set_track_height(doc: &mut Document, seq: SequenceId, track: TrackId, height: f32) {
    if let Some(p) = doc.timeline.as_mut() {
        if let Some(s) = p.sequences.get_mut(&seq) {
            if let Some(t) = s.track_mut(track) {
                t.height_px = height.clamp(28.0, 240.0);
            }
        }
    }
}

// ── Linked clip expansion (14 §M-2, gap #8's UI half) ───────────────────────
//
// Core's `link_group` (`photonic_core::timeline::clip::LinkGroupId`) marks
// clips that should move as a unit (e.g. an A/V pair split from one media
// import). The functions below are the ONLY place the GUI expands a
// single-clip edit intent into "also edit every linked partner" — always as
// extra commands appended to the primary command, committed through
// `commit_group` so the whole thing is exactly one undo step.
//
// Scope for this story is deliberately MOVE (+ cross-track move) and plain
// delete (`remove_clip`, a "lift" — leaves a gap):
// - Trim/ripple-trim/roll intentionally do NOT propagate — this matches the
//   reference-NLE convention 14 §M-2 already calls out ("Pr: trim is
//   independent, move is linked"), so a linked partner's own in/out points
//   stay put when the other half gets trimmed.
// - `ripple_delete` intentionally does NOT expand to partners either:
//   ripple-closing the gap on a partner's own track would require shifting
//   THAT track too, which is the not-yet-built Sync Lock concept (14 §M-9).
//   Ripple-deleting a linked clip today only removes+ripples its own track;
//   a linked partner is left in place until §M-9 lands.
// - No "detach audio from video" UI path exists yet (grep confirms) to seed
//   a link group from an edit — `ops::link_clips` is there on the core side
//   for whenever that story lands; until then link groups can only be
//   established directly against `TimelineProject` (e.g. by a test, or by
//   import-time wiring outside this story's territory).

/// Every OTHER clip in `clip`'s link group within `seq`, paired with the
/// track that currently owns it. Empty when `clip` isn't linked or has no
/// partners. Reuses `ops::clips_in_link_group` (a whole-project scan) rather
/// than re-deriving group membership, then resolves each id back to a track
/// within THIS sequence — link groups are a within-sequence concept, so a
/// member id that doesn't resolve to a track here is simply skipped (can't
/// happen in practice).
fn link_partners(
    p: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    clip: photonic_core::timeline::ClipId,
) -> Vec<(TrackId, photonic_core::timeline::ClipId)> {
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
/// ripple) to every linked partner of `clip` within `seq`, so deleting one
/// half of a linked pair takes the other half with it. See the module note
/// above for why `ripple_delete` deliberately does NOT use this.
pub(crate) fn expand_link_group_delete(
    p: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    clip: photonic_core::timeline::ClipId,
) -> Vec<TimelineCmd> {
    link_partners(p, seq, track, clip)
        .into_iter()
        .filter_map(|(ptrack, pclip)| ops::remove_clip(p, seq, ptrack, pclip).ok())
        .collect()
}

/// Commit `cmds` as a single command when there's exactly one — preserving
/// the exact undo shape a caller with no linked partners already relies on
/// (`Command::Timeline`, not a one-element `Batch`) — or as one
/// `Command::Batch` undo step when linked partners rode along.
fn commit_group(history: &mut CommandHistory, doc: &mut Document, mut cmds: Vec<TimelineCmd>) {
    match cmds.len() {
        0 => {}
        1 => commit(history, doc, cmds.pop().expect("len == 1 checked above")),
        _ => commit_batch(history, doc, cmds),
    }
}

// ── Clip edits ──────────────────────────────────────────────────────────────

/// CAP-019 GUI-arm surface (29 §3) — driven headlessly by the acceptance-story harness.
pub fn move_clip(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    clip: photonic_core::timeline::ClipId,
    new_start: Tick,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    let Ok(cmds) = ops::move_linked_clip(p, seq, track, clip, new_start, None) else {
        return;
    };
    commit_group(history, doc, cmds);
}

/// Shift every clip in `clips`, plus their link-group partners, by one shared
/// time `delta` and vertical `track_delta` as a single undo step (04 §2.6
/// multi-select, 210 §5).
///
/// Partners are folded into one moving set rather than expanded per clip the
/// way [`move_clip`] does it. Two reasons: a partner that is *also* selected
/// must not be moved twice, and `ops::move_clips` needs the complete set to
/// tell an internal collision (a clip the move is vacating) from a real one.
///
/// `track_delta` is a same-kind lane-index offset applied **per kind**: kinds
/// that cannot take the step (typical: a single audio track when an A/V link
/// partner rides along with a video vertical drag) stay on their own track and
/// receive time only — matching [`ops::move_linked_clip`] / single-clip
/// cross-track. Group move remains behind K-A5.
/// CAP-019 GUI-arm surface (29 §3) — driven headlessly by the acceptance-story harness.
pub fn move_clips(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    clips: &[(TrackId, photonic_core::timeline::ClipId)],
    delta: Tick,
    track_delta: i32,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    let Ok(moving) = ops::linked_moving_set(p, seq, clips) else {
        return;
    };
    let Ok(cmds) = ops::move_clips(p, seq, &moving, delta, track_delta) else {
        return;
    };
    commit_group(history, doc, cmds);
}

/// Move a clip to a different track via `ops::move_clip_to_track` — a single
/// lossless `MoveClip` command (its inverse restores the clip to its original
/// track + position), rather than composing remove+insert. A linked partner
/// rides along on ITS OWN track by the same delta (it is never itself
/// reassigned to `to_track`).
/// CAP-019 GUI-arm surface (29 §3) — driven headlessly by the acceptance-story harness.
pub fn move_clip_cross_track(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    from_track: TrackId,
    to_track: TrackId,
    clip: photonic_core::timeline::ClipId,
    new_start: Tick,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    let Ok(cmds) = ops::move_linked_clip(p, seq, from_track, clip, new_start, Some(to_track))
    else {
        return;
    };
    commit_group(history, doc, cmds);
}

/// CAP-019 GUI-arm surface (29 §3) — driven headlessly by the acceptance-story harness.
pub fn trim(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    clip: photonic_core::timeline::ClipId,
    new: ClipTiming,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    if let Ok(cmd) = ops::trim_clip(p, seq, track, clip, new) {
        commit(history, doc, cmd);
    }
}

/// K-A6 Edit Duration — apply a full position / duration / source_in write
/// (optionally rippling the end) as **one** undo step via
/// [`ops::edit_clip_timing`].
///
/// Returns `Ok(())` on success (including a no-op), or an error string for the
/// dialog surface when the pure planner rejects the request.
pub fn edit_duration(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    clip: photonic_core::timeline::ClipId,
    new: ClipTiming,
    ripple: bool,
) -> Result<(), String> {
    let Some(p) = doc.timeline.as_ref() else {
        return Err("No video project".into());
    };
    match ops::edit_clip_timing(p, seq, track, clip, new, ripple) {
        Ok(cmds) if cmds.is_empty() => Ok(()),
        Ok(cmds) => {
            commit_batch(history, doc, cmds);
            Ok(())
        }
        Err(ops::EditError::NonPositiveDuration) => {
            Err("Duration must be greater than zero".into())
        }
        Err(ops::EditError::Overlap) => {
            Err("Edit would overlap another clip or is out of range".into())
        }
        Err(e) => Err(format!("Edit rejected: {e:?}")),
    }
}

/// K-B14 Freeze frame — hold the source frame visible at clip-relative `at`
/// for the clip's whole duration. One undo step; no-op when already frozen
/// at the same frame.
pub fn freeze_frame(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    clip: photonic_core::timeline::ClipId,
    at: Tick,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    match ops::freeze_frame(p, seq, track, clip, at) {
        Ok(Some(cmd)) => commit(history, doc, cmd),
        Ok(None) => {}
        Err(_) => {}
    }
}

/// K-A3 Insert Space at `at` (sequence-relative) of `amount` ticks across
/// all unlocked tracks. One undo step.
pub fn insert_space(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    at: Tick,
    amount: Tick,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    if let Ok(cmds) = ops::insert_space(p, seq, at, amount) {
        commit_batch(history, doc, cmds);
    }
}

/// K-A3 Remove Space of `amount` at `at` across unlocked tracks.
pub fn remove_space(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    at: Tick,
    amount: Tick,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    if let Ok(cmds) = ops::remove_space(p, seq, at, amount) {
        commit_batch(history, doc, cmds);
    }
}

/// K-A3 pack unlocked tracks from `at` onward (remove all internal spaces).
pub fn remove_all_spaces_after(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    at: Tick,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    if let Ok(cmds) = ops::remove_all_spaces_after(p, seq, at) {
        commit_batch(history, doc, cmds);
    }
}

/// K-A3 delete every clip on unlocked tracks with `start >= at`.
pub fn remove_clips_after(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    at: Tick,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    if let Ok(cmds) = ops::remove_clips_after(p, seq, at) {
        commit_batch(history, doc, cmds);
    }
}

/// K-A8: create a subclip pool entry from a parent asset + source range.
/// Returns the new asset id when successful.
pub fn create_subclip(
    doc: &mut Document,
    history: &mut CommandHistory,
    parent: AssetId,
    range: (Tick, Tick),
    name: Option<String>,
) -> Option<AssetId> {
    let p = doc.timeline.as_ref()?;
    let (cmd, id) = ops::create_subclip(p, parent, range, name).ok()?;
    commit(history, doc, cmd);
    Some(id)
}

// ── Sync-locked track ripple propagation (14 §M-9) ──────────────────────────
//
// `Track::sync_lock` + its header toggle already exist (`tracks.rs`); this is
// the wiring that makes the toggle do something: when a ripple edit
// (`ripple_trim` / `ripple_delete`) shifts every clip at/after a point in
// time on the EDITED track, it must ALSO shift every clip at/after that same
// point on every OTHER track whose `sync_lock` is set — sync-locked tracks
// ripple together; unlocked tracks don't. The edited track itself always
// ripples regardless of its own `sync_lock` bit (that's just what a ripple
// edit *is*); `sync_lock` only governs whether *other* tracks tag along.
// An edit-locked track is skipped even if `sync_lock` is set — `locked`
// always wins, matching every other op's refusal to touch a locked track.

/// Ripple-trim (Shift+edge, 04 §2.4): end-edge trims ride the core
/// [`ops::ripple_trim`] batch (which already expands sync-locked siblings).
/// Start-edge (or non-end) updates fall back to `trim_clip` + a local
/// downstream `RippleEdit` with the shared core expand helper.
/// CAP-019 GUI-arm surface (29 §3) — driven headlessly by the acceptance-story harness.
pub fn ripple_trim(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    clip: photonic_core::timeline::ClipId,
    new: ClipTiming,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    let Some(t) = p.sequences.get(&seq).and_then(|s| s.track(track)) else {
        return;
    };
    let Some(c) = t.clips.iter().find(|c| c.id == clip) else {
        return;
    };
    // Prefer the core end-edge path when the timeline start is unchanged (the
    // common Shift+edge drag). Core already fans out to sync-locked tracks.
    if new.start == c.start {
        let new_boundary = new.start + new.duration;
        if let Ok(cmds) = ops::ripple_trim(p, seq, track, clip, ops::ClipEdge::End, new_boundary) {
            commit_group(history, doc, cmds);
            return;
        }
    }
    let Ok(trim_cmd) = ops::trim_clip(p, seq, track, clip, new) else {
        return;
    };
    // Delta by which the clip's end moved: downstream clips shift by the same.
    let old_end = c.end();
    let new_end = new.start + new.duration;
    let delta = new_end - old_end;
    let mut changes = Vec::new();
    if delta.0 != 0 {
        for other in &t.clips {
            if other.id != clip && other.start >= old_end {
                let old = ClipTiming::of(other);
                let shifted = ClipTiming {
                    start: other.start + delta,
                    ..old
                };
                if shifted.start.0 >= 0 {
                    changes.push((other.id, old, shifted));
                }
            }
        }
    }
    let mut cmds = vec![trim_cmd];
    if !changes.is_empty() {
        cmds.push(TimelineCmd::RippleEdit {
            seq,
            track,
            changes,
        });
    }
    cmds.extend(ops::expand_sync_lock_ripple(p, seq, track, old_end, delta));
    commit_group(history, doc, cmds);
}

/// CAP-019 GUI-arm surface (29 §3) — driven headlessly by the acceptance-story harness.
pub fn roll(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    left: photonic_core::timeline::ClipId,
    right: photonic_core::timeline::ClipId,
    delta: Tick,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    if let Ok(cmd) = ops::roll_edit(p, seq, track, left, right, delta) {
        commit(history, doc, cmd);
    }
}

/// CAP-019 GUI-arm surface (29 §3) — driven headlessly by the acceptance-story harness.
pub fn slip(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    clip: photonic_core::timeline::ClipId,
    new_source_in: Tick,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    if let Ok(cmd) = ops::slip_clip(p, seq, track, clip, new_source_in) {
        commit(history, doc, cmd);
    }
}

/// CAP-019 GUI-arm surface (29 §3) — driven headlessly by the acceptance-story harness.
pub fn slide(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    clip: photonic_core::timeline::ClipId,
    delta: Tick,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    if let Ok(cmd) = ops::slide_clip(p, seq, track, clip, delta) {
        commit(history, doc, cmd);
    }
}

/// CAP-019 GUI-arm surface (29 §3) — driven headlessly by the acceptance-story harness.
/// Split a clip at `at`. Returns `true` when a command was committed.
pub fn split(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    clip: photonic_core::timeline::ClipId,
    at: Tick,
) -> bool {
    let Some(p) = doc.timeline.as_ref() else {
        return false;
    };
    if let Ok(cmd) = ops::split_clip(p, seq, track, clip, at) {
        commit(history, doc, cmd);
        true
    } else {
        false
    }
}

/// Remove a clip. A linked partner (14 §M-2) is removed in the SAME undo
/// step — deleting one half of a linked pair takes the other half with it,
/// matching the "linked A/V clips move [and delete] as a unit" behavior a
/// reference NLE gives a linked selection.
/// CAP-019 GUI-arm surface (29 §3) — driven headlessly by the acceptance-story harness.
pub fn remove_clip(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    clip: photonic_core::timeline::ClipId,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    let Ok(cmd) = ops::remove_clip(p, seq, track, clip) else {
        return;
    };
    let mut cmds = vec![cmd];
    cmds.extend(expand_link_group_delete(p, seq, track, clip));
    commit_group(history, doc, cmds);
}

/// Ripple-delete a clip: remove it and shift every downstream clip on its own
/// track left by its duration. Sync-locked siblings ride along inside the core
/// [`ops::ripple_delete`] batch (14 §M-9) — no GUI-side re-expand.
pub(crate) fn ripple_delete(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    clip: photonic_core::timeline::ClipId,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    let Ok(cmds) = ops::ripple_delete(p, seq, track, clip) else {
        return;
    };
    commit_batch(history, doc, cmds);
}

/// Enable/disable a clip via the universal `SetClipProp` op (04 §2.6).
pub(crate) fn set_clip_enabled(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    clip: photonic_core::timeline::ClipId,
    enabled: bool,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    let Some(existing) = p
        .sequences
        .get(&seq)
        .and_then(|s| s.track(track))
        .and_then(|t| t.clips.iter().find(|c| c.id == clip).cloned())
    else {
        return;
    };
    let mut new = existing;
    new.enabled = enabled;
    if let Ok(cmd) = ops::set_clip_prop(p, seq, track, new) {
        commit(history, doc, cmd);
    }
}

/// Set (or clear, with `None`) a clip's D-12 stabilization recipe (22 §6.5).
///
/// Takes only a `ClipId` and finds its owning sequence/track itself, because
/// the caller (a file-picker action) has no reason to know either. Returns
/// whether the document changed, so the caller can decide what to report.
///
/// Clearing restores the unstabilized source path without touching the motion
/// metadata or cached analysis — 22 §6.5 requires removal be reversible.
pub fn set_clip_stabilization(
    doc: &mut Document,
    history: &mut CommandHistory,
    clip: photonic_core::timeline::ClipId,
    spec: Option<photonic_core::timeline::StabilizationSpec>,
) -> bool {
    // Reject an invalid recipe before it can reach the document: 22 §6.6 wants
    // the impossible case reported, and a stored-but-invalid spec would fail
    // later at analysis time with far less context.
    if let Some(s) = &spec {
        if s.validate().is_err() {
            return false;
        }
    }
    let Some(p) = doc.timeline.as_ref() else {
        return false;
    };
    let found = p.sequences.iter().find_map(|(seq_id, s)| {
        s.tracks().find_map(|t| {
            t.clips
                .iter()
                .find(|c| c.id == clip)
                .map(|c| (*seq_id, t.id, c.clone()))
        })
    });
    let Some((seq, track, existing)) = found else {
        return false;
    };
    let mut new = existing;
    new.stabilization = spec;
    match ops::set_clip_prop(p, seq, track, new) {
        Ok(cmd) => {
            commit(history, doc, cmd);
            true
        }
        Err(_) => false,
    }
}

/// Set (or clear, with `None`) a clip's organizational color label
/// (14-nle-parity §M-1, gap #7's UI half) — the clip context menu's "Label"
/// submenu routes through here; the swatch palette itself lives in
/// `clips::CLIP_LABEL_SWATCHES` (a GUI concern, per `Clip::color_label`'s
/// doc comment).
pub(crate) fn set_clip_color_label(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    clip: photonic_core::timeline::ClipId,
    label: Option<u8>,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    if let Ok(cmd) = ops::set_clip_color_label(p, seq, track, clip, label) {
        commit(history, doc, cmd);
    }
}

/// **Replace With Clip** (G-5, Premiere, gap G5 residual): swap `clip`'s
/// SOURCE in place via `ops::replace_clip_source` — timeline `start`,
/// `duration`, effects, transitions and grade untouched (a `set_clip_prop`
/// whole-clip diff, one undo step). `new_source_in` optionally re-anchors the
/// trim in-point (e.g. the tick a Match-Frame arm captured); `None` keeps the
/// clip's existing `source_in`. The GUI trigger is the clip context menu's
/// "Replace with clip" entry in `mod.rs`.
pub(crate) fn replace_clip_source(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    clip: photonic_core::timeline::ClipId,
    new_source: ClipSource,
    new_source_in: Option<Tick>,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    if let Ok(cmd) = ops::replace_clip_source(p, seq, track, clip, new_source, new_source_in) {
        commit(history, doc, cmd);
    }
}

// ── Media pool → timeline (05 §2) ────────────────────────────────────────────

/// Which track kind can host a clip of this asset kind. `None` = not
/// insertable as a clip (LUTs attach to grades, not tracks).
pub(crate) fn track_kind_for_asset(kind: AssetKind) -> Option<TrackKind> {
    match kind {
        AssetKind::Audio => Some(TrackKind::Audio),
        AssetKind::Video | AssetKind::Image | AssetKind::VectorDoc => Some(TrackKind::Video),
        AssetKind::Lut3d => None,
    }
}

/// The `ClipSource` for laying `asset` down as a clip: `Vector` for vector
/// docs, `Asset` for everything else (video/audio/image). Shared by
/// `clip_for_asset` (media-pool drop/insert) and Replace With Clip's
/// media-pool-selection arming (`mod.rs::replace_source_candidate`).
pub(crate) fn clip_source_for_asset(asset: &MediaAsset) -> ClipSource {
    match asset.kind {
        AssetKind::VectorDoc => ClipSource::Vector { asset: asset.id },
        _ => ClipSource::Asset { asset: asset.id },
    }
}

/// Build the default clip for dropping `asset` at `start`: duration from the
/// probe (images / probe-less assets default to 5 s), `Vector` source for
/// vector docs, clip named after the asset.
pub(crate) fn clip_for_asset(asset: &MediaAsset, start: Tick) -> Clip {
    let duration = asset
        .probe
        .as_ref()
        .map(|p| p.duration)
        .filter(|d| d.0 > 0)
        .unwrap_or(Tick(5 * TICKS_PER_SECOND));
    let mut clip = Clip::new(clip_source_for_asset(asset), start, duration);
    clip.name = crate::panels::media_pool::asset_display_name(asset);
    clip
}

/// Insert `asset` as a new clip starting at `start` on `track` (the timeline
/// drag-drop path). Kind-checked against the track; a rejected insert
/// (overlap, wrong lane) is a silent no-op like other invalid gestures.
pub(crate) fn insert_asset_clip(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track_id: TrackId,
    asset_id: AssetId,
    start: Tick,
) -> bool {
    let Some(p) = doc.timeline.as_ref() else {
        return false;
    };
    let Some(asset) = p.media.assets.get(&asset_id) else {
        return false;
    };
    let wanted = track_kind_for_asset(asset.kind);
    let track_kind = p
        .sequences
        .get(&seq)
        .and_then(|s| s.track(track_id))
        .map(|t| t.kind);
    if wanted.is_none() || track_kind != wanted {
        return false;
    }
    let clip = clip_for_asset(asset, start);
    match ops::insert_clip(p, seq, track_id, clip) {
        Ok(cmd) => {
            commit(history, doc, cmd);
            true
        }
        Err(_) => false,
    }
}

/// Insert `asset` at the playhead on the first compatible track that accepts
/// it (double-click / context-menu path / proposal 213 auto-place).
pub fn insert_asset_at_first_fit(
    doc: &mut Document,
    history: &mut CommandHistory,
    asset_id: AssetId,
    at: Tick,
) -> bool {
    let Some(p) = doc.timeline.as_ref() else {
        return false;
    };
    let Some(seq_id) = p.active_sequence else {
        return false;
    };
    let Some(asset) = p.media.assets.get(&asset_id) else {
        return false;
    };
    let Some(wanted) = track_kind_for_asset(asset.kind) else {
        return false;
    };
    let Some(seq) = p.sequences.get(&seq_id) else {
        return false;
    };
    let tracks: Vec<TrackId> = seq.tracks_for(wanted).iter().map(|t| t.id).collect();
    for track in tracks {
        if insert_asset_clip(doc, history, seq_id, track, asset_id, at) {
            return true;
        }
    }
    // No existing track of the wanted kind had room at `at` (or none exists yet
    // — a fresh sequence can be trackless). Add a track and insert there so the
    // action always lands instead of silently doing nothing.
    add_track(doc, history, seq_id, wanted);
    let new_track = doc
        .timeline
        .as_ref()
        .and_then(|p| p.sequences.get(&seq_id))
        .and_then(|s| s.tracks_for(wanted).last().map(|t| t.id));
    match new_track {
        Some(track) => insert_asset_clip(doc, history, seq_id, track, asset_id, at),
        None => false,
    }
}

// ── Markers & work range ─────────────────────────────────────────────────────

/// Add a marker to a sequence at `at` (double-click on the ruler, 04 §2.6/13
/// §1.1). `pub` for UI-path integration tests (command `video.add_marker`).
pub fn add_marker(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    at: Tick,
    name: impl Into<String>,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    if let Ok(cmd) = ops::add_marker(p, seq, Marker::new(at, name)) {
        commit(history, doc, cmd);
    }
}

/// Proposal 210 / `video.add_bookmark`: marker in the Bookmarks category.
/// Seeds the category registry when empty. One undo batch.
pub fn add_bookmark(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    at: Tick,
    name: impl Into<String>,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    let Ok(cmds) = ops::add_bookmark(p, seq, at, name) else {
        return;
    };
    if cmds.is_empty() {
        return;
    }
    commit_group(history, doc, cmds);
}

pub(crate) fn remove_marker(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    marker: MarkerId,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    if let Ok(cmd) = ops::remove_marker(p, seq, marker) {
        commit(history, doc, cmd);
    }
}

/// Change one field of a marker via `SetMarker` — the marker analogue of
/// `set_track_settings`.
fn set_marker_field(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    marker: MarkerId,
    edit: impl FnOnce(&mut Marker),
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    let Some(existing) = p
        .sequences
        .get(&seq)
        .and_then(|s| s.markers.iter().find(|m| m.id == marker).cloned())
    else {
        return;
    };
    let mut new = existing;
    edit(&mut new);
    if let Ok(cmd) = ops::set_marker(p, seq, new) {
        commit(history, doc, cmd);
    }
}

/// Rename a marker (context-menu "Rename").
pub(crate) fn rename_marker(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    marker: MarkerId,
    name: String,
) {
    set_marker_field(doc, history, seq, marker, |m| m.name = name);
}

/// Retime a marker (drag-to-retime on the ruler; commit once on release, one
/// undo step per gesture — same discipline as clip drags).
pub(crate) fn retime_marker(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    marker: MarkerId,
    at: Tick,
) {
    set_marker_field(doc, history, seq, marker, |m| m.at = at);
}

/// Set (or clear, with `None`) a sequence's preview/export work range — the
/// monitor's I/O transport buttons (04 §3.2) route through here instead of
/// mutating `Sequence::work_range` directly.
pub(crate) fn set_work_range(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    new: Option<(Tick, Tick)>,
) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    if let Ok(cmd) = ops::set_work_range(p, seq, new) {
        commit(history, doc, cmd);
    }
}

// ── NLE parity round-2 (spec 17 G1): add-edit-all-tracks / close-gap / simplify ──
//
// Batch editing verbs that fan out the shipped split / ripple primitives across
// tracks, each committed as ONE undo step. The pure planning fns (`split_targets`,
// `close_gap_plan`, `close_all_gaps_changes`, `through_edit_runs`) are unit-tested
// below; the committing wrappers build the `TimelineCmd` batch from the ORIGINAL
// project snapshot (like `ripple_delete`) and rely on remove-before-extend
// ordering so every intermediate state stays invariant-valid (`Command::Batch`
// validates after each sub-command).

/// Every unlocked track's clip the playhead tick `at` is strictly inside
/// (`start < at < end`), in timeline order — the split-worthy targets for "Add
/// Edit to All Tracks". An edge/gap yields none.
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

/// **Add Edit to All Tracks** (G1, Premiere Ctrl+Shift+K): split every unlocked
/// track's clip under the playhead in one undo step. Returns the number split.
pub(crate) fn split_all_tracks(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    at: Tick,
) -> usize {
    let Some(p) = doc.timeline.as_ref() else {
        return 0;
    };
    let Some(s) = p.sequences.get(&seq) else {
        return 0;
    };
    let targets = split_targets(s, at);
    let mut cmds = Vec::new();
    for (track, clip) in targets {
        if let Ok(cmd) = ops::split_clip(p, seq, track, clip, at) {
            cmds.push(cmd);
        }
    }
    let n = cmds.len();
    commit_batch(history, doc, cmds);
    n
}

/// Plan closing the gap that contains `at` among a track's sorted `clips`:
/// `(first_shifted_start, gap_width)` — every clip with `start >=
/// first_shifted_start` shifts LEFT by `gap_width`. `None` when `at` is inside a
/// clip, in trailing empty space, or on a flush boundary (no gap). Pure G1 math.
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

/// The `RippleEdit` change list that closes the gap containing `at` on `track`
/// (shift the post-gap clips left by the gap width). Empty when there is no gap.
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

/// **Close Gap** (G1): close the single gap containing `at` on `track`, one undo
/// step. `true` if a gap was closed (a locked track or no gap is a no-op).
pub(crate) fn close_gap_at(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    at: Tick,
) -> bool {
    let Some(p) = doc.timeline.as_ref() else {
        return false;
    };
    let Some(t) = p.sequences.get(&seq).and_then(|s| s.track(track)) else {
        return false;
    };
    if t.locked {
        return false;
    }
    let changes = close_gap_changes(t, at);
    if changes.is_empty() {
        return false;
    }
    commit(
        history,
        doc,
        TimelineCmd::RippleEdit {
            seq,
            track,
            changes,
        },
    );
    true
}

/// Whether an unlocked track has a closeable gap containing `at` (gates the gap
/// context-menu entry in `mod.rs`).
pub(crate) fn gap_at(doc: &Document, seq: SequenceId, track: TrackId, at: Tick) -> bool {
    doc.timeline
        .as_ref()
        .and_then(|p| p.sequences.get(&seq))
        .and_then(|s| s.track(track))
        .filter(|t| !t.locked)
        .is_some_and(|t| close_gap_plan(&t.clips, at).is_some())
}

/// **Close Gap at Playhead across all tracks** (G1): each unlocked track closes
/// the gap the playhead `at` sits in, all in ONE undo step. Returns the count.
pub(crate) fn close_gaps_at_playhead(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    at: Tick,
) -> usize {
    let Some(p) = doc.timeline.as_ref() else {
        return 0;
    };
    let Some(s) = p.sequences.get(&seq) else {
        return 0;
    };
    let mut cmds = Vec::new();
    for t in s.tracks() {
        if t.locked {
            continue;
        }
        let changes = close_gap_changes(t, at);
        if !changes.is_empty() {
            cmds.push(TimelineCmd::RippleEdit {
                seq,
                track: t.id,
                changes,
            });
        }
    }
    let n = cmds.len();
    commit_batch(history, doc, cmds);
    n
}

/// Repack sorted `clips` left-contiguous, keeping the first clip's start (removes
/// every INTERNAL gap; a leading offset is preserved). Returns only the clips
/// whose start changes. Pure G1 math.
fn close_all_gaps_changes(clips: &[Clip]) -> Vec<(ClipId, ClipTiming, ClipTiming)> {
    let mut changes = Vec::new();
    let mut cursor: Option<Tick> = None;
    for c in clips {
        let new_start = cursor.unwrap_or(c.start);
        if new_start != c.start {
            let old = ClipTiming::of(c);
            changes.push((
                c.id,
                old,
                ClipTiming {
                    start: new_start,
                    ..old
                },
            ));
        }
        cursor = Some(new_start + c.duration);
    }
    changes
}

/// **Close All Gaps** (G1): repack every unlocked track left-contiguous, one undo
/// step. Returns the number of tracks changed.
pub(crate) fn close_all_gaps(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
) -> usize {
    let Some(p) = doc.timeline.as_ref() else {
        return 0;
    };
    let Some(s) = p.sequences.get(&seq) else {
        return 0;
    };
    let mut cmds = Vec::new();
    for t in s.tracks() {
        if t.locked {
            continue;
        }
        let changes = close_all_gaps_changes(&t.clips);
        if !changes.is_empty() {
            cmds.push(TimelineCmd::RippleEdit {
                seq,
                track: t.id,
                changes,
            });
        }
    }
    let n = cmds.len();
    commit_batch(history, doc, cmds);
    n
}

/// Whether `b` is a "through edit" continuation of `a` — the same source played
/// on with no change (what a split with no edit leaves): flush on the timeline,
/// source-continuous, and identical in every property that makes merging them
/// lossless.
fn is_through_edit(a: &Clip, b: &Clip) -> bool {
    a.end() == b.start
        && a.source == b.source
        && a.speed == b.speed
        && b.source_in == a.source_in + a.speed.source_delta(a.duration)
        && a.effects == b.effects
        && a.grade == b.grade
        && a.transform == b.transform
        && a.reframe == b.reframe
        && a.composition == b.composition
        && a.audio == b.audio
        && a.enabled == b.enabled
        && a.color_label == b.color_label
        && a.link_group == b.link_group
        && a.transition_out.is_none()
        && b.transition_in.is_none()
}

/// Maximal runs of consecutive through-edits on a track: `(survivor_index,
/// merged_indices, merged_duration)` per run of ≥ 2 clips. The survivor keeps its
/// start/source_in and grows to `merged_duration`; the merged clips are removed.
/// Pure G1 detection.
fn through_edit_runs(clips: &[Clip]) -> Vec<(usize, Vec<usize>, Tick)> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i + 1 < clips.len() {
        let mut j = i;
        let mut dur = clips[i].duration;
        let mut merged = Vec::new();
        while j + 1 < clips.len() && is_through_edit(&clips[j], &clips[j + 1]) {
            merged.push(j + 1);
            dur = dur + clips[j + 1].duration;
            j += 1;
        }
        if merged.is_empty() {
            i += 1;
        } else {
            runs.push((i, merged, dur));
            i = j + 1;
        }
    }
    runs
}

/// **Simplify Sequence** (G1): merge every through-edit run back into one clip
/// across all unlocked tracks, one undo step. Returns the number of clips merged
/// away. Removes-before-extend ordering keeps each intermediate state valid.
pub(crate) fn simplify_sequence(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
) -> usize {
    let Some(p) = doc.timeline.as_ref() else {
        return 0;
    };
    let Some(s) = p.sequences.get(&seq) else {
        return 0;
    };
    let mut cmds: Vec<TimelineCmd> = Vec::new();
    let mut merged_count = 0;
    for t in s.tracks() {
        if t.locked {
            continue;
        }
        for (survivor, merged, new_dur) in through_edit_runs(&t.clips) {
            // Remove the merged-away clips first (frees the span) …
            for &idx in merged.iter().rev() {
                cmds.push(TimelineCmd::RemoveClip {
                    seq,
                    track: t.id,
                    clip: Box::new(t.clips[idx].clone()),
                });
                merged_count += 1;
            }
            // … then extend the survivor over the freed span.
            let surv = &t.clips[survivor];
            cmds.push(TimelineCmd::TrimClip {
                seq,
                track: t.id,
                clip: surv.id,
                old: ClipTiming::of(surv),
                new: ClipTiming {
                    start: surv.start,
                    duration: new_dur,
                    source_in: surv.source_in,
                },
            });
        }
    }
    commit_batch(history, doc, cmds);
    merged_count
}

#[cfg(test)]
mod g1_tests {
    use super::*;
    use photonic_core::timeline::{Sequence, TimelineProject};

    fn adj(start: i64, dur: i64) -> Clip {
        Clip::new(ClipSource::Adjustment, Tick(start), Tick(dur))
    }

    #[test]
    fn split_targets_collects_interior_clips_and_skips_locked_and_edges() {
        let mut s = Sequence::new("s", FrameRate::new(30, 1), 1920, 1080);
        let mut v1 = Track::new(TrackKind::Video, "V1");
        let mut v2 = Track::new(TrackKind::Video, "V2");
        let mut a1 = Track::new(TrackKind::Audio, "A1");
        let c1 = adj(0, 100); // V1 [0,100)
        let c2 = adj(0, 100); // V2 [0,100)
        let ca = adj(0, 40); // A1 [0,40) — playhead 50 is OUTSIDE
        let (id1, id2) = (c1.id, c2.id);
        v1.clips.push(c1);
        v2.clips.push(c2);
        a1.clips.push(ca);
        let (v1id, v2id) = (v1.id, v2.id);
        s.video_tracks.push(v1);
        s.video_tracks.push(v2);
        s.audio_tracks.push(a1);

        // Playhead 50: interior to V1 & V2 clips, past the A1 clip → two targets.
        let mut got = split_targets(&s, Tick(50));
        got.sort();
        let mut want = vec![(v1id, id1), (v2id, id2)];
        want.sort();
        assert_eq!(got, want);

        // Locking V2 drops it from the target set.
        s.video_tracks[1].locked = true;
        assert_eq!(split_targets(&s, Tick(50)), vec![(v1id, id1)]);

        // On an edge nothing is strictly interior.
        assert!(split_targets(&s, Tick(0)).is_empty());
        assert!(split_targets(&s, Tick(100)).is_empty());
    }

    #[test]
    fn close_gap_plan_finds_leading_and_internal_gaps_only() {
        // Leading gap [0,50), clip [50,150), gap [150,200), clip [200,300).
        let clips = vec![adj(50, 100), adj(200, 100)];
        // In the leading gap: shift everything left by 50 (first clip → 0).
        assert_eq!(close_gap_plan(&clips, Tick(20)), Some((Tick(50), Tick(50))));
        // In the internal gap [150,200): first shifted is clip@200, gap 50.
        assert_eq!(
            close_gap_plan(&clips, Tick(175)),
            Some((Tick(200), Tick(50)))
        );
        // Inside a clip → no gap.
        assert_eq!(close_gap_plan(&clips, Tick(100)), None);
        // In trailing empty space → no gap.
        assert_eq!(close_gap_plan(&clips, Tick(400)), None);
        // Flush clips have no internal gap.
        let flush = vec![adj(0, 100), adj(100, 100)];
        assert_eq!(close_gap_plan(&flush, Tick(150)), None);
        assert_eq!(close_gap_plan(&flush, Tick(50)), None);
    }

    #[test]
    fn close_all_gaps_repacks_internal_gaps_keeping_first_start() {
        // [10,60), gap, [100,150), gap, [200,260)
        let clips = vec![adj(10, 50), adj(100, 50), adj(200, 60)];
        let changes = close_all_gaps_changes(&clips);
        let by_id: std::collections::HashMap<_, _> =
            changes.iter().map(|(id, _o, n)| (*id, n.start)).collect();
        // The first clip is unchanged (keeps start 10) → not emitted.
        assert!(!by_id.contains_key(&clips[0].id));
        // Second → 60 (10+50); third → 110 (60+50).
        assert_eq!(by_id[&clips[1].id], Tick(60));
        assert_eq!(by_id[&clips[2].id], Tick(110));
    }

    #[test]
    fn through_edit_runs_merges_a_continuous_split_chain() {
        let asset = AssetId::new();
        let mk = |start: i64, dur: i64, src_in: i64| {
            let mut c = Clip::new(ClipSource::Asset { asset }, Tick(start), Tick(dur));
            c.source_in = Tick(src_in);
            c
        };
        // One source split into three flush, source-continuous pieces.
        let clips = vec![mk(0, 40, 0), mk(40, 60, 40), mk(100, 50, 100)];
        let runs = through_edit_runs(&clips);
        assert_eq!(runs.len(), 1);
        let (survivor, merged, new_dur) = &runs[0];
        assert_eq!(*survivor, 0);
        assert_eq!(merged, &vec![1, 2]);
        assert_eq!(*new_dur, Tick(150));

        // A source discontinuity (source_in jump) breaks the run.
        let broken = vec![mk(0, 40, 0), mk(40, 60, 999)];
        assert!(through_edit_runs(&broken).is_empty());
        // A gap (not flush) breaks the run.
        let gapped = vec![mk(0, 40, 0), mk(60, 60, 40)];
        assert!(through_edit_runs(&gapped).is_empty());
    }

    fn doc_one_video(clips: &[(i64, i64)]) -> (Document, CommandHistory, SequenceId, TrackId) {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("s", FrameRate::new(30, 1), 1920, 1080);
        let mut v = Track::new(TrackKind::Video, "V1");
        for (s0, d) in clips {
            v.clips.push(adj(*s0, *d));
        }
        let vid = v.id;
        seq.video_tracks.push(v);
        let seq_id = project.insert_sequence(seq);
        project.active_sequence = Some(seq_id);
        let mut doc = Document::new("t", 1920.0, 1080.0);
        doc.timeline = Some(project);
        (doc, CommandHistory::new(64), seq_id, vid)
    }

    fn starts(doc: &Document, seq: SequenceId, track: TrackId) -> Vec<Tick> {
        doc.timeline.as_ref().unwrap().sequences[&seq]
            .track(track)
            .unwrap()
            .clips
            .iter()
            .map(|c| c.start)
            .collect()
    }

    #[test]
    fn close_gaps_at_playhead_closes_one_gap_and_undoes_in_one_step() {
        let (mut doc, mut h, seq, v) = doc_one_video(&[(0, 100), (200, 100)]);
        // Playhead in the gap [100,200): clip@200 → 100.
        close_gaps_at_playhead(&mut doc, &mut h, seq, Tick(150));
        assert_eq!(starts(&doc, seq, v), vec![Tick(0), Tick(100)]);
        h.undo(&mut doc);
        assert_eq!(starts(&doc, seq, v), vec![Tick(0), Tick(200)]);
    }

    #[test]
    fn split_all_tracks_splits_interior_clips_in_one_undo_step() {
        let (mut doc, mut h, seq, v) = doc_one_video(&[(0, 100)]);
        let n = split_all_tracks(&mut doc, &mut h, seq, Tick(50));
        assert_eq!(n, 1);
        assert_eq!(starts(&doc, seq, v), vec![Tick(0), Tick(50)]);
        h.undo(&mut doc);
        assert_eq!(starts(&doc, seq, v), vec![Tick(0)]);
    }

    #[test]
    fn simplify_merges_a_split_with_no_change_back_into_one_clip() {
        // Insert one clip, split it at 50 (a no-change through-edit), then simplify.
        let (mut doc, mut h, seq, v) = doc_one_video(&[]);
        let clip = adj(0, 100);
        let p = doc.timeline.as_ref().unwrap();
        let cmd = ops::insert_clip(p, seq, v, clip).unwrap();
        h.execute_discrete(Command::Timeline(cmd), &mut doc);
        let clip_id = doc.timeline.as_ref().unwrap().sequences[&seq]
            .track(v)
            .unwrap()
            .clips[0]
            .id;
        split(&mut doc, &mut h, seq, v, clip_id, Tick(50));
        assert_eq!(starts(&doc, seq, v), vec![Tick(0), Tick(50)]);
        let merged = simplify_sequence(&mut doc, &mut h, seq);
        assert_eq!(merged, 1);
        assert_eq!(starts(&doc, seq, v), vec![Tick(0)]);
        assert_eq!(
            doc.timeline.as_ref().unwrap().sequences[&seq]
                .track(v)
                .unwrap()
                .clips[0]
                .duration,
            Tick(100)
        );
    }
}

#[cfg(test)]
mod format_tests {
    use super::*;
    use photonic_core::timeline::{Sequence, TimelineProject};

    fn doc_with_seq() -> (Document, CommandHistory, SequenceId) {
        let mut project = TimelineProject::new();
        let seq = Sequence::new("s", FrameRate::new(30, 1), 1920, 1080); // starts 16:9 @ index 0
        let seq_id = project.insert_sequence(seq);
        project.active_sequence = Some(seq_id);
        let mut doc = Document::new("t", 1920.0, 1080.0);
        doc.timeline = Some(project);
        (doc, CommandHistory::new(64), seq_id)
    }

    fn active(doc: &Document, seq_id: SequenceId) -> (usize, u32, u32) {
        let s = &doc.timeline.as_ref().unwrap().sequences[&seq_id];
        let f = &s.formats[s.active_format];
        (s.active_format, f.width, f.height)
    }

    #[test]
    fn switch_adds_then_reactivates_without_duplicating() {
        let (mut doc, mut h, seq) = doc_with_seq();
        // New aspect → added at index 1 and activated.
        switch_to_aspect(&mut h, &mut doc, seq, "9:16", 1080, 1920);
        assert_eq!(active(&doc, seq), (1, 1080, 1920));
        assert_eq!(
            doc.timeline.as_ref().unwrap().sequences[&seq].formats.len(),
            2
        );

        // Back to the original aspect → re-activates index 0, no new format.
        switch_to_aspect(&mut h, &mut doc, seq, "16:9", 1920, 1080);
        assert_eq!(active(&doc, seq), (0, 1920, 1080));
        assert_eq!(
            doc.timeline.as_ref().unwrap().sequences[&seq].formats.len(),
            2
        );

        // Repeating the current aspect is a no-op (no duplicate, still index 0).
        switch_to_aspect(&mut h, &mut doc, seq, "16:9", 1920, 1080);
        assert_eq!(
            doc.timeline.as_ref().unwrap().sequences[&seq].formats.len(),
            2
        );

        // Undo the reactivation, then the add — state walks back cleanly.
        h.undo(&mut doc);
        assert_eq!(active(&doc, seq), (1, 1080, 1920));
        h.undo(&mut doc);
        assert_eq!(active(&doc, seq), (0, 1920, 1080));
        assert_eq!(
            doc.timeline.as_ref().unwrap().sequences[&seq].formats.len(),
            1
        );
    }
}

#[cfg(test)]
mod link_group_tests {
    use super::*;
    use photonic_core::timeline::{ClipId, Sequence, TimelineProject};

    /// A project with one active sequence and an empty V1 + A1 track pair,
    /// ready for clips to be inserted and linked.
    fn doc_with_two_tracks() -> (Document, CommandHistory, SequenceId, TrackId, TrackId) {
        let mut project = TimelineProject::new();
        let seq = Sequence::new("s", FrameRate::new(30, 1), 1920, 1080);
        let seq_id = project.insert_sequence(seq);
        project.active_sequence = Some(seq_id);
        let mut doc = Document::new("t", 1920.0, 1080.0);
        doc.timeline = Some(project);
        let mut history = CommandHistory::new(64);

        let v = Track::new(TrackKind::Video, "V1");
        let v_id = v.id;
        let a = Track::new(TrackKind::Audio, "A1");
        let a_id = a.id;
        let p = doc.timeline.as_ref().unwrap();
        let add_v = ops::add_track(p, seq_id, v, None).unwrap();
        history.execute_discrete(Command::Timeline(add_v), &mut doc);
        let p = doc.timeline.as_ref().unwrap();
        let add_a = ops::add_track(p, seq_id, a, None).unwrap();
        history.execute_discrete(Command::Timeline(add_a), &mut doc);

        (doc, history, seq_id, v_id, a_id)
    }

    /// Insert an `Adjustment`-source clip `[start, start+duration)` on
    /// `track`, returning its id.
    fn insert(
        doc: &mut Document,
        history: &mut CommandHistory,
        seq: SequenceId,
        track: TrackId,
        start: Tick,
        duration: Tick,
    ) -> ClipId {
        let clip = Clip::new(ClipSource::Adjustment, start, duration);
        let id = clip.id;
        let p = doc.timeline.as_ref().unwrap();
        let cmd = ops::insert_clip(p, seq, track, clip).unwrap();
        history.execute_discrete(Command::Timeline(cmd), doc);
        id
    }

    /// Link two clips into the same group (mirrors what a future "detach
    /// audio from video" path would establish at import/split time).
    fn link(
        doc: &mut Document,
        history: &mut CommandHistory,
        seq: SequenceId,
        track_a: TrackId,
        clip_a: ClipId,
        track_b: TrackId,
        clip_b: ClipId,
    ) {
        let p = doc.timeline.as_ref().unwrap();
        let cmds = ops::link_clips(p, seq, track_a, clip_a, track_b, clip_b).unwrap();
        let batch = cmds.into_iter().map(Command::Timeline).collect();
        history.execute_discrete(Command::Batch(batch), doc);
    }

    fn start_of(doc: &Document, seq: SequenceId, track: TrackId, clip: ClipId) -> Tick {
        doc.timeline.as_ref().unwrap().sequences[&seq]
            .track(track)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id == clip)
            .unwrap()
            .start
    }

    fn clip_exists(doc: &Document, seq: SequenceId, track: TrackId, clip: ClipId) -> bool {
        doc.timeline.as_ref().unwrap().sequences[&seq]
            .track(track)
            .unwrap()
            .clips
            .iter()
            .any(|c| c.id == clip)
    }

    #[test]
    fn move_carries_the_linked_partner_by_the_same_delta_in_one_undo_step() {
        let (mut doc, mut history, seq, v_track, a_track) = doc_with_two_tracks();
        let vclip = insert(&mut doc, &mut history, seq, v_track, Tick(0), Tick(100));
        let aclip = insert(&mut doc, &mut history, seq, a_track, Tick(0), Tick(100));
        link(&mut doc, &mut history, seq, v_track, vclip, a_track, aclip);

        // Move the VIDEO clip by +50 ticks — the linked audio clip on its
        // own track must ride along by the identical delta.
        move_clip(&mut doc, &mut history, seq, v_track, vclip, Tick(50));
        assert_eq!(start_of(&doc, seq, v_track, vclip), Tick(50));
        assert_eq!(start_of(&doc, seq, a_track, aclip), Tick(50));

        // A single undo restores BOTH — proves it landed as one undo step,
        // not two.
        history.undo(&mut doc);
        assert_eq!(start_of(&doc, seq, v_track, vclip), Tick(0));
        assert_eq!(start_of(&doc, seq, a_track, aclip), Tick(0));
    }

    #[test]
    fn move_of_an_unlinked_clip_leaves_other_clips_untouched() {
        let (mut doc, mut history, seq, v_track, a_track) = doc_with_two_tracks();
        let vclip = insert(&mut doc, &mut history, seq, v_track, Tick(0), Tick(100));
        let aclip = insert(&mut doc, &mut history, seq, a_track, Tick(0), Tick(100));
        // No link this time.

        move_clip(&mut doc, &mut history, seq, v_track, vclip, Tick(30));
        assert_eq!(start_of(&doc, seq, v_track, vclip), Tick(30));
        assert_eq!(start_of(&doc, seq, a_track, aclip), Tick(0));
    }

    #[test]
    fn cross_track_move_carries_the_linked_partner_on_its_own_track() {
        let (mut doc, mut history, seq, v_track, a_track) = doc_with_two_tracks();
        // A second video track so the primary clip has somewhere to move
        // cross-track to, without touching the audio track's own position.
        let p = doc.timeline.as_ref().unwrap();
        let v2 = Track::new(TrackKind::Video, "V2");
        let v2_id = v2.id;
        let add_v2 = ops::add_track(p, seq, v2, None).unwrap();
        history.execute_discrete(Command::Timeline(add_v2), &mut doc);

        let vclip = insert(&mut doc, &mut history, seq, v_track, Tick(0), Tick(100));
        let aclip = insert(&mut doc, &mut history, seq, a_track, Tick(0), Tick(100));
        link(&mut doc, &mut history, seq, v_track, vclip, a_track, aclip);

        move_clip_cross_track(&mut doc, &mut history, seq, v_track, v2_id, vclip, Tick(20));

        // Primary clip moved to V2 at the new start.
        assert_eq!(start_of(&doc, seq, v2_id, vclip), Tick(20));
        assert!(!clip_exists(&doc, seq, v_track, vclip));
        // Linked audio clip stayed on A1 (never reassigned) but shifted by
        // the same +20 delta.
        assert_eq!(start_of(&doc, seq, a_track, aclip), Tick(20));

        history.undo(&mut doc);
        assert_eq!(start_of(&doc, seq, v_track, vclip), Tick(0));
        assert_eq!(start_of(&doc, seq, a_track, aclip), Tick(0));
    }

    #[test]
    fn delete_carries_the_linked_partner_in_one_undo_step() {
        let (mut doc, mut history, seq, v_track, a_track) = doc_with_two_tracks();
        let vclip = insert(&mut doc, &mut history, seq, v_track, Tick(0), Tick(100));
        let aclip = insert(&mut doc, &mut history, seq, a_track, Tick(0), Tick(100));
        link(&mut doc, &mut history, seq, v_track, vclip, a_track, aclip);

        remove_clip(&mut doc, &mut history, seq, v_track, vclip);
        assert!(!clip_exists(&doc, seq, v_track, vclip));
        assert!(!clip_exists(&doc, seq, a_track, aclip));

        // One undo restores BOTH.
        history.undo(&mut doc);
        assert!(clip_exists(&doc, seq, v_track, vclip));
        assert!(clip_exists(&doc, seq, a_track, aclip));
    }

    #[test]
    fn delete_of_an_unlinked_clip_leaves_other_clips_untouched() {
        let (mut doc, mut history, seq, v_track, a_track) = doc_with_two_tracks();
        let vclip = insert(&mut doc, &mut history, seq, v_track, Tick(0), Tick(100));
        let aclip = insert(&mut doc, &mut history, seq, a_track, Tick(0), Tick(100));

        remove_clip(&mut doc, &mut history, seq, v_track, vclip);
        assert!(!clip_exists(&doc, seq, v_track, vclip));
        assert!(clip_exists(&doc, seq, a_track, aclip));
    }

    // ── Multi-select body move (04 §2.6, 210 §5) ───────────────────────────

    /// A contiguous run moves as a body in ONE undo step. Contiguity matters:
    /// per-clip `move_clip` calls would each refuse on the neighbour they are
    /// about to vacate.
    #[test]
    fn move_clips_shifts_a_contiguous_run_in_one_undo_step() {
        let (mut doc, mut history, seq, v_track, _) = doc_with_two_tracks();
        let a = insert(&mut doc, &mut history, seq, v_track, Tick(100), Tick(100));
        let b = insert(&mut doc, &mut history, seq, v_track, Tick(200), Tick(100));
        let c = insert(&mut doc, &mut history, seq, v_track, Tick(300), Tick(100));

        move_clips(
            &mut doc,
            &mut history,
            seq,
            &[(v_track, a), (v_track, b), (v_track, c)],
            Tick(50),
            0,
        );
        assert_eq!(start_of(&doc, seq, v_track, a), Tick(150));
        assert_eq!(start_of(&doc, seq, v_track, b), Tick(250));
        assert_eq!(start_of(&doc, seq, v_track, c), Tick(350));

        // One undo restores all three — the whole gesture is a single step.
        history.undo(&mut doc);
        assert_eq!(start_of(&doc, seq, v_track, a), Tick(100));
        assert_eq!(start_of(&doc, seq, v_track, b), Tick(200));
        assert_eq!(start_of(&doc, seq, v_track, c), Tick(300));
    }

    /// A linked partner of a selected clip rides along without being selected,
    /// and is not moved twice when it happens to be selected as well.
    #[test]
    fn move_clips_carries_link_partners_exactly_once() {
        let (mut doc, mut history, seq, v_track, a_track) = doc_with_two_tracks();
        let vclip = insert(&mut doc, &mut history, seq, v_track, Tick(100), Tick(100));
        let aclip = insert(&mut doc, &mut history, seq, a_track, Tick(100), Tick(100));
        link(&mut doc, &mut history, seq, v_track, vclip, a_track, aclip);

        // Only the video clip is "selected"; the audio partner rides along.
        move_clips(
            &mut doc,
            &mut history,
            seq,
            &[(v_track, vclip)],
            Tick(40),
            0,
        );
        assert_eq!(start_of(&doc, seq, v_track, vclip), Tick(140));
        assert_eq!(
            start_of(&doc, seq, a_track, aclip),
            Tick(140),
            "linked partner rides along"
        );

        // Now select BOTH: the partner must not be shifted twice.
        move_clips(
            &mut doc,
            &mut history,
            seq,
            &[(v_track, vclip), (a_track, aclip)],
            Tick(40),
            0,
        );
        assert_eq!(start_of(&doc, seq, v_track, vclip), Tick(180));
        assert_eq!(
            start_of(&doc, seq, a_track, aclip),
            Tick(180),
            "a selected partner must move once, not once per mention"
        );
    }

    /// A blocked move is refused whole — no member shifts, so the user never
    /// gets a half-applied selection.
    #[test]
    fn move_clips_blocked_by_a_stationary_clip_moves_nothing() {
        let (mut doc, mut history, seq, v_track, _) = doc_with_two_tracks();
        let a = insert(&mut doc, &mut history, seq, v_track, Tick(100), Tick(100));
        let b = insert(&mut doc, &mut history, seq, v_track, Tick(200), Tick(100));
        // Stationary obstacle just past the run.
        let blocker = insert(&mut doc, &mut history, seq, v_track, Tick(320), Tick(50));

        move_clips(
            &mut doc,
            &mut history,
            seq,
            &[(v_track, a), (v_track, b)],
            Tick(50),
            0,
        );
        assert_eq!(start_of(&doc, seq, v_track, a), Tick(100), "nothing moved");
        assert_eq!(start_of(&doc, seq, v_track, b), Tick(200), "nothing moved");
        assert_eq!(start_of(&doc, seq, v_track, blocker), Tick(320));
    }

    /// Vertical multi-drop: same track_delta for every selected clip, one undo.
    #[test]
    fn move_clips_vertical_track_offset_one_undo() {
        let (mut doc, mut history, seq, v1, _) = doc_with_two_tracks();
        // doc_with_two_tracks gives V1 + A1; add V2 and V3 for vertical room.
        let v2 = {
            let p = doc.timeline.as_mut().unwrap();
            let s = p.sequences.get_mut(&seq).unwrap();
            let t = Track::new(TrackKind::Video, "V2");
            let id = t.id;
            s.video_tracks.push(t);
            s.video_tracks.push(Track::new(TrackKind::Video, "V3"));
            id
        };
        let a = insert(&mut doc, &mut history, seq, v1, Tick(0), Tick(100));
        let b = insert(&mut doc, &mut history, seq, v2, Tick(0), Tick(100));
        move_clips(&mut doc, &mut history, seq, &[(v1, a), (v2, b)], Tick(0), 1);
        // a on former v2 lane, b on V3
        let p = doc.timeline.as_ref().unwrap();
        let s = &p.sequences[&seq];
        assert!(s.track(v1).unwrap().clips.iter().all(|c| c.id != a));
        assert!(s.track(v2).unwrap().clips.iter().any(|c| c.id == a));
        history.undo(&mut doc);
        let p = doc.timeline.as_ref().unwrap();
        let s = &p.sequences[&seq];
        assert!(s.track(v1).unwrap().clips.iter().any(|c| c.id == a));
        assert!(s.track(v2).unwrap().clips.iter().any(|c| c.id == b));
    }

    /// Linked A/V: vertical multi on the video must not fail because the audio
    /// partner has only one lane — partner rides time-only on its own track.
    #[test]
    fn move_clips_vertical_with_linked_audio_partner_keeps_audio_track() {
        let (mut doc, mut history, seq, v1, a_track) = doc_with_two_tracks();
        let v2 = {
            let p = doc.timeline.as_mut().unwrap();
            let s = p.sequences.get_mut(&seq).unwrap();
            let t = Track::new(TrackKind::Video, "V2");
            let id = t.id;
            s.video_tracks.push(t);
            id
        };
        let vclip = insert(&mut doc, &mut history, seq, v1, Tick(0), Tick(100));
        let aclip = insert(&mut doc, &mut history, seq, a_track, Tick(0), Tick(100));
        link(&mut doc, &mut history, seq, v1, vclip, a_track, aclip);

        // Selection is only the video clip; ops_bridge folds the audio partner.
        // track_delta=1 would OOR the single audio track if applied to partners.
        move_clips(&mut doc, &mut history, seq, &[(v1, vclip)], Tick(30), 1);

        let p = doc.timeline.as_ref().unwrap();
        let s = &p.sequences[&seq];
        assert!(
            s.track(v1).unwrap().clips.iter().all(|c| c.id != vclip),
            "video must leave V1"
        );
        let v = s
            .track(v2)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id == vclip)
            .expect("video lands on V2");
        assert_eq!(v.start, Tick(30));
        let a = s
            .track(a_track)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id == aclip)
            .expect("audio partner stays on A1");
        assert_eq!(a.start, Tick(30), "audio rides time-only");
    }
}

#[cfg(test)]
mod sync_lock_tests {
    use super::*;
    use photonic_core::timeline::{ClipId, Sequence, TimelineProject};

    /// Three video tracks — A (the one that gets edited), B (sync-locked),
    /// C (plain, no lock) — each empty until the test inserts clips.
    fn doc_with_three_tracks() -> (
        Document,
        CommandHistory,
        SequenceId,
        TrackId,
        TrackId,
        TrackId,
    ) {
        let mut project = TimelineProject::new();
        let seq = Sequence::new("s", FrameRate::new(30, 1), 1920, 1080);
        let seq_id = project.insert_sequence(seq);
        project.active_sequence = Some(seq_id);
        let mut doc = Document::new("t", 1920.0, 1080.0);
        doc.timeline = Some(project);
        let mut history = CommandHistory::new(64);

        let a = Track::new(TrackKind::Video, "A");
        let a_id = a.id;
        let b = Track::new(TrackKind::Video, "B");
        let b_id = b.id;
        let c = Track::new(TrackKind::Video, "C");
        let c_id = c.id;

        let p = doc.timeline.as_ref().unwrap();
        let add_a = ops::add_track(p, seq_id, a, None).unwrap();
        history.execute_discrete(Command::Timeline(add_a), &mut doc);
        let p = doc.timeline.as_ref().unwrap();
        let add_b = ops::add_track(p, seq_id, b, None).unwrap();
        history.execute_discrete(Command::Timeline(add_b), &mut doc);
        let p = doc.timeline.as_ref().unwrap();
        let add_c = ops::add_track(p, seq_id, c, None).unwrap();
        history.execute_discrete(Command::Timeline(add_c), &mut doc);

        // B is sync-locked; A (edited) and C stay unlocked.
        let p = doc.timeline.as_ref().unwrap();
        let toggle_b = ops::toggle_sync_lock(p, seq_id, b_id).unwrap();
        history.execute_discrete(Command::Timeline(toggle_b), &mut doc);

        (doc, history, seq_id, a_id, b_id, c_id)
    }

    /// Insert an `Adjustment`-source clip `[start, start+duration)` on
    /// `track`, returning its id.
    fn insert(
        doc: &mut Document,
        history: &mut CommandHistory,
        seq: SequenceId,
        track: TrackId,
        start: Tick,
        duration: Tick,
    ) -> ClipId {
        let clip = Clip::new(ClipSource::Adjustment, start, duration);
        let id = clip.id;
        let p = doc.timeline.as_ref().unwrap();
        let cmd = ops::insert_clip(p, seq, track, clip).unwrap();
        history.execute_discrete(Command::Timeline(cmd), doc);
        id
    }

    fn start_of(doc: &Document, seq: SequenceId, track: TrackId, clip: ClipId) -> Tick {
        doc.timeline.as_ref().unwrap().sequences[&seq]
            .track(track)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id == clip)
            .unwrap()
            .start
    }

    fn clip_exists(doc: &Document, seq: SequenceId, track: TrackId, clip: ClipId) -> bool {
        doc.timeline.as_ref().unwrap().sequences[&seq]
            .track(track)
            .unwrap()
            .clips
            .iter()
            .any(|c| c.id == clip)
    }

    #[test]
    fn ripple_trim_shifts_the_sync_locked_track_but_not_the_unlocked_one_in_one_undo_step() {
        let (mut doc, mut history, seq, a, b, c) = doc_with_three_tracks();
        // Every track: a clip at [0,100) and a downstream clip at [200,300).
        let a1 = insert(&mut doc, &mut history, seq, a, Tick(0), Tick(100));
        let a2 = insert(&mut doc, &mut history, seq, a, Tick(200), Tick(100));
        let b1 = insert(&mut doc, &mut history, seq, b, Tick(0), Tick(100));
        let b2 = insert(&mut doc, &mut history, seq, b, Tick(200), Tick(100));
        let c1 = insert(&mut doc, &mut history, seq, c, Tick(0), Tick(100));
        let c2 = insert(&mut doc, &mut history, seq, c, Tick(200), Tick(100));

        // Ripple-trim a1's end from 100 to 150 — a +50 delta.
        let new_timing = ClipTiming {
            start: Tick(0),
            duration: Tick(150),
            source_in: Tick(0),
        };
        ripple_trim(&mut doc, &mut history, seq, a, a1, new_timing);

        // The edited track's own downstream clip shifts by +50.
        assert_eq!(start_of(&doc, seq, a, a2), Tick(250));
        // The sync-locked track's downstream clip shifts by the SAME +50,
        // even though nothing on B was directly edited.
        assert_eq!(start_of(&doc, seq, b, b2), Tick(250));
        // b1 precedes the ripple point — untouched.
        assert_eq!(start_of(&doc, seq, b, b1), Tick(0));
        // The unlocked track is completely unaffected.
        assert_eq!(start_of(&doc, seq, c, c2), Tick(200));
        assert_eq!(start_of(&doc, seq, c, c1), Tick(0));

        // A single undo restores every shifted clip on every track.
        history.undo(&mut doc);
        assert_eq!(start_of(&doc, seq, a, a2), Tick(200));
        assert_eq!(start_of(&doc, seq, b, b2), Tick(200));
        assert!(clip_exists(&doc, seq, a, a1));
        assert!(clip_exists(&doc, seq, b, b1));
        assert!(clip_exists(&doc, seq, c, c1));
    }

    #[test]
    fn ripple_delete_shifts_the_sync_locked_track_but_not_the_unlocked_one_in_one_undo_step() {
        let (mut doc, mut history, seq, a, b, c) = doc_with_three_tracks();
        let a1 = insert(&mut doc, &mut history, seq, a, Tick(0), Tick(100));
        let a2 = insert(&mut doc, &mut history, seq, a, Tick(200), Tick(100));
        let b1 = insert(&mut doc, &mut history, seq, b, Tick(0), Tick(100));
        let b2 = insert(&mut doc, &mut history, seq, b, Tick(200), Tick(100));
        let c1 = insert(&mut doc, &mut history, seq, c, Tick(0), Tick(100));
        let c2 = insert(&mut doc, &mut history, seq, c, Tick(200), Tick(100));

        // Ripple-delete a1 [0,100) — a -100 delta from point 0.
        ripple_delete(&mut doc, &mut history, seq, a, a1);

        assert!(!clip_exists(&doc, seq, a, a1));
        assert_eq!(start_of(&doc, seq, a, a2), Tick(100));
        // The sync-locked track's own clip at [0,100) is untouched (nothing
        // was removed FROM B) but its downstream clip shifts by the same
        // -100 the ripple applied on A.
        assert!(clip_exists(&doc, seq, b, b1));
        assert_eq!(start_of(&doc, seq, b, b1), Tick(0));
        assert_eq!(start_of(&doc, seq, b, b2), Tick(100));
        // The unlocked track is completely unaffected.
        assert_eq!(start_of(&doc, seq, c, c1), Tick(0));
        assert_eq!(start_of(&doc, seq, c, c2), Tick(200));

        // A single undo restores everything, including the removed clip.
        history.undo(&mut doc);
        assert!(clip_exists(&doc, seq, a, a1));
        assert_eq!(start_of(&doc, seq, a, a2), Tick(200));
        assert_eq!(start_of(&doc, seq, b, b2), Tick(200));
        assert_eq!(start_of(&doc, seq, c, c2), Tick(200));
    }

    #[test]
    fn ripple_trim_does_not_propagate_when_no_track_is_sync_locked() {
        let (mut doc, mut history, seq, a, b, c) = doc_with_three_tracks();
        // Turn B's sync-lock back off — no track is sync-locked now.
        let p = doc.timeline.as_ref().unwrap();
        let toggle_b_off = ops::toggle_sync_lock(p, seq, b).unwrap();
        history.execute_discrete(Command::Timeline(toggle_b_off), &mut doc);

        let a1 = insert(&mut doc, &mut history, seq, a, Tick(0), Tick(100));
        let a2 = insert(&mut doc, &mut history, seq, a, Tick(200), Tick(100));
        let b2 = insert(&mut doc, &mut history, seq, b, Tick(200), Tick(100));
        let c2 = insert(&mut doc, &mut history, seq, c, Tick(200), Tick(100));

        let new_timing = ClipTiming {
            start: Tick(0),
            duration: Tick(150),
            source_in: Tick(0),
        };
        ripple_trim(&mut doc, &mut history, seq, a, a1, new_timing);

        assert_eq!(start_of(&doc, seq, a, a2), Tick(250));
        assert_eq!(start_of(&doc, seq, b, b2), Tick(200));
        assert_eq!(start_of(&doc, seq, c, c2), Tick(200));
    }
}

#[cfg(test)]
mod replace_source_tests {
    use super::*;
    use photonic_core::timeline::{ClipEffect, EffectKind, Sequence, TimelineProject};

    /// A project with one active sequence, one video track, and one
    /// `Adjustment`-source clip [0,100) carrying an effect — "everything the
    /// editor built around the slot" that Replace With Clip must preserve.
    fn doc_with_seeded_clip() -> (Document, CommandHistory, SequenceId, TrackId, ClipId) {
        let mut project = TimelineProject::new();
        let seq = Sequence::new("s", FrameRate::new(30, 1), 1920, 1080);
        let seq_id = project.insert_sequence(seq);
        project.active_sequence = Some(seq_id);
        let mut doc = Document::new("t", 1920.0, 1080.0);
        doc.timeline = Some(project);
        let mut history = CommandHistory::new(64);

        let track = Track::new(TrackKind::Video, "V1");
        let track_id = track.id;
        let p = doc.timeline.as_ref().unwrap();
        let add_track = ops::add_track(p, seq_id, track, None).unwrap();
        history.execute_discrete(Command::Timeline(add_track), &mut doc);

        let mut clip = Clip::new(ClipSource::Adjustment, Tick(0), Tick(100));
        clip.effects.push(ClipEffect::new(EffectKind::Blur));
        let clip_id = clip.id;
        let p = doc.timeline.as_ref().unwrap();
        let insert = ops::insert_clip(p, seq_id, track_id, clip).unwrap();
        history.execute_discrete(Command::Timeline(insert), &mut doc);

        (doc, history, seq_id, track_id, clip_id)
    }

    fn find(doc: &Document, seq: SequenceId, track: TrackId, clip: ClipId) -> &Clip {
        doc.timeline.as_ref().unwrap().sequences[&seq]
            .track(track)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id == clip)
            .unwrap()
    }

    #[test]
    fn replace_swaps_source_keeps_start_duration_and_effects_in_one_undo_step() {
        let (mut doc, mut history, seq, track, clip) = doc_with_seeded_clip();
        let new_source = ClipSource::SolidColor {
            color: photonic_core::Color {
                r: 0.2,
                g: 0.4,
                b: 0.6,
                a: 1.0,
            },
        };

        replace_clip_source(
            &mut doc,
            &mut history,
            seq,
            track,
            clip,
            new_source.clone(),
            Some(Tick(20)),
        );

        let c = find(&doc, seq, track, clip);
        assert_eq!(c.source, new_source, "source swapped");
        assert_eq!(c.source_in, Tick(20), "source_in re-anchored");
        assert_eq!(c.start, Tick(0), "start preserved");
        assert_eq!(c.duration, Tick(100), "duration preserved");
        assert_eq!(c.effects.len(), 1, "effect stack preserved");

        // A single undo restores the original source, source_in and effect.
        history.undo(&mut doc);
        let c = find(&doc, seq, track, clip);
        assert_eq!(c.source, ClipSource::Adjustment);
        assert_eq!(c.source_in, Tick(0));
        assert_eq!(c.effects.len(), 1);
    }

    #[test]
    fn replace_with_no_source_in_keeps_the_existing_one() {
        let (mut doc, mut history, seq, track, clip) = doc_with_seeded_clip();
        let new_source = ClipSource::SolidColor {
            color: photonic_core::Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        };
        replace_clip_source(
            &mut doc,
            &mut history,
            seq,
            track,
            clip,
            new_source.clone(),
            None,
        );
        let c = find(&doc, seq, track, clip);
        assert_eq!(c.source, new_source);
        assert_eq!(c.source_in, Tick(0), "source_in untouched when None");
    }
}
