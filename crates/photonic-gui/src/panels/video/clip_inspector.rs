//! `DrawerGroup::ClipInspector` panel (04 §4.1) — the selected clip's
//! transform / speed / effects-stack / keyframes / transition params, the
//! `Clip`/`ClipEffect` analogue of the vector Inspector. Panel shell owned by
//! 04; widgets source from `prop_registry` (01 §6.2).
//!
//! The "Keyframes" section ([`draw_keyframes_section`]) docks
//! [`super::keyframe_editor::draw_embedded`] inline — the single
//! Effect-Controls surface (14 §G-9) — instead of requiring the separate
//! floating `egui::Window` (`keyframe_editor.rs::draw_window`). That float
//! remains available (pop-out button in the section, or a per-field
//! keyframe-diamond click) for live-playhead scrubbing / the wider
//! side-by-side layout; both write [`super::VideoPanelUi::keyframe_editor_target`].
//!
//! Every mutation here is built as a real `photonic_core::timeline::ops::*`
//! call (reading `ctx.doc`, never mutating it) and handed to the app via
//! [`crate::panels::PanelAction::ClipEditDiscrete`]/`ClipEditCoalesced` —
//! `PropPanelCtx` carries `doc: &Document` but no `&mut CommandHistory` (same
//! reason the media-pool panel routes through `PanelAction::Media*`), so
//! those two generic carriers are this panel's `ops_bridge` equivalent at the
//! drawer boundary. No direct `doc.timeline` mutation anywhere below —
//! grep-checked (13 §normative rule, 01 §10 CAP-019 parity).

use crate::panels::{PanelAction, PropPanelCtx};
use egui::{Color32, RichText, Ui};
use egui_phosphor::regular as ph;
// `SpeedKey` isn't re-exported at `timeline::` root (only `SpeedMap` is);
// reached via the `clip` submodule directly, same precedent as
// `app/timeline/mod.rs`'s `use photonic_core::timeline::clip::LinkGroupId`.
use photonic_core::timeline::anim::EasePreset;
use photonic_core::timeline::clip::SpeedKey;
// `VfxOwner` (K-B1/K-B2 effect-stack scope) isn't re-exported at `timeline::`
// root either — same precedent as `SpeedKey` above.
use super::param_expr;
use photonic_core::timeline::commands::VfxOwner;
use photonic_core::timeline::{
    ops, prop_registry, Clip, ClipId, EffectKind, EffectParams, Interp, PropTargetKind, PropValue,
    Ratio, SequenceId, SpeedMap, Tick, TimelineProject, TrackId, TrackKind, Transition,
    TransitionKind, TransitionParams, TICKS_PER_SECOND,
};

const MUTED: Color32 = Color32::from_rgb(0x7A, 0x7A, 0x9A); // `secondary`
const ACCENT: Color32 = Color32::from_rgb(0x6E, 0x56, 0xCF); // `primary`
const SPEED_CURVE_MAX: f64 = 10.0;

/// Live drag session for the speed curve. Holds a full working copy of the
/// keys so the painted handles track the pointer even when the document
/// commit lags a frame (or the parent `ScrollArea` would otherwise scroll).
#[derive(Clone)]
struct SpeedCurveDrag {
    clip: ClipId,
    /// Index into `keys` (kept sorted).
    index: usize,
    /// Working key list for the duration of the drag.
    keys: Vec<SpeedKey>,
}

/// Left-rail Clip Inspector drawer. Reads the timeline selection and clip data
/// via `ctx` (`ctx.video.selection`, `ctx.doc`).
pub(crate) fn draw_clip_inspector(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let Some(project) = ctx.doc.timeline.as_ref() else {
        ui.label(RichText::new("No video project yet.").color(MUTED));
        return;
    };
    let selection = ctx.video.selection;
    if selection.is_empty() {
        ui.label(RichText::new("No clip selected.").color(MUTED));
        return;
    }
    if selection.len() > 1 {
        // 13 §16 finding #6: multi-clip common-value editing is unresolved by
        // any spec doc; showing the (single-clip) inspector for the first
        // selected clip is the documented interim behavior, not silent data
        // loss — the other clips are simply not editable from here yet.
        ui.label(
            RichText::new(format!(
                "{} clips selected — showing the first (multi-clip editing not yet supported).",
                selection.len()
            ))
            .color(MUTED)
            .small(),
        );
    }
    let clip_id = selection[0];
    let Some((seq_id, track_id, clip)) = locate_clip(project, clip_id) else {
        ui.label(RichText::new("Selected clip no longer exists.").color(MUTED));
        return;
    };

    let mut action: Option<PanelAction> = None;
    let active_format = project
        .sequences
        .get(&seq_id)
        .map(|s| s.active_format)
        .unwrap_or(0);

    ui.label(RichText::new(&clip.name).strong());
    ui.add_space(2.0);

    // K-A6: numeric Edit Duration form (position / in / out / duration + ripple).
    if ui
        .button(format!("{} Edit duration…", ph::TIMER))
        .on_hover_text("Frame-accurate position, source in/out, and duration (K-A6)")
        .clicked()
    {
        action = Some(PanelAction::OpenEditDuration {
            seq: seq_id,
            track: track_id,
            clip: clip_id,
        });
    }
    // K-B14: freeze at the clip's current source_in (inspector has no playhead;
    // the command / context menu freeze at the playhead).
    if ui
        .button(format!("{} Freeze frame", ph::PAUSE))
        .on_hover_text("Hold the current source frame for this clip's duration (zero speed; K-B14)")
        .clicked()
    {
        action = Some(PanelAction::FreezeFrame {
            seq: seq_id,
            track: track_id,
            clip: clip_id,
            at: Tick::ZERO, // relative to current source_in via existing speed
        });
    }
    ui.add_space(4.0);

    draw_transform_section(ui, ctx, project, seq_id, track_id, clip, &mut action);
    draw_speed_section(ui, project, seq_id, track_id, clip, &mut action);
    draw_reframe_section(
        ui,
        project,
        seq_id,
        track_id,
        clip,
        active_format,
        &mut action,
    );
    draw_level_horizon_section(
        ui,
        project,
        seq_id,
        track_id,
        clip,
        active_format,
        &mut action,
    );
    draw_stabilization_section(ui, project, seq_id, track_id, clip, &mut action);
    draw_effects_section(ui, project, seq_id, track_id, clip, &mut action);
    draw_keyframes_section(ui, ctx, project, clip, &mut action);
    draw_transitions_section(ui, project, seq_id, track_id, clip, &mut action);

    if action.is_some() {
        ctx.action = action;
    }
}

// ── Clip location (session `ClipId` → (seq, track, &Clip)) ─────────────────

/// `ctx.video.selection` (04 §2.6 session state) carries only `ClipId`s, so
/// the inspector resolves the owning sequence/track itself — no other panel
/// exposes this lookup.
fn locate_clip(project: &TimelineProject, clip_id: ClipId) -> Option<(SequenceId, TrackId, &Clip)> {
    for (seq_id, seq) in &project.sequences {
        for track in seq.tracks() {
            if let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) {
                return Some((*seq_id, track.id, clip));
            }
        }
    }
    None
}

/// Push `new_clip` as `SetClipProp`, coalesced (drag-scrub numeric fields).
fn set_clip_coalesced(
    project: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    new_clip: Clip,
    action: &mut Option<PanelAction>,
) {
    if let Ok(cmd) = ops::set_clip_prop(project, seq, track, new_clip) {
        *action = Some(PanelAction::ClipEditCoalesced(cmd));
    }
}

/// Push `new_clip` as `SetClipProp`, one discrete undo step (toggle/button).
fn set_clip_discrete(
    project: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    new_clip: Clip,
    action: &mut Option<PanelAction>,
) {
    if let Ok(cmd) = ops::set_clip_prop(project, seq, track, new_clip) {
        *action = Some(PanelAction::ClipEditDiscrete(cmd));
    }
}

/// A keyframe-diamond toggle beside an animatable field (13 §5.1): clicking it
/// targets the keyframe editor at this clip (`VideoPanelUi::keyframe_editor_target`,
/// tagged `[clip_inspector]`) rather than adding/removing a keyframe inline —
/// per-field add/remove-at-playhead is the keyframe editor's own job once that
/// surface exists; this panel wires the field for its documented purpose
/// (targeting) without inventing a second keyframing UI.
fn keyframe_diamond(ui: &mut Ui, ctx: &mut PropPanelCtx, clip_id: ClipId, has_track: bool) {
    let glyph = if has_track { "\u{25C6}" } else { "\u{25C7}" }; // filled / hollow diamond
    let color = if has_track { ACCENT } else { MUTED };
    if ui
        .add(egui::Button::new(RichText::new(glyph).color(color)).small())
        .on_hover_text("Animate this property (opens the keyframe editor)")
        .clicked()
    {
        *ctx.video.keyframe_editor_target = Some(clip_id);
    }
}

// ── Transform (13 §5.1) ──────────────────────────────────────────────────────

fn draw_transform_section(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    project: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    clip: &Clip,
    action: &mut Option<PanelAction>,
) {
    egui::CollapsingHeader::new("Transform")
        .default_open(true)
        .id_salt("clip_inspector_transform")
        .show(ui, |ui| {
            let base = clip.transform.base;
            let has_track = !clip.transform.tracks.is_empty();
            let mut new_t = base;
            let mut changed = false;
            egui::Grid::new("clip_transform_grid")
                .num_columns(3)
                .spacing([4.0, 2.0])
                .show(ui, |ui| {
                    changed |= transform_row(ui, "X", &mut new_t.x, None);
                    keyframe_diamond(ui, ctx, clip.id, has_track);
                    ui.end_row();
                    changed |= transform_row(ui, "Y", &mut new_t.y, None);
                    keyframe_diamond(ui, ctx, clip.id, has_track);
                    ui.end_row();
                    changed |= transform_row(ui, "Scale X", &mut new_t.scale_x, Some(0.0..=1000.0));
                    keyframe_diamond(ui, ctx, clip.id, has_track);
                    ui.end_row();
                    changed |= transform_row(ui, "Scale Y", &mut new_t.scale_y, Some(0.0..=1000.0));
                    keyframe_diamond(ui, ctx, clip.id, has_track);
                    ui.end_row();
                    changed |= transform_row_suffixed(ui, "Rotation", &mut new_t.rotation, " rad");
                    keyframe_diamond(ui, ctx, clip.id, has_track);
                    ui.end_row();
                    changed |= transform_row(ui, "Anchor X", &mut new_t.anchor_x, None);
                    keyframe_diamond(ui, ctx, clip.id, has_track);
                    ui.end_row();
                    changed |= transform_row(ui, "Anchor Y", &mut new_t.anchor_y, None);
                    keyframe_diamond(ui, ctx, clip.id, has_track);
                    ui.end_row();
                    changed |= transform_row(ui, "Opacity", &mut new_t.opacity, Some(0.0..=1.0));
                    keyframe_diamond(ui, ctx, clip.id, has_track);
                    ui.end_row();
                });
            if changed {
                let mut new_clip = clip.clone();
                new_clip.transform.base = new_t;
                set_clip_coalesced(project, seq, track, new_clip, action);
            }
        });
}

fn transform_row(
    ui: &mut Ui,
    label: &str,
    v: &mut f64,
    range: Option<std::ops::RangeInclusive<f64>>,
) -> bool {
    ui.label(label);
    // K-B6: arithmetic / %vars + middle-click reset. Transform siblings are not
    // threaded here — frame size alone is enough for `%w`/`%h` style maths.
    let range_tuple = range.as_ref().map(|r| (*r.start(), *r.end()));
    let default = param_expr::neutral_float_default(range_tuple);
    let vars = param_expr::vars_from_params([], None, None);
    param_expr::float_drag(ui, v, default, range_tuple, &vars, 0.5)
}

