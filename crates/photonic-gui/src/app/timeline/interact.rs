//! Pointer-intent computation for the timeline (04 §2.3–§2.5): edge-zone hit
//! testing, snapping-candidate selection, and the transient drag/marquee state.
//!
//! Everything above the "3/4-point source editing" section is pure or
//! session-transient — no `doc.timeline` mutation and no history calls (that is
//! `ops_bridge.rs`'s job). `mod.rs` calls these to turn raw pointer positions
//! into an intent, then hands the intent to `ops_bridge`.
//!
//! The one sanctioned exception is the Insert/Overwrite/Lift/Extract edit
//! helpers at the bottom (spec 16 §4): the story that owns `ops_bridge.rs` is a
//! separate territory, so per that spec these four `ops::*`→`CommandHistory`
//! bridges live here instead. They mirror `ops_bridge`'s discipline exactly —
//! one atomic, undoable batch per edit — and are the sole place this module
//! touches history.

use photonic_core::document::Document;
use photonic_core::history::{Command, CommandHistory};
use photonic_core::timeline::{
    ops, Clip, ClipId, ClipSource, ClipTiming, Sequence, SequenceId, Tick, TimelineCmd, TrackId,
    TrackKind,
};

/// Which part of a clip rect the pointer is over (04 §2.3: 6px edge zones = trim,
/// interior = body/move).
///
/// `pub` so UI-path integration tests (43-gesture-chrome-ui-paths) can lock the
/// zone classification against `EDGE_HIT_PX` without re-implementing hit math.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipZone {
    LeftEdge,
    Body,
    RightEdge,
}

/// The active drag gesture kind (resolved from zone + modifiers at drag-start).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum DragKind {
    Move,
    TrimStart,
    TrimEnd,
    RippleTrimStart,
    RippleTrimEnd,
    Slip,
    Slide,
    /// Roll the boundary shared with the clip to the right (`right` is that clip).
    Roll {
        right: ClipId,
    },
}

/// Transient per-gesture state, stashed in egui temp memory across frames so the
/// gesture reads its drag-start snapshot rather than re-deriving it each frame
/// (04 §7 immediate-mode mitigation).
#[derive(Clone)]
pub(crate) struct DragState {
    pub kind: DragKind,
    pub track: TrackId,
    pub clip: ClipId,
    /// Lane-space tick under the pointer when the drag began.
    pub grab_tick: Tick,
    /// Timing of the primary clip at drag-start (the undo `old`).
    pub orig: ClipTiming,
    /// Track the pointer currently hovers (for a cross-track move), else `track`.
    pub dest_track: TrackId,
    /// Snap candidates, precomputed once at drag-start in priority order (04 §2.5).
    pub candidates: Vec<Tick>,
    /// Set once the pointer has actually moved — distinguishes a click from a drag.
    pub moved: bool,
}

// ── K-A7 Grab item / arrow-key nudge ─────────────────────────────────────────

/// Keyboard grab session (K-A7): selected clip is "held" so arrow keys preview a
/// move; Enter commits one undo unit, Esc cancels. Mirrors mouse-drag commit-on-
/// release so N nudges never pollute the undo stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GrabSession {
    pub seq: SequenceId,
    pub track: TrackId,
    pub clip: ClipId,
    /// Original timing at grab engage (commit delta is relative to this).
    pub orig_start: Tick,
    pub duration: Tick,
    /// Live preview placement (ghost).
    pub preview_start: Tick,
    pub preview_track: TrackId,
}

/// Horizontal / vertical nudge intent while a [`GrabSession`] is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GrabNudge {
    /// Earlier on the timeline (`ArrowLeft`).
    Earlier,
    /// Later on the timeline (`ArrowRight`).
    Later,
    /// Previous track of the same kind (`ArrowUp` — higher video lane / earlier audio).
    TrackPrev,
    /// Next track of the same kind (`ArrowDown`).
    TrackNext,
}

impl GrabSession {
    /// Seed a grab from a live clip. Returns `None` if the clip cannot be found
    /// or its track is locked.
    pub fn seed(seq: &Sequence, seq_id: SequenceId, track: TrackId, clip: ClipId) -> Option<Self> {
        let t = seq.track(track)?;
        if t.locked {
            return None;
        }
        let c = t.clips.iter().find(|c| c.id == clip)?;
        Some(Self {
            seq: seq_id,
            track,
            clip,
            orig_start: c.start,
            duration: c.duration,
            preview_start: c.start,
            preview_track: track,
        })
    }

    /// Whether the preview differs from the grab-start placement (anything to commit).
    pub fn is_dirty(&self) -> bool {
        self.preview_start != self.orig_start || self.preview_track != self.track
    }
}

/// Apply one keyboard nudge to `session` against `seq` (pure — no doc mutation).
///
/// Horizontal steps by `frame_step` ticks (caller supplies 1× or N× frame).
/// Vertical moves to the previous/next **unlocked** track of the same kind.
/// Overlap / out-of-range previews are still written; commit validates via ops.
pub(crate) fn apply_grab_nudge(
    session: &mut GrabSession,
    seq: &Sequence,
    nudge: GrabNudge,
    frame_step: Tick,
) {
    match nudge {
        GrabNudge::Earlier => {
            let step = frame_step.0.max(1);
            session.preview_start = Tick((session.preview_start.0 - step).max(0));
        }
        GrabNudge::Later => {
            let step = frame_step.0.max(1);
            session.preview_start = Tick(session.preview_start.0.saturating_add(step));
        }
        GrabNudge::TrackPrev | GrabNudge::TrackNext => {
            let Some(cur) = seq.track(session.preview_track) else {
                return;
            };
            let kind = cur.kind;
            let lane: Vec<TrackId> = seq
                .tracks()
                .filter(|t| t.kind == kind && !t.locked)
                .map(|t| t.id)
                .collect();
            let Some(idx) = lane.iter().position(|id| *id == session.preview_track) else {
                return;
            };
            let next = match nudge {
                GrabNudge::TrackPrev => idx.checked_sub(1),
                GrabNudge::TrackNext => {
                    let n = idx + 1;
                    if n < lane.len() {
                        Some(n)
                    } else {
                        None
                    }
                }
                _ => None,
            };
            if let Some(i) = next {
                session.preview_track = lane[i];
            }
        }
    }
}

/// Transient marquee (rubber-band) select state.
#[derive(Clone, Copy)]
pub(crate) struct Marquee {
    pub start: egui::Pos2,
    pub additive: bool,
}

/// One painted clip rect offered up for pointer hit-testing, tagged with its
/// track and that track's lock state (04 §2.3 / 14-nle-parity QW-2).
#[derive(Clone, Copy)]
pub(crate) struct HitCandidate {
    pub track: TrackId,
    pub clip: ClipId,
    pub rect: egui::Rect,
    pub locked: bool,
}

/// Hit-test a pointer position against painted clip candidates, honoring
/// track lock: a candidate whose track is locked is never a hit. Selection,
/// drag-start, and the context-menu target all resolve through this one
/// function, so a locked track rejects every clip edit while its clips stay
/// fully visible/paintable — only interaction is gated here, not painting
/// (`clips.rs::paint_lane` draws locked lanes unconditionally, plus a
/// diagonal-hatch cue).
pub(crate) fn hit_at(
    pos: egui::Pos2,
    edge: f32,
    candidates: &[HitCandidate],
) -> Option<(TrackId, ClipId, egui::Rect, ClipZone)> {
    for h in candidates {
        if h.locked {
            continue;
        }
        if pos.y >= h.rect.top() && pos.y <= h.rect.bottom() {
            if let Some(z) = hit_zone(h.rect.left(), h.rect.right(), pos.x, edge) {
                return Some((h.track, h.clip, h.rect, z));
            }
        }
    }
    None
}

