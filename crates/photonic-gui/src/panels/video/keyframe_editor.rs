//! Keyframe / curve editor (04 §4.1, 01 §6, 13 §5.2/§16) — the editing UI over
//! the generic [`AnimProps`](photonic_core::timeline::AnimProps) animation system.
//!
//! Three surfaces, all owned here:
//! - [`draw_window`] — a floating curve editor for the selected clip: an
//!   animatable-property picker (transform + effect params, 01 §6.2 registry) on
//!   the left, and a Bezier-capable curve editor (property tracks, Hold/Linear/
//!   Bezier easing, add/move/delete keyframes) on the right. Enabling AS-3
//!   motion-graphics: place a `VectorDoc` clip (media pool → timeline) and
//!   animate its `transform.*` here. No longer auto-*opens* on a timeline
//!   selection change (G-9, see [`draw_embedded`]) — it stays wherever the
//!   user left it and only follows the selection while already open; reached
//!   via a keyframe-diamond click (`clip_inspector.rs`) or the docked
//!   section's pop-out button.
//! - [`draw_embedded`] — the **G-9 effect-controls unification** docked
//!   counterpart: the same picker + Float curve drawing
//!   (`draw_prop_picker`/`draw_curve_area`, reused verbatim, not
//!   reimplemented), laid out as a compact vertical stack instead of a
//!   floating `egui::Window`, for `clip_inspector.rs`'s "Keyframes"
//!   collapsible section — the default, always-visible per-clip surface now,
//!   closing the "two surfaces for one concept" gap (14 §G-9) between the
//!   fixed Motion/Opacity/Speed fields (`clip_inspector.rs`) and the curve
//!   editor. `clip_inspector.rs` is `PropPanelCtx`-based (`doc: &Document`, no
//!   `&mut CommandHistory`), so this returns a `PanelAction` instead of
//!   committing directly.
//! - [`paint_clip_automation`] — the timeline automation-lane indicator: keyframe
//!   diamonds painted along a clip's body so animated clips read at a glance.
//!
//! **Every mutation flows through a pure core op → `CommandHistory`**
//! (`draw_window`) **or a pure core op → `PanelAction`** (`draw_embedded`) —
//! this module never touches `doc.timeline` directly either way. Drag
//! gestures preview a ghost and commit ONE step on release (one undo step per
//! gesture, the same discipline `app/timeline/ops_bridge.rs` uses for clip
//! drags).

use egui::{Align2, Color32, FontId, Pos2, Rect, Rounding, Sense, Shape, Stroke, Vec2};
use egui_phosphor::regular as ph;

use crate::app::timeline::TimelineView;
use crate::panels::PanelAction;
use photonic_core::document::Document;
use photonic_core::history::{Command, CommandHistory};
use photonic_core::timeline::{
    anim, ops, prop_registry, AnimTarget, Clip, ClipId, FrameRate, Interp, Keyframe,
    KeyframeClipboard, PropPath, PropTargetKind, PropValue, PropValueKind, PropertyTrack, Tick,
    TimelineCmd, TimelineProject,
};

// ── egui memory keys ─────────────────────────────────────────────────────────

const STATE_ID: &str = "photonic_kf_editor_state";
const DRAG_ID: &str = "photonic_kf_editor_drag";
/// Session keyframe-interchange clipboard (26 §10 K-B11).
const KF_CLIPBOARD_ID: &str = "photonic_kf_clipboard";

/// Persistent (session-only) editor UI state, kept in egui memory so this module
/// needs no new `PhotonicApp` fields. Reset when the targeted clip changes.
#[derive(Clone, Copy, Default)]
struct EditorState {
    /// Clip these selections belong to (reset row/keyframe when it changes).
    clip: Option<ClipId>,
    /// Selected property-row index into [`build_rows`]'s flattened list.
    sel_row: usize,
    /// Clip-relative tick of the selected keyframe, if any.
    sel_kf_at: Option<i64>,
    /// Last timeline selection observed — drives `draw_window`'s
    /// follow-while-open (not auto-open, see its doc comment) on selection
    /// change.
    last_selection: Option<ClipId>,
    /// Whether `last_selection` has ever been recorded (distinguishes "no
    /// selection yet" from "deselected" on the very first frame).
    seeded: bool,
}

/// Which grip of a keyframe an in-progress drag holds.
#[derive(Clone, Copy, PartialEq)]
enum Grip {
    /// The keyframe point itself (moves time + value).
    Point,
    /// The outgoing Bezier control handle (near this keyframe).
    HandleOut,
    /// The incoming Bezier control handle (near the next keyframe).
    HandleIn,
}

#[derive(Clone, Copy)]
struct KfDrag {
    row: usize,
    /// Clip-relative tick of the dragged keyframe at gesture start.
    orig_at: i64,
    grip: Grip,
}

// ── Property rows (the animatable-property picker) ────────────────────────────

/// One animatable property of the targeted clip: its registry entry plus the
/// [`AnimTarget`] that addresses its keyframe lane.
struct PropRow {
    group: String,
    label: String,
    path: PropPath,
    target: AnimTarget,
    kind: PropValueKind,
    range: Option<(f64, f64)>,
}

/// Flatten a clip's animatable properties into picker rows: the eight
/// `transform.*` fields, then each effect's registry params (01 §6.2). Grade-op
/// automation is authored from the color page (07); the picker scope here is
/// "clip / effect / transform" per 13 §5.
fn build_rows(clip: &Clip) -> Vec<PropRow> {
    let mut rows = Vec::new();
    for e in prop_registry::entries(PropTargetKind::ClipTransform) {
        rows.push(PropRow {
            group: "Transform".to_string(),
            label: pretty_label(e.path.strip_prefix("transform.").unwrap_or(e.path)),
            path: PropPath::new(e.path),
            target: AnimTarget::ClipTransform { clip: clip.id },
            kind: e.kind,
            range: e.range,
        });
    }
    for (i, fx) in clip.effects.iter().enumerate() {
        let group = effect_name(fx.kind).to_string();
        for e in prop_registry::entries(PropTargetKind::Effect(fx.kind)) {
            rows.push(PropRow {
                group: group.clone(),
                label: pretty_label(e.path.strip_prefix("params.").unwrap_or(e.path)),
                path: PropPath::new(e.path),
                target: AnimTarget::ClipEffect {
                    clip: clip.id,
                    effect_index: i,
                },
                kind: e.kind,
                range: e.range,
            });
        }
    }
    // Proposal 210 / 211: clip gain is a first-class animatable lane for the
    // graph editor and on-clip envelope.
    if clip.audio.is_some() {
        for e in prop_registry::entries(PropTargetKind::ClipAudioParams) {
            rows.push(PropRow {
                group: "Audio".to_string(),
                label: pretty_label(e.path.strip_prefix("params.").unwrap_or(e.path)),
                path: PropPath::new(e.path),
                target: AnimTarget::ClipAudio { clip: clip.id },
                kind: e.kind,
                range: e.range,
            });
        }
    }
    rows
}

fn effect_name(kind: photonic_core::timeline::EffectKind) -> &'static str {
    use photonic_core::timeline::EffectKind as K;
    match kind {
        K::Blur => "Blur",
        K::Sharpen => "Sharpen",
        K::Glow => "Glow",
        K::ChromaKey => "Chroma Key",
        K::LumaKey => "Luma Key",
        K::Invert => "Invert",
        K::MaskShapeGen => "Mask Shape",
        _ => "Effect",
    }
}

/// Turn a registry leaf like `scale_x` / `slope[0]` into a display label.
fn pretty_label(leaf: &str) -> String {
    let mut out = String::with_capacity(leaf.len());
    let mut upper = true;
    for c in leaf.chars() {
        match c {
            '_' => {
                out.push(' ');
                upper = true;
            }
            c if upper => {
                out.extend(c.to_uppercase());
                upper = false;
            }
            c => out.push(c),
        }
    }
    out
}

// ── Reading current animated values (pure) ───────────────────────────────────

/// The keyframe lane for `(target, path)` on `clip`, if one exists.
fn track_for<'a>(
    clip: &'a Clip,
    target: &AnimTarget,
    path: &PropPath,
) -> Option<&'a PropertyTrack> {
    match target {
        AnimTarget::ClipTransform { .. } => clip.transform.track(path),
        AnimTarget::ClipEffect { effect_index, .. } => {
            clip.effects.get(*effect_index)?.params.track(path)
        }
        AnimTarget::ClipAudio { .. } => clip.audio.as_ref()?.params.track(path),
        _ => None,
    }
}