/// Same as [`transform_row`] with a unit suffix — used for `rotation`, which
/// `photonic_video::graph::compile::clip_transform_matrix` evaluates in
/// radians (`glam::Mat3::from_angle`), unlike every other transform field
/// (plain document-space units) — the suffix keeps that unit legible instead
/// of a bare unlabeled float (04 §3.3/`app/reframe.rs` pins the same
/// convention for the on-canvas rotate handle).
fn transform_row_suffixed(ui: &mut Ui, label: &str, v: &mut f64, suffix: &str) -> bool {
    ui.label(label);
    let drag = egui::DragValue::new(v)
        .speed(0.02)
        .fixed_decimals(3)
        .suffix(suffix);
    ui.add(drag).changed()
}

// ── Speed (13 §5.1, 01 §5.1, 14-nle-parity G-11) ─────────────────────────────

/// UI percent + reverse flag → an exact `Ratio` at 1/1000 precision — exact
/// enough for a UI-driven speed field (01 §5.1's "exact rational" rule
/// governs storage/eval, not that every possible value must be
/// keyboard-reachable at infinite precision). Shared by the constant-speed
/// field and every ramp-point row below.
fn ratio_from_pct(pct: f64, reversed: bool) -> Ratio {
    let den: u32 = 1000;
    let mag = ((pct / 100.0) * den as f64).round().clamp(0.0, 10_000.0) as i32;
    Ratio::new(if reversed { -mag } else { mag }, den)
}

/// Default three-section ramp (four eased handles) used when the user first
/// enables speed ramping or opens the curve with fewer than four points.
/// Sections: hold → dip → punch → settle — CapCut/smooth-cut style.
fn default_three_section_ramp(duration: Tick, base: Ratio) -> Vec<SpeedKey> {
    // A freeze-frame is a valid zero-rate speed map. Keep it zero when the
    // user opens the ramp editor; the first nonzero speed must be explicit.
    if base.num == 0 {
        return vec![
            SpeedKey::new(Tick::ZERO, base),
            SpeedKey::new(duration, base),
        ];
    }
    let sign = if base.num < 0 { -1.0 } else { 1.0 };
    let base_f = base.as_f64().abs().clamp(0.25, 4.0);
    let at = |fraction: f64| {
        Tick((duration.0 as f64 * fraction).round() as i64).clamp(Tick::ZERO, duration)
    };
    let ratio = |speed: f64| Ratio::new((sign * speed * 1000.0).round() as i32, 1000);
    let ease = EasePreset::EaseInOut.interp();
    vec![
        SpeedKey::eased(at(0.0), ratio(base_f), ease),
        SpeedKey::eased(at(1.0 / 3.0), ratio((base_f * 0.45).max(0.1)), ease),
        SpeedKey::eased(at(2.0 / 3.0), ratio((base_f * 2.2).min(8.0)), ease),
        // Last key is a hold so the settle speed persists to the clip end.
        SpeedKey::new(duration, ratio(base_f)),
    ]
}

fn preset_speed_keys(name: &str, duration: Tick) -> Vec<SpeedKey> {
    let at = |fraction: f64| Tick((duration.0 as f64 * fraction).round() as i64);
    let eased = |fraction: f64, speed: f64| {
        SpeedKey::eased(
            at(fraction),
            Ratio::new((speed * 1000.0).round() as i32, 1000),
            EasePreset::EaseInOut.interp(),
        )
    };
    let last =
        |speed: f64| SpeedKey::new(duration, Ratio::new((speed * 1000.0).round() as i32, 1000));
    match name {
        // Explicit three-section smooth-cut defaults (also the enable seed).
        "Smooth" => default_three_section_ramp(duration, Ratio::ONE),
        "Flow" => vec![
            eased(0.0, 1.0),
            eased(0.25, 0.5),
            eased(0.65, 2.0),
            last(1.0),
        ],
        "Hero" => vec![
            eased(0.0, 1.0),
            eased(0.35, 0.25),
            eased(0.65, 4.0),
            last(1.0),
        ],
        "Action" => vec![
            eased(0.0, 1.0),
            eased(0.25, 0.5),
            eased(0.55, 8.0),
            last(1.0),
        ],
        "Fast Lane" => vec![
            eased(0.0, 1.0),
            eased(0.2, 3.0),
            eased(0.75, 5.0),
            last(1.0),
        ],
        _ => Vec::new(),
    }
}

fn speed_to_curve_y(speed: f64, rect: egui::Rect) -> f32 {
    let normalized = speed.signum() * (1.0 + speed.abs()).ln() / (1.0 + SPEED_CURVE_MAX).ln();
    rect.center().y - normalized as f32 * rect.height() * 0.45
}

fn curve_y_to_speed(y: f32, rect: egui::Rect) -> f64 {
    let normalized = ((rect.center().y - y) / (rect.height() * 0.45)) as f64;
    normalized.signum() * ((normalized.abs().min(1.0) * (1.0 + SPEED_CURVE_MAX).ln()).exp() - 1.0)
}

/// Handle hit size in screen px — large enough for easy grab inside a
/// scrollable drawer (where small targets lose the drag to the ScrollArea).
const SPEED_HANDLE_HIT_PX: f32 = 22.0;
const SPEED_HANDLE_DRAW_PX: f32 = 7.0;

fn speed_curve_canvas(
    ui: &mut Ui,
    clip: &Clip,
    keys: &[SpeedKey],
    frame_ticks: i64,
) -> Option<Vec<SpeedKey>> {
    let desired = egui::vec2(ui.available_width().max(160.0), 160.0);
    let (rect, bg_response) = ui.allocate_exact_size(desired, egui::Sense::click());

    let drag_id = ui.id().with(("speed_curve_drag", clip.id));
    // Prefer the live working copy while a drag is in progress so the paint
    // path tracks the pointer even before history applies this frame's commit.
    let mut live = ui
        .data(|d| d.get_temp::<SpeedCurveDrag>(drag_id))
        .filter(|d| d.clip == clip.id);
    let mut sorted = live.as_ref().map(|d| d.keys.clone()).unwrap_or_else(|| {
        let mut k = keys.to_vec();
        k.sort_by_key(|key| key.at.0);
        k
    });

    let dur = clip.duration.0.max(1) as f32;
    let tick_to_x = |t: i64| rect.left() + rect.width() * (t as f32 / dur).clamp(0.0, 1.0);
    let x_to_tick = |x: f32| -> i64 {
        let u = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64;
        (u * clip.duration.0 as f64).round() as i64
    };

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, MUTED.gamma_multiply(0.55)),
    );

    // Soft vertical bands for sections between keys.
    if sorted.len() >= 2 {
        for (i, pair) in sorted.windows(2).enumerate() {
            let x0 = tick_to_x(pair[0].at.0);
            let x1 = tick_to_x(pair[1].at.0);
            if x1 > x0 {
                let tint = if i % 2 == 0 {
                    ACCENT.gamma_multiply(0.08)
                } else {
                    MUTED.gamma_multiply(0.06)
                };
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(x0, rect.top()),
                        egui::pos2(x1, rect.bottom()),
                    ),
                    0.0,
                    tint,
                );
            }
        }
    }

    for value in [-10.0, -1.0, 0.0, 1.0, 10.0] {
        let y = speed_to_curve_y(value, rect);
        let stroke = if value == 0.0 {
            egui::Stroke::new(1.0, ACCENT.gamma_multiply(0.7))
        } else {
            egui::Stroke::new(1.0, MUTED.gamma_multiply(0.35))
        };
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            stroke,
        );
        painter.text(
            egui::pos2(rect.left() + 3.0, y - 1.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{value:.0}x"),
            egui::FontId::proportional(9.0),
            MUTED,
        );
    }

    // Smooth sampled curve (eased segments via the real evaluator).
    let map = SpeedMap::Keyframed {
        keys: sorted.clone(),
    };
    let samples = 96usize.max(rect.width() as usize / 2);
    let mut curve: Vec<egui::Pos2> = Vec::with_capacity(samples + 1);
    for i in 0..=samples {
        let t = Tick((clip.duration.0 as f64 * i as f64 / samples as f64).round() as i64);
        let speed = map.ratio_at_f64(t);
        curve.push(egui::pos2(tick_to_x(t.0), speed_to_curve_y(speed, rect)));
    }
    if curve.len() > 1 {
        painter.add(egui::Shape::line(curve, egui::Stroke::new(2.0, ACCENT)));
    }

    let mut result: Option<Vec<SpeedKey>> = None;
    let pointer = ui.ctx().pointer_interact_pos();
    let primary_down = ui.input(|i| i.pointer.primary_down());

    // ── Per-handle interact (beats ScrollArea drag-to-scroll) ─────────────
    // One `ui.interact` per handle with a large hit rect. The drawer is a
    // `ScrollArea::both()`, which steals canvas-level drag; individual
    // handle widgets take pointer priority and track reliably.
    let n = sorted.len();
    for i in 0..n {
        let p = egui::pos2(
            tick_to_x(sorted[i].at.0),
            speed_to_curve_y(sorted[i].ratio.as_f64(), rect),
        );
        let handle_rect =
            egui::Rect::from_center_size(p, egui::vec2(SPEED_HANDLE_HIT_PX, SPEED_HANDLE_HIT_PX));
        let hid = ui.id().with(("speed_handle", clip.id, i));
        let resp = ui.interact(handle_rect, hid, egui::Sense::click_and_drag());

        if resp.hovered() {
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Grab);
        }
        if resp.dragged() || (resp.is_pointer_button_down_on() && primary_down) {
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Grabbing);
            ui.ctx().request_repaint();
            // Tell the parent drawer ScrollArea to stop stealing this drag.
            ui.data_mut(|d| {
                d.insert_temp(egui::Id::new("speed_curve_dragging"), true);
            });

            // Start / continue a live session keyed by this handle.
            if live.as_ref().map(|d| d.index) != Some(i) {
                let mut keys_copy = sorted.clone();
                keys_copy.sort_by_key(|k| k.at.0);
                // Re-find index after sort (should match i if already sorted).
                let index = keys_copy
                    .iter()
                    .position(|k| k.at == sorted[i].at && k.ratio == sorted[i].ratio)
                    .unwrap_or(i)
                    .min(keys_copy.len().saturating_sub(1));
                live = Some(SpeedCurveDrag {
                    clip: clip.id,
                    index,
                    keys: keys_copy,
                });
            }

            if let (Some(drag), Some(pos)) = (live.as_mut(), pointer) {
                if drag.clip == clip.id && drag.index < drag.keys.len() {
                    let mut at = x_to_tick(pos.x).clamp(0, clip.duration.0);
                    let lo = if drag.index == 0 {
                        0
                    } else {
                        drag.keys[drag.index - 1].at.0 + 1
                    };
                    let hi = if drag.index + 1 >= drag.keys.len() {
                        clip.duration.0
                    } else {
                        (drag.keys[drag.index + 1].at.0 - 1).max(lo)
                    };
                    // Endpoints: pin time, free speed. Interiors: free both.
                    if drag.index == 0 {
                        at = 0;
                    } else if drag.index + 1 == drag.keys.len() {
                        at = clip.duration.0;
                    } else {
                        at = at.clamp(lo, hi);
                    }
                    let speed =
                        curve_y_to_speed(pos.y, rect).clamp(-SPEED_CURVE_MAX, SPEED_CURVE_MAX);
                    drag.keys[drag.index].at = Tick(at);
                    drag.keys[drag.index].ratio = Ratio::new((speed * 1000.0).round() as i32, 1000);
                    sorted = drag.keys.clone();
                    result = Some(sorted.clone());
                    ui.data_mut(|d| d.insert_temp(drag_id, drag.clone()));
                }
            }
        }

        // Draw handle at (possibly live-updated) position.
        let draw_p = egui::pos2(
            tick_to_x(sorted[i].at.0),
            speed_to_curve_y(sorted[i].ratio.as_f64(), rect),
        );
        let r = if i == 0 || i + 1 == n {
            SPEED_HANDLE_DRAW_PX + 1.0
        } else {
            SPEED_HANDLE_DRAW_PX
        };
        let fill = if resp.dragged() || resp.is_pointer_button_down_on() {
            Color32::WHITE
        } else {
            ACCENT
        };
        painter.circle_filled(draw_p, r, fill);
        painter.circle_stroke(draw_p, r, egui::Stroke::new(1.5, Color32::WHITE));
        if resp.dragged() || resp.is_pointer_button_down_on() {
            painter.circle_stroke(draw_p, r + 3.0, egui::Stroke::new(1.0, ACCENT));
        }
    }

    // Release: frame-snap interior points, clear live session.
    let any_handle_down = (0..n).any(|i| {
        let hid = ui.id().with(("speed_handle", clip.id, i));
        ui.ctx()
            .read_response(hid)
            .is_some_and(|r| r.is_pointer_button_down_on() || r.dragged())
    });
    if !primary_down || (!any_handle_down && live.is_some()) {
        if let Some(mut drag) = ui.data(|d| d.get_temp::<SpeedCurveDrag>(drag_id)) {
            if drag.clip == clip.id
                && drag.index > 0
                && drag.index + 1 < drag.keys.len()
                && !primary_down
            {
                let ft = frame_ticks.max(1);
                let snapped = ((drag.keys[drag.index].at.0 as f64 / ft as f64).round() as i64) * ft;
                let lo = drag.keys[drag.index - 1].at.0 + 1;
                let hi = (drag.keys[drag.index + 1].at.0 - 1).max(lo);
                drag.keys[drag.index].at = Tick(snapped.clamp(lo, hi));
                result = Some(drag.keys.clone());
            }
        }
        if !primary_down {
            ui.data_mut(|d| d.remove::<SpeedCurveDrag>(drag_id));
        }
    }

    // Click empty curve (not on a handle) → add eased interior point.
    if bg_response.clicked() {
        if let Some(pos) = pointer {
            let on_handle = sorted.iter().any(|key| {
                let p = egui::pos2(
                    tick_to_x(key.at.0),
                    speed_to_curve_y(key.ratio.as_f64(), rect),
                );
                p.distance(pos) <= SPEED_HANDLE_HIT_PX * 0.5
            });
            if !on_handle {
                let ft = frame_ticks.max(1);
                let raw = x_to_tick(pos.x);
                let at =
                    Tick(((raw as f64 / ft as f64).round() as i64 * ft).clamp(0, clip.duration.0));
                if !sorted.iter().any(|key| key.at == at) {
                    let ratio = Ratio::new(
                        (curve_y_to_speed(pos.y, rect).clamp(-SPEED_CURVE_MAX, SPEED_CURVE_MAX)
                            * 1000.0)
                            .round() as i32,
                        1000,
                    );
                    sorted.push(SpeedKey::eased(at, ratio, EasePreset::EaseInOut.interp()));
                    sorted.sort_by_key(|key| key.at.0);
                    result = Some(sorted);
                }
            }
        }
    }

    result
}