/// Hit-test a pointer x against a clip rect `[x0, x1]`. Returns the zone, or
/// `None` if the pointer is outside the rect.
///
/// `edge` is the *requested* handle width — [`EDGE_HIT_PX`](super::layout::EDGE_HIT_PX)
/// (12px per 41 R-9), which is wider than the 6px that gets painted. Two clamps
/// keep that from costing the body (move) zone on short clips:
///
/// - Clips narrower than two *painted* handles split at the midpoint, so a tiny
///   clip stays trimmable from both sides (unchanged behaviour).
/// - Above that, the handle is capped at a third of the width, so every clip
///   wide enough to have had a body zone at 6px still has one at 12px. Without
///   this, widening the handle would make any clip under 24px un-draggable —
///   trading a trim-target win for a move-target regression.
pub fn hit_zone(x0: f32, x1: f32, px: f32, edge: f32) -> Option<ClipZone> {
    if px < x0 || px > x1 {
        return None;
    }
    let width = x1 - x0;
    if width <= 2.0 * super::layout::EDGE_ZONE_PX {
        let mid = x0 + width * 0.5;
        return Some(if px < mid {
            ClipZone::LeftEdge
        } else {
            ClipZone::RightEdge
        });
    }
    let edge = edge.min(width / 3.0);
    if px <= x0 + edge {
        Some(ClipZone::LeftEdge)
    } else if px >= x1 - edge {
        Some(ClipZone::RightEdge)
    } else {
        Some(ClipZone::Body)
    }
}

/// Choose a snap target for `value` from `candidates` (priority-ordered). Returns
/// the first candidate within `threshold` ticks, so a higher-priority candidate
/// wins even when a lower-priority one is marginally closer (04 §2.5).
pub fn nearest_snap(value: Tick, candidates: &[Tick], threshold: Tick) -> Option<Tick> {
    let thr = threshold.0.abs();
    candidates
        .iter()
        .copied()
        .find(|c| (c.0 - value.0).abs() <= thr)
}

/// Esc drag-cancel decision (210 §5 / 43 §2.1).
///
/// When true, the timeline frame must **drop** transient `DragState` / marquee
/// and must **not** call `commit_drag`. The document is never mutated mid-drag
/// (only a ghost is painted), so clearing state is the whole cancel.
pub fn should_cancel_drag(escape_pressed: bool, drag_active: bool) -> bool {
    escape_pressed && drag_active
}

/// Apply snapping (when enabled) then frame-quantize. `value` is a raw lane tick;
/// the result always lands on a frame boundary (04 §2.1).
pub(crate) fn snap_and_quantize(
    value: Tick,
    candidates: &[Tick],
    threshold: Tick,
    snap_enabled: bool,
    frame_rate: photonic_core::timeline::FrameRate,
) -> Tick {
    let snapped = if snap_enabled {
        nearest_snap(value, candidates, threshold).unwrap_or(value)
    } else {
        value
    };
    frame_rate.snap(snapped)
}

/// Accumulates snap targets in priority order, dropping exact duplicates so a
/// low-priority restatement of a tick can never displace the higher-priority one
/// [`nearest_snap`] would have picked, and negatives (a target before the
/// sequence start is unreachable).
#[derive(Default)]
struct SnapAccum {
    out: Vec<Tick>,
    seen: std::collections::HashSet<Tick>,
}

impl SnapAccum {
    fn push(&mut self, t: Tick) {
        if t.0 >= 0 && self.seen.insert(t) {
            self.out.push(t);
        }
    }
}

/// Push every keyframe of `c`, projected to sequence time, into `acc`.
///
/// A keyframe's `at` is clip-relative (`clip.start + at` is its sequence tick —
/// the same convention the keyframe editor's `playhead_local` inverts), so a
/// lane entry outside `[0, duration]` is currently trimmed off the clip and is
/// not a visible target. Orphaned lanes (01 §6.2) are skipped: they are retained
/// but neither evaluated nor drawn, so they must not attract a drag.
fn push_clip_keyframes(acc: &mut SnapAccum, c: &Clip) {
    let lanes = c
        .transform
        .tracks
        .iter()
        .chain(c.effects.iter().flat_map(|e| e.params.tracks.iter()))
        .chain(
            c.grade
                .iter()
                .flat_map(|g| g.ops.iter().flat_map(|o| o.params.tracks.iter())),
        )
        .chain(c.audio.iter().flat_map(|a| a.params.tracks.iter()));
    for lane in lanes {
        if lane.orphaned {
            continue;
        }
        for kf in &lane.keyframes {
            if kf.at.0 >= 0 && kf.at <= c.duration {
                acc.push(c.start + kf.at);
            }
        }
    }
}

/// Collect every snap target of `seq` in priority order (26 K-A4).
///
/// `focus` is the `(track, clip)` a drag is operating on: that track's edges
/// lead the ordering, and everything belonging to the moving clip (its edges,
/// its clip markers, its keyframes) is excluded because it travels *with* the
/// drag. `None` collects the whole sequence with no bias — the navigation view.
///
/// `playhead` is optional for the same reason: "jump to the next snap" must not
/// treat the position it is jumping *from* as a target.
fn collect_snap_targets(
    seq: &Sequence,
    focus: Option<(TrackId, ClipId)>,
    playhead: Option<Tick>,
) -> SnapAccum {
    let focus_track = focus.map(|(t, _)| t);
    let moving = focus.map(|(_, c)| c);
    let mut acc = SnapAccum::default();
    // 1. Same-track clip edges.
    if let Some(t) = focus_track.and_then(|id| seq.track(id)) {
        for c in &t.clips {
            if Some(c.id) != moving {
                acc.push(c.start);
                acc.push(c.end());
            }
        }
    }
    // 2. Other-track clip edges.
    for t in seq.tracks() {
        if Some(t.id) == focus_track {
            continue;
        }
        for c in &t.clips {
            if Some(c.id) == moving {
                continue;
            }
            acc.push(c.start);
            acc.push(c.end());
        }
    }
    // 3. Playhead.
    if let Some(p) = playhead {
        acc.push(p);
    }
    // 4. Sequence markers — Kdenlive's "guides". A ranged marker contributes
    //    two targets, start and end (35 §1.6).
    for m in &seq.markers {
        acc.push(m.at);
        if m.is_range() {
            acc.push(m.end());
        }
    }
    // 5. Zone in/out: the sequence work range used for preview + export.
    if let Some((zin, zout)) = seq.work_range {
        acc.push(zin);
        acc.push(zout);
    }
    // 6. Clip markers, projected to sequence time (35 §1.6). Same range rule as
    //    sequence markers; a marker trimmed off the clip is not a target.
    for t in seq.tracks() {
        for c in &t.clips {
            if Some(c.id) == moving {
                continue;
            }
            for m in &c.markers {
                if m.at.0 < 0 || m.at > c.duration {
                    continue;
                }
                acc.push(c.marker_sequence_tick(m));
                if m.is_range() && m.end() <= c.duration {
                    acc.push(c.start + m.end());
                }
            }
        }
    }
    // 7. Keyframes.
    for t in seq.tracks() {
        for c in &t.clips {
            if Some(c.id) == moving {
                continue;
            }
            push_clip_keyframes(&mut acc, c);
        }
    }
    // 8. Sequence start. Last on purpose: when tick 0 is also a clip edge it has
    //    already been recorded at its real priority, so this only matters for an
    //    otherwise empty head of the timeline.
    acc.push(Tick::ZERO);
    acc
}

/// Build the snap-candidate list for a drag on `track`, excluding everything
/// that belongs to `moving` (it moves with the drag), in the priority order
/// 04 §2.5 mandates, extended by the targets 26 K-A4 requires:
///
/// 1. same-track clip edges → 2. clip edges on every other track →
/// 3. playhead → 4. sequence markers/guides (both ends of a ranged marker) →
/// 5. zone in/out (the work range) → 6. clip markers → 7. keyframes →
/// 8. sequence start.
///
/// Deterministic and duplicate-free: computed once at drag-start, and equal
/// ticks collapse to their highest-priority occurrence.
pub(crate) fn build_snap_candidates(
    seq: &Sequence,
    track: TrackId,
    moving: ClipId,
    playhead: Tick,
) -> Vec<Tick> {
    collect_snap_targets(seq, Some((track, moving)), Some(playhead)).out
}

/// Every snap target in `seq`, ascending and deduped — the navigation view of
/// the set [`build_snap_candidates`] ranks (26 K-A4 Previous/Next Snap). The
/// playhead is deliberately absent: it is where the jump starts, not a target.
#[allow(dead_code)] // consumed by the not-yet-wired prev/next-snap commands
pub(crate) fn snap_points(seq: &Sequence) -> Vec<Tick> {
    let mut v = collect_snap_targets(seq, None, None).out;
    v.sort_unstable();
    v
}