/// The static base value behind `(target, path)` — the value used when no
/// keyframe overrides it.
fn base_value(clip: &Clip, target: &AnimTarget, path: &PropPath) -> Option<PropValue> {
    match target {
        AnimTarget::ClipTransform { .. } => {
            let t = &clip.transform.base;
            let v = match path.as_str() {
                "transform.x" => t.x,
                "transform.y" => t.y,
                "transform.scale_x" => t.scale_x,
                "transform.scale_y" => t.scale_y,
                "transform.rotation" => t.rotation,
                "transform.anchor_x" => t.anchor_x,
                "transform.anchor_y" => t.anchor_y,
                "transform.opacity" => t.opacity,
                _ => return None,
            };
            Some(PropValue::Float(v))
        }
        AnimTarget::ClipEffect { effect_index, .. } => clip
            .effects
            .get(*effect_index)?
            .params
            .base
            .get(path.as_str())
            .copied(),
        AnimTarget::ClipAudio { .. } => {
            let g = clip.audio.as_ref()?.params.base.gain_db;
            match path.as_str() {
                "params.gain_db" | "gain_db" => Some(PropValue::Float(g)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The value of `(target, path)` at clip-relative tick `dt` — the keyframed
/// evaluation if a lane exists, else the base (mirrors the engine's read path,
/// 01 §6 / compile.rs).
fn value_at(clip: &Clip, target: &AnimTarget, path: &PropPath, dt: Tick) -> Option<PropValue> {
    let base = base_value(clip, target, path)?;
    match track_for(clip, target, path) {
        Some(tr) if !tr.keyframes.is_empty() => Some(anim::eval(tr, &base, dt)),
        _ => Some(base),
    }
}

/// Extract a scalar from a [`PropValue`] for the curve editor (Float only; other
/// kinds have no 1-D curve).
fn as_f64(v: &PropValue) -> Option<f64> {
    match v {
        PropValue::Float(f) => Some(*f),
        PropValue::Enum(e) => Some(*e as f64),
        _ => None,
    }
}

// ── Coordinate mapping (pure, unit-tested) ───────────────────────────────────

/// Clip-relative tick of the playhead, or `None` when the playhead is outside
/// the clip (so keyframes are only added within the clip's span).
fn playhead_local(playhead: Tick, clip: &Clip) -> Option<Tick> {
    let dt = playhead - clip.start;
    if dt.0 >= 0 && dt <= clip.duration {
        Some(dt)
    } else {
        None
    }
}

/// Map a clip-relative tick to a screen x inside `rect` (x-axis = `[0, dur]`).
fn tick_to_x(at: Tick, dur: Tick, rect: Rect) -> f32 {
    let d = dur.0.max(1) as f32;
    rect.left() + (at.0 as f32 / d) * rect.width()
}

/// Inverse of [`tick_to_x`]: screen x → clip-relative tick, clamped to `[0, dur]`.
fn x_to_tick(x: f32, dur: Tick, rect: Rect) -> Tick {
    let d = dur.0.max(1) as f32;
    let frac = ((x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
    Tick((frac * d).round() as i64)
}

/// Map a value to a screen y inside `rect` (value axis inverted: `vmax` at top).
fn value_to_y(v: f64, vmin: f64, vmax: f64, rect: Rect) -> f32 {
    let span = (vmax - vmin).abs().max(1e-9);
    let frac = ((v - vmin) / span).clamp(0.0, 1.0) as f32;
    rect.bottom() - frac * rect.height()
}

/// Inverse of [`value_to_y`].
fn y_to_value(y: f32, vmin: f64, vmax: f64, rect: Rect) -> f64 {
    let frac = ((rect.bottom() - y) / rect.height().max(1.0)).clamp(0.0, 1.0) as f64;
    vmin + frac * (vmax - vmin)
}

/// The value axis range for a Float lane: the registry range if bounded, else an
/// auto-fit over the keyframe values (with padding) so an unbounded property
/// (position, rotation) still frames its curve.
fn value_range(range: Option<(f64, f64)>, track: Option<&PropertyTrack>, base: f64) -> (f64, f64) {
    if let Some((lo, hi)) = range {
        if hi > lo {
            return (lo, hi);
        }
    }
    let mut lo = base;
    let mut hi = base;
    if let Some(tr) = track {
        for k in &tr.keyframes {
            if let Some(v) = as_f64(&k.value) {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
    }
    if (hi - lo).abs() < 1e-6 {
        // Flat curve: give it a symmetric window around the value.
        let pad = if base.abs() > 1.0 { base.abs() } else { 1.0 };
        (base - pad, base + pad)
    } else {
        let pad = (hi - lo) * 0.15;
        (lo - pad, hi + pad)
    }
}

/// Nearest point within `radius` px of `pos`, returning its index.
fn hit_index(points: &[Pos2], pos: Pos2, radius: f32) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, p) in points.iter().enumerate() {
        let d = p.distance(pos);
        if d <= radius && best.map(|(_, bd)| d < bd).unwrap_or(true) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

/// Screen positions of a Bezier segment's two control handles, in the segment's
/// `[k0..k1]` time×value box. Handles are normalized `[0,1]²` (CSS-like), so
/// `out_handle` sits near `k0` and `in_handle` near `k1`.
fn handle_positions(
    k0: (f32, f32),
    k1: (f32, f32),
    out_handle: [f64; 2],
    in_handle: [f64; 2],
) -> (Pos2, Pos2) {
    let lerp = |a: f32, b: f32, t: f64| a + (b - a) * t as f32;
    let out = Pos2::new(
        lerp(k0.0, k1.0, out_handle[0]),
        lerp(k0.1, k1.1, out_handle[1]),
    );
    let inp = Pos2::new(
        lerp(k0.0, k1.0, in_handle[0]),
        lerp(k0.1, k1.1, in_handle[1]),
    );
    (out, inp)
}

/// Recover a normalized handle `[x,y]` from a dragged screen position within the
/// segment box `k0..k1`; x is clamped to `[0,1]` (CSS-timing rule), y is free but
/// bounded so a fat-fingered drag can't fling the curve to infinity.
fn handle_from_pos(pos: Pos2, k0: (f32, f32), k1: (f32, f32)) -> [f64; 2] {
    let dx = (k1.0 - k0.0) as f64;
    let dy = (k1.1 - k0.1) as f64;
    let nx = if dx.abs() < 1e-6 {
        0.0
    } else {
        ((pos.x - k0.0) as f64 / dx).clamp(0.0, 1.0)
    };
    let ny = if dy.abs() < 1e-6 {
        0.0
    } else {
        ((pos.y - k0.1) as f64 / dy).clamp(-1.0, 2.0)
    };
    [nx, ny]
}

/// Snap a clip-relative tick to the frame grid.
fn snap_frame(at: Tick, fps: FrameRate) -> Tick {
    let tpf = fps.ticks_per_frame().0.max(1);
    Tick(((at.0 + tpf / 2) / tpf) * tpf)
}

// ── Ease presets (26 §10 K-B12) ──────────────────────────────────────────────

/// Label shown for a Bezier curve that matches no named preset.
const CUSTOM_EASE_LABEL: &str = "Custom";

/// The readout for a keyframe's outgoing easing: the preset name when the stored
/// handles are one of [`anim::EasePreset::ALL`], else `Custom` (a hand-dragged
/// curve). Never lies about which preset is applied — the name comes from
/// [`anim::EasePreset::from_interp`], i.e. from the stored handles.
fn ease_readout(interp: &Interp) -> String {
    match anim::EasePreset::from_interp(interp) {
        Some(p) => format!("{} · {}", p.label(), p.css()),
        None => match interp {
            Interp::Bezier {
                out_handle: [x1, y1],
                in_handle: [x2, y2],
            } => format!("{CUSTOM_EASE_LABEL} · cubic-bezier({x1:.3}, {y1:.3}, {x2:.3}, {y2:.3})"),
            // Hold/Linear always resolve to a preset, so this is unreachable in
            // practice; keep it total rather than panicking in a paint path.
            _ => CUSTOM_EASE_LABEL.to_string(),
        },
    }
}

/// One preset row in the toolbar / context menu: is it the preset currently
/// applied to `interp`?
///
/// Compares *preset identity*, not the `Interp` variant. Every eased preset is
/// an `Interp::Bezier`, so a variant-level comparison lights up all four at once
/// — the defect this replaced, pinned by
/// `ease_toolbar_marks_exactly_one_preset_active`.
fn preset_is_active(interp: Option<&Interp>, preset: anim::EasePreset) -> bool {
    interp
        .and_then(anim::EasePreset::from_interp)
        .map(|p| p == preset)
        .unwrap_or(false)
}

/// Hover text for a preset button: the name plus the exact handles it writes, so
/// the curve a preset applies is inspectable before it is committed.
fn preset_hover(preset: anim::EasePreset) -> String {
    match preset.handles() {
        Some(_) => format!("{} — {}", preset.label(), preset.css()),
        None => format!("{} — {} (no Bezier handles)", preset.label(), preset.css()),
    }
}

// ── Pending edits → CommandHistory ───────────────────────────────────────────

/// An edit the UI decided to make this frame, applied after rendering releases
/// the (read-only) borrow of the cloned clip.
enum KfAction {
    Set {
        target: AnimTarget,
        path: PropPath,
        kf: Keyframe,
    },
    Move {
        target: AnimTarget,
        path: PropPath,
        old_at: Tick,
        kf: Keyframe,
    },
    Remove {
        target: AnimTarget,
        path: PropPath,
        at: Tick,
    },
    SetInterp {
        target: AnimTarget,
        path: PropPath,
        at: Tick,
        interp: Interp,
    },
    /// K-B11: paste a keyframe clipboard onto `target` (identity mapping).
    /// `reanchor: Some(t)` lands the clipboard anchor at `t`; `None` uses
    /// offset 0.
    PasteKeyframes {
        target: AnimTarget,
        clipboard: KeyframeClipboard,
        reanchor: Option<Tick>,
    },
}

fn commit(history: &mut CommandHistory, doc: &mut Document, cmd: TimelineCmd) {
    history.execute_discrete(Command::Timeline(cmd), doc);
}

/// Resolve one [`KfAction`] into the `TimelineCmd`(s) it applies: one for
/// `Set`/`Remove`/`SetInterp`, two (remove old + set new, mirroring a `Move`
/// gesture's time change) for a `Move`. Pure — reads `project`, never mutates
/// it — so both [`apply_action`] (`draw_window`, commits via
/// `CommandHistory`) and [`kf_actions_to_panel_action`] (`draw_embedded`,
/// returns a `PanelAction`) build on this one conversion instead of
/// duplicating the ops-building logic per surface.
fn kf_action_cmds(project: &TimelineProject, action: KfAction) -> Vec<TimelineCmd> {
    match action {
        KfAction::Set { target, path, kf } => vec![ops::set_keyframe(project, target, path, kf)],
        KfAction::Remove { target, path, at } => ops::remove_keyframe(project, target, path, at)
            .into_iter()
            .collect(),
        KfAction::SetInterp {
            target,
            path,
            at,
            interp,
        } => ops::set_keyframe_interp(project, target, path, at, interp)
            .into_iter()
            .collect(),
        KfAction::PasteKeyframes {
            target,
            clipboard,
            reanchor,
        } => {
            let result = match reanchor {
                Some(at) => ops::paste_keyframes_reanchored(project, target, &clipboard, &[], at),
                None => ops::paste_keyframes(project, target, &clipboard, &[], Tick::ZERO),
            };
            result.unwrap_or_default()
        }
        KfAction::Move {
            target,
            path,
            old_at,
            kf,
        } => {
            if old_at == kf.at {
                return kf_action_cmds(project, KfAction::Set { target, path, kf });
            }
            // Move in time = remove the old lane entry + set the new one, as ONE
            // undo step (both built against the pre-move project state).
            let Ok(rm) = ops::remove_keyframe(project, target.clone(), path.clone(), old_at) else {
                return Vec::new();
            };
            let set = ops::set_keyframe(project, target, path, kf);
            vec![rm, set]
        }
    }
}

/// Apply one collected [`KfAction`] as a single undo step against `history`
/// (the `draw_window` commit path — never mutates `doc.timeline` in place).
fn apply_action(doc: &mut Document, history: &mut CommandHistory, action: KfAction) {
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    let cmds = kf_action_cmds(p, action);
    apply_cmds(doc, history, cmds);
}

/// Commit `cmds` as ONE undo step: a lone command via [`commit`], or a
/// `Command::Batch` when a gesture (e.g. `Move`) produced more than one —
/// same one-step-per-gesture discipline the module doc comment promises.
fn apply_cmds(doc: &mut Document, history: &mut CommandHistory, cmds: Vec<TimelineCmd>) {
    match cmds.len() {
        0 => {}
        1 => commit(history, doc, cmds.into_iter().next().expect("len==1")),
        _ => {
            let batch = Command::Batch(cmds.into_iter().map(Command::Timeline).collect());
            history.execute_discrete(batch, doc);
        }
    }
}

/// The [`draw_embedded`] counterpart to [`apply_action`]: convert this
/// frame's collected [`KfAction`]s into a single [`PanelAction`] for the
/// `PropPanelCtx` boundary (`draw_embedded` has no `&mut CommandHistory` —
/// see `clip_inspector.rs`'s module doc) instead of committing directly. In
/// practice at most one `KfAction` fires per frame (a click or a
/// drag-release), so the batch branch only fires for a `Move` gesture's
/// remove+set pair.
fn kf_actions_to_panel_action(
    project: &TimelineProject,
    actions: Vec<KfAction>,
) -> Option<PanelAction> {
    let mut cmds: Vec<TimelineCmd> = Vec::new();
    for a in actions {
        cmds.extend(kf_action_cmds(project, a));
    }
    match cmds.len() {
        0 => None,
        1 => Some(PanelAction::ClipEditDiscrete(
            cmds.into_iter().next().expect("len==1"),
        )),
        _ => Some(PanelAction::ClipEditBatch(cmds)),
    }
}

// ── Clip lookup ──────────────────────────────────────────────────────────────

fn find_clip(p: &TimelineProject, id: ClipId) -> Option<&Clip> {
    p.sequences
        .values()
        .flat_map(|s| s.tracks())
        .flat_map(|t| t.clips.iter())
        .find(|c| c.id == id)
}

fn find_clip_fps(p: &TimelineProject, id: ClipId) -> Option<FrameRate> {
    for s in p.sequences.values() {
        if s.tracks().any(|t| t.clips.iter().any(|c| c.id == id)) {
            return Some(s.frame_rate);
        }
    }
    None
}

// ── egui memory helpers ──────────────────────────────────────────────────────

fn load_state(ctx: &egui::Context) -> EditorState {
    ctx.data_mut(|d| d.get_temp::<EditorState>(egui::Id::new(STATE_ID)))
        .unwrap_or_default()
}

fn save_state(ctx: &egui::Context, st: EditorState) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(STATE_ID), st));
}

/// Reset the row/keyframe selection when the targeted clip changes, and clamp
/// `sel_row` to the (possibly shrunk) row list. Shared by [`draw_window`] and
/// [`draw_embedded`] — both surfaces read the same [`EditorState`] blob (see
/// `draw_embedded`'s doc comment for why that's correct, not a state-sync
/// bug), so this keeps "what changed" agreement between them in one place
/// (13 §5.2).
fn sync_state_for_clip(st: &mut EditorState, clip_id: ClipId, rows_len: usize) {
    if st.clip != Some(clip_id) {
        st.clip = Some(clip_id);
        st.sel_row = 0;
        st.sel_kf_at = None;
    }
    if st.sel_row >= rows_len {
        st.sel_row = 0;
    }
}

// ── Public: floating curve editor window ─────────────────────────────────────

/// The floating keyframe / curve editor (04 §4.1). No longer auto-*opens* on
/// a timeline-selection change (G-9 — that job now belongs to the
/// always-visible [`draw_embedded`] section in `clip_inspector.rs`); it only
/// *follows* the selection to stay in sync while the user already has it
/// open (so a popped-out window doesn't go stale showing a deselected clip).
/// Opened by an explicit `target = Some(clip_id)` write — a keyframe-diamond
/// click or the docked section's pop-out button (both in `clip_inspector.rs`)
/// — and the window's close button clears it. All state lives in egui memory
/// + the passed `target` field, so no new `PhotonicApp` fields are required.
pub(crate) fn draw_window(
    ctx: &egui::Context,
    doc: &mut Document,
    history: &mut CommandHistory,
    target: &mut Option<ClipId>,
    selection: &[ClipId],
    playhead: Tick,
) {
    let mut st = load_state(ctx);

    // Follow the timeline selection while already open; never auto-open.
    let sel = selection.first().copied();
    if !st.seeded {
        st.last_selection = sel;
        st.seeded = true;
    } else if sel != st.last_selection {
        st.last_selection = sel;
        if target.is_some() {
            *target = sel;
        }
    }

    let Some(clip_id) = *target else {
        save_state(ctx, st);
        return;
    };

    // Clone the targeted clip + its frame rate so rendering holds no borrow of
    // `doc` while we later mutate it through history.
    let resolved = doc.timeline.as_ref().and_then(|p| {
        find_clip(p, clip_id)
            .cloned()
            .zip(find_clip_fps(p, clip_id))
    });
    let Some((clip, fps)) = resolved else {
        *target = None;
        save_state(ctx, st);
        return;
    };

    let rows = build_rows(&clip);
    sync_state_for_clip(&mut st, clip_id, rows.len());

    let mut actions: Vec<KfAction> = Vec::new();
    let mut open = true;

    egui::Window::new(format!("{}  Keyframes", ph::CHART_LINE))
        .id(egui::Id::new("photonic_kf_editor_window"))
        .open(&mut open)
        .resizable(true)
        .collapsible(true)
        .default_size(Vec2::new(560.0, 300.0))
        .min_width(360.0)
        .min_height(200.0)
        .show(ctx, |ui| {
            render_editor(ui, &clip, fps, playhead, &rows, &mut st, &mut actions);
        });

    if !open {
        *target = None;
    }
    save_state(ctx, st);

    for a in actions {
        apply_action(doc, history, a);
    }
}

// ── Public: docked curve editor (Clip Inspector section, G-9) ───────────────

/// Fixed heights for [`draw_embedded`]'s two stacked regions. `egui`
/// panels/scroll-areas need an explicit size when nested inside another
/// panel's own scroll area (the Clip Inspector body already scrolls) — else
/// `available_size()` reports the (effectively unbounded) remaining scroll
/// content height and the section balloons to fill it.
const EMBEDDED_PICKER_HEIGHT: f32 = 110.0;
const EMBEDDED_CURVE_HEIGHT: f32 = 190.0;

/// `PropPanelCtx` (`clip_inspector.rs`'s boundary) carries no live playhead —
/// threading one through would touch `panels/mod.rs` + `app/mod.rs`, outside
/// this story's territory. Returning a tick one before `clip.start` makes
/// every `playhead_local` call below resolve to "off-clip" deterministically
/// (no arithmetic overflow risk, unlike an extreme sentinel), so the picker's
/// live-value column reads `—` instead of a wrong/stale number, and the
/// add/remove-at-playhead diamond click + playhead marker line are simply
/// inert — the floating window (which *does* get the real playhead from
/// `app/timeline/mod.rs`) remains the live-playhead surface; see the pop-out
/// button in `clip_inspector.rs`'s "Keyframes" section.
fn no_live_playhead(clip: &Clip) -> Tick {
    Tick(clip.start.0 - 1)
}

/// The docked keyframe/curve editor (G-9 effect-controls unification): the
/// same [`draw_prop_picker`] + [`draw_curve_area`] drawing `draw_window` uses
/// for its side-by-side floating layout, stacked vertically instead (picker
/// above curve) to fit the narrow left-rail Clip Inspector. Called from
/// `clip_inspector.rs`'s "Keyframes" collapsible section for the selected
/// clip; full curve editing (drag keyframes, Bezier handles, double-click
/// add, right-click ease/delete, Del key) works exactly as in the floating
/// window — only the live-playhead value readout and add/remove-at-playhead
/// diamond are inert here (no playhead at this boundary; see
/// [`no_live_playhead`]).
///
/// Shares [`EditorState`] (egui memory, `STATE_ID`) with `draw_window` rather
/// than keeping separate state: both surfaces always target the same clip
/// when the float is open (`draw_window` follows the selection while open,
/// see its doc comment), so one shared row/keyframe selection is correct —
/// popping out mid-edit doesn't lose "which property/keyframe was selected".
pub(crate) fn draw_embedded(
    ui: &mut egui::Ui,
    project: &TimelineProject,
    clip: &Clip,
) -> Option<PanelAction> {
    let rows = build_rows(clip);
    if rows.is_empty() {
        ui.label(
            egui::RichText::new("No animatable properties on this clip.")
                .weak()
                .small(),
        );
        return None;
    }

    let fps = find_clip_fps(project, clip.id).unwrap_or(FrameRate::FPS_30);
    let playhead = no_live_playhead(clip);

    let mut st = load_state(ui.ctx());
    sync_state_for_clip(&mut st, clip.id, rows.len());

    ui.label(
        egui::RichText::new(
            "Curve editing is fully live here; live playhead value + add/remove-at-playhead \
             need the pop-out window.",
        )
        .weak()
        .small(),
    );

    let mut actions: Vec<KfAction> = Vec::new();

    let picker_w = ui.available_width();
    ui.allocate_ui(Vec2::new(picker_w, EMBEDDED_PICKER_HEIGHT), |ui| {
        draw_prop_picker(ui, clip, playhead, &rows, &mut st, &mut actions);
    });

    ui.separator();

    let curve_w = ui.available_width();
    ui.allocate_ui(Vec2::new(curve_w, EMBEDDED_CURVE_HEIGHT), |ui| {
        draw_curve_area(ui, clip, fps, playhead, &rows, &mut st, &mut actions);
    });

    save_state(ui.ctx(), st);

    kf_actions_to_panel_action(project, actions)
}

#[allow(clippy::too_many_arguments)]
fn render_editor(
    ui: &mut egui::Ui,
    clip: &Clip,
    fps: FrameRate,
    playhead: Tick,
    rows: &[PropRow],
    st: &mut EditorState,
    actions: &mut Vec<KfAction>,
) {
    // Header: clip name + local playhead readout.
    ui.horizontal(|ui| {
        let name = if clip.name.is_empty() {
            "Clip"
        } else {
            &clip.name
        };
        ui.label(egui::RichText::new(name).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            match playhead_local(playhead, clip) {
                Some(dt) => ui.label(
                    egui::RichText::new(format!("t {:.2}s", dt.0 as f64 / 30_000.0))
                        .monospace()
                        .weak(),
                ),
                None => ui.label(egui::RichText::new("playhead off-clip").weak().small()),
            };
        });
    });
    // K-B11: copy / paste keyframes for the selected property row (or all
    // tracks of its AnimTarget). Clipboard lives in egui memory (session).
    draw_interchange_bar(ui, clip, rows, st, actions);
    ui.separator();

    egui::SidePanel::left("kf_prop_picker")
        .resizable(true)
        .default_width(190.0)
        .min_width(140.0)
        .show_inside(ui, |ui| {
            draw_prop_picker(ui, clip, playhead, rows, st, actions);
        });

    egui::CentralPanel::default().show_inside(ui, |ui| {
        draw_curve_area(ui, clip, fps, playhead, rows, st, actions);
    });
}

/// K-B11 toolbar: copy the selected row's track (or every track on its target)
/// and paste with identity mapping / re-anchor to clip start or playhead-local.
fn draw_interchange_bar(
    ui: &mut egui::Ui,
    clip: &Clip,
    rows: &[PropRow],
    st: &EditorState,
    actions: &mut Vec<KfAction>,
) {
    let row = rows.get(st.sel_row);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Interchange").small().weak());
        let copy_one = ui
            .add_enabled(row.is_some(), egui::Button::new("Copy path").small())
            .on_hover_text("Copy the selected property's keyframes (K-B11)");
        let copy_all = ui
            .add_enabled(row.is_some(), egui::Button::new("Copy all").small())
            .on_hover_text("Copy every keyframed track on this target");
        if let Some(row) = row {
            if copy_one.clicked() {
                if let Some(tr) = track_for(clip, &row.target, &row.path) {
                    if !tr.keyframes.is_empty() {
                        let board = KeyframeClipboard::from_tracks(vec![tr.clone()]);
                        ui.ctx().data_mut(|d| {
                            d.insert_temp(egui::Id::new(KF_CLIPBOARD_ID), board);
                        });
                    }
                }
            }
            if copy_all.clicked() {
                // Collect non-empty tracks that share this row's AnimTarget.
                let tracks: Vec<PropertyTrack> = rows
                    .iter()
                    .filter(|r| r.target == row.target)
                    .filter_map(|r| track_for(clip, &r.target, &r.path).cloned())
                    .filter(|t| !t.keyframes.is_empty())
                    .collect();
                if !tracks.is_empty() {
                    let board = KeyframeClipboard::from_tracks(tracks);
                    ui.ctx().data_mut(|d| {
                        d.insert_temp(egui::Id::new(KF_CLIPBOARD_ID), board);
                    });
                }
            }
        }

        let has_clip = ui
            .ctx()
            .data(|d| d.get_temp::<KeyframeClipboard>(egui::Id::new(KF_CLIPBOARD_ID)))
            .map(|b| !b.is_empty())
            .unwrap_or(false);
        let paste = ui
            .add_enabled(
                has_clip && row.is_some(),
                egui::Button::new("Paste").small(),
            )
            .on_hover_text("Paste clipboard onto the selected target (identity paths, offset 0)");
        let paste_re = ui
            .add_enabled(
                has_clip && row.is_some(),
                egui::Button::new("Paste @0").small(),
            )
            .on_hover_text("Paste re-anchored so the first key lands at clip start");
        if let Some(row) = row {
            if paste.clicked() {
                if let Some(board) = ui
                    .ctx()
                    .data(|d| d.get_temp::<KeyframeClipboard>(egui::Id::new(KF_CLIPBOARD_ID)))
                {
                    actions.push(KfAction::PasteKeyframes {
                        target: row.target.clone(),
                        clipboard: board,
                        reanchor: None,
                    });
                }
            }
            if paste_re.clicked() {
                if let Some(board) = ui
                    .ctx()
                    .data(|d| d.get_temp::<KeyframeClipboard>(egui::Id::new(KF_CLIPBOARD_ID)))
                {
                    actions.push(KfAction::PasteKeyframes {
                        target: row.target.clone(),
                        clipboard: board,
                        reanchor: Some(Tick::ZERO),
                    });
                }
            }
        }
    });
}

/// Left column: the animatable-property picker with a keyframe-diamond toggle
/// per row (13 §5.2 — filled when keyframed, hollow otherwise, `primary` ring
/// when interpolating at the current tick).
fn draw_prop_picker(
    ui: &mut egui::Ui,
    clip: &Clip,
    playhead: Tick,
    rows: &[PropRow],
    st: &mut EditorState,
    actions: &mut Vec<KfAction>,
) {
    let local = playhead_local(playhead, clip);
    let accent = ui.visuals().selection.stroke.color;
    let muted = ui.visuals().weak_text_color();

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut last_group: Option<&str> = None;
            for (i, row) in rows.iter().enumerate() {
                if last_group != Some(row.group.as_str()) {
                    if last_group.is_some() {
                        ui.add_space(4.0);
                    }
                    ui.label(
                        egui::RichText::new(row.group.to_uppercase())
                            .small()
                            .color(muted),
                    );
                    last_group = Some(row.group.as_str());
                }

                let track = track_for(clip, &row.target, &row.path);
                let has_any = track.map(|t| !t.keyframes.is_empty()).unwrap_or(false);
                let at_dt = local.and_then(|dt| {
                    track.and_then(|t| t.keyframes.iter().find(|k| k.at == dt).map(|_| dt))
                });
                let interpolating = has_any
                    && local
                        .map(|dt| within_track_span(track.unwrap(), dt))
                        .unwrap_or(false);

                ui.horizontal(|ui| {
                    // Keyframe diamond toggle (add/remove at playhead).
                    let (drect, dresp) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::click());
                    paint_diamond(
                        ui.painter(),
                        drect.center(),
                        5.0,
                        has_any,
                        interpolating,
                        accent,
                        muted,
                    );
                    if local.is_some() {
                        dresp.clone().on_hover_text(if at_dt.is_some() {
                            "Remove keyframe at playhead"
                        } else {
                            "Add keyframe at playhead"
                        });
                    }
                    if dresp.clicked() {
                        if let Some(dt) = local {
                            if let Some(at) = at_dt {
                                actions.push(KfAction::Remove {
                                    target: row.target.clone(),
                                    path: row.path.clone(),
                                    at,
                                });
                            } else if let Some(v) = value_at(clip, &row.target, &row.path, dt) {
                                actions.push(KfAction::Set {
                                    target: row.target.clone(),
                                    path: row.path.clone(),
                                    kf: Keyframe::new(dt, v, default_interp(row.kind)),
                                });
                            }
                        }
                    }

                    // Selectable property label.
                    let selected = st.sel_row == i;
                    if ui.selectable_label(selected, &row.label).clicked() {
                        st.sel_row = i;
                        st.sel_kf_at = None;
                    }

                    // Current value readout (right-aligned, mono).
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let text = local
                            .and_then(|dt| value_at(clip, &row.target, &row.path, dt))
                            .map(fmt_value)
                            .unwrap_or_else(|| "—".to_string());
                        ui.label(egui::RichText::new(text).monospace().small().color(muted));
                    });
                });
            }
        });
}