/// The piecewise-constant ramp speed in effect at clip-relative tick `t`,
/// mirroring `photonic_core::timeline::clip`'s private `integrate_ramp`
/// segment definition (the first key's ratio holds before it, the last key's
/// after it; an empty ramp is identity). `sorted` must already be sorted by
/// `at` (every caller here sorts once up front rather than re-sorting per
/// lookup). Used only to seed a newly-added point with the speed already in
/// effect there, so adding a point never itself changes existing playback.
fn ramp_ratio_at(sorted: &[SpeedKey], t: Tick) -> Ratio {
    sorted
        .iter()
        .rev()
        .find(|k| k.at <= t)
        .or_else(|| sorted.first())
        .map(|k| k.ratio)
        .unwrap_or(Ratio::ONE)
}

fn draw_speed_section(
    ui: &mut Ui,
    project: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    clip: &Clip,
    action: &mut Option<PanelAction>,
) {
    egui::CollapsingHeader::new("Speed")
        .default_open(false)
        .id_salt("clip_inspector_speed")
        .show(ui, |ui| {
            let mut is_ramped = matches!(clip.speed, SpeedMap::Keyframed { .. });
            let toggled = ui
                .checkbox(&mut is_ramped, "Speed ramp (variable speed)")
                .on_hover_text(
                    "Off: one constant speed for the whole clip. On: a ramp of \
                     speed points you add/edit below (G-11).",
                )
                .changed();
            if toggled {
                let mut new_clip = clip.clone();
                new_clip.speed = if is_ramped {
                    // Seed a three-section smooth-cut ramp (four handles) so the
                    // curve editor opens with draggable sections, not a flat
                    // single key. Base magnitude follows the previous constant
                    // speed so enabling the ramp still reads as "that speed,
                    // shaped."
                    let seed = match clip.speed {
                        SpeedMap::Constant(r) => r,
                        SpeedMap::Keyframed { .. } => Ratio::ONE,
                    };
                    SpeedMap::Keyframed {
                        keys: default_three_section_ramp(clip.duration, seed),
                    }
                } else {
                    let r = match &clip.speed {
                        SpeedMap::Keyframed { keys } => {
                            let mut sorted = keys.clone();
                            sorted.sort_by_key(|k| k.at.0);
                            ramp_ratio_at(&sorted, Tick::ZERO)
                        }
                        SpeedMap::Constant(r) => *r,
                    };
                    SpeedMap::Constant(r)
                };
                set_clip_discrete(project, seq, track, new_clip, action);
                // Auto-open the curve editor so the three sections are visible
                // and immediately draggable (CapCut-style "open → drag").
                if is_ramped {
                    let curve_edit_id = ui.id().with(("speed_curve_edit", clip.id));
                    ui.data_mut(|data| data.insert_temp(curve_edit_id, true));
                }
            }
            ui.add_space(2.0);
            match &clip.speed {
                SpeedMap::Constant(r) => {
                    draw_constant_speed_row(ui, *r, project, seq, track, clip, action);
                }
                SpeedMap::Keyframed { keys } => {
                    draw_speed_ramp_editor(ui, keys, project, seq, track, clip, action);
                }
            }
        });
}

fn draw_constant_speed_row(
    ui: &mut Ui,
    r: Ratio,
    project: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    clip: &Clip,
    action: &mut Option<PanelAction>,
) {
    let mut pct = r.as_f64().abs() * 100.0;
    let mut reversed = r.num < 0;
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Speed:");
        changed |= ui
            .add(
                egui::DragValue::new(&mut pct)
                    .speed(1.0)
                    .range(1.0..=10000.0)
                    .suffix("%"),
            )
            .changed();
        changed |= ui.checkbox(&mut reversed, "Reverse").changed();
    });
    if changed {
        let mut new_clip = clip.clone();
        new_clip.speed = SpeedMap::Constant(ratio_from_pct(pct, reversed));
        set_clip_coalesced(project, seq, track, new_clip, action);
    }
}

/// Speed-ramp editor with presets, a direct signed-speed curve, and precise
/// point controls. The curve is clip-relative, so changing it never resizes
/// the timeline slot.
fn draw_speed_ramp_editor(
    ui: &mut Ui,
    keys: &[SpeedKey],
    project: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    clip: &Clip,
    action: &mut Option<PanelAction>,
) {
    let mut sorted: Vec<SpeedKey> = keys.to_vec();
    sorted.sort_by_key(|k| k.at.0);
    let clip_secs = clip.duration.as_seconds_f64().max(0.01);
    let frame_ticks = project
        .sequences
        .get(&seq)
        .map(|sequence| sequence.frame_rate.ticks_per_frame().0)
        .unwrap_or(TICKS_PER_SECOND / 30);

    // One mutation per frame at most: whichever row/button changed last wins,
    // same one-write-per-frame shape as every other section in this file.
    let mut new_keys: Option<Vec<SpeedKey>> = None;
    let mut discrete = false;

    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Presets").color(MUTED).small());
        for preset in ["Smooth", "Flow", "Hero", "Action", "Fast Lane"] {
            if ui.small_button(preset).clicked() {
                new_keys = Some(preset_speed_keys(preset, clip.duration));
                discrete = true;
                // Presets are meant to be tweaked on the curve immediately.
                ui.data_mut(|data| {
                    data.insert_temp(ui.id().with(("speed_curve_edit", clip.id)), true)
                });
            }
        }
    });
    let curve_edit_id = ui.id().with(("speed_curve_edit", clip.id));
    // Default the curve open for ramps so the three sections are the first
    // thing the user sees (toggle still lets them hide it).
    let mut editing_curve = ui
        .data(|data| data.get_temp::<bool>(curve_edit_id))
        .unwrap_or(true);
    if ui
        .small_button(if editing_curve {
            "Hide speed curve"
        } else {
            "Edit speed curve"
        })
        .clicked()
    {
        editing_curve = !editing_curve;
        ui.data_mut(|data| data.insert_temp(curve_edit_id, editing_curve));
    }

    // Expand a legacy single-key (or empty) ramp into three sections the first
    // time the curve is shown — never stomp a multi-point ramp the user has
    // already shaped (2–3 custom handles stay as-is).
    if editing_curve && sorted.len() <= 1 && new_keys.is_none() {
        let base = sorted.first().map(|k| k.ratio).unwrap_or(Ratio::ONE);
        new_keys = Some(default_three_section_ramp(clip.duration, base));
        discrete = true;
        sorted = new_keys.clone().unwrap_or(sorted);
    }

    if editing_curve {
        ui.label(
            RichText::new(
                "Drag the handles (three sections by default). Click empty curve to add a point; drag through 0× for reverse.",
            )
            .color(MUTED)
            .small(),
        );
        if let Some(edited) = speed_curve_canvas(ui, clip, &sorted, frame_ticks) {
            new_keys = Some(edited);
        }
    }

    if sorted.is_empty() {
        ui.label(
            RichText::new("No ramp points yet — add one to start shaping the ramp.")
                .color(MUTED)
                .small(),
        );
    } else {
        egui::Grid::new(("clip_speed_ramp_grid", clip.id))
            .num_columns(5)
            .spacing([4.0, 2.0])
            .show(ui, |ui| {
                ui.label(RichText::new("At").color(MUTED).small());
                ui.label(RichText::new("Speed").color(MUTED).small());
                ui.label(RichText::new("Rev").color(MUTED).small());
                ui.label(RichText::new("Ease").color(MUTED).small());
                ui.label("");
                ui.end_row();

                for (i, key) in sorted.iter().enumerate() {
                    let mut at_secs = key.at.as_seconds_f64();
                    let mut pct = key.ratio.as_f64().abs() * 100.0;
                    let mut reversed = key.ratio.num < 0;
                    let at_resp = ui.add(
                        egui::DragValue::new(&mut at_secs)
                            .speed(0.05)
                            .range(0.0..=clip_secs)
                            .suffix("s"),
                    );
                    let pct_resp = ui.add(
                        egui::DragValue::new(&mut pct)
                            .speed(1.0)
                            .range(0.0..=1000.0)
                            .suffix("%"),
                    );
                    let rev_resp = ui.checkbox(&mut reversed, "");
                    let mut interp = key.interp;
                    egui::ComboBox::from_id_salt(("speed_interp", clip.id, key.at))
                        .selected_text(match interp {
                            Interp::Hold => "Hold",
                            Interp::Linear => "Linear",
                            Interp::Bezier { .. } => "Bezier",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut interp, Interp::Hold, "Hold");
                            ui.selectable_value(&mut interp, Interp::Linear, "Linear");
                            ui.selectable_value(
                                &mut interp,
                                EasePreset::EaseInOut.interp(),
                                "Ease In-Out",
                            );
                        });
                    let remove = ui
                        .add(egui::Button::new(RichText::new(ph::X)).small())
                        .on_hover_text("Remove point");
                    if at_resp.changed()
                        || pct_resp.changed()
                        || rev_resp.changed()
                        || interp != key.interp
                    {
                        let mut edited = sorted.clone();
                        edited[i] = SpeedKey {
                            at: Tick((at_secs * TICKS_PER_SECOND as f64).round() as i64)
                                .clamp(Tick::ZERO, clip.duration),
                            ratio: ratio_from_pct(pct, reversed),
                            interp,
                        };
                        new_keys = Some(edited);
                    }
                    if remove.clicked() {
                        let mut edited = sorted.clone();
                        edited.remove(i);
                        new_keys = Some(edited);
                        discrete = true;
                    }
                    ui.end_row();
                }
            });
    }

    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::new(RichText::new(format!("{} Add point", ph::PLUS))).small())
            .clicked()
        {
            let mut edited = sorted.clone();
            let default_at = match sorted.last() {
                None => Tick(clip.duration.0 / 2),
                Some(last) => {
                    let one_sec_later = Tick(last.at.0.saturating_add(TICKS_PER_SECOND));
                    if one_sec_later < clip.duration {
                        one_sec_later
                    } else {
                        // Clip too short for a full extra second — split the
                        // remaining gap to the clip's end instead.
                        Tick((last.at.0 + clip.duration.0) / 2)
                    }
                }
            }
            .min(clip.duration);
            let default_ratio = ramp_ratio_at(&sorted, default_at);
            edited.push(SpeedKey::new(default_at, default_ratio));
            new_keys = Some(edited);
            discrete = true;
        }
        if !sorted.is_empty() && ui.small_button("Clear ramp").clicked() {
            new_keys = Some(Vec::new());
            discrete = true;
        }
    });

    if let Some(first) = sorted.first() {
        let mut selected_interp = first.interp;
        ui.collapsing("Selected segment controls", |ui| {
            ui.label(RichText::new("Use the row's Ease picker to select a segment. Custom Bezier handles apply to the first segment.").small().color(MUTED));
            if let Interp::Bezier {
                mut out_handle,
                mut in_handle,
            } = selected_interp
            {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Out");
                    changed |= ui.add(egui::Slider::new(&mut out_handle[0], 0.0..=1.0).text("X")).changed();
                    changed |= ui.add(egui::Slider::new(&mut out_handle[1], 0.0..=1.0).text("Y")).changed();
                });
                ui.horizontal(|ui| {
                    ui.label("In");
                    changed |= ui.add(egui::Slider::new(&mut in_handle[0], 0.0..=1.0).text("X")).changed();
                    changed |= ui.add(egui::Slider::new(&mut in_handle[1], 0.0..=1.0).text("Y")).changed();
                });
                if changed {
                    selected_interp = Interp::Bezier { out_handle, in_handle };
                    let mut edited = sorted.clone();
                    edited[0].interp = selected_interp;
                    new_keys = Some(edited);
                }
            }
        });
    }

    if let Some(mut edited) = new_keys {
        edited.sort_by_key(|k| k.at.0);
        let mut new_clip = clip.clone();
        new_clip.speed = SpeedMap::Keyframed { keys: edited };
        if discrete {
            set_clip_discrete(project, seq, track, new_clip, action);
        } else {
            set_clip_coalesced(project, seq, track, new_clip, action);
        }
    }
}