/// Resolve a drag-start into its gesture kind and the *primary* clip the gesture
/// operates on (04 §2.4 modifier/hit-test scheme). Pure: the caller supplies the
/// dragged clip plus, for edge drags, the id of the neighbour that shares the
/// exact boundary on that side (`None` when there is no flush neighbour, so a
/// Roll is impossible).
///
/// Rules:
/// - Body: Alt+Shift ⇒ Slide, Alt ⇒ Slip, plain ⇒ Move (primary = `clip`).
/// - LeftEdge with a flush left neighbour and no Shift ⇒ Roll (primary = the LEFT
///   clip, `Roll.right` = `clip`); Shift ⇒ RippleTrimStart; plain ⇒ TrimStart.
/// - RightEdge with a flush right neighbour and no Shift ⇒ Roll (primary = `clip`,
///   `Roll.right` = the right neighbour); Shift ⇒ RippleTrimEnd; plain ⇒ TrimEnd.
///
/// Shift always suppresses Roll in favour of a ripple trim, matching the drag
/// commit logic in `mod.rs`.
pub(crate) fn resolve_drag_kind(
    zone: ClipZone,
    alt: bool,
    shift: bool,
    clip: ClipId,
    left_shared: Option<ClipId>,
    right_shared: Option<ClipId>,
) -> (DragKind, ClipId) {
    match zone {
        ClipZone::Body => {
            let k = if alt && shift {
                DragKind::Slide
            } else if alt {
                DragKind::Slip
            } else {
                DragKind::Move
            };
            (k, clip)
        }
        ClipZone::LeftEdge => {
            if let (Some(prev), false) = (left_shared, shift) {
                (DragKind::Roll { right: clip }, prev)
            } else if shift {
                (DragKind::RippleTrimStart, clip)
            } else {
                (DragKind::TrimStart, clip)
            }
        }
        ClipZone::RightEdge => {
            if let (Some(next), false) = (right_shared, shift) {
                (DragKind::Roll { right: next }, clip)
            } else if shift {
                (DragKind::RippleTrimEnd, clip)
            } else {
                (DragKind::TrimEnd, clip)
            }
        }
    }
}

/// Apply a marquee (rubber-band) rect over the visible clip rects, mutating
/// `selection` (04 §2.6). Replace semantics (`additive == false`) clears the
/// selection first; additive (Ctrl/Shift held) keeps it. A clip joins the
/// selection when its rect *intersects* the marquee — touching or partial
/// overlap counts, full containment is not required — and a clip already in the
/// selection is never pushed twice.
pub(crate) fn apply_marquee(
    marquee: egui::Rect,
    hits: impl IntoIterator<Item = (egui::Rect, ClipId)>,
    additive: bool,
    selection: &mut Vec<ClipId>,
) {
    if !additive {
        selection.clear();
    }
    for (rect, clip) in hits {
        if rect.intersects(marquee) && !selection.contains(&clip) {
            selection.push(clip);
        }
    }
}

// ── 3/4-point source editing (spec 16 — Insert / Overwrite / Lift / Extract) ──
//
// `PendingSource` is the GUI-session "armed" source range for a 3-point edit
// (spec 16 §1) — the source + its in/out, held ONLY in session state, never in
// the document. The four `do_*_edit` helpers turn that (plus a timeline point or
// work-range) into one atomic, undoable `TimelineCmd` batch via the pure core
// ops, and commit it as a single undo step.

/// The armed source for an Insert/Overwrite (spec 16 §1): a clip source plus its
/// trim in/out and originating track kind. Duration = `src_out − src_in`. Held
/// in GUI session state only.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingSource {
    /// The source op to lay down (carries the asset id for asset/vector sources).
    pub source: ClipSource,
    /// Trim in-point into the source media.
    pub src_in: Tick,
    /// Trim out-point into the source media (`source_in + duration`).
    pub src_out: Tick,
    /// Display name carried from the armed clip.
    pub name: String,
    /// Which track kind the source came from → which lane the edit targets.
    pub kind: TrackKind,
}

impl PendingSource {
    /// Arm from an existing timeline `clip` on a track of `kind` (spec 16 §4
    /// minimal arming): capture its source, trim in-point and out-point so an
    /// Insert/Overwrite lays down the same source region.
    pub fn from_clip(clip: &Clip, kind: TrackKind) -> Self {
        PendingSource {
            source: clip.source.clone(),
            src_in: clip.source_in,
            src_out: clip.source_in + clip.duration,
            name: clip.name.clone(),
            kind,
        }
    }

    /// Duration of the armed range (`src_out − src_in`); ≥ 0 for a range
    /// captured via [`PendingSource::from_clip`].
    pub fn duration(&self) -> Tick {
        self.src_out - self.src_in
    }

    /// Build the concrete clip to drop at `at` on the timeline, preserving the
    /// source trim in-point and name.
    pub fn to_clip(&self, at: Tick) -> Clip {
        let mut c = Clip::new(self.source.clone(), at, self.duration());
        c.source_in = self.src_in;
        c.name = self.name.clone();
        c
    }
}

/// Arm a 3-point source from the first selected clip on `seq` (spec 16 §4
/// minimal arming — selecting a timeline clip makes it the source). Video lanes
/// are scanned before audio (timeline order); `None` when the selection resolves
/// to no clip.
pub(crate) fn pending_source_from_selection(
    seq: &Sequence,
    selection: &[ClipId],
) -> Option<PendingSource> {
    for t in seq.tracks() {
        for c in &t.clips {
            if selection.contains(&c.id) {
                return Some(PendingSource::from_clip(c, t.kind));
            }
        }
    }
    None
}

/// Resolve which track receives an edit for `kind` (spec 16 §1, M-3 minimal
/// source-patch): the `explicit` patch target when it still exists on a lane of
/// `kind`, else the first ENABLED track of that kind, else the first track of
/// that kind. `None` = no track of that kind exists.
pub(crate) fn resolve_target_track(
    seq: &Sequence,
    kind: TrackKind,
    explicit: Option<TrackId>,
) -> Option<TrackId> {
    let lane = match kind {
        TrackKind::Video | TrackKind::Text => &seq.video_tracks,
        TrackKind::Audio => &seq.audio_tracks,
    };
    if let Some(id) = explicit {
        if lane.iter().any(|t| t.id == id) {
            return Some(id);
        }
    }
    lane.iter()
        .find(|t| t.enabled)
        .or_else(|| lane.first())
        .map(|t| t.id)
}

// ── NLE parity round-2 (spec 17 G2/G3): clip resolution for keyboard edits ──

/// The clip a keyboard trim (spec 17 G2, Q/W) should act on: the selected clip
/// the playhead is strictly inside (`start < at < end`) if any, else the first
/// such clip. Locked tracks are skipped (their clips reject edits). `None` when
/// the playhead is on an edge or in a gap on every unlocked track.
pub(crate) fn trim_target_at(
    seq: &Sequence,
    selection: &[ClipId],
    at: Tick,
) -> Option<(TrackId, ClipId)> {
    // Prefer a selected clip the playhead is strictly inside.
    for t in seq.tracks() {
        if t.locked {
            continue;
        }
        for c in &t.clips {
            if selection.contains(&c.id) && c.start < at && at < c.end() {
                return Some((t.id, c.id));
            }
        }
    }
    // Else the first clip the playhead is strictly inside.
    for t in seq.tracks() {
        if t.locked {
            continue;
        }
        for c in &t.clips {
            if c.start < at && at < c.end() {
                return Some((t.id, c.id));
            }
        }
    }
    None
}

/// The clip a Match-Frame / Reveal (spec 17 G3) should read: the selected clip
/// the playhead sits on (`start <= at < end`) if any, else the first such clip.
/// Unlike [`trim_target_at`] this includes a clip whose start is exactly at the
/// playhead and does NOT skip locked tracks (reading a clip is not an edit).
pub(crate) fn clip_at_playhead(
    seq: &Sequence,
    selection: &[ClipId],
    at: Tick,
) -> Option<(TrackId, ClipId)> {
    for t in seq.tracks() {
        for c in &t.clips {
            if selection.contains(&c.id) && c.start <= at && at < c.end() {
                return Some((t.id, c.id));
            }
        }
    }
    for t in seq.tracks() {
        for c in &t.clips {
            if c.start <= at && at < c.end() {
                return Some((t.id, c.id));
            }
        }
    }
    None
}

/// The first selected clip in timeline order (video lanes then audio), skipping
/// locked tracks. Backs the Extend-Edit target (spec 17 G2, E).
pub(crate) fn first_selected(seq: &Sequence, selection: &[ClipId]) -> Option<(TrackId, ClipId)> {
    for t in seq.tracks() {
        if t.locked {
            continue;
        }
        for c in &t.clips {
            if selection.contains(&c.id) {
                return Some((t.id, c.id));
            }
        }
    }
    None
}