/// Whether `dt` lies within `[first.at, last.at]` — i.e. the property is being
/// actively interpolated (not merely keyframed somewhere).
fn within_track_span(track: &PropertyTrack, dt: Tick) -> bool {
    match (track.keyframes.first(), track.keyframes.last()) {
        (Some(f), Some(l)) => dt >= f.at && dt <= l.at,
        _ => false,
    }
}

fn default_interp(kind: PropValueKind) -> Interp {
    // Non-interpolable kinds always step; seed them Hold so the declared interp
    // matches the eval behaviour (01 §6).
    match kind {
        PropValueKind::Bool | PropValueKind::Enum => Interp::Hold,
        _ => Interp::Linear,
    }
}

fn fmt_value(v: PropValue) -> String {
    match v {
        PropValue::Float(f) => format!("{f:.3}"),
        PropValue::Vec2([x, y]) => format!("{x:.2},{y:.2}"),
        PropValue::Color(c) => format!("#{:02X}{:02X}{:02X}", to_u8(c.r), to_u8(c.g), to_u8(c.b)),
        PropValue::Bool(b) => if b { "on" } else { "off" }.to_string(),
        PropValue::Enum(e) => e.to_string(),
    }
}

fn to_u8(f: f32) -> u8 {
    (f.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Draw a keyframe diamond: filled when the property is keyframed, hollow
/// outline otherwise, with a `primary` ring when interpolating (13 §5.2).
fn paint_diamond(
    painter: &egui::Painter,
    c: Pos2,
    r: f32,
    filled: bool,
    ring: bool,
    accent: Color32,
    muted: Color32,
) {
    let pts = vec![
        Pos2::new(c.x, c.y - r),
        Pos2::new(c.x + r, c.y),
        Pos2::new(c.x, c.y + r),
        Pos2::new(c.x - r, c.y),
    ];
    if filled {
        painter.add(Shape::convex_polygon(pts.clone(), accent, Stroke::NONE));
    } else {
        painter.add(Shape::convex_polygon(
            pts.clone(),
            Color32::TRANSPARENT,
            Stroke::new(1.0, muted),
        ));
    }
    if ring {
        painter.add(Shape::convex_polygon(
            pts,
            Color32::TRANSPARENT,
            Stroke::new(1.5, accent),
        ));
    }
}

/// Right column: the curve editor plot for the selected property + an ease
/// toolbar. Float properties get the full time×value curve with draggable points
/// and Bezier handles; other kinds fall back to a message (their keyframes are
/// still toggleable from the picker diamond).
#[allow(clippy::too_many_arguments)]
fn draw_curve_area(
    ui: &mut egui::Ui,
    clip: &Clip,
    fps: FrameRate,
    playhead: Tick,
    rows: &[PropRow],
    st: &mut EditorState,
    actions: &mut Vec<KfAction>,
) {
    let Some(row) = rows.get(st.sel_row) else {
        ui.weak("No animatable property.");
        return;
    };

    // Ease toolbar — sets the selected keyframe's outgoing interpolation.
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(&row.label).strong());
        ui.separator();
        let sel_at = st.sel_kf_at.map(Tick);
        let cur_interp = sel_at.and_then(|at| {
            track_for(clip, &row.target, &row.path)
                .and_then(|t| t.keyframes.iter().find(|k| k.at == at).map(|k| k.interp))
        });
        ui.add_enabled_ui(sel_at.is_some(), |ui| {
            for preset in anim::EasePreset::ALL {
                let active = preset_is_active(cur_interp.as_ref(), *preset);
                if ui
                    .selectable_label(active, preset.label())
                    .on_hover_text(preset_hover(*preset))
                    .clicked()
                {
                    if let Some(at) = sel_at {
                        actions.push(KfAction::SetInterp {
                            target: row.target.clone(),
                            path: row.path.clone(),
                            at,
                            interp: preset.interp(),
                        });
                    }
                }
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(sel_at.is_some(), egui::Button::new(ph::TRASH).small())
                .on_hover_text("Delete selected keyframe (Del)")
                .clicked()
            {
                if let Some(at) = sel_at {
                    actions.push(KfAction::Remove {
                        target: row.target.clone(),
                        path: row.path.clone(),
                        at,
                    });
                    st.sel_kf_at = None;
                }
            }
            // Which easing the selected keyframe actually carries — the preset
            // name, or `Custom · cubic-bezier(…)` for hand-dragged handles, so a
            // curve the toolbar cannot highlight still reads out (K-B12).
            if let Some(i) = cur_interp.as_ref() {
                ui.label(
                    egui::RichText::new(ease_readout(i))
                        .monospace()
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            }
        });
    });

    if !matches!(row.kind, PropValueKind::Float) {
        ui.centered_and_justified(|ui| {
            ui.weak(format!(
                "{:?} keyframes — toggle from the diamond; curve editing is Float-only in v1.",
                row.kind
            ));
        });
        return;
    }

    draw_float_curve(ui, clip, fps, playhead, row, st, actions);
}

/// The Float curve editor: grid, playhead, keyframe diamonds, interpolation
/// polyline, Bezier handles for the selected keyframe, and pointer editing
/// (double-click add / drag move / right-click menu). Drags preview a ghost and
/// commit ONE step on release.
#[allow(clippy::too_many_arguments)]
fn draw_float_curve(
    ui: &mut egui::Ui,
    clip: &Clip,
    fps: FrameRate,
    playhead: Tick,
    row: &PropRow,
    st: &mut EditorState,
    actions: &mut Vec<KfAction>,
) {
    let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let v = ui.visuals();
    let accent = v.selection.stroke.color;
    let grid = v.widgets.noninteractive.bg_stroke.color;
    let text = v.text_color();
    let muted = v.weak_text_color();

    let track = track_for(clip, &row.target, &row.path);
    let base = base_value(clip, &row.target, &row.path)
        .and_then(|b| as_f64(&b))
        .unwrap_or(0.0);
    let (vmin, vmax) = value_range(row.range, track, base);
    let dur = clip.duration;

    // Backdrop + border.
    painter.rect_filled(rect, Rounding::same(3.0), v.extreme_bg_color);
    painter.rect_stroke(rect, Rounding::same(3.0), Stroke::new(1.0, grid));

    // Horizontal value gridlines + labels (min / mid / max).
    for frac in [0.0f32, 0.5, 1.0] {
        let y = rect.bottom() - frac * rect.height();
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(1.0, grid.gamma_multiply(0.5)),
        );
        let val = vmin + frac as f64 * (vmax - vmin);
        painter.text(
            Pos2::new(rect.left() + 3.0, y),
            Align2::LEFT_CENTER,
            format!("{val:.2}"),
            FontId::monospace(9.0),
            muted,
        );
    }

    // Interpolation polyline (sample eval across the clip span).
    if let Some(tr) = track.filter(|t| !t.keyframes.is_empty()) {
        let base_pv = PropValue::Float(base);
        let steps = (rect.width() as usize).clamp(16, 512);
        let mut poly = Vec::with_capacity(steps + 1);
        for s in 0..=steps {
            let at = Tick((s as i64 * dur.0.max(1)) / steps as i64);
            if let Some(val) = as_f64(&anim::eval(tr, &base_pv, at)) {
                poly.push(Pos2::new(
                    tick_to_x(at, dur, rect),
                    value_to_y(val, vmin, vmax, rect),
                ));
            }
        }
        painter.add(Shape::line(poly, Stroke::new(1.5, accent)));
    }

    // Playhead marker.
    if let Some(dt) = playhead_local(playhead, clip) {
        let x = tick_to_x(dt, dur, rect);
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.0, accent.gamma_multiply(0.7)),
        );
    }

    // Keyframe screen points.
    let kfs: Vec<Keyframe> = track.map(|t| t.keyframes.clone()).unwrap_or_default();
    let points: Vec<Pos2> = kfs
        .iter()
        .filter_map(|k| {
            as_f64(&k.value).map(|val| {
                Pos2::new(
                    tick_to_x(k.at, dur, rect),
                    value_to_y(val, vmin, vmax, rect),
                )
            })
        })
        .collect();

    // ── Pointer editing ──────────────────────────────────────────────────────
    let drag_id = egui::Id::new(DRAG_ID);
    let hover = resp.hover_pos();

    // Selected keyframe's Bezier handle screen positions (if applicable).
    let sel_idx = st
        .sel_kf_at
        .and_then(|at| kfs.iter().position(|k| k.at.0 == at));
    let handles = sel_idx.and_then(|i| {
        let k0 = kfs.get(i)?;
        let _next = kfs.get(i + 1)?; // segment exists ⇒ handles are meaningful
        if let Interp::Bezier {
            out_handle,
            in_handle,
        } = k0.interp
        {
            let p0 = (points[i].x, points[i].y);
            let p1 = (points[i + 1].x, points[i + 1].y);
            Some((i, handle_positions(p0, p1, out_handle, in_handle)))
        } else {
            None
        }
    });

    // Begin a drag: prefer a handle grip, else a keyframe point.
    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let mut started = None;
            if let Some((hi, (out_p, in_p))) = handles {
                if out_p.distance(pos) <= 7.0 {
                    started = Some(KfDrag {
                        row: st.sel_row,
                        orig_at: kfs[hi].at.0,
                        grip: Grip::HandleOut,
                    });
                } else if in_p.distance(pos) <= 7.0 {
                    started = Some(KfDrag {
                        row: st.sel_row,
                        orig_at: kfs[hi].at.0,
                        grip: Grip::HandleIn,
                    });
                }
            }
            if started.is_none() {
                if let Some(i) = hit_index(&points, pos, 8.0) {
                    st.sel_kf_at = Some(kfs[i].at.0);
                    started = Some(KfDrag {
                        row: st.sel_row,
                        orig_at: kfs[i].at.0,
                        grip: Grip::Point,
                    });
                }
            }
            if let Some(d) = started {
                ui.data_mut(|m| m.insert_temp(drag_id, d));
            }
        }
    }

    let active_drag = ui.data(|m| m.get_temp::<KfDrag>(drag_id));

    // Preview during drag.
    if resp.dragged() {
        if let (Some(d), Some(pos)) = (active_drag, resp.interact_pointer_pos()) {
            if d.row == st.sel_row {
                match d.grip {
                    Grip::Point => {
                        let mut at = snap_frame(x_to_tick(pos.x, dur, rect), fps);
                        at = Tick(at.0.clamp(0, dur.0));
                        let val = y_to_value(pos.y, vmin, vmax, rect);
                        let gx = tick_to_x(at, dur, rect);
                        let gy = value_to_y(val, vmin, vmax, rect);
                        paint_diamond(&painter, Pos2::new(gx, gy), 6.0, true, true, accent, muted);
                        painter.text(
                            Pos2::new(gx + 8.0, gy - 8.0),
                            Align2::LEFT_BOTTOM,
                            format!("{val:.2}"),
                            FontId::monospace(9.0),
                            text,
                        );
                    }
                    Grip::HandleOut | Grip::HandleIn => {
                        if let Some(i) = kfs.iter().position(|k| k.at.0 == d.orig_at) {
                            if let (Some(&p0), Some(&p1)) = (points.get(i), points.get(i + 1)) {
                                let h = handle_from_pos(pos, (p0.x, p0.y), (p1.x, p1.y));
                                let hp = Pos2::new(
                                    p0.x + (p1.x - p0.x) * h[0] as f32,
                                    p0.y + (p1.y - p0.y) * h[1] as f32,
                                );
                                let anchor = if d.grip == Grip::HandleOut { p0 } else { p1 };
                                painter.line_segment(
                                    [anchor, hp],
                                    Stroke::new(1.0, accent.gamma_multiply(0.7)),
                                );
                                painter.circle_filled(hp, 3.5, accent);
                            }
                        }
                    }
                }
            }
        }
    }

    // Commit on release.
    if resp.drag_stopped() {
        let d = ui.data(|m| m.get_temp::<KfDrag>(drag_id));
        ui.data_mut(|m| m.remove::<KfDrag>(drag_id));
        if let (Some(d), Some(pos)) = (d, hover.or(resp.interact_pointer_pos())) {
            match d.grip {
                Grip::Point => {
                    if let Some(k) = kfs.iter().find(|k| k.at.0 == d.orig_at) {
                        let mut at = snap_frame(x_to_tick(pos.x, dur, rect), fps);
                        at = Tick(at.0.clamp(0, dur.0));
                        let val = y_to_value(pos.y, vmin, vmax, rect);
                        actions.push(KfAction::Move {
                            target: row.target.clone(),
                            path: row.path.clone(),
                            old_at: Tick(d.orig_at),
                            kf: Keyframe::new(at, PropValue::Float(val), k.interp),
                        });
                        st.sel_kf_at = Some(at.0);
                    }
                }
                Grip::HandleOut | Grip::HandleIn => {
                    if let Some(i) = kfs.iter().position(|k| k.at.0 == d.orig_at) {
                        if let (Some(&p0), Some(&p1)) = (points.get(i), points.get(i + 1)) {
                            let (mut out_h, mut in_h) = match kfs[i].interp {
                                Interp::Bezier {
                                    out_handle,
                                    in_handle,
                                } => (out_handle, in_handle),
                                _ => ([0.0, 0.0], [1.0, 1.0]),
                            };
                            let h = handle_from_pos(pos, (p0.x, p0.y), (p1.x, p1.y));
                            if d.grip == Grip::HandleOut {
                                out_h = h;
                            } else {
                                in_h = h;
                            }
                            actions.push(KfAction::SetInterp {
                                target: row.target.clone(),
                                path: row.path.clone(),
                                at: kfs[i].at,
                                interp: Interp::Bezier {
                                    out_handle: out_h,
                                    in_handle: in_h,
                                },
                            });
                        }
                    }
                }
            }
        }
    }

    // Static keyframe diamonds + Bezier handles (skip the point being dragged).
    let dragging_at = active_drag
        .filter(|d| d.grip == Grip::Point)
        .map(|d| d.orig_at);
    for (i, p) in points.iter().enumerate() {
        if Some(kfs[i].at.0) == dragging_at {
            continue;
        }
        let selected = st.sel_kf_at == Some(kfs[i].at.0);
        paint_diamond(
            &painter,
            *p,
            if selected { 6.0 } else { 4.5 },
            true,
            selected,
            accent,
            muted,
        );
    }
    if active_drag.is_none() {
        if let Some((i, (out_p, in_p))) = handles {
            let p0 = points[i];
            let p1 = points[i + 1];
            painter.line_segment([p0, out_p], Stroke::new(1.0, accent.gamma_multiply(0.6)));
            painter.line_segment([p1, in_p], Stroke::new(1.0, accent.gamma_multiply(0.6)));
            painter.circle_filled(out_p, 3.0, accent);
            painter.circle_filled(in_p, 3.0, accent);
        }
    }

    // Double-click empty space → add a keyframe at the clicked time+value.
    if resp.double_clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            if hit_index(&points, pos, 8.0).is_none() {
                let at = Tick(
                    snap_frame(x_to_tick(pos.x, dur, rect), fps)
                        .0
                        .clamp(0, dur.0),
                );
                let val = y_to_value(pos.y, vmin, vmax, rect);
                actions.push(KfAction::Set {
                    target: row.target.clone(),
                    path: row.path.clone(),
                    kf: Keyframe::new(at, PropValue::Float(val), Interp::Linear),
                });
                st.sel_kf_at = Some(at.0);
            }
        }
    }

    // Single click on empty space clears the keyframe selection.
    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            match hit_index(&points, pos, 8.0) {
                Some(i) => st.sel_kf_at = Some(kfs[i].at.0),
                None => st.sel_kf_at = None,
            }
        }
    }

    // Right-click a keyframe → easing / delete menu.
    let menu_idx = hover.and_then(|pos| hit_index(&points, pos, 8.0));
    if let Some(mi) = menu_idx {
        let at = kfs[mi].at;
        let cur = kfs[mi].interp;
        resp.context_menu(|ui| {
            ui.label(egui::RichText::new("Easing").small().weak());
            for preset in anim::EasePreset::ALL {
                let active = preset_is_active(Some(&cur), *preset);
                if ui
                    .selectable_label(active, preset.label())
                    .on_hover_text(preset_hover(*preset))
                    .clicked()
                {
                    actions.push(KfAction::SetInterp {
                        target: row.target.clone(),
                        path: row.path.clone(),
                        at,
                        interp: preset.interp(),
                    });
                    ui.close_menu();
                }
            }
            ui.separator();
            if ui
                .button(format!("{}  Delete keyframe", ph::TRASH))
                .clicked()
            {
                actions.push(KfAction::Remove {
                    target: row.target.clone(),
                    path: row.path.clone(),
                    at,
                });
                st.sel_kf_at = None;
                ui.close_menu();
            }
        });
    }

    // Delete key removes the selected keyframe (keyboard path, 13 §16).
    if resp.hovered()
        && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
    {
        if let Some(at) = st.sel_kf_at {
            actions.push(KfAction::Remove {
                target: row.target.clone(),
                path: row.path.clone(),
                at: Tick(at),
            });
            st.sel_kf_at = None;
        }
    }
}