// ── Reframe (05 §4.2, CAP-012) ───────────────────────────────────────────────

/// Scale the renderer's normalized source back to its display aspect, covering
/// the output. Store ordinary transforms so UI and MCP retain identical edits.
fn cover_scales(sw: u32, sh: u32, pixel_aspect: f64, fw: u32, fh: u32) -> Option<(f64, f64)> {
    if sw == 0 || sh == 0 || fw == 0 || fh == 0 || !pixel_aspect.is_finite() || pixel_aspect <= 0.0
    {
        return None;
    }
    let ratio = (sw as f64 * pixel_aspect / sh as f64) / (fw as f64 / fh as f64);
    Some(if ratio >= 1.0 {
        (ratio, 1.0)
    } else {
        (1.0, 1.0 / ratio)
    })
}

fn draw_reframe_section(
    ui: &mut Ui,
    project: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    clip: &Clip,
    active_format: usize,
    action: &mut Option<PanelAction>,
) {
    // Recomputed here (not a 9th parameter — clippy's too-many-arguments
    // limit) from the same `project`/`seq` every other section already takes.
    let format_count = project
        .sequences
        .get(&seq)
        .map(|s| s.formats.len())
        .unwrap_or(1);
    egui::CollapsingHeader::new("Reframe")
        .default_open(true)
        .id_salt("clip_inspector_reframe")
        .show(ui, |ui| {
            // Per-format override dot row (13 §5.1: "small format-index chip
            // row shows which formats already have overrides").
            ui.horizontal(|ui| {
                for i in 0..format_count {
                    let overridden = clip.reframe.contains_key(&i);
                    let color = if i == active_format { ACCENT } else { MUTED };
                    let glyph = if overridden { "\u{25CF}" } else { "\u{25CB}" };
                    ui.label(RichText::new(glyph).color(color))
                        .on_hover_text(format!(
                            "Format {i}{}{}",
                            if i == active_format { " (active)" } else { "" },
                            if overridden { " — has override" } else { "" }
                        ));
                }
            });

            let has_override = clip.reframe.contains_key(&active_format);
            let base = clip
                .reframe
                .get(&active_format)
                .copied()
                .unwrap_or(clip.transform.base);
            let mut new_t = base;
            let mut changed = false;
            let format = project.sequences.get(&seq).and_then(|s| s.formats.get(active_format));
            if let Some(format) = format {
                ui.label(format!("{} · {} × {}", format.name, format.width, format.height));
                let source = clip.source.asset()
                    .and_then(|id| project.media.assets.get(&id))
                    .and_then(|asset| asset.probe.as_ref())
                    .and_then(|probe| probe.video.as_ref());
                let scales = source.and_then(|v| cover_scales(
                    v.width, v.height, v.pixel_aspect as f64, format.width, format.height,
                ));
                if ui.add_enabled(scales.is_some(), egui::Button::new("Fill frame"))
                    .on_hover_text("Center and crop to fill this format without stretching. Resets position and rotation for this format.")
                    .clicked()
                {
                    if let Some((scale_x, scale_y)) = scales {
                        let mut new_clip = clip.clone();
                        new_clip.reframe.insert(active_format, photonic_core::timeline::ClipTransform {
                            scale_x, scale_y, opacity: base.opacity, ..Default::default()
                        });
                        set_clip_discrete(project, seq, track, new_clip, action);
                    }
                }
                if scales.is_none() {
                    ui.label(RichText::new("Probe source media to enable Fill frame.").small().color(MUTED));
                }
            }
            egui::Grid::new("clip_reframe_grid")
                .num_columns(2)
                .spacing([4.0, 2.0])
                .show(ui, |ui| {
                    changed |= transform_row(ui, "X", &mut new_t.x, None);
                    ui.end_row();
                    changed |= transform_row(ui, "Y", &mut new_t.y, None);
                    ui.end_row();
                    changed |= transform_row(ui, "Scale X", &mut new_t.scale_x, Some(0.0..=1000.0));
                    ui.end_row();
                    changed |= transform_row(ui, "Scale Y", &mut new_t.scale_y, Some(0.0..=1000.0));
                    ui.end_row();
                    changed |= transform_row_suffixed(ui, "Rotation", &mut new_t.rotation, " rad");
                    ui.end_row();
                });
            if changed {
                let mut new_clip = clip.clone();
                new_clip.reframe.insert(active_format, new_t);
                set_clip_coalesced(project, seq, track, new_clip, action);
            }
            ui.add_enabled_ui(has_override, |ui| {
                if ui.button("Reset reframe for this format").clicked() {
                    let mut new_clip = clip.clone();
                    new_clip.reframe.remove(&active_format);
                    set_clip_discrete(project, seq, track, new_clip, action);
                }
            });
        });
}

// ── Level horizon (18 DJI parity D-5) ───────────────────────────────────────

fn draw_level_horizon_section(
    ui: &mut Ui,
    project: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    clip: &Clip,
    active_format: usize,
    action: &mut Option<PanelAction>,
) {
    let Some(sequence) = project.sequences.get(&seq) else {
        return;
    };
    if sequence.track(track).map(|t| t.kind) != Some(TrackKind::Video) {
        return;
    }
    let format = sequence
        .formats
        .get(active_format)
        .unwrap_or_else(|| sequence.format());

    egui::CollapsingHeader::new("Level Horizon")
        .default_open(false)
        .id_salt("clip_inspector_level_horizon")
        .show(ui, |ui| {
            ui.label(
                RichText::new(format!(
                    "Applies to active format: {} ({} x {})",
                    format.name, format.width, format.height
                ))
                .color(MUTED)
                .small(),
            )
            .on_hover_text(
                "This correction creates or updates only the active format's reframe override.",
            );

            let effective = clip
                .reframe
                .get(&active_format)
                .copied()
                .unwrap_or(clip.transform.base);
            let mut correction_degrees =
                crate::app::reframe::normalized_horizon_degrees(effective.rotation)
                    .unwrap_or(0.0);
            ui.horizontal(|ui| {
                let label = ui.label("Correction");
                let changed = ui
                    .add(
                        egui::DragValue::new(&mut correction_degrees)
                            .speed(0.1)
                            .fixed_decimals(2)
                            .range(-180.0..=180.0)
                            .suffix(" deg"),
                    )
                    .labelled_by(label.id)
                    .on_hover_text(
                        "Roll correction for the active format, shown in degrees. Zero applies no correction.",
                    )
                    .changed();
                if changed && correction_degrees.is_finite() {
                    let mut new_t = effective;
                    new_t.rotation = correction_degrees.clamp(-180.0, 180.0).to_radians();
                    let mut new_clip = clip.clone();
                    new_clip.reframe.insert(active_format, new_t);
                    set_clip_coalesced(project, seq, track, new_clip, action);
                }
            });

            let centered = crate::app::reframe::horizon_auto_crop_is_centered(
                format.width as f64,
                format.height as f64,
                &effective,
            );
            let crop_scales = crate::app::reframe::horizon_auto_crop_scales(
                format.width as f64,
                format.height as f64,
                &effective,
            );
            let needs_crop = crop_scales.is_some_and(|(scale_x, scale_y)| {
                (scale_x - effective.scale_x).abs() > f64::EPSILON
                    || (scale_y - effective.scale_y).abs() > f64::EPSILON
            });
            let crop_tooltip = if !centered {
                "Auto-crop requires zero X/Y translation and an anchor at the active format's center."
            } else if crop_scales.is_none() {
                "Auto-crop requires a finite correction angle and positive finite X/Y scales."
            } else if !needs_crop {
                "The current scale already hides the rotated corners."
            } else {
                "Raises both scales by the smallest shared multiplier needed to hide the rotated corners."
            };
            let crop_response = ui
                .add_enabled(needs_crop, egui::Button::new("Auto-crop to hide corners"))
                .on_hover_text(crop_tooltip);
            if let (true, Some((scale_x, scale_y))) = (crop_response.clicked(), crop_scales) {
                let mut new_t = effective;
                // A shared multiplier preserves intentional X/Y scale ratios;
                // only scale changes, so position, anchor, opacity, and the
                // user's correction angle remain exactly as entered.
                new_t.scale_x = scale_x;
                new_t.scale_y = scale_y;
                let mut new_clip = clip.clone();
                new_clip.reframe.insert(active_format, new_t);
                set_clip_discrete(project, seq, track, new_clip, action);
            }
        });
}