/// The track a roll-to-playhead (spec 17 G2, Shift+Q/W) should target: the track
/// of the clip under/selected at the playhead if resolvable, else the first
/// unlocked track with at least two clips (a possible cut). `None` when no
/// unlocked track can host a roll.
pub(crate) fn roll_target_track(seq: &Sequence, selection: &[ClipId], at: Tick) -> Option<TrackId> {
    if let Some((track, _)) =
        trim_target_at(seq, selection, at).or_else(|| first_selected(seq, selection))
    {
        return Some(track);
    }
    seq.tracks()
        .find(|t| !t.locked && t.clips.len() >= 2)
        .map(|t| t.id)
}

/// Commit `cmds` as ONE undo step (spec 16 §2 "batched into one undo step").
/// A single command keeps the `Command::Timeline` shape the rest of the timeline
/// relies on rather than a one-element `Batch`; an empty batch is a no-op.
fn commit_edit(history: &mut CommandHistory, doc: &mut Document, mut cmds: Vec<TimelineCmd>) {
    match cmds.len() {
        0 => {}
        1 => history.execute_discrete(
            Command::Timeline(cmds.pop().expect("len == 1 checked above")),
            doc,
        ),
        _ => {
            let batch = cmds.into_iter().map(Command::Timeline).collect();
            history.execute_discrete(Command::Batch(batch), doc);
        }
    }
}

/// **Insert** (spec 16 §2, Premiere `,`): open a gap at `at` on `track` and drop
/// `source` in, rippling downstream clips right. One undo step; `true` if
/// applied (a rejected edit — bad track, non-positive duration, nested cycle —
/// is a silent no-op like other invalid gestures).
pub fn do_insert_edit(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    at: Tick,
    source: Clip,
) -> bool {
    let Some(p) = doc.timeline.as_ref() else {
        return false;
    };
    match ops::insert_edit(p, seq, track, at, source) {
        Ok(cmds) => {
            commit_edit(history, doc, cmds);
            true
        }
        Err(_) => false,
    }
}

/// **Overwrite** (spec 16 §2, Premiere `.`): drop `source` at `at` on `track`,
/// replacing whatever it covers with no ripple. One undo step; `true` if applied.
pub(crate) fn do_overwrite_edit(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    at: Tick,
    source: Clip,
) -> bool {
    let Some(p) = doc.timeline.as_ref() else {
        return false;
    };
    match ops::overwrite_edit(p, seq, track, at, source) {
        Ok(cmds) => {
            commit_edit(history, doc, cmds);
            true
        }
        Err(_) => false,
    }
}

/// **Lift** (spec 16 §2, Premiere `;`): clear `range` on `track`, leaving a gap
/// (no ripple). One undo step; `true` if applied.
pub(crate) fn do_lift_edit(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    range: (Tick, Tick),
) -> bool {
    let Some(p) = doc.timeline.as_ref() else {
        return false;
    };
    match ops::lift_edit(p, seq, track, range) {
        Ok(cmds) => {
            commit_edit(history, doc, cmds);
            true
        }
        Err(_) => false,
    }
}

/// **Extract** (spec 16 §2, Premiere `'`): clear `range` on `track` AND ripple
/// everything after it left to close the gap. One undo step; `true` if applied.
pub(crate) fn do_extract_edit(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    range: (Tick, Tick),
) -> bool {
    let Some(p) = doc.timeline.as_ref() else {
        return false;
    };
    match ops::extract_edit(p, seq, track, range) {
        Ok(cmds) => {
            commit_edit(history, doc, cmds);
            true
        }
        Err(_) => false,
    }
}

/// Razor/blade decision (spec 16 §4, M-4): a lane click at `at` on `clip` splits
/// it only when strictly interior (`clip.start < at < clip.end()`); a click on
/// an edge is not a split. Returns the split tick to hand to [`do_razor_split`].
pub(crate) fn razor_split_tick(clip: &Clip, at: Tick) -> Option<Tick> {
    (clip.start < at && at < clip.end()).then_some(at)
}