// ── Public: timeline automation-lane indicator ───────────────────────────────

/// Paint a clip's keyframe positions as small diamonds along the top edge of its
/// timeline body (04 §4.1 automation-lane integration). Read-only: a glanceable
/// cue that a clip is animated; editing happens in [`draw_window`]. Also paints
/// the clip **gain envelope** (proposal 210) when the clip has audio.
pub(crate) fn paint_clip_automation(
    painter: &egui::Painter,
    view: &TimelineView,
    lane_left: f32,
    clip: &Clip,
    clip_rect: Rect,
    accent: Color32,
) {
    paint_gain_envelope(painter, view, lane_left, clip, clip_rect, accent);

    // Unique clip-relative keyframe ticks across transform + effect + audio lanes.
    let mut ats: Vec<i64> = Vec::new();
    for tr in &clip.transform.tracks {
        for k in &tr.keyframes {
            ats.push(k.at.0);
        }
    }
    for fx in &clip.effects {
        for tr in &fx.params.tracks {
            for k in &tr.keyframes {
                ats.push(k.at.0);
            }
        }
    }
    if let Some(audio) = &clip.audio {
        for tr in &audio.params.tracks {
            for k in &tr.keyframes {
                ats.push(k.at.0);
            }
        }
    }
    if ats.is_empty() {
        return;
    }
    ats.sort_unstable();
    ats.dedup();

    let y = clip_rect.top() + 4.0;
    let r = 2.5;
    let mut shapes: Vec<Shape> = Vec::with_capacity(ats.len());
    for at in ats {
        let x = view.tick_to_x(clip.start + Tick(at), lane_left);
        if x < clip_rect.left() + r || x > clip_rect.right() - r {
            continue;
        }
        shapes.push(Shape::convex_polygon(
            vec![
                Pos2::new(x, y - r),
                Pos2::new(x + r, y),
                Pos2::new(x, y + r),
                Pos2::new(x - r, y),
            ],
            accent,
            Stroke::NONE,
        ));
    }
    painter.extend(shapes);
}