// ── Effects stack (13 §5.1, 08 §2) ──────────────────────────────────────────

/// Whether `clip` carries any effect in its stack (enabled or disabled) — a
/// glanceable "has effects" signal. Exposed `pub(crate)` so another surface
/// (e.g. a future `app/timeline/clips.rs` fx badge, 14 §M-8) can read it
/// without re-deriving the effects-stack check itself; this story's territory
/// stops at `clip_inspector.rs`/`effects_browser.rs`, so wiring the badge into
/// the timeline lane paint is left to whoever picks that up — until then this
/// has no in-crate caller, hence the allow.
#[allow(dead_code)]
pub(crate) fn clip_has_effects(clip: &Clip) -> bool {
    !clip.effects.is_empty()
}

/// Display name for a stacked effect — prefer the live manifest name so
/// K-B16 catalogue entries (and any future id) show correctly; fall back to
/// the seven v1 kind labels, then a generic "Effect".
/// D-12 gyro stabilization recipe editor (22 §6.5).
///
/// Shows the motion source and its status, the lens profile, the strength and
/// crop controls, and an Analyze action. Only video clips can carry motion
/// metadata, so the section hides itself elsewhere rather than offering a
/// control that could never do anything.
///
/// Slider edits go through [`set_clip_coalesced`] so a drag commits **one**
/// undo entry (22 §6.5), while discrete choices commit immediately.
fn draw_stabilization_section(
    ui: &mut Ui,
    project: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    clip: &Clip,
    action: &mut Option<PanelAction>,
) {
    use photonic_core::timeline::{LensProfileRef, MotionSourceRef, StabilizationCropMode};

    let Some(sequence) = project.sequences.get(&seq) else {
        return;
    };
    if sequence.track(track).map(|t| t.kind) != Some(TrackKind::Video) {
        return;
    }

    egui::CollapsingHeader::new("Stabilization")
        .default_open(false)
        .id_salt("clip_inspector_stabilization")
        .show(ui, |ui| {
            let Some(spec) = clip.stabilization.as_ref() else {
                ui.label(
                    RichText::new("No motion metadata bound to this clip.")
                        .color(MUTED)
                        .small(),
                );
                ui.label(
                    RichText::new(
                        "Import a gyro sidecar (.gcsv or Photonic gyro JSON) to stabilize \
                         this shot.",
                    )
                    .color(MUTED)
                    .small(),
                );
                if ui
                    .button(format!("{} Import motion metadata…", ph::FILE_ARROW_UP))
                    .on_hover_text(
                        "Bind a gyro/IMU sidecar to this clip. Camera-embedded telemetry \
                         is not yet supported.",
                    )
                    .clicked()
                {
                    *action = Some(PanelAction::ImportMotionMetadata { clip: clip.id });
                }
                return;
            };

            // ── source + status ─────────────────────────────────────────
            let source_label = match &spec.binding.source {
                MotionSourceRef::Embedded { .. } => "Embedded in media".to_string(),
                MotionSourceRef::Sidecar { path, .. } => path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string()),
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new("Source").color(MUTED).small());
                ui.label(RichText::new(source_label).small()).on_hover_text(
                    match &spec.binding.source {
                        MotionSourceRef::Sidecar { path, .. } => path.display().to_string(),
                        MotionSourceRef::Embedded { .. } => {
                            "Telemetry carried inside the media file.".to_string()
                        }
                    },
                );
            });

            // Whether an analysis exists is the single most useful status:
            // until one does, the clip renders unstabilized no matter what the
            // sliders say.
            let analyzed = spec.analysis_key.is_some();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Status").color(MUTED).small());
                if analyzed {
                    ui.label(RichText::new("Analyzed").color(ACCENT).small());
                } else {
                    ui.label(
                        RichText::new("Not analyzed — clip renders unstabilized")
                            .color(MUTED)
                            .small(),
                    );
                }
            });

            let anchors = spec.binding.sync.anchors.len();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Clock sync").color(MUTED).small());
                let text = match anchors {
                    0 => "Dialect-declared".to_string(),
                    1 => "1 anchor (offset)".to_string(),
                    n => format!("{n} anchors (offset + rate)"),
                };
                ui.label(RichText::new(text).small()).on_hover_text(
                    "Sensor time is not video time. One anchor fixes an offset; two or \
                     more also fit the clock rate and report drift.",
                );
            });

            ui.horizontal(|ui| {
                ui.label(RichText::new("Lens").color(MUTED).small());
                let text = match &spec.binding.lens {
                    LensProfileRef::RotationOnly => "Rotation only (uncalibrated)".to_string(),
                    LensProfileRef::UserFile { path, .. } => path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string()),
                    LensProfileRef::Bundled { id } => id.clone(),
                };
                ui.label(RichText::new(text).small()).on_hover_text(
                    "Without a calibrated lens, only rotation is corrected — lens \
                     distortion is left alone.",
                );
            });

            ui.add_space(4.0);
            ui.separator();

            // ── strength controls ───────────────────────────────────────
            let mut edited = spec.clone();
            let mut changed = false;

            ui.horizontal(|ui| {
                let label = ui.label("Smoothness");
                let mut v = edited.smoothness as f64;
                if ui
                    .add(egui::Slider::new(&mut v, 0.0..=1.0).fixed_decimals(2))
                    .labelled_by(label.id)
                    .on_hover_text(
                        "How hard to smooth the camera path. Zero follows the original \
                         motion exactly; higher is steadier but demands more crop.",
                    )
                    .changed()
                {
                    edited.smoothness = v as f32;
                    changed = true;
                }
            });

            ui.horizontal(|ui| {
                let label = ui.label("Horizon lock");
                let mut v = edited.horizon_lock as f64;
                if ui
                    .add(egui::Slider::new(&mut v, 0.0..=1.0).fixed_decimals(2))
                    .labelled_by(label.id)
                    .on_hover_text(
                        "Level the horizon against measured gravity. Needs accelerometer \
                         data in the motion source; has no effect without it.",
                    )
                    .changed()
                {
                    edited.horizon_lock = v as f32;
                    changed = true;
                }
            });

            ui.horizontal(|ui| {
                let label = ui.label("Max zoom");
                let mut v = edited.max_zoom as f64;
                if ui
                    .add(egui::Slider::new(&mut v, 1.0..=2.0).fixed_decimals(2))
                    .labelled_by(label.id)
                    .on_hover_text(
                        "Ceiling on how far the crop solver may zoom in to hide the edges \
                         the correction swings out of frame.",
                    )
                    .changed()
                {
                    edited.max_zoom = v as f32;
                    changed = true;
                }
            });

            if changed {
                let mut new_clip = clip.clone();
                new_clip.stabilization = Some(edited.clone());
                set_clip_coalesced(project, seq, track, new_clip, action);
            }

            // ── crop mode (discrete) ────────────────────────────────────
            let current = spec.crop_mode;
            let mode_label = |m: StabilizationCropMode| match m {
                StabilizationCropMode::StaticSafe => "Static safe",
                StabilizationCropMode::Dynamic => "Dynamic",
                StabilizationCropMode::TransparentEdges => "Transparent edges",
                // Forward-compat: a mode written by a newer build is shown as
                // itself rather than silently remapped.
                _ => "Unknown (from a newer build)",
            };
            ui.horizontal(|ui| {
                ui.label("Crop");
                egui::ComboBox::from_id_salt("clip_inspector_stab_crop")
                    .selected_text(mode_label(current))
                    .show_ui(ui, |ui| {
                        for m in [
                            StabilizationCropMode::StaticSafe,
                            StabilizationCropMode::Dynamic,
                            StabilizationCropMode::TransparentEdges,
                        ] {
                            if ui.selectable_label(current == m, mode_label(m)).clicked()
                                && current != m
                            {
                                let mut new_spec = spec.clone();
                                new_spec.crop_mode = m;
                                let mut new_clip = clip.clone();
                                new_clip.stabilization = Some(new_spec);
                                set_clip_discrete(project, seq, track, new_clip, action);
                            }
                        }
                    });
            });

            ui.add_space(4.0);

            // ── actions ─────────────────────────────────────────────────
            ui.horizontal(|ui| {
                let verb = if analyzed { "Reanalyze" } else { "Analyze" };
                if ui
                    .button(format!("{} {verb}", ph::WAVEFORM))
                    .on_hover_text(
                        "Read the motion data, integrate the camera path, and solve the \
                         crop. Runs in the background.",
                    )
                    .clicked()
                {
                    *action = Some(PanelAction::AnalyzeStabilization { clip: clip.id });
                }
                if ui
                    .button(format!("{} Remove", ph::TRASH))
                    .on_hover_text(
                        "Return this clip to its unstabilized source. The motion metadata \
                         and cached analysis are kept.",
                    )
                    .clicked()
                {
                    let mut new_clip = clip.clone();
                    new_clip.stabilization = None;
                    set_clip_discrete(project, seq, track, new_clip, action);
                }
            });
        });
}

fn effect_label(effect: &photonic_core::timeline::ClipEffect) -> String {
    use photonic_core::timeline::manifest;
    if !effect.id.is_empty() {
        if let Some(m) = manifest(effect.id.clone()) {
            return m.name.to_string();
        }
        // Unknown / future id: surface the id rather than a blank "Effect".
        return effect.id.as_str().to_string();
    }
    match effect.kind {
        EffectKind::Blur => "Blur".into(),
        EffectKind::Sharpen => "Sharpen".into(),
        EffectKind::Glow => "Glow".into(),
        EffectKind::ChromaKey => "Chroma Key".into(),
        EffectKind::LumaKey => "Luma Key".into(),
        EffectKind::Invert => "Invert".into(),
        EffectKind::MaskShapeGen => "Mask Shape".into(),
        EffectKind::Unknown(tag) => tag.as_str().to_string(),
        _ => "Effect".into(),
    }
}

/// Which of the four effect stacks (26 §10 K-B1/K-B2, 35 §2) the Effects
/// section is editing. Session-only UI state, parked in egui temp memory
/// rather than `VideoPanelUi` — same idiom `node_editor.rs` uses for its
/// per-graph view state, and it keeps this story inside `panels/video/`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
enum FxScopeTab {
    #[default]
    Clip,
    Track,
    Master,
    Asset,
}

fn fx_scope_id() -> egui::Id {
    egui::Id::new("clip_inspector_fx_scope")
}

/// The asset a clip inherits an effect stack from (K-B2), if it has one.
/// `Adjustment` / `SolidColor` / `Text` / nested-sequence clips have no bin
/// asset, so the Asset tab is unavailable for them.
fn clip_asset(clip: &Clip) -> Option<photonic_core::timeline::AssetId> {
    match clip.source {
        photonic_core::timeline::ClipSource::Asset { asset }
        | photonic_core::timeline::ClipSource::Vector { asset } => Some(asset),
        _ => None,
    }
}

fn owner_for(tab: FxScopeTab, seq: SequenceId, track: TrackId, clip: &Clip) -> Option<VfxOwner> {
    match tab {
        FxScopeTab::Clip => Some(VfxOwner::Clip(clip.id)),
        FxScopeTab::Track => Some(VfxOwner::Track(track)),
        FxScopeTab::Master => Some(VfxOwner::Master(seq)),
        FxScopeTab::Asset => clip_asset(clip).map(VfxOwner::Asset),
    }
}