/// Consume a lane click while the razor tool is active (spec 16 §4 M-4):
/// resolve `at` against `clip`'s live extent via [`razor_split_tick`] and, if
/// interior, split it there through the same `ops::split_clip` path
/// `timeline_split_at_playhead` uses (mirrors `ops_bridge::split`'s one-undo-
/// step discipline). `true` if the click was consumed as a split — the caller
/// should skip its normal select-on-click handling in that case; an edge/
/// outside click or a missing clip returns `false` and falls through to
/// ordinary selection.
///
/// The lane-click call site lives in the timeline-panel story's `self_interact`
/// (its territory) — the one-line seam is the "Selection on click" block:
/// when `razor_active`, try `do_razor_split(doc, history, seq_id, track, clip,
/// tick_at(pos))` first and only fall back to `apply_selection` on `false`.
pub(crate) fn do_razor_split(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    clip: ClipId,
    at: Tick,
) -> bool {
    let Some(p) = doc.timeline.as_ref() else {
        return false;
    };
    let Some(c) = p
        .sequences
        .get(&seq)
        .and_then(|s| s.track(track))
        .and_then(|t| t.clips.iter().find(|c| c.id == clip))
    else {
        return false;
    };
    let Some(split_at) = razor_split_tick(c, at) else {
        return false;
    };
    match ops::split_clip(p, seq, track, clip, split_at) {
        Ok(cmd) => {
            commit_edit(history, doc, vec![cmd]);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::FrameRate;

    #[test]
    fn hit_zone_classifies_edges_and_body() {
        // A 100px-wide clip from 200..300, 6px edges.
        assert_eq!(hit_zone(200.0, 300.0, 202.0, 6.0), Some(ClipZone::LeftEdge));
        assert_eq!(
            hit_zone(200.0, 300.0, 298.0, 6.0),
            Some(ClipZone::RightEdge)
        );
        assert_eq!(hit_zone(200.0, 300.0, 250.0, 6.0), Some(ClipZone::Body));
        assert_eq!(hit_zone(200.0, 300.0, 199.0, 6.0), None);
        assert_eq!(hit_zone(200.0, 300.0, 301.0, 6.0), None);
    }

    #[test]
    fn hit_zone_narrow_clip_splits_at_midpoint() {
        // 8px clip (< 2*6): no body, split at midpoint.
        assert_eq!(hit_zone(100.0, 108.0, 101.0, 6.0), Some(ClipZone::LeftEdge));
        assert_eq!(
            hit_zone(100.0, 108.0, 107.0, 6.0),
            Some(ClipZone::RightEdge)
        );
        assert!(hit_zone(100.0, 108.0, 104.0, 6.0).is_some());
    }

    /// 41 R-9 / 210 §5: the trim handle is *hit* at 12px even though it paints
    /// at 6px, so a pointer 10px inside the edge still grabs the trim.
    #[test]
    fn hit_zone_honours_the_widened_trim_handle() {
        let edge = super::super::layout::EDGE_HIT_PX;
        // 100px clip: 10px in is inside the 12px handle (it was Body at 6px).
        assert_eq!(
            hit_zone(200.0, 300.0, 210.0, edge),
            Some(ClipZone::LeftEdge)
        );
        assert_eq!(
            hit_zone(200.0, 300.0, 290.0, edge),
            Some(ClipZone::RightEdge)
        );
        assert_eq!(hit_zone(200.0, 300.0, 250.0, edge), Some(ClipZone::Body));
    }

    /// Widening the handle must not cost the body (move) zone: every clip wide
    /// enough to have had a body at 6px still has one at 12px, because
    /// `hit_zone` caps the handle at a third of the width. Without the cap a
    /// clip under 24px would become un-draggable — a move regression traded for
    /// a trim win.
    #[test]
    fn widened_handle_never_swallows_the_body_zone() {
        let edge = super::super::layout::EDGE_HIT_PX;
        for width in [13.0f32, 16.0, 20.0, 24.0, 30.0, 48.0, 200.0] {
            let (x0, x1) = (100.0f32, 100.0 + width);
            let mid = x0 + width * 0.5;
            assert_eq!(
                hit_zone(x0, x1, mid, edge),
                Some(ClipZone::Body),
                "a {width}px clip must stay draggable from its middle"
            );
            // Both handles still resolve as trims at the very edge.
            assert_eq!(hit_zone(x0, x1, x0 + 0.5, edge), Some(ClipZone::LeftEdge));
            assert_eq!(hit_zone(x0, x1, x1 - 0.5, edge), Some(ClipZone::RightEdge));
        }
    }

    #[test]
    fn hit_at_locked_track_rejects_the_edit_intent() {
        let track = TrackId::new();
        let clip = ClipId::new();
        let rect = egui::Rect::from_min_max(egui::pos2(100.0, 10.0), egui::pos2(200.0, 40.0));
        let pos = egui::pos2(150.0, 25.0); // dead center of the rect.

        // Locked track: a pointer squarely over the clip is never a hit — no
        // selection, no drag-start, no context-menu target (14-nle-parity
        // QW-2 — a locked track rejects every clip edit intent).
        let locked = [HitCandidate {
            track,
            clip,
            rect,
            locked: true,
        }];
        assert_eq!(hit_at(pos, 6.0, &locked), None);

        // Identical geometry, unlocked, resolves normally — proves the miss
        // above is the lock guard, not a bug in the rect math.
        let unlocked = [HitCandidate {
            track,
            clip,
            rect,
            locked: false,
        }];
        assert_eq!(
            hit_at(pos, 6.0, &unlocked),
            Some((track, clip, rect, ClipZone::Body))
        );
    }

    #[test]
    fn hit_at_falls_through_a_locked_candidate_to_an_unlocked_one() {
        // Two overlapping candidates at the same point (e.g. adjacent lanes'
        // edge case) — the locked one must not shadow a legitimately
        // unlocked hit later in the list.
        let locked_track = TrackId::new();
        let locked_clip = ClipId::new();
        let unlocked_track = TrackId::new();
        let unlocked_clip = ClipId::new();
        let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 30.0));
        let candidates = [
            HitCandidate {
                track: locked_track,
                clip: locked_clip,
                rect,
                locked: true,
            },
            HitCandidate {
                track: unlocked_track,
                clip: unlocked_clip,
                rect,
                locked: false,
            },
        ];
        assert_eq!(
            hit_at(egui::pos2(50.0, 15.0), 6.0, &candidates),
            Some((unlocked_track, unlocked_clip, rect, ClipZone::Body))
        );
    }

    #[test]
    fn should_cancel_drag_only_when_escape_and_active() {
        assert!(!should_cancel_drag(false, false));
        assert!(!should_cancel_drag(false, true));
        assert!(!should_cancel_drag(true, false));
        assert!(should_cancel_drag(true, true));
    }

    #[test]
    fn nearest_snap_within_threshold_and_priority() {
        let value = Tick(1000);
        // First within threshold wins even if a later one is closer.
        let candidates = [Tick(1050), Tick(1010)];
        assert_eq!(nearest_snap(value, &candidates, Tick(60)), Some(Tick(1050)));
        // Nothing within threshold → None.
        assert_eq!(nearest_snap(value, &candidates, Tick(5)), None);
        // Empty candidates → None.
        assert_eq!(nearest_snap(value, &[], Tick(100)), None);
    }

    #[test]
    fn snap_and_quantize_lands_on_frame_boundary() {
        let fr = FrameRate::FPS_30;
        let tpf = fr.ticks_per_frame().0;
        // No snap candidates, snapping on → pure frame quantize.
        let v = Tick(5 * tpf + 7);
        let out = snap_and_quantize(v, &[], Tick(0), true, fr);
        assert_eq!(out.0 % tpf, 0);
        assert_eq!(out, Tick(5 * tpf));
    }

    #[test]
    fn snap_candidates_exclude_moving_and_order_by_priority() {
        let mut s = Sequence::new("s", FrameRate::FPS_30, 1920, 1080);
        let mut v = photonic_core::timeline::Track::new(TrackKind::Video, "V1");
        let moving = ClipId::new();
        let mut c0 = photonic_core::timeline::Clip::new(
            photonic_core::timeline::ClipSource::Adjustment,
            Tick(0),
            Tick(100),
        );
        c0.id = moving;
        let c1 = photonic_core::timeline::Clip::new(
            photonic_core::timeline::ClipSource::Adjustment,
            Tick(100),
            Tick(50),
        );
        v.clips.push(c0);
        v.clips.push(c1);
        let track_id = v.id;
        s.video_tracks.push(v);
        let cands = build_snap_candidates(&s, track_id, moving, Tick(42));
        // Moving clip's own edges (0, 100) are excluded; c1's edges present;
        // playhead present.
        assert!(cands.contains(&Tick(100)));
        assert!(cands.contains(&Tick(150)));
        assert!(cands.contains(&Tick(42)));
        // Playhead is after the same-track edges in priority order.
        let play_idx = cands.iter().position(|t| *t == Tick(42)).unwrap();
        let edge_idx = cands.iter().position(|t| *t == Tick(150)).unwrap();
        assert!(edge_idx < play_idx);
    }

    /// A sequence exercising every 26 K-A4 snap tier at a distinct tick, so a
    /// membership or ordering assertion names exactly one tier.
    ///
    /// Returns `(sequence, focus track, moving clip)`.
    fn snap_fixture() -> (Sequence, TrackId, ClipId) {
        use photonic_core::timeline::{
            ClipAudio, ClipEffect, EffectKind, Interp, Keyframe, Marker, PropValue, Track,
        };

        // V1 — the focus track: the dragged clip plus one neighbour that carries
        // a point and a ranged clip marker.
        let mut v1 = Track::new(TrackKind::Video, "V1");
        let mut moving = Clip::new(ClipSource::Adjustment, Tick(1000), Tick(200));
        let moving_id = moving.id;
        // Everything below belongs to the dragged clip and travels with it.
        moving
            .markers
            .push(Marker::clip_scoped(Tick(50), "on moving"));
        moving
            .transform
            .track_mut(&"transform.opacity".into())
            .insert_keyframe(Keyframe::new(
                Tick(60),
                PropValue::Float(1.0),
                Interp::Linear,
            ));

        let mut neighbour = Clip::new(ClipSource::Adjustment, Tick(2000), Tick(300));
        neighbour
            .markers
            .push(Marker::clip_scoped(Tick(100), "point"));
        let mut ranged = Marker::clip_scoped(Tick(200), "ranged");
        ranged.duration = Tick(50);
        neighbour.markers.push(ranged);
        // An orphaned lane is retained but never evaluated or drawn (01 §6.2).
        let orphan = neighbour.transform.track_mut(&"transform.gone".into());
        orphan.orphaned = true;
        orphan.insert_keyframe(Keyframe::new(
            Tick(280),
            PropValue::Float(0.0),
            Interp::Linear,
        ));
        v1.clips.push(moving);
        v1.clips.push(neighbour);
        let track_id = v1.id;

        // V2 — another track, carrying keyframes on three different lane owners.
        let mut v2 = Track::new(TrackKind::Video, "V2");
        let mut other = Clip::new(ClipSource::Adjustment, Tick(5000), Tick(400));
        let lane = other.transform.track_mut(&"transform.opacity".into());
        lane.insert_keyframe(Keyframe::new(
            Tick(100),
            PropValue::Float(0.5),
            Interp::Linear,
        ));
        // Trimmed off the clip (`at` past the out point) → not a visible target.
        lane.insert_keyframe(Keyframe::new(
            Tick(9999),
            PropValue::Float(0.0),
            Interp::Linear,
        ));
        let mut audio = ClipAudio::new();
        audio
            .params
            .track_mut(&"gain_db".into())
            .insert_keyframe(Keyframe::new(
                Tick(200),
                PropValue::Float(-3.0),
                Interp::Linear,
            ));
        other.audio = Some(audio);
        let mut fx = ClipEffect::new(EffectKind::Blur);
        fx.params
            .track_mut(&"radius".into())
            .insert_keyframe(Keyframe::new(
                Tick(300),
                PropValue::Float(4.0),
                Interp::Linear,
            ));
        other.effects.push(fx);
        v2.clips.push(other);

        let mut s = Sequence::new("s", FrameRate::FPS_30, 1920, 1080);
        s.video_tracks.push(v1);
        s.video_tracks.push(v2);
        // Guides: one point, one ranged (two targets).
        s.markers.push(Marker::new(Tick(8000), "guide"));
        let mut ranged_guide = Marker::new(Tick(9000), "ranged guide");
        ranged_guide.duration = Tick(500);
        s.markers.push(ranged_guide);
        s.work_range = Some((Tick(11_000), Tick(12_000)));
        (s, track_id, moving_id)
    }

    #[test]
    fn snap_candidates_cover_every_k_a4_tier_in_priority_order() {
        let (s, track, moving) = snap_fixture();
        let cands = build_snap_candidates(&s, track, moving, Tick(7000));
        let idx = |t: i64| {
            cands
                .iter()
                .position(|c| *c == Tick(t))
                .unwrap_or_else(|| panic!("missing snap target {t}: {cands:?}"))
        };

        // Every tier is present …
        let same_track_edge = idx(2000);
        let other_track_edge = idx(5000);
        let playhead = idx(7000);
        let guide = idx(8000);
        let ranged_guide_end = idx(9500);
        let zone_in = idx(11_000);
        let clip_marker = idx(2100);
        let clip_marker_end = idx(2250);
        let transform_keyframe = idx(5100);
        let sequence_start = idx(0);
        // … including the second half of each pair.
        idx(2300); // same-track edge (neighbour out)
        idx(5400); // other-track edge
        idx(9000); // ranged guide start
        idx(12_000); // zone out
        idx(2200); // ranged clip-marker start
        idx(5200); // audio-lane keyframe
        idx(5300); // effect-lane keyframe

        // … in the documented priority order.
        assert!(same_track_edge < other_track_edge);
        assert!(other_track_edge < playhead);
        assert!(playhead < guide);
        assert!(guide < ranged_guide_end);
        assert!(ranged_guide_end < zone_in);
        assert!(zone_in < clip_marker);
        assert!(clip_marker < clip_marker_end);
        assert!(clip_marker_end < transform_keyframe);
        assert!(transform_keyframe < sequence_start);

        // The dragged clip contributes nothing: not its edges, not its clip
        // marker, not its keyframe — they all move with the drag.
        for absent in [Tick(1000), Tick(1200), Tick(1050), Tick(1060)] {
            assert!(!cands.contains(&absent), "{absent:?} moves with the drag");
        }
        // A keyframe trimmed off its clip is not a target (5000 + 9999), nor is
        // an orphaned lane's keyframe (2000 + 280).
        assert!(!cands.contains(&Tick(14_999)), "trimmed-off keyframe");
        assert!(!cands.contains(&Tick(2280)), "orphaned lane keyframe");
    }

    #[test]
    fn snap_candidates_dedupe_and_keep_the_highest_priority_occurrence() {
        use photonic_core::timeline::Marker;
        let (mut s, track, moving) = snap_fixture();
        // Restate two ticks that are already clip edges as a guide and a zone.
        s.markers.push(Marker::new(Tick(5400), "on a clip edge"));
        s.work_range = Some((Tick(2000), Tick(5400)));
        let cands = build_snap_candidates(&s, track, moving, Tick(5400));
        for t in [Tick(2000), Tick(5400)] {
            assert_eq!(
                cands.iter().filter(|c| **c == t).count(),
                1,
                "{t:?} restated by a lower tier must not be listed twice"
            );
        }
        // The surviving entry is the high-priority one: 2000 is a same-track
        // edge, so it still outranks every other-track edge.
        let same_track = cands.iter().position(|c| *c == Tick(2000)).unwrap();
        let other_track = cands.iter().position(|c| *c == Tick(5000)).unwrap();
        assert!(same_track < other_track);
    }

    #[test]
    fn snap_points_are_sorted_deduped_and_unbiased() {
        let (s, _track, _moving) = snap_fixture();
        let pts = snap_points(&s);
        assert!(
            pts.windows(2).all(|w| w[0] < w[1]),
            "ascending and deduped: {pts:?}"
        );
        assert_eq!(pts.first(), Some(&Tick::ZERO), "sequence start leads");
        // No clip is excluded here — navigation has no moving clip, so the
        // fixture's dragged clip contributes its edges, marker and keyframe.
        for t in [Tick(1000), Tick(1200), Tick(1050), Tick(1060)] {
            assert!(pts.contains(&t), "{t:?} missing from navigation targets");
        }
        // The playhead is where a jump starts, never a target of its own.
        assert!(!pts.contains(&Tick(7000)));
    }

    #[test]
    fn resolve_drag_kind_body_modifiers() {
        let clip = ClipId::new();
        // Plain body ⇒ Move.
        assert_eq!(
            resolve_drag_kind(ClipZone::Body, false, false, clip, None, None),
            (DragKind::Move, clip)
        );
        // Alt+body ⇒ Slip.
        assert_eq!(
            resolve_drag_kind(ClipZone::Body, true, false, clip, None, None),
            (DragKind::Slip, clip)
        );
        // Alt+Shift+body ⇒ Slide.
        assert_eq!(
            resolve_drag_kind(ClipZone::Body, true, true, clip, None, None),
            (DragKind::Slide, clip)
        );
        // Shift alone on the body is not a slide — still Move.
        assert_eq!(
            resolve_drag_kind(ClipZone::Body, false, true, clip, None, None),
            (DragKind::Move, clip)
        );
    }

    #[test]
    fn resolve_drag_kind_left_edge() {
        let clip = ClipId::new();
        let prev = ClipId::new();
        // Flush left neighbour, no Shift ⇒ Roll with the LEFT clip as primary and
        // `clip` as the right side of the shared boundary.
        assert_eq!(
            resolve_drag_kind(ClipZone::LeftEdge, false, false, clip, Some(prev), None),
            (DragKind::Roll { right: clip }, prev)
        );
        // Shift suppresses Roll even with a flush neighbour ⇒ RippleTrimStart.
        assert_eq!(
            resolve_drag_kind(ClipZone::LeftEdge, false, true, clip, Some(prev), None),
            (DragKind::RippleTrimStart, clip)
        );
        // No flush neighbour, no Shift ⇒ plain TrimStart.
        assert_eq!(
            resolve_drag_kind(ClipZone::LeftEdge, false, false, clip, None, None),
            (DragKind::TrimStart, clip)
        );
        // No neighbour + Shift ⇒ RippleTrimStart.
        assert_eq!(
            resolve_drag_kind(ClipZone::LeftEdge, false, true, clip, None, None),
            (DragKind::RippleTrimStart, clip)
        );
    }

    #[test]
    fn resolve_drag_kind_right_edge() {
        let clip = ClipId::new();
        let next = ClipId::new();
        // Flush right neighbour, no Shift ⇒ Roll with `clip` as primary (left of
        // the boundary) and the neighbour as the right side.
        assert_eq!(
            resolve_drag_kind(ClipZone::RightEdge, false, false, clip, None, Some(next)),
            (DragKind::Roll { right: next }, clip)
        );
        // Shift suppresses Roll ⇒ RippleTrimEnd.
        assert_eq!(
            resolve_drag_kind(ClipZone::RightEdge, false, true, clip, None, Some(next)),
            (DragKind::RippleTrimEnd, clip)
        );
        // No flush neighbour, no Shift ⇒ plain TrimEnd.
        assert_eq!(
            resolve_drag_kind(ClipZone::RightEdge, false, false, clip, None, None),
            (DragKind::TrimEnd, clip)
        );
    }

    #[test]
    fn trim_target_prefers_selected_then_any_interior_clip() {
        use photonic_core::timeline::Track;
        let mut s = Sequence::new("s", FrameRate::FPS_30, 1920, 1080);
        let mut v1 = Track::new(TrackKind::Video, "V1");
        let mut v2 = Track::new(TrackKind::Video, "V2");
        let c1 = Clip::new(ClipSource::Adjustment, Tick(0), Tick(100)); // V1 [0,100)
        let c2 = Clip::new(ClipSource::Adjustment, Tick(0), Tick(100)); // V2 [0,100)
        let (id1, id2) = (c1.id, c2.id);
        v1.clips.push(c1);
        v2.clips.push(c2);
        let (v1id, v2id) = (v1.id, v2.id);
        s.video_tracks.push(v1);
        s.video_tracks.push(v2);

        // Playhead 50 is interior to both; the selected V2 clip wins.
        assert_eq!(trim_target_at(&s, &[id2], Tick(50)), Some((v2id, id2)));
        // No selection → the first interior clip in track order (V1).
        assert_eq!(trim_target_at(&s, &[], Tick(50)), Some((v1id, id1)));
        // On an edge nothing is strictly interior.
        assert_eq!(trim_target_at(&s, &[], Tick(0)), None);
        assert_eq!(trim_target_at(&s, &[], Tick(100)), None);
        // A locked track is skipped even when its clip is the selection.
        s.video_tracks[1].locked = true;
        assert_eq!(trim_target_at(&s, &[id2], Tick(50)), Some((v1id, id1)));
    }

    #[test]
    fn apply_marquee_intersect_not_contains() {
        let clip = ClipId::new();
        let marquee = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(50.0, 50.0));
        // Partial overlap (not fully contained) still selects — intersect, not
        // contains, is the rule.
        let partial = egui::Rect::from_min_max(egui::pos2(40.0, 40.0), egui::pos2(100.0, 100.0));
        let mut sel = Vec::new();
        apply_marquee(marquee, [(partial, clip)], false, &mut sel);
        assert_eq!(sel, vec![clip]);

        // A rect entirely outside the marquee is not selected.
        let outside = egui::Rect::from_min_max(egui::pos2(60.0, 60.0), egui::pos2(80.0, 80.0));
        let mut sel = Vec::new();
        apply_marquee(marquee, [(outside, clip)], false, &mut sel);
        assert!(sel.is_empty());
    }

    #[test]
    fn apply_marquee_replace_vs_additive() {
        let existing = ClipId::new();
        let hit = ClipId::new();
        let inside = egui::Rect::from_min_max(egui::pos2(10.0, 10.0), egui::pos2(20.0, 20.0));
        let marquee = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(50.0, 50.0));

        // Replace: prior selection is cleared, only the hit survives.
        let mut sel = vec![existing];
        apply_marquee(marquee, [(inside, hit)], false, &mut sel);
        assert_eq!(sel, vec![hit]);

        // Additive: prior selection is retained and the hit appended.
        let mut sel = vec![existing];
        apply_marquee(marquee, [(inside, hit)], true, &mut sel);
        assert_eq!(sel, vec![existing, hit]);

        // Additive never duplicates an already-selected clip.
        let mut sel = vec![hit];
        apply_marquee(marquee, [(inside, hit)], true, &mut sel);
        assert_eq!(sel, vec![hit]);
    }
}