/// Draw the clip gain polyline over the clip body (proposal 210).
/// Maps `gain_db ∈ [-96, +12]` to the clip rect vertically; samples ~1 point
/// per 6 screen px. No-op without `clip.audio`.
fn paint_gain_envelope(
    painter: &egui::Painter,
    view: &TimelineView,
    lane_left: f32,
    clip: &Clip,
    clip_rect: Rect,
    accent: Color32,
) {
    let Some(audio) = &clip.audio else {
        return;
    };
    const GAIN_FLOOR: f64 = -96.0;
    const GAIN_CEIL: f64 = 12.0;
    let path = PropPath::new("params.gain_db");
    let target = AnimTarget::ClipAudio { clip: clip.id };
    let w = clip_rect.width().max(1.0);
    let samples = ((w / 6.0).ceil() as usize).clamp(2, 256);
    let mut points: Vec<Pos2> = Vec::with_capacity(samples);
    let pad = 3.0;
    let y_top = clip_rect.top() + pad;
    let y_bot = clip_rect.bottom() - pad;
    let y_span = (y_bot - y_top).max(1.0);
    for i in 0..samples {
        let frac = i as f32 / (samples - 1) as f32;
        let dt = Tick(((clip.duration.0 as f64) * frac as f64).round() as i64);
        let gain = match value_at(clip, &target, &path, dt) {
            Some(PropValue::Float(g)) => g,
            _ => audio.params.base.gain_db,
        };
        let t = ((gain - GAIN_FLOOR) / (GAIN_CEIL - GAIN_FLOOR)).clamp(0.0, 1.0) as f32;
        // Higher gain → higher on screen.
        let y = y_bot - t * y_span;
        let x = view
            .tick_to_x(clip.start + dt, lane_left)
            .clamp(clip_rect.left() + 1.0, clip_rect.right() - 1.0);
        points.push(Pos2::new(x, y));
    }
    if points.len() >= 2 {
        painter.add(Shape::line(
            points,
            Stroke::new(1.5, accent.gamma_multiply(0.85)),
        ));
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::{ClipSource, EffectKind};

    fn plot() -> Rect {
        Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(200.0, 100.0))
    }

    #[test]
    fn tick_x_roundtrips_within_a_frame() {
        let rect = plot();
        let dur = Tick(3000);
        for at in [0i64, 750, 1500, 3000] {
            let x = tick_to_x(Tick(at), dur, rect);
            let back = x_to_tick(x, dur, rect);
            assert!((back.0 - at).abs() <= 1, "at={at} back={}", back.0);
        }
    }

    #[test]
    fn value_y_roundtrips() {
        let rect = plot();
        for v in [-1.0f64, 0.0, 0.5, 1.0] {
            let y = value_to_y(v, -1.0, 1.0, rect);
            let back = y_to_value(y, -1.0, 1.0, rect);
            assert!((back - v).abs() < 1e-3, "v={v} back={back}");
        }
    }

    #[test]
    fn value_to_y_inverts_axis() {
        let rect = plot();
        // Max value sits at the top (smaller y), min at the bottom.
        assert!(value_to_y(1.0, 0.0, 1.0, rect) < value_to_y(0.0, 0.0, 1.0, rect));
        assert_eq!(value_to_y(1.0, 0.0, 1.0, rect), rect.top());
        assert_eq!(value_to_y(0.0, 0.0, 1.0, rect), rect.bottom());
    }

    #[test]
    fn x_to_tick_clamps_to_span() {
        let rect = plot();
        let dur = Tick(1000);
        assert_eq!(x_to_tick(-50.0, dur, rect).0, 0);
        assert_eq!(x_to_tick(9999.0, dur, rect).0, 1000);
    }

    #[test]
    fn value_range_uses_registry_bounds_when_present() {
        let (lo, hi) = value_range(Some((0.0, 1.0)), None, 0.5);
        assert_eq!((lo, hi), (0.0, 1.0));
    }

    #[test]
    fn value_range_autofits_unbounded_props() {
        let mut tr = PropertyTrack::new("transform.x");
        tr.keyframes.push(Keyframe::new(
            Tick(0),
            PropValue::Float(-10.0),
            Interp::Linear,
        ));
        tr.keyframes.push(Keyframe::new(
            Tick(100),
            PropValue::Float(30.0),
            Interp::Linear,
        ));
        let (lo, hi) = value_range(None, Some(&tr), 0.0);
        assert!(lo < -10.0 && hi > 30.0, "range=({lo},{hi})");
    }

    #[test]
    fn value_range_flat_curve_pads_symmetrically() {
        let (lo, hi) = value_range(None, None, 5.0);
        assert!(lo < 5.0 && hi > 5.0);
        assert!(
            (5.0 - lo - (hi - 5.0)).abs() < 1e-6,
            "symmetric around base"
        );
    }

    #[test]
    fn hit_index_picks_nearest_within_radius() {
        let pts = [
            Pos2::new(0.0, 0.0),
            Pos2::new(50.0, 0.0),
            Pos2::new(100.0, 0.0),
        ];
        assert_eq!(hit_index(&pts, Pos2::new(48.0, 2.0), 8.0), Some(1));
        assert_eq!(hit_index(&pts, Pos2::new(25.0, 0.0), 8.0), None);
    }

    #[test]
    fn playhead_local_bounds() {
        let mut clip = Clip::new(ClipSource::Adjustment, Tick(1000), Tick(500));
        clip.start = Tick(1000);
        assert_eq!(playhead_local(Tick(1000), &clip), Some(Tick(0)));
        assert_eq!(playhead_local(Tick(1250), &clip), Some(Tick(250)));
        assert_eq!(playhead_local(Tick(1500), &clip), Some(Tick(500)));
        assert_eq!(playhead_local(Tick(999), &clip), None);
        assert_eq!(playhead_local(Tick(1501), &clip), None);
    }

    #[test]
    fn snap_frame_quantizes_to_grid() {
        let fps = FrameRate::FPS_30; // 1000 ticks/frame at 30_000 tps.
        let tpf = fps.ticks_per_frame().0;
        let snapped = snap_frame(Tick(tpf + tpf / 3), fps);
        assert_eq!(snapped.0 % tpf, 0);
        assert_eq!(snapped.0, tpf, "rounds down toward nearest frame");
    }

    #[test]
    fn handle_positions_and_inverse_agree() {
        let k0 = (0.0f32, 100.0);
        let k1 = (200.0f32, 0.0);
        let (out_p, _in_p) = handle_positions(k0, k1, [0.5, 0.0], [1.0, 1.0]);
        let recovered = handle_from_pos(out_p, k0, k1);
        assert!((recovered[0] - 0.5).abs() < 1e-3, "x={}", recovered[0]);
    }

    #[test]
    fn build_rows_covers_transform_and_effects() {
        let mut clip = Clip::new(
            ClipSource::Vector {
                asset: photonic_core::timeline::AssetId::new(),
            },
            Tick(0),
            Tick(1000),
        );
        clip.effects
            .push(photonic_core::timeline::ClipEffect::new(EffectKind::Blur));
        let rows = build_rows(&clip);
        // 8 transform props + Blur's radius.
        assert!(rows.iter().any(|r| r.path.as_str() == "transform.x"));
        assert!(rows.iter().any(|r| r.path.as_str() == "transform.opacity"));
        assert!(rows.iter().any(|r| matches!(
            r.target,
            AnimTarget::ClipEffect {
                effect_index: 0,
                ..
            }
        ) && r.path.as_str() == "params.radius"));
    }

    #[test]
    fn value_at_evaluates_keyframes() {
        let mut clip = Clip::new(ClipSource::Adjustment, Tick(0), Tick(1000));
        let path = PropPath::new("transform.x");
        let tr = clip.transform.track_mut(&path);
        tr.insert_keyframe(Keyframe::new(
            Tick(0),
            PropValue::Float(0.0),
            Interp::Linear,
        ));
        tr.insert_keyframe(Keyframe::new(
            Tick(1000),
            PropValue::Float(100.0),
            Interp::Linear,
        ));
        let target = AnimTarget::ClipTransform { clip: clip.id };
        let v = value_at(&clip, &target, &path, Tick(500)).unwrap();
        assert_eq!(as_f64(&v), Some(50.0));
    }

    // ── Named easing presets (26 §10 K-B12) ─────────────────────────────────

    /// The toolbar/context-menu highlight must mark exactly ONE preset for a
    /// keyframe that carries one — and none for a hand-dragged curve.
    ///
    /// Sensitivity: the same loop run with the *pre-fix* rule (compare the
    /// `Interp` variant, which is what the old `interp_short` comparison did)
    /// must still light up all four Bezier presets, so this test fails if the
    /// active-preset rule ever regresses to variant matching.
    #[test]
    fn ease_toolbar_marks_exactly_one_preset_active() {
        let variant_rule = |cur: &Interp, p: anim::EasePreset| {
            std::mem::discriminant(cur) == std::mem::discriminant(&p.interp())
        };
        let bezier_count = anim::EasePreset::ALL
            .iter()
            .filter(|p| p.handles().is_some())
            .count();
        assert!(bezier_count >= 2, "need several Bezier presets to compare");

        for applied in anim::EasePreset::ALL {
            let cur = applied.interp();
            let active: Vec<&str> = anim::EasePreset::ALL
                .iter()
                .filter(|p| preset_is_active(Some(&cur), **p))
                .map(|p| p.label())
                .collect();
            assert_eq!(
                active,
                vec![applied.label()],
                "{} highlighted {active:?}",
                applied.label()
            );

            if applied.handles().is_some() {
                let by_variant = anim::EasePreset::ALL
                    .iter()
                    .filter(|p| variant_rule(&cur, **p))
                    .count();
                assert_eq!(
                    by_variant,
                    bezier_count,
                    "the pre-fix variant rule should still be ambiguous for {}",
                    applied.label()
                );
            }
        }

        // A hand-dragged curve is no preset: nothing is highlighted.
        let custom = Interp::Bezier {
            out_handle: [0.31, 0.72],
            in_handle: [0.91, 0.24],
        };
        assert!(anim::EasePreset::ALL
            .iter()
            .all(|p| !preset_is_active(Some(&custom), *p)));
        // …and no selection highlights nothing either.
        assert!(anim::EasePreset::ALL
            .iter()
            .all(|p| !preset_is_active(None, *p)));
    }

    #[test]
    fn ease_readout_names_presets_and_falls_back_to_custom() {
        for p in anim::EasePreset::ALL {
            let text = ease_readout(&p.interp());
            assert!(text.starts_with(p.label()), "{p:?} read out as {text}");
            assert!(text.contains(&p.css()), "{p:?} read out as {text}");
        }
        let custom = Interp::Bezier {
            out_handle: [0.31, 0.72],
            in_handle: [0.91, 0.24],
        };
        let text = ease_readout(&custom);
        assert!(text.starts_with(CUSTOM_EASE_LABEL), "{text}");
        assert!(text.contains("0.310"), "{text}");
        // Hover text names the curve it will write.
        assert!(anim::EasePreset::EaseInOut
            .css()
            .starts_with("cubic-bezier("));
        assert!(
            preset_hover(anim::EasePreset::EaseInOut).contains(&anim::EasePreset::EaseInOut.css())
        );
    }

    /// Applying a preset is ONE user verb ⇒ ONE undo unit, and undo restores the
    /// previous easing exactly (DoD 4). Exercises the same `KfAction::SetInterp`
    /// → `kf_action_cmds` path both editor surfaces commit through.
    #[test]
    fn applying_a_preset_is_one_undo_unit() {
        use photonic_core::history::Command;
        use photonic_core::timeline::{Sequence, Track, TrackKind};

        let mut clip = Clip::new(ClipSource::Adjustment, Tick(0), Tick(1000));
        let path = PropPath::new("transform.x");
        let tr = clip.transform.track_mut(&path);
        tr.insert_keyframe(Keyframe::new(
            Tick(0),
            PropValue::Float(0.0),
            Interp::Linear,
        ));
        tr.insert_keyframe(Keyframe::new(
            Tick(500),
            PropValue::Float(100.0),
            Interp::Linear,
        ));
        let clip_id = clip.id;

        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("S", FrameRate::FPS_30, 1920, 1080);
        let mut vtrack = Track::new(TrackKind::Video, "V1");
        vtrack.clips.push(clip);
        seq.video_tracks.push(vtrack);
        project.insert_sequence(seq);
        let mut doc = Document::new("t", 100.0, 100.0);
        doc.timeline = Some(project);

        let target = AnimTarget::ClipTransform { clip: clip_id };
        let preset = anim::EasePreset::EaseInOut;
        let cmds = kf_action_cmds(
            doc.timeline.as_ref().unwrap(),
            KfAction::SetInterp {
                target: target.clone(),
                path: path.clone(),
                at: Tick(0),
                interp: preset.interp(),
            },
        );
        assert_eq!(cmds.len(), 1, "one preset click must be one command");

        let cmd = cmds.into_iter().next().unwrap();
        let inverse = Command::Timeline(cmd.clone()).inverse(&doc).unwrap();
        Command::Timeline(cmd).apply(&mut doc);

        let read = |doc: &Document| {
            find_clip(doc.timeline.as_ref().unwrap(), clip_id)
                .unwrap()
                .transform
                .track(&path)
                .unwrap()
                .keyframes[0]
                .interp
        };
        // The stored form is plain Bezier handles, and reads back as the preset.
        assert_eq!(read(&doc), preset.interp());
        assert_eq!(
            anim::EasePreset::from_interp(&read(&doc)),
            Some(preset),
            "applied preset must be identifiable from what was stored"
        );

        inverse.apply(&mut doc);
        assert_eq!(read(&doc), Interp::Linear, "undo must restore the easing");
    }

    #[test]
    fn pretty_label_titlecases() {
        assert_eq!(pretty_label("scale_x"), "Scale X");
        assert_eq!(pretty_label("opacity"), "Opacity");
    }
}