/// One line of orientation per scope — which stack the rows below belong to and
/// where it sits in the evaluation order (35 §2: asset → clip → track → master).
fn scope_hint(tab: FxScopeTab, project: &TimelineProject, track: TrackId, clip: &Clip) -> String {
    match tab {
        FxScopeTab::Clip => format!("This clip only — \"{}\".", clip.name),
        FxScopeTab::Track => {
            let name = project
                .sequences
                .values()
                .find_map(|s| s.track(track))
                .map(|t| t.name.clone())
                .unwrap_or_default();
            format!("Every clip on track \"{name}\", after they fold together.")
        }
        FxScopeTab::Master => {
            "The whole sequence, after all tracks are merged. Applied last.".to_string()
        }
        FxScopeTab::Asset => {
            "Every timeline instance of this media, before the clip's own stack.".to_string()
        }
    }
}

fn draw_effects_section(
    ui: &mut Ui,
    project: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    clip: &Clip,
    action: &mut Option<PanelAction>,
) {
    egui::CollapsingHeader::new("Effects")
        .default_open(true)
        .id_salt("clip_inspector_effects")
        .show(ui, |ui| {
            let mut tab: FxScopeTab = ui.data(|d| d.get_temp(fx_scope_id())).unwrap_or_default();
            let has_asset = clip_asset(clip).is_some();
            ui.horizontal_wrapped(|ui| {
                for (t, label) in [
                    (FxScopeTab::Clip, "Clip"),
                    (FxScopeTab::Track, "Track"),
                    (FxScopeTab::Master, "Master"),
                    (FxScopeTab::Asset, "Asset"),
                ] {
                    let enabled = t != FxScopeTab::Asset || has_asset;
                    let resp = ui.add_enabled(enabled, egui::SelectableLabel::new(tab == t, label));
                    if resp.clicked() {
                        tab = t;
                    }
                    if t == FxScopeTab::Asset && !has_asset {
                        resp.on_hover_text(
                            "This clip has no bin asset (adjustment / title / solid).",
                        );
                    }
                }
            });
            if tab == FxScopeTab::Asset && !has_asset {
                tab = FxScopeTab::Clip;
            }
            ui.data_mut(|d| d.insert_temp(fx_scope_id(), tab));
            ui.label(
                RichText::new(scope_hint(tab, project, track, clip))
                    .color(MUTED)
                    .small(),
            );

            let Some(owner) = owner_for(tab, seq, track, clip) else {
                return;
            };
            let Ok(stack) = ops::effect_stack(project, owner) else {
                ui.label(RichText::new("Stack unavailable.").color(MUTED).small());
                return;
            };

            let drop_rect = ui.available_rect_before_wrap();
            for i in 0..stack.len() {
                let effect = &stack[i];
                ui.push_id(("fx_row", scope_salt(owner), i), |ui| {
                    ui.horizontal(|ui| {
                        let mut enabled = effect.enabled;
                        if ui.checkbox(&mut enabled, "").changed() {
                            let mut new_effect = effect.clone();
                            new_effect.enabled = enabled;
                            if let Ok(cmd) = ops::set_effect_scoped(project, owner, i, new_effect) {
                                *action = Some(PanelAction::ClipEditDiscrete(cmd));
                            }
                        }
                        let name = effect_label(effect);
                        let label = if enabled {
                            RichText::new(name)
                        } else {
                            RichText::new(name).color(MUTED)
                        };
                        ui.label(label);
                        // Keyboard-reachable reorder fallback (13 §16 finding
                        // #1's flagged a11y gap for drag-only stacks) — up/down
                        // buttons instead of/alongside drag.
                        if ui
                            .add_enabled(i > 0, egui::Button::new("\u{2191}").small())
                            .on_hover_text("Move up")
                            .clicked()
                        {
                            if let Ok(cmd) = ops::reorder_effects_scoped(
                                project,
                                owner,
                                swapped_order(stack.len(), i, i - 1),
                            ) {
                                *action = Some(PanelAction::ClipEditDiscrete(cmd));
                            }
                        }
                        if ui
                            .add_enabled(i + 1 < stack.len(), egui::Button::new("\u{2193}").small())
                            .on_hover_text("Move down")
                            .clicked()
                        {
                            if let Ok(cmd) = ops::reorder_effects_scoped(
                                project,
                                owner,
                                swapped_order(stack.len(), i, i + 1),
                            ) {
                                *action = Some(PanelAction::ClipEditDiscrete(cmd));
                            }
                        }
                        if ui
                            .button("\u{2715}")
                            .on_hover_text("Remove effect")
                            .clicked()
                        {
                            if let Ok(cmd) = ops::remove_effect_scoped(project, owner, i) {
                                *action = Some(PanelAction::ClipEditDiscrete(cmd));
                            }
                        }
                    });
                    egui::CollapsingHeader::new("params")
                        .id_salt(("fx_params", scope_salt(owner), i))
                        .default_open(false)
                        .show(ui, |ui| {
                            draw_effect_params(ui, project, owner, effect, i, action);
                        });
                });
            }
            // K-B4: apply a saved preset to *this* scope, or save this scope's
            // stack (plus its grade) as a new one. Applying is one undo unit;
            // saving is a config-file write and is not undoable — the bar's
            // hover text says which is which.
            super::effect_presets::draw_scope_preset_bar(ui, project, owner, action);

            if stack.is_empty() {
                // Only the clip stack has the double-click shortcut (the
                // Effects browser applies to the *clip selection*); every scope
                // accepts a drag onto this section.
                let hint = if tab == FxScopeTab::Clip {
                    "No effects — drag one from the Effects browser, or double-click it \
                     there to apply to this clip."
                } else {
                    "No effects — drag one from the Effects browser onto this section."
                };
                ui.label(RichText::new(hint).color(MUTED).small());
            }

            // Drop target for an Effects-browser drag (13 §6.3: "the Clip
            // Inspector's effects-stack section" is a valid drop target,
            // mirroring the media-pool → timeline `AssetDrag` idiom). The drop
            // lands on whichever scope tab is showing.
            if let Some(payload) =
                egui::DragAndDrop::payload::<super::effects_browser::EffectDrag>(ui.ctx())
            {
                let hovering = ui
                    .ctx()
                    .pointer_latest_pos()
                    .is_some_and(|p| drop_rect.contains(p));
                if hovering {
                    ui.painter()
                        .rect_stroke(drop_rect, 3.0, egui::Stroke::new(1.5, ACCENT));
                    if ui.input(|i| i.pointer.any_released()) {
                        egui::DragAndDrop::clear_payload(ui.ctx());
                        let effect =
                            photonic_core::timeline::ClipEffect::from_manifest(payload.id.clone())
                                .or_else(|| {
                                    // Fallback for a payload whose id this build no longer
                                    // knows (shouldn't happen for MANIFESTS-driven drags).
                                    payload
                                        .id
                                        .legacy_kind()
                                        .map(photonic_core::timeline::ClipEffect::new)
                                });
                        if let Some(effect) = effect {
                            if let Ok(cmd) = ops::add_effect_scoped(project, owner, effect, None) {
                                *action = Some(PanelAction::ClipEditDiscrete(cmd));
                            }
                        }
                    }
                }
            }
        });
}

/// A stable per-scope egui id seed, so switching tabs does not collide widget
/// ids between two stacks that happen to have the same row count. `VfxOwner`
/// is `Hash`, so it is its own salt — this exists only to name the intent.
fn scope_salt(owner: VfxOwner) -> VfxOwner {
    owner
}

/// The `new_order` permutation `reorder_effects` expects: swap the elements
/// currently at `a`/`b` in the identity ordering.
fn swapped_order(len: usize, a: usize, b: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    order.swap(a, b);
    order
}

/// Static (non-keyframed) param editor for one stacked effect at any scope.
/// Writes a `SetEffect` command — the one carrier that works for clip, track,
/// master and asset stacks alike (a track stack has no `SetClipProp`-style
/// whole-owner snapshot to ride).
fn draw_effect_params(
    ui: &mut Ui,
    project: &TimelineProject,
    owner: VfxOwner,
    effect: &photonic_core::timeline::ClipEffect,
    effect_idx: usize,
    action: &mut Option<PanelAction>,
) {
    let entries = prop_registry::entries(PropTargetKind::Effect(effect.kind));
    if entries.is_empty() {
        ui.label(RichText::new("No parameters.").color(MUTED).small());
        return;
    }
    // K-B6: cross-param refs (`%radius`, bare `amount`) + optional frame size.
    let float_pairs: Vec<(&str, f64)> = effect
        .params
        .base
        .entries
        .iter()
        .filter_map(|(p, v)| match v {
            PropValue::Float(f) => Some((p.as_str(), *f)),
            _ => None,
        })
        .collect();
    let (frame_w, frame_h) = active_frame_size(project);
    let vars = param_expr::vars_from_params(float_pairs, frame_w, frame_h);
    let seeded = EffectParams::seed(PropTargetKind::Effect(effect.kind));

    egui::Grid::new(("fx_params_grid", scope_salt(owner), effect_idx))
        .num_columns(2)
        .spacing([4.0, 2.0])
        .show(ui, |ui| {
            for entry in entries {
                let Some(cur) = effect.params.base.get(entry.path) else {
                    continue;
                };
                ui.label(param_short_label(entry.path));
                let mut new_value: Option<PropValue> = None;
                match *cur {
                    PropValue::Float(f) => {
                        let mut v = f;
                        let default = match seeded.get(entry.path) {
                            Some(PropValue::Float(d)) => *d,
                            _ => param_expr::neutral_float_default(entry.range),
                        };
                        if param_expr::float_drag(ui, &mut v, default, entry.range, &vars, 0.01) {
                            new_value = Some(PropValue::Float(v));
                        }
                    }
                    PropValue::Bool(b) => {
                        let mut v = b;
                        let resp = ui.checkbox(&mut v, "");
                        let mut changed = resp.changed();
                        // Middle-click reset → seed default (false for bools).
                        if resp.middle_clicked() && v {
                            v = false;
                            changed = true;
                        }
                        if changed {
                            new_value = Some(PropValue::Bool(v));
                        }
                    }
                    PropValue::Color(c) => {
                        let mut v = c;
                        if crate::color_popup::ColorPopup::swatch_color(ui, &mut v).changed() {
                            new_value = Some(PropValue::Color(v));
                        }
                    }
                    PropValue::Vec2(v2) => {
                        let mut v = v2;
                        let r0 = ui.add(egui::DragValue::new(&mut v[0]).speed(0.01));
                        let r1 = ui.add(egui::DragValue::new(&mut v[1]).speed(0.01));
                        if r0.changed() || r1.changed() {
                            new_value = Some(PropValue::Vec2(v));
                        }
                    }
                    PropValue::Enum(e) => {
                        let mut v = e;
                        if ui.add(egui::DragValue::new(&mut v)).changed() {
                            new_value = Some(PropValue::Enum(v));
                        }
                    }
                }
                if let Some(v) = new_value {
                    let mut new_effect = effect.clone();
                    new_effect.params.base.set(entry.path, v);
                    // Coalesced: a drag on one param is one undo unit
                    // (`TimelineCmd::coalesce` merges same owner + index).
                    if let Ok(cmd) = ops::set_effect_scoped(project, owner, effect_idx, new_effect)
                    {
                        *action = Some(PanelAction::ClipEditCoalesced(cmd));
                    }
                }
                ui.end_row();
            }
        });
}