#[cfg(test)]
mod edit_tests {
    use super::*;
    use photonic_core::timeline::{FrameRate, Sequence, TimelineProject, Track};

    fn adj_clip(start: i64, dur: i64) -> Clip {
        Clip::new(ClipSource::Adjustment, Tick(start), Tick(dur))
    }

    /// A document with one active sequence whose video/audio lanes are created
    /// empty from `(name, disabled)` specs. Returns the sequence id and the new
    /// track ids per lane.
    fn doc_with_tracks(
        video: &[(&str, bool)],
        audio: &[(&str, bool)],
    ) -> (Document, SequenceId, Vec<TrackId>, Vec<TrackId>) {
        let mut seq = Sequence::new("s", FrameRate::FPS_30, 1920, 1080);
        let mut vids = Vec::new();
        for (name, disabled) in video {
            let mut t = Track::new(TrackKind::Video, *name);
            t.enabled = !*disabled;
            vids.push(t.id);
            seq.video_tracks.push(t);
        }
        let mut auds = Vec::new();
        for (name, disabled) in audio {
            let mut t = Track::new(TrackKind::Audio, *name);
            t.enabled = !*disabled;
            auds.push(t.id);
            seq.audio_tracks.push(t);
        }
        let mut project = TimelineProject::new();
        let seq_id = project.insert_sequence(seq);
        project.active_sequence = Some(seq_id);
        let mut doc = Document::new("t", 1920.0, 1080.0);
        doc.timeline = Some(project);
        (doc, seq_id, vids, auds)
    }