/// Active sequence frame size for K-B6 `%w`/`%h` expressions, if known.
fn active_frame_size(project: &TimelineProject) -> (Option<f64>, Option<f64>) {
    let Some(seq_id) = project.active_sequence else {
        return (None, None);
    };
    let Some(seq) = project.sequences.get(&seq_id) else {
        return (None, None);
    };
    let fmt = seq
        .formats
        .get(seq.active_format)
        .or_else(|| seq.formats.first());
    match fmt {
        Some(f) => (Some(f.width as f64), Some(f.height as f64)),
        None => (None, None),
    }
}

/// `"params.radius"` → `"radius"` for a compact grid label.
fn param_short_label(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

// ── Keyframes (13 §5.2, 14 §G-9 effect-controls unification) ────────────────

/// The docked keyframe/curve editor section — replaces the floating
/// `egui::Window` as the default, always-visible per-clip Effect-Controls
/// surface (14 §G-9: "two surfaces for one concept" between this inspector's
/// fixed Motion/Opacity/Speed fields and the curve editor). The actual
/// picker + curve drawing is [`super::keyframe_editor::draw_embedded`],
/// reused verbatim from the floating editor (`keyframe_editor.rs:draw_window`)
/// rather than reimplemented; this fn is only the section chrome plus a
/// pop-out button that still opens the float for live-playhead scrubbing or
/// the wider side-by-side layout (watch-out: "keep the float option").
fn draw_keyframes_section(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    project: &TimelineProject,
    clip: &Clip,
    action: &mut Option<PanelAction>,
) {
    egui::CollapsingHeader::new("Keyframes")
        .default_open(true)
        .id_salt("clip_inspector_keyframes")
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Transform + effect params, animated over the clip")
                        .color(MUTED)
                        .small(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(egui::Button::new(RichText::new(ph::ARROW_SQUARE_OUT)).small())
                        .on_hover_text("Pop out to a floating window (live playhead scrubbing)")
                        .clicked()
                    {
                        *ctx.video.keyframe_editor_target = Some(clip.id);
                    }
                });
            });
            if let Some(a) = super::keyframe_editor::draw_embedded(ui, project, clip) {
                *action = Some(a);
            }
        });
}

// ── Transitions (13 §5.1, 08 §2.0b) ─────────────────────────────────────────

const TRANSITION_KINDS: [TransitionKind; 6] = [
    TransitionKind::CrossDissolve,
    TransitionKind::DipToBlack,
    TransitionKind::DipToColor,
    TransitionKind::Wipe,
    TransitionKind::Push,
    TransitionKind::LumaWipe,
];

fn transition_label(kind: TransitionKind) -> &'static str {
    match kind {
        TransitionKind::CrossDissolve => "Cross Dissolve",
        TransitionKind::DipToBlack => "Dip to Black",
        TransitionKind::DipToColor => "Dip to Color",
        TransitionKind::Wipe => "Wipe",
        TransitionKind::Push => "Push",
        TransitionKind::LumaWipe => "Masked Wipe",
        // Forward-compat (39 §2.2): show the preserved tag; renders as a cut.
        TransitionKind::Unknown(t) => t.as_str(),
        // `#[non_exhaustive]`: a kind a newer build adds shows a placeholder.
        _ => "Unsupported",
    }
}

fn draw_transitions_section(
    ui: &mut Ui,
    project: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    clip: &Clip,
    action: &mut Option<PanelAction>,
) {
    egui::CollapsingHeader::new("Transitions")
        .default_open(false)
        .id_salt("clip_inspector_transitions")
        .show(ui, |ui| {
            draw_one_transition(ui, project, seq, track, clip, true, action);
            ui.separator();
            draw_one_transition(ui, project, seq, track, clip, false, action);
        });
}

fn draw_one_transition(
    ui: &mut Ui,
    project: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    clip: &Clip,
    is_in: bool,
    action: &mut Option<PanelAction>,
) {
    let label = if is_in { "In" } else { "Out" };
    let current = if is_in {
        &clip.transition_in
    } else {
        &clip.transition_out
    };
    ui.horizontal(|ui| {
        ui.label(label).on_hover_text(if is_in {
            "Blends from the preceding clip at the cut using its available source handles"
        } else {
            "Fades at the end of a clip before a gap or sequence end"
        });
        match current {
            None => {
                if ui.small_button("Add Transition").clicked() {
                    let mut new_clip = clip.clone();
                    let t = Transition::new(
                        TransitionKind::CrossDissolve,
                        Tick(TICKS_PER_SECOND / 2), // 0.5s default overlap
                    );
                    if is_in {
                        new_clip.transition_in = Some(t);
                    } else {
                        new_clip.transition_out = Some(t);
                    }
                    set_clip_discrete(project, seq, track, new_clip, action);
                }
            }
            Some(t) => {
                let mut kind = t.kind;
                egui::ComboBox::new(("transition_kind", clip.id, is_in), "")
                    .selected_text(transition_label(kind))
                    .show_ui(ui, |ui| {
                        for k in TRANSITION_KINDS {
                            ui.selectable_value(&mut kind, k, transition_label(k));
                        }
                    });
                let mut secs = t.duration.as_seconds_f64();
                let dur_changed = ui
                    .add(
                        egui::DragValue::new(&mut secs)
                            .speed(0.05)
                            .range(0.01..=60.0)
                            .suffix("s"),
                    )
                    .changed();
                if kind != t.kind || dur_changed {
                    let mut new_clip = clip.clone();
                    let new_t = Transition {
                        kind,
                        duration: Tick((secs * TICKS_PER_SECOND as f64) as i64),
                        params: if kind == t.kind {
                            t.params
                        } else {
                            TransitionParams::default()
                        },
                    };
                    if is_in {
                        new_clip.transition_in = Some(new_t);
                    } else {
                        new_clip.transition_out = Some(new_t);
                    }
                    set_clip_coalesced(project, seq, track, new_clip, action);
                }
                if ui.small_button("Remove").clicked() {
                    let mut new_clip = clip.clone();
                    if is_in {
                        new_clip.transition_in = None;
                    } else {
                        new_clip.transition_out = None;
                    }
                    set_clip_discrete(project, seq, track, new_clip, action);
                }
            }
        }
    });
    if let Some(t) = current {
        let mut params = t.params;
        ui.push_id(("transition_parameters", clip.id, is_in), |ui| {
            draw_transition_parameters(ui, t.kind, &mut params);
        });
        if params != t.params {
            let mut new_clip = clip.clone();
            let updated = Transition { params, ..*t };
            if is_in {
                new_clip.transition_in = Some(updated);
            } else {
                new_clip.transition_out = Some(updated);
            }
            set_clip_coalesced(project, seq, track, new_clip, action);
        }
    }
}