    /// Push a clip directly onto a track (test setup only — no history).
    fn seed(doc: &mut Document, seq: SequenceId, track: TrackId, start: i64, dur: i64) {
        doc.timeline
            .as_mut()
            .unwrap()
            .sequences
            .get_mut(&seq)
            .unwrap()
            .track_mut(track)
            .unwrap()
            .clips
            .push(adj_clip(start, dur));
    }

    fn clips_of(doc: &Document, seq: SequenceId, track: TrackId) -> Vec<(Tick, Tick)> {
        doc.timeline.as_ref().unwrap().sequences[&seq]
            .track(track)
            .unwrap()
            .clips
            .iter()
            .map(|c| (c.start, c.duration))
            .collect()
    }

    fn content_end(doc: &Document, seq: SequenceId, track: TrackId) -> Tick {
        doc.timeline.as_ref().unwrap().sequences[&seq]
            .track(track)
            .unwrap()
            .clips
            .iter()
            .map(|c| c.end())
            .max()
            .unwrap_or(Tick::ZERO)
    }

    // ── Arming: clip → PendingSource (spec 16 §4 minimal) ────────────────────

    #[test]
    fn arming_captures_the_selected_clips_source_range_and_kind() {
        let mut seq = Sequence::new("s", FrameRate::FPS_30, 1920, 1080);
        let mut v = Track::new(TrackKind::Video, "V1");
        let mut c = adj_clip(0, 100);
        c.source_in = Tick(30); // trimmed 30 ticks into the source
        c.name = "shot".into();
        let id = c.id;
        v.clips.push(c);
        seq.video_tracks.push(v);

        let ps = pending_source_from_selection(&seq, &[id]).expect("armed");
        assert_eq!(ps.src_in, Tick(30));
        assert_eq!(ps.src_out, Tick(130)); // source_in + duration
        assert_eq!(ps.duration(), Tick(100));
        assert_eq!(ps.kind, TrackKind::Video);
        assert_eq!(ps.name, "shot");

        // The concrete clip lands at `at`, keeping the source trim + name.
        let placed = ps.to_clip(Tick(500));
        assert_eq!((placed.start, placed.duration), (Tick(500), Tick(100)));
        assert_eq!(placed.source_in, Tick(30));
        assert_eq!(placed.name, "shot");

        // An empty selection arms nothing.
        assert!(pending_source_from_selection(&seq, &[]).is_none());
    }

    // ── Target-track resolution (spec 16 §1 M-3 minimal) ─────────────────────

    #[test]
    fn target_resolution_prefers_explicit_then_first_enabled() {
        // V1 disabled, V2 enabled.
        let (doc, seq, vids, _a) = doc_with_tracks(&[("V1", true), ("V2", false)], &[]);
        let s = &doc.timeline.as_ref().unwrap().sequences[&seq];

        // No patch → first ENABLED video track (V2), not merely the first.
        assert_eq!(
            resolve_target_track(s, TrackKind::Video, None),
            Some(vids[1])
        );
        // Explicit valid patch wins even though V1 is disabled.
        assert_eq!(
            resolve_target_track(s, TrackKind::Video, Some(vids[0])),
            Some(vids[0])
        );
        // A stale/foreign patch id falls back to first enabled.
        assert_eq!(
            resolve_target_track(s, TrackKind::Video, Some(TrackId::new())),
            Some(vids[1])
        );
        // No audio lane exists → None.
        assert_eq!(resolve_target_track(s, TrackKind::Audio, None), None);
    }

    // ── The four edits (thin bridges over the tested core ops) ───────────────

    #[test]
    fn insert_lands_source_at_the_playhead_on_the_resolved_target_one_undo_step() {
        // V1 disabled so the resolved target is V2 — proves at-playhead
        // placement lands on the patch target, not lane 0.
        let (mut doc, seq, vids, _a) = doc_with_tracks(&[("V1", true), ("V2", false)], &[]);
        let mut hist = CommandHistory::new(64);
        let target = resolve_target_track(
            &doc.timeline.as_ref().unwrap().sequences[&seq],
            TrackKind::Video,
            None,
        )
        .unwrap();
        assert_eq!(target, vids[1]);

        let playhead = Tick(200);
        assert!(do_insert_edit(
            &mut doc,
            &mut hist,
            seq,
            target,
            playhead,
            adj_clip(0, 50)
        ));
        // Source landed at the playhead on V2; V1 untouched.
        assert_eq!(clips_of(&doc, seq, vids[1]), vec![(Tick(200), Tick(50))]);
        assert!(clips_of(&doc, seq, vids[0]).is_empty());

        // Exactly one undo step removes it.
        assert!(hist.undo(&mut doc));
        assert!(clips_of(&doc, seq, vids[1]).is_empty());
    }

    #[test]
    fn insert_ripples_downstream_and_grows_the_track() {
        let (mut doc, seq, vids, _a) = doc_with_tracks(&[("V1", false)], &[]);
        let v = vids[0];
        seed(&mut doc, seq, v, 0, 100); // existing clip [0,100)
        let mut hist = CommandHistory::new(64);

        // Insert dur-50 at the head → existing clip ripples right by 50.
        assert!(do_insert_edit(
            &mut doc,
            &mut hist,
            seq,
            v,
            Tick(0),
            adj_clip(0, 50)
        ));
        assert_eq!(content_end(&doc, seq, v), Tick(150)); // grew by the source duration
        assert_eq!(
            clips_of(&doc, seq, v),
            vec![(Tick(0), Tick(50)), (Tick(50), Tick(100))]
        );
    }