fn draw_transition_parameters(ui: &mut Ui, kind: TransitionKind, params: &mut TransitionParams) {
    use photonic_core::timeline::{EaseCurve, LumaWipeMap, WipeDirection};
    egui::ComboBox::from_label("Easing")
        .selected_text(match params.curve {
            EaseCurve::Linear => "Linear",
            EaseCurve::EaseIn => "Ease in",
            EaseCurve::EaseOut => "Ease out",
            EaseCurve::EaseInOut => "Ease in and out",
        })
        .show_ui(ui, |ui| {
            for (value, label) in [
                (EaseCurve::Linear, "Linear"),
                (EaseCurve::EaseIn, "Ease in"),
                (EaseCurve::EaseOut, "Ease out"),
                (EaseCurve::EaseInOut, "Ease in and out"),
            ] {
                ui.selectable_value(&mut params.curve, value, label);
            }
        });
    if matches!(kind, TransitionKind::Wipe | TransitionKind::Push) {
        egui::ComboBox::from_label("Direction")
            .selected_text(format!("{:?}", params.direction))
            .show_ui(ui, |ui| {
                for value in [
                    WipeDirection::Left,
                    WipeDirection::Right,
                    WipeDirection::Up,
                    WipeDirection::Down,
                ] {
                    ui.selectable_value(&mut params.direction, value, format!("{value:?}"));
                }
            });
    }
    if kind == TransitionKind::LumaWipe {
        let maps = [
            (LumaWipeMap::LinearH, "Horizontal"),
            (LumaWipeMap::LinearV, "Vertical"),
            (LumaWipeMap::Radial, "Iris"),
            (LumaWipeMap::BarnDoorH, "Barn doors"),
            (LumaWipeMap::Clock, "Clock"),
        ];
        let selected = maps
            .iter()
            .find(|(value, _)| *value == params.luma_map)
            .map_or("Mask", |(_, label)| *label);
        egui::ComboBox::from_label("Mask shape")
            .selected_text(selected)
            .show_ui(ui, |ui| {
                for (value, label) in maps {
                    ui.selectable_value(&mut params.luma_map, value, label);
                }
            });
        ui.checkbox(&mut params.invert, "Reverse mask");
    }
    if matches!(kind, TransitionKind::Wipe | TransitionKind::LumaWipe) {
        ui.add(egui::Slider::new(&mut params.softness, 0.0..=0.5).text("Edge softness"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masked_wipe_reverse_control_is_undoable() {
        use photonic_core::history::{Command, CommandHistory};
        use photonic_core::timeline::media::{ProbedColor, ScanType, VideoStreamInfo};
        use photonic_core::timeline::{
            AssetKind, AssetSource, ClipSource, FrameRate, MediaAsset, MediaProbe, Sequence, Track,
        };
        let mut project = TimelineProject::new();
        let mut asset = MediaAsset::new(
            AssetKind::Video,
            AssetSource::File {
                path: "fixture.mov".into(),
                rel_path: None,
            },
        );
        let mut probe = MediaProbe::basic(Tick(1000), "mov", "h264");
        probe.video = Some(VideoStreamInfo {
            width: 3840,
            height: 2160,
            frame_rate: FrameRate::FPS_30,
            pixel_aspect: 1.0,
            color: ProbedColor::default(),
            keyframe_index_cached: false,
            scan: ScanType::Progressive,
        });
        asset.probe = Some(probe);
        let asset_id = project.media.insert(asset);
        let mut seq = Sequence::new("Portrait", FrameRate::FPS_30, 1080, 1920);
        let mut track = Track::new(TrackKind::Video, "V1");
        let mut clip = Clip::new(ClipSource::Asset { asset: asset_id }, Tick(0), Tick(1000));
        clip.transition_in = Some(Transition::new(
            TransitionKind::LumaWipe,
            Tick(TICKS_PER_SECOND / 2),
        ));
        let (sid, tid, cid) = (seq.id, track.id, clip.id);
        track.clips.push(clip.clone());
        seq.video_tracks.push(track);
        project.insert_sequence(seq);
        let ctx = egui::Context::default();
        ctx.style_mut(|s| s.animation_time = 0.0);
        let mut action = None;
        let mut run = |events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 800.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        draw_one_transition(ui, &project, sid, tid, &clip, true, &mut action)
                    });
                },
            )
        };
        let output = run(vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text() == "Reverse mask" => {
                    Some(t.pos + t.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("Masked wipe parameters are visible");
        let _ = run(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        let _ = run(vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        let Some(PanelAction::ClipEditCoalesced(cmd)) = action else {
            panic!("click must produce one discrete edit");
        };
        let mut doc = photonic_core::Document::new("test", 1080.0, 1920.0);
        doc.timeline = Some(project);
        let before = doc.timeline.clone();
        let mut history = CommandHistory::new(10);
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
        let after = doc.timeline.clone();
        let (_, _, edited) = locate_clip(doc.timeline.as_ref().unwrap(), cid).unwrap();
        assert!(edited.transition_in.as_ref().unwrap().params.invert);
        assert!(history.undo(&mut doc));
        assert_eq!(doc.timeline, before);
        assert!(history.redo(&mut doc));
        assert_eq!(doc.timeline, after);
    }

    #[test]
    fn fill_frame_button_emits_one_undoable_edit() {
        use photonic_core::history::{Command, CommandHistory};
        use photonic_core::timeline::media::{ProbedColor, ScanType, VideoStreamInfo};
        use photonic_core::timeline::{
            AssetKind, AssetSource, ClipSource, FrameRate, MediaAsset, MediaProbe, Sequence, Track,
        };
        let mut project = TimelineProject::new();
        let mut asset = MediaAsset::new(
            AssetKind::Video,
            AssetSource::File {
                path: "fixture.mov".into(),
                rel_path: None,
            },
        );
        let mut probe = MediaProbe::basic(Tick(1000), "mov", "h264");
        probe.video = Some(VideoStreamInfo {
            width: 3840,
            height: 2160,
            frame_rate: FrameRate::FPS_30,
            pixel_aspect: 1.0,
            color: ProbedColor::default(),
            keyframe_index_cached: false,
            scan: ScanType::Progressive,
        });
        asset.probe = Some(probe);
        let asset_id = project.media.insert(asset);
        let mut seq = Sequence::new("Portrait", FrameRate::FPS_30, 1080, 1920);
        let mut track = Track::new(TrackKind::Video, "V1");
        let clip = Clip::new(ClipSource::Asset { asset: asset_id }, Tick(0), Tick(1000));
        let (sid, tid, cid) = (seq.id, track.id, clip.id);
        track.clips.push(clip.clone());
        seq.video_tracks.push(track);
        project.insert_sequence(seq);
        let ctx = egui::Context::default();
        ctx.style_mut(|s| s.animation_time = 0.0);
        let mut action = None;
        let mut run = |events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 800.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        draw_reframe_section(ui, &project, sid, tid, &clip, 0, &mut action)
                    });
                },
            )
        };
        let output = run(vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text() == "Fill frame" => {
                    Some(t.pos + t.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("Fill frame is visible in the inspector without opening another panel");
        let _ = run(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        let _ = run(vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        let Some(PanelAction::ClipEditDiscrete(cmd)) = action else {
            panic!("click must produce one discrete edit");
        };
        let mut doc = photonic_core::Document::new("test", 1080.0, 1920.0);
        doc.timeline = Some(project);
        let before = doc.timeline.clone();
        let mut history = CommandHistory::new(10);
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
        let after = doc.timeline.clone();
        let (_, _, edited) = locate_clip(doc.timeline.as_ref().unwrap(), cid).unwrap();
        assert!((edited.reframe[&0].scale_x - 256.0 / 81.0).abs() < 1e-10);
        assert!(history.undo(&mut doc));
        assert_eq!(doc.timeline, before);
        assert!(history.redo(&mut doc));
        assert_eq!(doc.timeline, after);
    }

    #[test]
    fn fill_frame_preserves_source_aspect_for_portrait_and_landscape() {
        let (x, y) = cover_scales(3840, 2160, 1.0, 1080, 1920).unwrap();
        assert!((x - 256.0 / 81.0).abs() < 1e-10);
        assert_eq!(y, 1.0);
        let (x, y) = cover_scales(1080, 1920, 1.0, 1920, 1080).unwrap();
        assert_eq!(x, 1.0);
        assert!((y - 256.0 / 81.0).abs() < 1e-10);
        assert!(cover_scales(0, 2160, 1.0, 1080, 1920).is_none());
        assert!(cover_scales(3840, 2160, f64::NAN, 1080, 1920).is_none());
    }

    #[test]
    fn swapped_order_swaps_only_the_two_indices() {
        assert_eq!(swapped_order(4, 1, 2), vec![0, 2, 1, 3]);
        assert_eq!(swapped_order(3, 0, 2), vec![2, 1, 0]);
        assert_eq!(swapped_order(1, 0, 0), vec![0]);
    }

    /// The scope tabs resolve to the right owner, and the Asset tab is only
    /// offered for a clip that actually has a bin asset (K-B2).
    #[test]
    fn scope_tabs_resolve_to_the_right_owner() {
        use photonic_core::timeline::{AssetId, ClipSource};

        let seq = SequenceId::new();
        let track = TrackId::new();
        let asset = AssetId::new();
        let media_clip = Clip::new(ClipSource::Asset { asset }, Tick(0), Tick(1000));
        assert_eq!(clip_asset(&media_clip), Some(asset));
        assert_eq!(
            owner_for(FxScopeTab::Clip, seq, track, &media_clip),
            Some(VfxOwner::Clip(media_clip.id))
        );
        assert_eq!(
            owner_for(FxScopeTab::Track, seq, track, &media_clip),
            Some(VfxOwner::Track(track))
        );
        assert_eq!(
            owner_for(FxScopeTab::Master, seq, track, &media_clip),
            Some(VfxOwner::Master(seq))
        );
        assert_eq!(
            owner_for(FxScopeTab::Asset, seq, track, &media_clip),
            Some(VfxOwner::Asset(asset))
        );

        // An adjustment clip has no bin asset: the Asset tab has no owner.
        let adj = Clip::new(ClipSource::Adjustment, Tick(0), Tick(1000));
        assert_eq!(clip_asset(&adj), None);
        assert_eq!(owner_for(FxScopeTab::Asset, seq, track, &adj), None);
        assert_eq!(
            owner_for(FxScopeTab::Clip, seq, track, &adj),
            Some(VfxOwner::Clip(adj.id))
        );
    }

    /// The panel's four buttons (add / remove / reorder / param) all build a
    /// real `ops::*_scoped` command against a track stack, and each is one
    /// undoable step whose inverse restores the exact prior stack. This is the
    /// same code path `draw_effects_section` runs; the widget layer above it is
    /// what egui owns.
    #[test]
    fn track_scope_panel_actions_build_undoable_commands() {
        use photonic_core::history::Command;
        use photonic_core::timeline::time::FrameRate;
        use photonic_core::timeline::{ClipEffect, ClipSource, Sequence, TimelineProject, Track};
        use photonic_core::Document;

        let mut project = TimelineProject::new();
        let mut sequence = Sequence::new("S", FrameRate::FPS_30, 1920, 1080);
        let mut vtrack = Track::new(TrackKind::Video, "V1");
        vtrack
            .clips
            .push(Clip::new(ClipSource::Adjustment, Tick(0), Tick(100)));
        let track_id = vtrack.id;
        sequence.video_tracks.push(vtrack);
        project.insert_sequence(sequence);
        let mut doc = Document::new("t", 100.0, 100.0);
        doc.timeline = Some(project);

        let owner = VfxOwner::Track(track_id);
        for kind in [EffectKind::Blur, EffectKind::Sharpen] {
            let p = doc.timeline.as_ref().unwrap();
            let cmd = ops::add_effect_scoped(p, owner, ClipEffect::new(kind), None).unwrap();
            Command::Timeline(cmd).apply(&mut doc);
        }
        assert_eq!(
            ops::effect_stack(doc.timeline.as_ref().unwrap(), owner)
                .unwrap()
                .len(),
            2
        );

        // "Move up" on row 1 — the panel's `swapped_order` permutation.
        let p = doc.timeline.as_ref().unwrap();
        let cmd = ops::reorder_effects_scoped(p, owner, swapped_order(2, 1, 0)).unwrap();
        let inverse = cmd.inverse(&doc).unwrap();
        Command::Timeline(cmd).apply(&mut doc);
        assert_eq!(
            ops::effect_stack(doc.timeline.as_ref().unwrap(), owner).unwrap()[0].kind,
            EffectKind::Sharpen
        );
        Command::Timeline(inverse).apply(&mut doc);
        assert_eq!(
            ops::effect_stack(doc.timeline.as_ref().unwrap(), owner).unwrap()[0].kind,
            EffectKind::Blur
        );

        // The enable checkbox and the param grid both go through SetEffect.
        let p = doc.timeline.as_ref().unwrap();
        let mut off = ops::effect_stack(p, owner).unwrap()[0].clone();
        off.enabled = false;
        let cmd = ops::set_effect_scoped(p, owner, 0, off).unwrap();
        let inverse = cmd.inverse(&doc).unwrap();
        Command::Timeline(cmd).apply(&mut doc);
        assert!(!ops::effect_stack(doc.timeline.as_ref().unwrap(), owner).unwrap()[0].enabled);
        Command::Timeline(inverse).apply(&mut doc);
        assert!(ops::effect_stack(doc.timeline.as_ref().unwrap(), owner).unwrap()[0].enabled);

        // The "×" button.
        let p = doc.timeline.as_ref().unwrap();
        let cmd = ops::remove_effect_scoped(p, owner, 0).unwrap();
        let inverse = cmd.inverse(&doc).unwrap();
        Command::Timeline(cmd).apply(&mut doc);
        assert_eq!(
            ops::effect_stack(doc.timeline.as_ref().unwrap(), owner)
                .unwrap()
                .len(),
            1
        );
        Command::Timeline(inverse).apply(&mut doc);
        let stack = ops::effect_stack(doc.timeline.as_ref().unwrap(), owner).unwrap();
        assert_eq!(stack.len(), 2);
        assert_eq!(stack[0].kind, EffectKind::Blur, "re-inserted at index 0");
    }

    #[test]
    fn param_short_label_strips_prefix() {
        assert_eq!(param_short_label("params.radius"), "radius");
        assert_eq!(param_short_label("transform.x"), "x");
        assert_eq!(param_short_label("noprefix"), "noprefix");
    }

    #[test]
    fn clip_has_effects_reflects_stack_emptiness() {
        use photonic_core::timeline::ClipSource;

        let mut clip = Clip::new(ClipSource::Adjustment, Tick(0), Tick(1000));
        assert!(!clip_has_effects(&clip));
        clip.effects
            .push(photonic_core::timeline::ClipEffect::new(EffectKind::Blur));
        assert!(clip_has_effects(&clip));
    }

    #[test]
    fn ratio_from_pct_round_trips_forward_and_reverse() {
        assert_eq!(ratio_from_pct(100.0, false), Ratio::new(1000, 1000));
        assert_eq!(ratio_from_pct(200.0, false), Ratio::new(2000, 1000));
        assert_eq!(ratio_from_pct(50.0, true), Ratio::new(-500, 1000));
        assert_eq!(ratio_from_pct(0.0, false), Ratio::new(0, 1000));
    }

    #[test]
    fn ramp_ratio_at_holds_first_before_and_last_after() {
        let keys = vec![
            SpeedKey::new(Tick(1000), Ratio::new(500, 1000)),
            SpeedKey::new(Tick(3000), Ratio::new(2000, 1000)),
        ];
        // Already sorted by `at` — every caller sorts before calling.
        assert_eq!(ramp_ratio_at(&keys, Tick(0)), Ratio::new(500, 1000));
        assert_eq!(ramp_ratio_at(&keys, Tick(1000)), Ratio::new(500, 1000));
        assert_eq!(ramp_ratio_at(&keys, Tick(2000)), Ratio::new(500, 1000));
        assert_eq!(ramp_ratio_at(&keys, Tick(3000)), Ratio::new(2000, 1000));
        assert_eq!(ramp_ratio_at(&keys, Tick(9999)), Ratio::new(2000, 1000));
    }

    #[test]
    fn ramp_ratio_at_empty_ramp_is_identity() {
        assert_eq!(ramp_ratio_at(&[], Tick(500)), Ratio::ONE);
    }

    #[test]
    fn zero_rate_ramp_seed_stays_frozen() {
        let keys = default_three_section_ramp(Tick(9_000), Ratio::new(0, 1));
        assert_eq!(keys.len(), 2);
        assert!(keys.iter().all(|key| key.ratio.num == 0));
        let map = SpeedMap::Keyframed { keys };
        assert_eq!(map.source_delta(Tick(9_000)), Tick::ZERO);
        assert!(map.validate_for_duration(Tick(9_000)).is_ok());
    }
}