    #[test]
    fn overwrite_keeps_duration_and_punches_a_hole() {
        let (mut doc, seq, vids, _a) = doc_with_tracks(&[("V1", false)], &[]);
        let v = vids[0];
        seed(&mut doc, seq, v, 0, 200); // [0,200)
        let mut hist = CommandHistory::new(64);

        assert!(do_overwrite_edit(
            &mut doc,
            &mut hist,
            seq,
            v,
            Tick(50),
            adj_clip(0, 50)
        ));
        // No ripple: overall end unchanged; a dur-50 source now occupies [50,100).
        assert_eq!(content_end(&doc, seq, v), Tick(200));
        assert!(clips_of(&doc, seq, v).contains(&(Tick(50), Tick(50))));
    }

    #[test]
    fn lift_leaves_a_gap_and_extract_closes_it() {
        // Lift: clears the range, no ripple → total end unchanged.
        let (mut doc, seq, vids, _a) = doc_with_tracks(&[("V1", false)], &[]);
        let v = vids[0];
        seed(&mut doc, seq, v, 0, 300); // [0,300)
        let mut hist = CommandHistory::new(64);
        assert!(do_lift_edit(
            &mut doc,
            &mut hist,
            seq,
            v,
            (Tick(100), Tick(200))
        ));
        assert_eq!(
            clips_of(&doc, seq, v),
            vec![(Tick(0), Tick(100)), (Tick(200), Tick(100))]
        );
        assert_eq!(content_end(&doc, seq, v), Tick(300)); // gap left, duration kept

        // Extract: clears + ripples left → shrinks by the range width.
        let (mut doc, seq, vids, _a) = doc_with_tracks(&[("V1", false)], &[]);
        let v = vids[0];
        seed(&mut doc, seq, v, 0, 300);
        let mut hist = CommandHistory::new(64);
        assert!(do_extract_edit(
            &mut doc,
            &mut hist,
            seq,
            v,
            (Tick(100), Tick(200))
        ));
        assert_eq!(
            clips_of(&doc, seq, v),
            vec![(Tick(0), Tick(100)), (Tick(100), Tick(100))]
        );
        assert_eq!(content_end(&doc, seq, v), Tick(200)); // closed the 100-tick gap
    }

    // ── Razor guard (spec 16 §4 M-4) ─────────────────────────────────────────

    #[test]
    fn razor_splits_only_the_interior_never_an_edge() {
        let c = adj_clip(100, 100); // [100,200)
        assert_eq!(razor_split_tick(&c, Tick(150)), Some(Tick(150)));
        assert_eq!(razor_split_tick(&c, Tick(100)), None); // start edge
        assert_eq!(razor_split_tick(&c, Tick(200)), None); // end edge
        assert_eq!(razor_split_tick(&c, Tick(50)), None); // outside
    }

    // ── Razor click → split consumption (spec 16 §4 M-4) ─────────────────────

    #[test]
    fn razor_split_consumes_an_interior_click_in_one_undo_step() {
        let (mut doc, seq, vids, _a) = doc_with_tracks(&[("V1", false)], &[]);
        let v = vids[0];
        seed(&mut doc, seq, v, 100, 100); // [100,200)
        let clip_id = doc.timeline.as_ref().unwrap().sequences[&seq]
            .track(v)
            .unwrap()
            .clips[0]
            .id;
        let mut hist = CommandHistory::new(64);

        assert!(do_razor_split(
            &mut doc,
            &mut hist,
            seq,
            v,
            clip_id,
            Tick(150)
        ));
        assert_eq!(
            clips_of(&doc, seq, v),
            vec![(Tick(100), Tick(50)), (Tick(150), Tick(50))]
        );

        // One undo step restores the single clip.
        assert!(hist.undo(&mut doc));
        assert_eq!(clips_of(&doc, seq, v), vec![(Tick(100), Tick(100))]);
    }

    #[test]
    fn razor_split_leaves_edge_and_outside_clicks_unconsumed() {
        let (mut doc, seq, vids, _a) = doc_with_tracks(&[("V1", false)], &[]);
        let v = vids[0];
        seed(&mut doc, seq, v, 100, 100); // [100,200)
        let clip_id = doc.timeline.as_ref().unwrap().sequences[&seq]
            .track(v)
            .unwrap()
            .clips[0]
            .id;
        let mut hist = CommandHistory::new(64);

        assert!(!do_razor_split(
            &mut doc,
            &mut hist,
            seq,
            v,
            clip_id,
            Tick(100)
        )); // start edge
        assert!(!do_razor_split(
            &mut doc,
            &mut hist,
            seq,
            v,
            clip_id,
            Tick(200)
        )); // end edge
        assert!(!do_razor_split(
            &mut doc,
            &mut hist,
            seq,
            v,
            clip_id,
            Tick(50)
        )); // outside
        assert_eq!(clips_of(&doc, seq, v), vec![(Tick(100), Tick(100))]); // untouched, no undo entry
        assert!(!hist.undo(&mut doc));
    }

    #[test]
    fn razor_split_on_an_unknown_clip_is_a_no_op() {
        let (mut doc, seq, vids, _a) = doc_with_tracks(&[("V1", false)], &[]);
        let v = vids[0];
        seed(&mut doc, seq, v, 100, 100);
        let mut hist = CommandHistory::new(64);

        assert!(!do_razor_split(
            &mut doc,
            &mut hist,
            seq,
            v,
            ClipId::new(),
            Tick(150)
        ));
        assert_eq!(clips_of(&doc, seq, v), vec![(Tick(100), Tick(100))]);
    }
}

#[cfg(test)]
mod grab_tests {
    use super::*;
    use photonic_core::timeline::{FrameRate, Sequence, Track};

    fn seq_two_video() -> (Sequence, TrackId, TrackId, ClipId) {
        let mut seq = Sequence::new("s", FrameRate::FPS_30, 1920, 1080);
        let mut v1 = Track::new(TrackKind::Video, "V1");
        let c = Clip::new(ClipSource::Adjustment, Tick(100), Tick(50));
        let clip = c.id;
        v1.clips.push(c);
        let t1 = v1.id;
        let v2 = Track::new(TrackKind::Video, "V2");
        let t2 = v2.id;
        seq.video_tracks.push(v1);
        seq.video_tracks.push(v2);
        (seq, t1, t2, clip)
    }

    #[test]
    fn grab_seed_and_horizontal_nudge() {
        let (seq, t1, _t2, clip) = seq_two_video();
        let mut g = GrabSession::seed(&seq, seq.id, t1, clip).unwrap();
        assert!(!g.is_dirty());
        let tpf = FrameRate::FPS_30.ticks_per_frame();
        apply_grab_nudge(&mut g, &seq, GrabNudge::Later, tpf);
        assert_eq!(g.preview_start, Tick(100 + tpf.0));
        assert!(g.is_dirty());
        apply_grab_nudge(&mut g, &seq, GrabNudge::Earlier, tpf);
        assert_eq!(g.preview_start, Tick(100));
        // Clamp at zero.
        g.preview_start = Tick(0);
        apply_grab_nudge(&mut g, &seq, GrabNudge::Earlier, tpf);
        assert_eq!(g.preview_start, Tick(0));
    }

    #[test]
    fn grab_track_nudge_same_kind() {
        let (seq, t1, t2, clip) = seq_two_video();
        let mut g = GrabSession::seed(&seq, seq.id, t1, clip).unwrap();
        apply_grab_nudge(&mut g, &seq, GrabNudge::TrackNext, Tick(1));
        assert_eq!(g.preview_track, t2);
        apply_grab_nudge(&mut g, &seq, GrabNudge::TrackNext, Tick(1));
        assert_eq!(g.preview_track, t2); // no further
        apply_grab_nudge(&mut g, &seq, GrabNudge::TrackPrev, Tick(1));
        assert_eq!(g.preview_track, t1);
    }

    #[test]
    fn grab_seed_rejects_locked_track() {
        let (mut seq, t1, _t2, clip) = seq_two_video();
        seq.track_mut(t1).unwrap().locked = true;
        assert!(GrabSession::seed(&seq, seq.id, t1, clip).is_none());
    }
}
