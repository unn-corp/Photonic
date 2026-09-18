//! Color page (04 §4.1 / 07 §5-6), split across two surfaces:
//! - the right-drawer `RightDrawerGroup::ColorControls` group — a grade-op stack
//!   (wheels / curves / HSL qualifier / LUT / primaries) for the selected clip's
//!   grade ([`draw_color_controls`]);
//! - the floating, dockable scopes panel — waveform / parade / vectorscope /
//!   histogram, parked beside the program monitor ([`draw_scopes_panel`]).
//!
//! **Every mutation goes through a pure core op → `CommandHistory`** (07 §1: a
//! grade edit is a `SetGrade{old,new}` whole-value swap, `timeline/ops.rs`), never
//! a direct `doc.timeline` mutation. Each frame we clone the selected clip's
//! `Grade`, let the widgets accumulate edits into that clone, and commit one
//! `SetGrade` if it changed — so undo/redo, autosave, MCP and the engine mirror
//! all observe the edit through the one sanctioned channel.
//!
//! Scopes read `photonic_render::scopes` GPU compute over the engine's **scope
//! tap** (K-E2): `EngineFrame::scope_tap`, which is the selected clip's texture
//! after its `Grade` and before the track fold, or the folded program before
//! `CaptionOverlay` (03 §3.6 readback point as amended by 27 A-7 to 07 §5's
//! per-clip-with-fallback wording). It is deliberately NOT the presented frame,
//! which is post-fold and post-caption — the signal 26 K-E2 flagged as measuring
//! the wrong thing. The tap point is chosen in-panel and sent to the engine by
//! the caller as `EngineCmd::SetScopeTap`; when the playhead is not over the
//! chosen clip the engine falls back to the program tap and the panel relabels
//! (13 §10.2 — never blank).
//!
//! Choosing a tap point mutates no document state, so it is session-only view
//! state (egui temp data) and NOT a `Command` — there is nothing to undo, the
//! same rule `ViewNodeOverride` follows.

use egui::{pos2, vec2, Color32, Pos2, Rect, RichText, Sense, Stroke, TextureOptions, Ui, Vec2};
use egui_phosphor::regular as ph;

use photonic_core::document::Document;
use photonic_core::history::{Command, CommandHistory};
use photonic_core::timeline::{
    ops, AssetId, AssetKind, AssetSource, CdlParams, ClipId, Grade, GradeOp, GradeOpId,
    GradeOpKind, GradeOpParams, LutInterp, SequenceId, TrackId,
};

use photonic_video::graph::ScopeTapPoint;

use super::param_expr;
use photonic_video::session::EngineFrame;

use crate::panels::{eyedropper_btn, EyedropperTarget, PanelAction};

use super::{ColorPageTab, ScopeKind};

// ─────────────────────────────────────────────────────────────────────────────
// Theme-token accessors (DESIGN.md — read from live visuals so the panel tracks
// the light/dark theme switch instead of hard-coding hex).
// ─────────────────────────────────────────────────────────────────────────────

/// `primary` accent (electric violet) — active states, offset vectors, readouts.
fn accent(ui: &Ui) -> Color32 {
    ui.visuals().selection.stroke.color
}
/// `secondary` muted label / neutral dot.
fn muted(ui: &Ui) -> Color32 {
    ui.visuals().weak_text_color()
}
/// `on-surface` text / control-point dots / scope trace.
fn on_surface(ui: &Ui) -> Color32 {
    ui.visuals().text_color()
}
/// `surface-widget` — disc / plot fills.
fn surface_widget(ui: &Ui) -> Color32 {
    ui.visuals().extreme_bg_color
}
/// `border` — disc / plot outlines.
fn border(ui: &Ui) -> Color32 {
    ui.visuals().widgets.noninteractive.bg_stroke.color
}

/// Section header (13 §6.5, dim-muted `#50506E`), matching `tools_panel`.
fn section_header(ui: &mut Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(
        RichText::new(text)
            .small()
            .color(crate::theme::section_header_color(ui)),
    );
    ui.add_space(2.0);
}

/// Nudge curve point `i` by `(dx, dy) * step`, applying the same endpoint /
/// neighbour clamping as pointer drag: endpoints keep their pinned x (0 or 1),
/// interior points stay strictly between their neighbours, and y is clamped to
/// `[0, 1]`. Extracted so the keyboard path is unit-testable without an egui
/// context (41 §9 step 1).
pub(crate) fn nudge_point(
    points: &[(f32, f32)],
    i: usize,
    dx: f32,
    dy: f32,
    step: f32,
) -> (f32, f32) {
    let mut p = points[i];
    if i != 0 && i != points.len() - 1 {
        let lo = points[i - 1].0 + 1e-3;
        let hi = points[i + 1].0 - 1e-3;
        p.0 = (p.0 + dx * step).clamp(lo, hi);
    }
    p.1 = (p.1 + dy * step).clamp(0.0, 1.0);
    p
}

// ─────────────────────────────────────────────────────────────────────────────
// Clip / grade plumbing — locate the selected clip, route edits through ops.
// ─────────────────────────────────────────────────────────────────────────────

/// The location of the first selected clip in the active sequence, or `None`.
fn locate_clip(doc: &Document, selection: &[ClipId]) -> Option<(SequenceId, TrackId, ClipId)> {
    let clip_id = *selection.first()?;
    let proj = doc.timeline.as_ref()?;
    let seq_id = proj.active_sequence?;
    let seq = proj.sequences.get(&seq_id)?;
    for t in seq.video_tracks.iter() {
        if t.clips.iter().any(|c| c.id == clip_id) {
            return Some((seq_id, t.id, clip_id));
        }
    }
    None
}

/// Clone the clip's current grade (default-empty if it has none yet).
fn current_grade(doc: &Document, seq: SequenceId, track: TrackId, clip: ClipId) -> Grade {
    doc.timeline
        .as_ref()
        .and_then(|p| p.sequences.get(&seq))
        .and_then(|s| s.track(track))
        .and_then(|t| t.clips.iter().find(|c| c.id == clip))
        .and_then(|c| c.grade.clone())
        .unwrap_or_default()
}

/// Commit a whole-grade replacement as one undoable `SetGrade` step (07 §1).
/// An empty, un-bypassed stack collapses to no grade so it round-trips cleanly.
fn commit_grade(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    clip: ClipId,
    new: Grade,
) {
    let new = if new.ops.is_empty() && !new.bypass {
        None
    } else {
        Some(new)
    };
    let cmd = match doc.timeline.as_ref() {
        Some(proj) => match ops::set_grade(proj, seq, track, clip, new) {
            Ok(cmd) => cmd,
            Err(_) => return,
        },
        None => return,
    };
    history.execute_discrete(Command::Timeline(cmd), doc);
}

/// LUT assets already in the media pool, as `(id, display name)` (07 §1: LUTs are
/// referenced `AssetKind::Lut3d` files, never embedded).
fn lut_assets(doc: &Document) -> Vec<(AssetId, String)> {
    let Some(proj) = doc.timeline.as_ref() else {
        return Vec::new();
    };
    let mut out: Vec<(AssetId, String)> = proj
        .media
        .assets
        .values()
        .filter(|a| a.kind == AssetKind::Lut3d)
        .map(|a| (a.id, asset_name(&a.source)))
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

fn asset_name(source: &AssetSource) -> String {
    match source {
        AssetSource::File { path, .. } => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "LUT".to_string()),
        AssetSource::EmbeddedVector { .. } => "embedded".to_string(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Grade-op defaults + labels
// ─────────────────────────────────────────────────────────────────────────────

fn kind_label(kind: GradeOpKind) -> &'static str {
    match kind {
        GradeOpKind::Exposure => "Exposure",
        GradeOpKind::Contrast => "Contrast",
        GradeOpKind::WhiteBalance => "White Balance",
        GradeOpKind::Cdl => "CDL",
        GradeOpKind::Wheels => "Wheels",
        GradeOpKind::Curves => "Curves",
        GradeOpKind::HslQualifier => "HSL Qualifier",
        GradeOpKind::Lut3d => "3D LUT",
        // Forward-compat (39 §2.2): show the preserved tag as the display name;
        // the op is non-editable but retained verbatim.
        GradeOpKind::Unknown(t) => t.as_str(),
        // `#[non_exhaustive]`: a kind a newer build adds shows a placeholder.
        _ => "Unsupported",
    }
}

/// A fresh op of `kind` seeded with its neutral (identity) parameters (07 §3).
fn default_op(kind: GradeOpKind, luts: &[(AssetId, String)]) -> GradeOp {
    let params = match kind {
        GradeOpKind::Exposure => GradeOpParams::Exposure { stops: 0.0 },
        GradeOpKind::Contrast => GradeOpParams::Contrast {
            pivot: 0.5,
            amount: 0.0,
        },
        GradeOpKind::WhiteBalance => GradeOpParams::WhiteBalance {
            temp: 0.0,
            tint: 0.0,
        },
        GradeOpKind::Cdl => GradeOpParams::Cdl {
            slope: [1.0; 3],
            offset: [0.0; 3],
            power: [1.0; 3],
            sat: 1.0,
        },
        GradeOpKind::Wheels => GradeOpParams::Wheels {
            lift: [0.0; 3],
            gamma: [1.0; 3],
            gain: [1.0; 3],
            sat: 1.0,
        },
        GradeOpKind::Curves => GradeOpParams::Curves {
            master: vec![(0.0, 0.0), (1.0, 1.0)],
            red: Vec::new(),
            green: Vec::new(),
            blue: Vec::new(),
            hue_vs_hue: Vec::new(),
            hue_vs_sat: Vec::new(),
        },
        GradeOpKind::HslQualifier => GradeOpParams::HslQualifier {
            hue: [0.0, 1.0],
            sat: [0.0, 1.0],
            lum: [0.0, 1.0],
            softness: 0.1,
            correction: CdlParams::identity(),
        },
        GradeOpKind::Lut3d => GradeOpParams::Lut3d {
            asset: luts.first().map(|(id, _)| *id).unwrap_or_default(),
            intensity: 1.0,
            interp: LutInterp::Trilinear,
        },
        // The add-corrector menu (`ALL_KINDS`) only offers the eight known
        // kinds, and an unknown op is a load-only state that is never created
        // from the UI (39 §2.2 rule 4: never guess). This arm is unreachable.
        _ => unreachable!("default_op is only called for user-selectable known kinds"),
    };
    GradeOp::new(kind, params)
}

/// Full add-corrector catalog, in the 07 §4.4 seed order.
const ALL_KINDS: [GradeOpKind; 8] = [
    GradeOpKind::WhiteBalance,
    GradeOpKind::Exposure,
    GradeOpKind::Contrast,
    GradeOpKind::Cdl,
    GradeOpKind::Wheels,
    GradeOpKind::HslQualifier,
    GradeOpKind::Curves,
    GradeOpKind::Lut3d,
];

/// The `GradeOpKind` a primary-corrector [`ColorPageTab`] quick-adds/selects.
fn tab_kind(tab: ColorPageTab) -> GradeOpKind {
    match tab {
        ColorPageTab::Wheels => GradeOpKind::Wheels,
        ColorPageTab::Curves => GradeOpKind::Curves,
        ColorPageTab::Qualifier => GradeOpKind::HslQualifier,
        ColorPageTab::Lut => GradeOpKind::Lut3d,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Right-drawer Color Controls
// ─────────────────────────────────────────────────────────────────────────────

/// Right-rail Color Controls drawer: grade-op stack + per-op editors for the
/// selected clip's grade (07 §5), the global bypass (before/after), the
/// primary-corrector quick tabs, and the scopes-panel toggle. Called from
/// `app/mod.rs`'s right-drawer match with the live `doc`/`history` (edits commit
/// through [`commit_grade`]) and the app's pending `PanelAction` queue.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_color_controls(
    ui: &mut Ui,
    doc: &mut Document,
    history: &mut CommandHistory,
    actions: &mut Vec<PanelAction>,
    selection: &[ClipId],
    selected_op: &mut Option<GradeOpId>,
    tab: &mut ColorPageTab,
    scopes_open: &mut bool,
) {
    let Some((seq, track, clip)) = locate_clip(doc, selection) else {
        ui.add_space(8.0);
        ui.label(RichText::new("Select a clip in the timeline to grade it.").color(muted(ui)));
        return;
    };

    // Read-only snapshot up front so the later `&mut doc` commit doesn't collide
    // with the media-pool borrow.
    let luts = lut_assets(doc);
    let orig = current_grade(doc, seq, track, clip);
    let mut g = orig.clone();

    // ── Pinned header: global bypass (= before/after, 07 §5) + scopes toggle ──
    ui.horizontal(|ui| {
        // Bare-key bypass toggle. Suppressed only while a text field is capturing
        // keys (`wants_keyboard_input`), never on global focus-emptiness (41 §3
        // R-5); `consume_key` so a focused TextEdit that also wants 'D' wins.
        let d_pressed = !ui.ctx().wants_keyboard_input()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::D));
        let resp = ui.selectable_label(g.bypass, format!("{} Bypass", ph::EYE_SLASH));
        if resp.clicked() || d_pressed {
            g.bypass = !g.bypass;
        }
        resp.on_hover_text("Show the ungraded image (before/after). Shortcut: D");

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .selectable_label(*scopes_open, format!("{} Scopes", ph::WAVE_SINE))
                .on_hover_text("Toggle the floating waveform / vectorscope / histogram panel")
                .clicked()
            {
                *scopes_open = !*scopes_open;
            }
        });
    });

    ui.separator();

    // ── Quick grade strip (proposal 213 AS-1) — Exposure / Contrast / Sat ────
    // Ensures the three ops exist and edits them in place; one SetGrade at
    // end-of-frame still covers the whole strip. Full correctors remain below.
    section_header(ui, "QUICK GRADE");
    {
        for kind in [
            GradeOpKind::Exposure,
            GradeOpKind::Contrast,
            GradeOpKind::Cdl,
        ] {
            if g.ops.iter().all(|o| o.kind != kind) {
                g.ops.push(default_op(kind, &luts));
            }
        }
        // Edit first matching op of each kind (identity-seeded if we just added it).
        if let Some(op) = g.ops.iter_mut().find(|o| o.kind == GradeOpKind::Exposure) {
            if let GradeOpParams::Exposure { stops } = &mut op.params.base {
                labelled(ui, "Exposure", |ui| {
                    ui.add(
                        egui::Slider::new(stops, -3.0..=3.0)
                            .step_by(0.01)
                            .suffix(" st"),
                    );
                });
            }
        }
        if let Some(op) = g.ops.iter_mut().find(|o| o.kind == GradeOpKind::Contrast) {
            if let GradeOpParams::Contrast { amount, .. } = &mut op.params.base {
                labelled(ui, "Contrast", |ui| {
                    ui.add(egui::Slider::new(amount, -1.0..=1.0).step_by(0.01));
                });
            }
        }
        if let Some(op) = g.ops.iter_mut().find(|o| o.kind == GradeOpKind::Cdl) {
            if let GradeOpParams::Cdl { sat, .. } = &mut op.params.base {
                labelled(ui, "Saturation", |ui| {
                    ui.add(egui::Slider::new(sat, 0.0..=2.0).step_by(0.01));
                });
            }
        }
        ui.label(
            RichText::new("Full wheels / curves / LUT below")
                .small()
                .color(muted(ui)),
        );
    }

    ui.add_space(4.0);
    ui.separator();

    // ── Primary-corrector quick tabs (07 §4.4) — select existing or create ────
    section_header(ui, "PRIMARIES");
    ui.horizontal_wrapped(|ui| {
        for t in [
            ColorPageTab::Wheels,
            ColorPageTab::Curves,
            ColorPageTab::Qualifier,
            ColorPageTab::Lut,
        ] {
            let kind = tab_kind(t);
            let existing = g.ops.iter().find(|o| o.kind == kind).map(|o| o.id);
            let active = *tab == t && *selected_op == existing && existing.is_some();
            if ui
                .selectable_label(active, kind_label(kind))
                .on_hover_text("Select this corrector, or add one if the stack has none")
                .clicked()
            {
                *tab = t;
                match existing {
                    Some(id) => *selected_op = Some(id),
                    None => {
                        let op = default_op(kind, &luts);
                        *selected_op = Some(op.id);
                        g.ops.push(op);
                    }
                }
            }
        }
    });

    ui.add_space(4.0);

    // ── Add-corrector menu (full catalog) ────────────────────────────────────
    ui.menu_button(format!("{} Add corrector", ph::PLUS), |ui| {
        for kind in ALL_KINDS {
            if ui.button(kind_label(kind)).clicked() {
                let op = default_op(kind, &luts);
                *selected_op = Some(op.id);
                g.ops.push(op);
                ui.close_menu();
            }
        }
    });

    ui.add_space(4.0);
    section_header(ui, "GRADE STACK");
    if g.ops.is_empty() {
        ui.label(RichText::new("No correctors yet — add one above.").color(muted(ui)));
    }

    // ── Op stack: enable / select / reorder / remove ─────────────────────────
    let mut move_up: Option<usize> = None;
    let mut move_down: Option<usize> = None;
    let mut remove: Option<usize> = None;
    let n = g.ops.len();
    for (i, op) in g.ops.iter_mut().enumerate() {
        let is_selected = *selected_op == Some(op.id);
        ui.horizontal(|ui| {
            ui.checkbox(&mut op.enabled, "")
                .on_hover_text("Enable / bypass this corrector");
            let unknown = matches!(op.params.base, GradeOpParams::Unknown(_));
            let label = if unknown {
                RichText::new("Unsupported op").color(ui.visuals().warn_fg_color)
            } else if op.enabled {
                RichText::new(kind_label(op.kind))
            } else {
                RichText::new(kind_label(op.kind)).color(muted(ui))
            };
            if ui.selectable_label(is_selected, label).clicked() {
                *selected_op = Some(op.id);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(ph::X).on_hover_text("Remove").clicked() {
                    remove = Some(i);
                }
                if ui
                    .add_enabled(i + 1 < n, egui::Button::new(ph::CARET_DOWN).small())
                    .on_hover_text("Move down")
                    .clicked()
                {
                    move_down = Some(i);
                }
                if ui
                    .add_enabled(i > 0, egui::Button::new(ph::CARET_UP).small())
                    .on_hover_text("Move up")
                    .clicked()
                {
                    move_up = Some(i);
                }
            });
        });
    }
    if let Some(i) = move_up {
        g.ops.swap(i, i - 1);
    }
    if let Some(i) = move_down {
        g.ops.swap(i, i + 1);
    }
    if let Some(i) = remove {
        let removed = g.ops.remove(i);
        if *selected_op == Some(removed.id) {
            *selected_op = None;
        }
    }

    // ── Selected-op editor ───────────────────────────────────────────────────
    ui.separator();
    match selected_op.and_then(|sel| g.ops.iter().position(|o| o.id == sel)) {
        Some(idx) => draw_op_editor(ui, &mut g.ops[idx], &luts, actions, seq, track, clip),
        None => {
            ui.label(RichText::new("Select a corrector to edit it.").color(muted(ui)));
        }
    }

    // ── Commit one SetGrade if anything changed this frame ───────────────────
    if g != orig {
        commit_grade(doc, history, seq, track, clip, g);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-op editors — each mutates `op.params.base`; the caller commits the grade.
// ─────────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn draw_op_editor(
    ui: &mut Ui,
    op: &mut GradeOp,
    luts: &[(AssetId, String)],
    actions: &mut Vec<PanelAction>,
    seq: SequenceId,
    track: TrackId,
    clip: ClipId,
) {
    section_header(ui, kind_label(op.kind));
    let op_id = op.id;
    match &mut op.params.base {
        GradeOpParams::Exposure { stops } => {
            labelled(ui, "Stops", |ui| {
                ui.add(egui::Slider::new(stops, -6.0..=6.0).step_by(0.01));
            });
        }
        GradeOpParams::Contrast { pivot, amount } => {
            labelled(ui, "Amount", |ui| {
                ui.add(egui::Slider::new(amount, -1.0..=1.0).step_by(0.001));
            });
            labelled(ui, "Pivot", |ui| {
                ui.add(egui::Slider::new(pivot, 0.0..=1.0).step_by(0.001));
            });
        }
        GradeOpParams::WhiteBalance { temp, tint } => {
            labelled(ui, "Temp", |ui| {
                ui.add(egui::Slider::new(temp, -1.0..=1.0).step_by(0.001));
            });
            labelled(ui, "Tint", |ui| {
                ui.add(egui::Slider::new(tint, -1.0..=1.0).step_by(0.001));
            });
        }
        GradeOpParams::Cdl {
            slope,
            offset,
            power,
            sat,
        } => cdl_editor(ui, slope, offset, power, sat),
        GradeOpParams::Wheels {
            lift,
            gamma,
            gain,
            sat,
        } => {
            ui.horizontal(|ui| {
                wheel_dial(ui, "Lift", lift, 0.0);
                wheel_dial(ui, "Gamma", gamma, 1.0);
                wheel_dial(ui, "Gain", gain, 1.0);
            });
            labelled(ui, "Saturation", |ui| {
                ui.add(egui::Slider::new(sat, 0.0..=2.0).step_by(0.001));
            });
        }
        GradeOpParams::Curves {
            master,
            red,
            green,
            blue,
            hue_vs_hue,
            hue_vs_sat,
        } => curves_editor(ui, master, red, green, blue, hue_vs_hue, hue_vs_sat),
        GradeOpParams::HslQualifier {
            hue,
            sat,
            lum,
            softness,
            correction,
        } => qualifier_editor(
            ui, hue, sat, lum, softness, correction, actions, seq, track, clip, op_id,
        ),
        GradeOpParams::Lut3d {
            asset,
            intensity,
            interp,
        } => lut_editor(ui, asset, intensity, interp, luts),
        GradeOpParams::Unknown(_) => {
            ui.label(
                RichText::new(
                    "This corrector was made by a newer Photonic build and can't be edited here. \
                     It is preserved untouched.",
                )
                .color(ui.visuals().warn_fg_color),
            );
        }
    }

    // Per-op mask affordance (07 §4). Power windows are the v1 shape; the
    // on-canvas handle editor (13 §9.1.1) is a later monitor-overlay story, so
    // we surface mask presence + a clear-mask control here.
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let has_mask = op.mask.is_some();
        ui.label(
            RichText::new(if has_mask { "Masked" } else { "Full frame" })
                .small()
                .color(muted(ui)),
        );
        if has_mask && ui.small_button("Clear mask").clicked() {
            op.mask = None;
        }
    });
}

fn labelled(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized([72.0, 16.0], egui::Label::new(RichText::new(label).small()));
        add(ui);
    });
}

fn cdl_editor(
    ui: &mut Ui,
    slope: &mut [f32; 3],
    offset: &mut [f32; 3],
    power: &mut [f32; 3],
    sat: &mut f32,
) {
    let ch = ["R", "G", "B"];
    // K-B6: arithmetic / middle-click reset. CDL channel defaults: slope 1,
    // offset 0, power 1 (identity grade).
    let empty = std::collections::HashMap::new();
    labelled(ui, "Slope", |ui| {
        for c in 0..3 {
            ui.scope(|ui| {
                param_expr::float_drag_f32(ui, &mut slope[c], 1.0, Some((0.0, 4.0)), &empty, 0.005);
            })
            .response
            .on_hover_text(ch[c]);
        }
    });
    labelled(ui, "Offset", |ui| {
        for c in 0..3 {
            ui.scope(|ui| {
                param_expr::float_drag_f32(
                    ui,
                    &mut offset[c],
                    0.0,
                    Some((-1.0, 1.0)),
                    &empty,
                    0.002,
                );
            })
            .response
            .on_hover_text(ch[c]);
        }
    });
    labelled(ui, "Power", |ui| {
        for c in 0..3 {
            ui.scope(|ui| {
                param_expr::float_drag_f32(ui, &mut power[c], 1.0, Some((0.1, 4.0)), &empty, 0.005);
            })
            .response
            .on_hover_text(ch[c]);
        }
    });
    labelled(ui, "Saturation", |ui| {
        ui.add(egui::Slider::new(sat, 0.0..=2.0).step_by(0.001));
    });
}

fn lut_editor(
    ui: &mut Ui,
    asset: &mut AssetId,
    intensity: &mut f32,
    interp: &mut LutInterp,
    luts: &[(AssetId, String)],
) {
    if luts.is_empty() {
        ui.label(
            RichText::new("No LUTs in the media pool. Import a .cube file to grade with it here.")
                .color(muted(ui)),
        );
    } else {
        section_header(ui, "LUT BROWSER");
        egui::ScrollArea::vertical()
            .max_height(120.0)
            .id_salt("lut_browser")
            .show(ui, |ui| {
                for (id, name) in luts {
                    if ui.selectable_label(*asset == *id, name).clicked() {
                        *asset = *id;
                    }
                }
            });
    }
    labelled(ui, "Intensity", |ui| {
        ui.add(egui::Slider::new(intensity, 0.0..=1.0).step_by(0.01));
    });
    labelled(ui, "Interp", |ui| {
        if ui
            .selectable_label(*interp == LutInterp::Trilinear, "Trilinear")
            .clicked()
        {
            *interp = LutInterp::Trilinear;
        }
        if ui
            .selectable_label(*interp == LutInterp::Tetrahedral, "Tetrahedral")
            .on_hover_text("Higher quality at LUT-grid edges (07 §6.5)")
            .clicked()
        {
            *interp = LutInterp::Tetrahedral;
        }
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// Wheels dial (13 §9.1.1) — 2D chroma disc + precise numeric readouts (kbd path)
// ─────────────────────────────────────────────────────────────────────────────

/// Draw one lift/gamma/gain dial: a chroma disc plus three numeric readouts (the
/// keyboard fallback, 13 §9.6). `neutral` is the per-channel identity (0.0 for
/// lift, 1.0 for gamma/gain). Mutates `v` in place.
fn wheel_dial(ui: &mut Ui, label: &str, v: &mut [f32; 3], neutral: f32) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).small().color(muted(ui)));
        let size = 60.0;
        let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let center = rect.center();
        let radius = size * 0.5 - 3.0;

        painter.circle_filled(center, radius, surface_widget(ui));
        painter.circle_stroke(center, radius, Stroke::new(1.0, border(ui)));
        painter.line_segment(
            [
                pos2(center.x - radius, center.y),
                pos2(center.x + radius, center.y),
            ],
            Stroke::new(0.5, border(ui)),
        );
        painter.line_segment(
            [
                pos2(center.x, center.y - radius),
                pos2(center.x, center.y + radius),
            ],
            Stroke::new(0.5, border(ui)),
        );

        let delta = [v[0] - neutral, v[1] - neutral, v[2] - neutral];
        let chroma = deltas_to_chroma_xy(delta);
        let tip = center + chroma * (radius / CHROMA_FULL_SCALE);

        // Drag maps the pointer (relative to centre) back to a pure-chroma RGB
        // delta (no luma shift), preserving each channel's shared luma offset.
        if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                let rel = (p - center) / (radius / CHROMA_FULL_SCALE);
                let rel = clamp_len(rel, CHROMA_FULL_SCALE);
                let new_delta = chroma_to_deltas(rel);
                let luma = (delta[0] + delta[1] + delta[2]) / 3.0;
                for c in 0..3 {
                    v[c] = neutral + new_delta[c] + luma;
                }
            }
        }
        if resp.double_clicked() {
            *v = [neutral; 3];
        }

        painter.circle_filled(center, 1.5, muted(ui));
        if chroma.length() > 0.001 {
            painter.line_segment([center, tip], Stroke::new(1.5, accent(ui)));
            painter.circle_filled(tip, 2.5, accent(ui));
        }

        let ch = ["R", "G", "B"];
        for c in 0..3 {
            ui.horizontal(|ui| {
                ui.label(RichText::new(ch[c]).small().color(muted(ui)));
                let range = if neutral == 0.0 {
                    -0.5..=0.5
                } else {
                    0.0..=2.0
                };
                ui.add(egui::DragValue::new(&mut v[c]).speed(0.002).range(range));
            });
        }
    });
}

/// Full-scale chroma radius in RGB-delta space that maps to the disc edge.
const CHROMA_FULL_SCALE: f32 = 0.5;

/// 120°-spaced primary directions on the colour wheel: R up, G lower-left,
/// B lower-right (screen space, y-down). Unit vectors.
fn primary_dirs() -> [Vec2; 3] {
    let deg = [90.0_f32, 210.0, 330.0];
    let mut out = [Vec2::ZERO; 3];
    for c in 0..3 {
        let r = deg[c].to_radians();
        out[c] = vec2(r.cos(), -r.sin());
    }
    out
}

fn dot(a: Vec2, b: Vec2) -> f32 {
    a.x * b.x + a.y * b.y
}

/// Project a pure-chroma RGB delta onto the 2D colour wheel (luma removed).
pub(crate) fn deltas_to_chroma_xy(delta: [f32; 3]) -> Vec2 {
    let luma = (delta[0] + delta[1] + delta[2]) / 3.0;
    let dirs = primary_dirs();
    let mut xy = Vec2::ZERO;
    for c in 0..3 {
        xy += dirs[c] * (delta[c] - luma);
    }
    xy
}

/// Invert a 2D wheel position into a pure-chroma RGB delta (channel sum == 0).
/// For 120°-spaced unit directions, `d_c = (2/3)(xy · u_c)` reproduces `xy`
/// exactly while keeping the channel sum zero (no luma shift).
pub(crate) fn chroma_to_deltas(xy: Vec2) -> [f32; 3] {
    let dirs = primary_dirs();
    let mut out = [0.0; 3];
    for c in 0..3 {
        out[c] = (2.0 / 3.0) * dot(xy, dirs[c]);
    }
    out
}

fn clamp_len(v: Vec2, max: f32) -> Vec2 {
    let len = v.length();
    if len > max && len > 0.0 {
        v * (max / len)
    } else {
        v
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Curve editor (07 §3.6 / 13 §9) — draggable control points, per-channel tabs.
// ─────────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn curves_editor(
    ui: &mut Ui,
    master: &mut Vec<(f32, f32)>,
    red: &mut Vec<(f32, f32)>,
    green: &mut Vec<(f32, f32)>,
    blue: &mut Vec<(f32, f32)>,
    hue_vs_hue: &mut Vec<(f32, f32)>,
    hue_vs_sat: &mut Vec<(f32, f32)>,
) {
    let tab_id = ui.id().with("curve_ch");
    let mut ch: usize = ui.data(|d| d.get_temp::<usize>(tab_id).unwrap_or(0));
    ui.horizontal_wrapped(|ui| {
        for (i, name) in ["RGB", "R", "G", "B", "H-H", "H-S"].iter().enumerate() {
            if ui.selectable_label(ch == i, *name).clicked() {
                ch = i;
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(tab_id, ch));

    let points: &mut Vec<(f32, f32)> = match ch {
        0 => master,
        1 => red,
        2 => green,
        3 => blue,
        4 => hue_vs_hue,
        _ => hue_vs_sat,
    };
    if points.is_empty() {
        *points = vec![(0.0, 0.0), (1.0, 1.0)];
    }
    curve_plot(ui, points, ch);
}

/// Draw + edit a control-point curve. Drag to move, double-click empty area to
/// add, right-click / Delete to remove. Arrow keys nudge the selected point
/// (13 §9.6 keyboard mitigation).
fn curve_plot(ui: &mut Ui, points: &mut Vec<(f32, f32)>, channel: usize) {
    let w = ui.available_width().min(240.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, w * 0.8), Sense::click_and_drag());
    let painter = ui.painter_at(rect);

    painter.rect_filled(rect, 3.0, surface_widget(ui));
    for k in 1..4 {
        let t = k as f32 / 4.0;
        let x = rect.left() + t * rect.width();
        let y = rect.top() + t * rect.height();
        painter.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(0.5, border(ui)),
        );
        painter.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(0.5, border(ui)),
        );
    }

    let to_screen = |p: (f32, f32)| curve_to_screen(p, rect);
    let from_screen = |s: Pos2| screen_to_curve(s, rect);

    let sel_id = ui.id().with(("curve_sel", channel));
    let mut sel: Option<usize> = ui.data(|d| d.get_temp::<Option<usize>>(sel_id)).flatten();

    let hit = 10.0;
    if let Some(p) = resp.interact_pointer_pos() {
        if resp.drag_started() || resp.clicked() {
            sel = nearest_point(points, &to_screen, p, hit);
            // Selecting a point focuses the plot so arrow-nudge below is reachable.
            resp.request_focus();
        }
        if resp.dragged() {
            if let Some(i) = sel {
                let mut np = from_screen(p);
                if i == 0 {
                    np.0 = 0.0;
                } else if i == points.len() - 1 {
                    np.0 = 1.0;
                } else {
                    let lo = points[i - 1].0 + 1e-3;
                    let hi = points[i + 1].0 - 1e-3;
                    np.0 = np.0.clamp(lo, hi);
                }
                np.1 = np.1.clamp(0.0, 1.0);
                points[i] = np;
            }
        }
        if resp.double_clicked() && nearest_point(points, &to_screen, p, hit).is_none() {
            sel = Some(insert_sorted(points, from_screen(p)));
        }
        if resp.secondary_clicked() {
            if let Some(i) = nearest_point(points, &to_screen, p, hit) {
                if i != 0 && i != points.len() - 1 {
                    points.remove(i);
                    sel = None;
                }
            }
        }
    }

    // Keyboard nudge / delete for the selected point.
    //
    // Gated on this plot holding focus. It was previously gated on
    // `!keyboard_captured(ui)` — i.e. on *nothing anywhere* having focus — which
    // meant the nudge stopped working the moment the plot itself was focused.
    // Focus-scoped handling is what makes typing safe (41 §3 R-5), so the
    // suppression the old gate reached for is a property of this check.
    if let Some(i) = sel {
        if resp.has_focus() {
            // Hold the arrow keys on the focused plot across frames. Without an
            // EventFilter, egui's focus navigation turns the first Arrow into a
            // focus move and steals focus off the plot, so only one nudge would
            // land (41 §3 R-4/R-5). Mirror egui's own Slider; leave `tab`/`escape`
            // false so Tab still exits the plot and Esc can free it.
            ui.ctx().memory_mut(|m| {
                m.set_focus_lock_filter(
                    resp.id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        ..Default::default()
                    },
                )
            });
            let (dx, dy, big) = ui.input(|inp| {
                (
                    (inp.key_pressed(egui::Key::ArrowRight) as i32
                        - inp.key_pressed(egui::Key::ArrowLeft) as i32) as f32,
                    (inp.key_pressed(egui::Key::ArrowUp) as i32
                        - inp.key_pressed(egui::Key::ArrowDown) as i32) as f32,
                    inp.modifiers.shift,
                )
            });
            if dx != 0.0 || dy != 0.0 {
                let step = if big { 0.05 } else { 0.005 };
                points[i] = nudge_point(points, i, dx, dy, step);
            }
            let del = ui.input(|inp| {
                inp.key_pressed(egui::Key::Delete) || inp.key_pressed(egui::Key::Backspace)
            });
            if del && i != 0 && i != points.len() - 1 {
                points.remove(i);
                sel = None;
            }
        }
    }

    let mut sorted = points.clone();
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let line: Vec<Pos2> = sorted.iter().map(|&p| to_screen(p)).collect();
    if line.len() >= 2 {
        painter.add(egui::Shape::line(line, Stroke::new(1.5, accent(ui))));
    }
    for (i, &p) in points.iter().enumerate() {
        let s = to_screen(p);
        painter.circle_filled(s, 3.0, on_surface(ui));
        if sel == Some(i) {
            painter.circle_stroke(s, 5.0, Stroke::new(1.5, accent(ui)));
        }
    }
    painter.rect_stroke(rect, 3.0, Stroke::new(1.0, border(ui)));

    ui.data_mut(|d| d.insert_temp(sel_id, sel));
    ui.label(
        RichText::new("Drag to move · double-click to add · right-click/Del to remove")
            .small()
            .color(muted(ui)),
    );
}

/// Curve (0..1, 0..1) → screen. y is flipped (1.0 = top).
pub(crate) fn curve_to_screen(p: (f32, f32), rect: Rect) -> Pos2 {
    pos2(
        rect.left() + p.0.clamp(0.0, 1.0) * rect.width(),
        rect.bottom() - p.1.clamp(0.0, 1.0) * rect.height(),
    )
}

/// Screen → curve (0..1, 0..1), clamped.
pub(crate) fn screen_to_curve(s: Pos2, rect: Rect) -> (f32, f32) {
    let x = ((s.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
    let y = ((rect.bottom() - s.y) / rect.height().max(1.0)).clamp(0.0, 1.0);
    (x, y)
}

/// Index of the point whose screen position is within `threshold` px of `pointer`.
pub(crate) fn nearest_point(
    points: &[(f32, f32)],
    to_screen: &impl Fn((f32, f32)) -> Pos2,
    pointer: Pos2,
    threshold: f32,
) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, &p) in points.iter().enumerate() {
        let d = to_screen(p).distance(pointer);
        if d <= threshold && best.map(|(_, bd)| d < bd).unwrap_or(true) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

/// Insert a point keeping the vector x-sorted; returns its new index.
pub(crate) fn insert_sorted(points: &mut Vec<(f32, f32)>, p: (f32, f32)) -> usize {
    let idx = points
        .iter()
        .position(|q| q.0 > p.0)
        .unwrap_or(points.len());
    points.insert(idx, p);
    idx
}

// ─────────────────────────────────────────────────────────────────────────────
// HSL qualifier (07 §3.7 / 13 §9) — eyedropper + swatch seed + range gates.
// ─────────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn qualifier_editor(
    ui: &mut Ui,
    hue: &mut [f32; 2],
    sat: &mut [f32; 2],
    lum: &mut [f32; 2],
    softness: &mut f32,
    correction: &mut CdlParams,
    actions: &mut Vec<PanelAction>,
    seq: SequenceId,
    track: TrackId,
    clip: ClipId,
    op: GradeOpId,
) {
    ui.horizontal(|ui| {
        // Eyedropper — extends the app-wide EyedropperTarget (07 §5 / 13 §9.3):
        // the next canvas click seeds this qualifier centre from the sampled colour.
        if eyedropper_btn(ui) {
            actions.push(PanelAction::StartEyedropper(
                EyedropperTarget::GradeQualifier {
                    seq,
                    track,
                    clip,
                    op,
                },
            ));
        }
        ui.label(
            RichText::new("Pick key colour off the monitor")
                .small()
                .color(muted(ui)),
        );
    });

    // Reliable in-panel seed: a target-colour swatch (keyboard/click accessible)
    // seeding hue/sat/lum centre ± a default half-width when changed.
    let seed_id = ui.id().with("qual_seed");
    let mut seed: [f32; 4] = ui.data(|d| {
        d.get_temp::<[f32; 4]>(seed_id)
            .unwrap_or([0.6, 0.4, 0.3, 1.0])
    });
    ui.horizontal(|ui| {
        ui.label(RichText::new("Seed colour").small().color(muted(ui)));
        let resp = crate::color_popup::ColorPopup::swatch_f32(ui, &mut seed);
        if resp.changed() {
            let (h, s, l) = rgb_to_hsl(seed[0], seed[1], seed[2]);
            let (nh, ns, nl) = seed_qualifier(h, s, l);
            *hue = nh;
            *sat = ns;
            *lum = nl;
        }
    });
    ui.data_mut(|d| d.insert_temp(seed_id, seed));

    range_gate(ui, "Hue", hue);
    range_gate(ui, "Sat", sat);
    range_gate(ui, "Lum", lum);
    labelled(ui, "Softness", |ui| {
        ui.add(egui::Slider::new(softness, 0.0..=1.0).step_by(0.01));
    });

    // Highlight-matte preview toggle (07 §5 / 13 §9); the monitor-side matte
    // overlay is a later story, so the workflow control is surfaced + persisted.
    let hl_id = ui.id().with("qual_highlight");
    let mut highlight: bool = ui.data(|d| d.get_temp::<bool>(hl_id).unwrap_or(false));
    if ui
        .selectable_label(highlight, "Highlight matte")
        .on_hover_text("Preview the isolated qualifier matte (white = qualified)")
        .clicked()
    {
        highlight = !highlight;
    }
    ui.data_mut(|d| d.insert_temp(hl_id, highlight));

    ui.add_space(4.0);
    section_header(ui, "SECONDARY CORRECTION (CDL)");
    cdl_editor(
        ui,
        &mut correction.slope,
        &mut correction.offset,
        &mut correction.power,
        &mut correction.sat,
    );
}

/// A min/max range as two clamped drag values keeping `lo <= hi`.
fn range_gate(ui: &mut Ui, label: &str, range: &mut [f32; 2]) {
    labelled(ui, label, |ui| {
        ui.add(
            egui::DragValue::new(&mut range[0])
                .speed(0.005)
                .range(0.0..=1.0),
        );
        ui.label("–");
        ui.add(
            egui::DragValue::new(&mut range[1])
                .speed(0.005)
                .range(0.0..=1.0),
        );
    });
    if range[0] > range[1] {
        range.swap(0, 1);
    }
}

/// Seed a qualifier's hue/sat/lum gates around a sampled HSL colour with a
/// sensible default half-width, clamped to `[0,1]` (07 §3.7 gate domain).
pub(crate) fn seed_qualifier(h: f32, s: f32, l: f32) -> ([f32; 2], [f32; 2], [f32; 2]) {
    let hw = |v: f32, w: f32| [(v - w).max(0.0), (v + w).min(1.0)];
    (hw(h, 0.06), hw(s, 0.20), hw(l, 0.20))
}

/// RGB (0..1) → HSL (all 0..1). Hue normalized to 0..1.
pub(crate) fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    let d = max - min;
    if d.abs() < 1e-6 {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } / 6.0;
    (h.rem_euclid(1.0), s.clamp(0.0, 1.0), l.clamp(0.0, 1.0))
}

// ─────────────────────────────────────────────────────────────────────────────
// Floating scopes panel (07 §6 / 13 §10)
// ─────────────────────────────────────────────────────────────────────────────

/// Which readback point the user asked the scopes to measure (K-E2). Session-only
/// view state — it mutates no document, so it is deliberately egui temp data and
/// not a `Command` (see the module header on the one-undo-unit rule).
#[derive(Copy, Clone, Default, PartialEq, Eq)]
pub(crate) enum TapMode {
    /// Follow the timeline selection: scope the selected clip, program otherwise.
    #[default]
    Clip,
    /// Pin the program tap regardless of selection.
    Program,
}

/// Resolve the panel's tap request from the mode and the current selection
/// (13 §10.2: with nothing selected there is no clip to scope, so the request is
/// the program). Pure, so it is unit-testable without an egui context.
pub(crate) fn requested_tap(mode: TapMode, selection: &[ClipId]) -> ScopeTapPoint {
    match (mode, selection.first()) {
        (TapMode::Clip, Some(clip)) => ScopeTapPoint::Clip(*clip),
        _ => ScopeTapPoint::Program,
    }
}

/// The "Scoping: …" line (13 §10.2). Named from what the engine ACTUALLY tapped,
/// never from the request, plus a reason when the two disagree — the panel must
/// not claim to be scoping a clip it is not.
pub(crate) fn scope_tap_label(doc: &Document, want: ScopeTapPoint, got: ScopeTapPoint) -> String {
    match got {
        ScopeTapPoint::Clip(clip) => clip_name(doc, clip),
        ScopeTapPoint::Program if matches!(want, ScopeTapPoint::Clip(_)) => {
            "Program (clip not under the playhead)".to_string()
        }
        ScopeTapPoint::Program => "Program".to_string(),
    }
}

/// Floating / dockable scopes window (07 §6): waveform / parade / vectorscope /
/// histogram, GPU-computed via `photonic_render::scopes` over the engine's
/// **scope tap** and painted from the read-back bins. Its own window close button
/// clears `open`.
///
/// K-E2: the measured texture is the engine's `EngineFrame::scope_tap` — the
/// selected clip's post-`Grade`, pre-fold texture, or the program pre-
/// `CaptionOverlay` — never the presented (post-caption, post-fold) frame, which
/// is the signal 26 K-E2 flagged as "the wrong thing". Returns the tap the panel
/// wants so the caller can hand it to the engine.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_scopes_panel(
    ctx: &egui::Context,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: Option<&EngineFrame>,
    doc: &Document,
    selection: &[ClipId],
    open: &mut bool,
    kind: &mut ScopeKind,
) -> ScopeTapPoint {
    let mut is_open = *open;
    let mode_id = egui::Id::new("scopes_tap_mode");
    let mut mode: TapMode = ctx.data(|d| d.get_temp(mode_id).unwrap_or_default());
    let want = requested_tap(mode, selection);
    let got = frame.map(|f| f.scope_tap_point).unwrap_or(want);

    egui::Window::new("Scopes")
        .open(&mut is_open)
        .resizable(true)
        .default_size([320.0, 360.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (k, name) in [
                    (ScopeKind::Waveform, "Waveform"),
                    (ScopeKind::Parade, "Parade"),
                    (ScopeKind::Vectorscope, "Vectorscope"),
                    (ScopeKind::Histogram, "Histogram"),
                    (ScopeKind::AudioSpectrum, "Spectrum"),
                ] {
                    if ui.selectable_label(*kind == k, name).clicked() {
                        *kind = k;
                    }
                }
            });
            // K-E2 tap-point picker (07 §5's per-clip-with-fallback). Same
            // `selectable_label` idiom as the scope-kind row above, so it
            // inherits the same focus/hit-target behaviour.
            ui.horizontal(|ui| {
                ui.label(RichText::new("Tap").small().color(muted(ui)));
                for (m, name, tip) in [
                    (
                        TapMode::Clip,
                        "Clip",
                        "Scope the selected clip after its grade, before the track fold",
                    ),
                    (
                        TapMode::Program,
                        "Program",
                        "Scope the whole sequence before caption overlay",
                    ),
                ] {
                    if ui
                        .selectable_label(mode == m, name)
                        .on_hover_text(tip)
                        .clicked()
                    {
                        mode = m;
                    }
                }
            });
            ui.label(
                RichText::new(format!("Scoping: {}", scope_tap_label(doc, want, got)))
                    .small()
                    .color(muted(ui)),
            );
            ui.separator();

            // K-E1 audio spectrum is independent of the video tap.
            if *kind == ScopeKind::AudioSpectrum {
                draw_audio_spectrum(ui, ctx);
                return;
            }
            let Some(tap) = frame.and_then(|f| f.scope_tap.as_ref()) else {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("No signal — play or seek the sequence.").color(muted(ui)),
                    );
                });
                return;
            };
            // The tap texture comes out of the node pool and is bucket-padded, so
            // every scope reads its LOGICAL extent (03 §3.4) — measuring the
            // padding would put a phantom black spike in each plot.
            let (tex, w, h) = (tap.texture.as_ref(), tap.width, tap.height);
            match *kind {
                ScopeKind::Histogram => draw_histogram(ui, device, queue, tex, w, h),
                ScopeKind::Parade => draw_parade(ui, device, queue, tex, w, h),
                ScopeKind::Waveform => draw_waveform(ui, device, queue, tex, w, h),
                ScopeKind::Vectorscope => draw_vectorscope(ui, device, queue, tex, w, h),
                ScopeKind::AudioSpectrum => draw_audio_spectrum(ui, ctx),
            }
        });

    ctx.data_mut(|d| d.insert_temp(mode_id, mode));
    *open = is_open;
    requested_tap(mode, selection)
}

/// A clip's display name, or a short id fallback when it is not in the document.
fn clip_name(doc: &Document, clip: ClipId) -> String {
    doc.timeline
        .as_ref()
        .and_then(|p| {
            p.sequences
                .values()
                .flat_map(|s| s.video_tracks.iter().chain(s.audio_tracks.iter()))
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip)
        })
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "Program".to_string())
}

/// K-E1 histogram component mask: bit0=Y, bit1=R, bit2=G, bit3=B.
#[derive(Clone, Copy)]
struct HistChannels {
    y: bool,
    r: bool,
    g: bool,
    b: bool,
}

impl Default for HistChannels {
    fn default() -> Self {
        Self {
            y: true,
            r: true,
            g: true,
            b: true,
        }
    }
}

fn draw_histogram(
    ui: &mut Ui,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    logical_w: u32,
    logical_h: u32,
) {
    let h =
        photonic_render::scopes::histogram_gpu_logical(device, queue, tex, logical_w, logical_h);
    // K-E1: channel toggles (session-only, egui temp data).
    let id = ui.id().with("hist_channels");
    let mut ch = ui
        .data(|d| d.get_temp::<HistChannels>(id))
        .unwrap_or_default();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Show").small().color(muted(ui)));
        ui.checkbox(&mut ch.y, "Y");
        ui.checkbox(&mut ch.r, "R");
        ui.checkbox(&mut ch.g, "G");
        ui.checkbox(&mut ch.b, "B");
    });
    ui.data_mut(|d| d.insert_temp(id, ch));

    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, w * 0.6), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, Color32::from_rgb(7, 7, 11));

    let bins = h.luma.len();
    let mut max = 1u32;
    if ch.y {
        max = max.max(h.luma.iter().copied().max().unwrap_or(0));
    }
    if ch.r {
        max = max.max(h.red.iter().copied().max().unwrap_or(0));
    }
    if ch.g {
        max = max.max(h.green.iter().copied().max().unwrap_or(0));
    }
    if ch.b {
        max = max.max(h.blue.iter().copied().max().unwrap_or(0));
    }
    let max = max.max(1) as f32;
    let bw = rect.width() / bins as f32;
    let filled = |data: &[u32], color: Color32| {
        for (i, &c) in data.iter().enumerate() {
            let x = rect.left() + i as f32 * bw;
            let hgt = (c as f32 / max) * rect.height();
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(x, rect.bottom() - hgt),
                    pos2(x + bw.max(1.0), rect.bottom()),
                ),
                0.0,
                color,
            );
        }
    };
    let line = |data: &[u32], color: Color32| {
        for (i, &c) in data.iter().enumerate() {
            let x = rect.left() + i as f32 * bw;
            let hgt = (c as f32 / max) * rect.height();
            if hgt > 0.5 {
                painter.line_segment(
                    [pos2(x, rect.bottom()), pos2(x, rect.bottom() - hgt)],
                    Stroke::new(1.0, color),
                );
            }
        }
    };
    if ch.y {
        filled(&h.luma, on_surface(ui).gamma_multiply(0.45));
    }
    if ch.r {
        line(&h.red, Color32::from_rgb(220, 90, 90));
    }
    if ch.g {
        line(&h.green, Color32::from_rgb(90, 200, 110));
    }
    if ch.b {
        line(&h.blue, Color32::from_rgb(100, 130, 230));
    }
    painter.rect_stroke(rect, 3.0, Stroke::new(1.0, border(ui)));
}

fn draw_parade(
    ui: &mut Ui,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    logical_w: u32,
    logical_h: u32,
) {
    // The render API's waveform is luma-only, so render the three per-channel
    // histograms side by side as an RGB level parade (real per-channel data).
    let h =
        photonic_render::scopes::histogram_gpu_logical(device, queue, tex, logical_w, logical_h);
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, w * 0.6), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, Color32::from_rgb(7, 7, 11));
    let max = h
        .red
        .iter()
        .chain(h.green.iter())
        .chain(h.blue.iter())
        .copied()
        .max()
        .unwrap_or(1)
        .max(1) as f32;
    let cols = [
        (&h.red, Color32::from_rgb(220, 90, 90)),
        (&h.green, Color32::from_rgb(90, 200, 110)),
        (&h.blue, Color32::from_rgb(100, 130, 230)),
    ];
    let panel_w = rect.width() / 3.0;
    for (ci, (data, color)) in cols.iter().enumerate() {
        let x0 = rect.left() + ci as f32 * panel_w;
        let bins = data.len();
        let bw = panel_w / bins as f32;
        for (i, &c) in data.iter().enumerate() {
            let x = x0 + i as f32 * bw;
            let hgt = (c as f32 / max) * rect.height();
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(x, rect.bottom() - hgt),
                    pos2(x + bw.max(1.0), rect.bottom()),
                ),
                0.0,
                *color,
            );
        }
    }
    painter.rect_stroke(rect, 3.0, Stroke::new(1.0, border(ui)));
}

fn draw_waveform(
    ui: &mut Ui,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    logical_w: u32,
    logical_h: u32,
) {
    let wf =
        photonic_render::scopes::waveform_gpu_logical(device, queue, tex, logical_w, logical_h);
    let out_w = 256usize;
    let out_h = wf.bins;
    let mut pixels = vec![Color32::from_rgb(7, 7, 11); out_w * out_h];
    let peak = wf.data.iter().copied().max().unwrap_or(1).max(1) as f32;
    let src_w = wf.width.max(1);
    for ox in 0..out_w {
        let sx = ox * src_w / out_w;
        for bin in 0..out_h {
            let c = wf.count(sx, bin) as f32;
            if c > 0.0 {
                let a = (c / peak).sqrt().clamp(0.0, 1.0);
                let v = ((a * 215.0) as u8).saturating_add(20);
                let row = out_h - 1 - bin; // luma high → top
                pixels[row * out_w + ox] = Color32::from_gray(v);
            }
        }
    }
    scope_image(ui, "scope_waveform", out_w, out_h, pixels, None);
}

fn draw_vectorscope(
    ui: &mut Ui,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    logical_w: u32,
    logical_h: u32,
) {
    // K-E1 matrix switch: GPU path is BT.709-only today; when the user picks
    // BT.601 we recompute on a CPU readback of the same texture via the
    // matrix-aware vectorscope (scopes stay correct for SD footage).
    let id = ui.id().with("vs_matrix");
    let mut use_601 = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Matrix").small().color(muted(ui)));
        if ui.selectable_label(!use_601, "Rec.709").clicked() {
            use_601 = false;
        }
        if ui.selectable_label(use_601, "Rec.601").clicked() {
            use_601 = true;
        }
    });
    ui.data_mut(|d| d.insert_temp(id, use_601));

    let matrix = if use_601 {
        photonic_render::color::Matrix::Bt601
    } else {
        photonic_render::color::Matrix::Bt709
    };
    let vs = photonic_render::scopes::vectorscope_gpu_logical_matrix(
        device, queue, tex, logical_w, logical_h, matrix,
    );
    let n = vs.size;
    let mut pixels = vec![Color32::from_rgb(7, 7, 11); n * n];
    let peak = vs.data.iter().copied().max().unwrap_or(1).max(1) as f32;
    for cr in 0..n {
        for cb in 0..n {
            let c = vs.count(cb, cr) as f32;
            if c > 0.0 {
                let a = (c / peak).sqrt().clamp(0.0, 1.0);
                let v = ((a * 215.0) as u8).saturating_add(20);
                let row = n - 1 - cr; // Cr axis points up
                pixels[row * n + cb] = Color32::from_gray(v);
            }
        }
    }
    scope_image(
        ui,
        "scope_vectorscope",
        n,
        n,
        pixels,
        Some(draw_vectorscope_guides),
    );
}

/// K-E1: audio spectrum (dB vs frequency) from the engine feeder's latest
/// master-bus DFT. Pure drawing over a status snapshot — zero document state.
fn draw_audio_spectrum(ui: &mut Ui, ctx: &egui::Context) {
    // Session bridge stores spectrum via PhotonicApp → status; fall back to
    // empty. The window call site has no engine handle, so we read egui temp
    // filled by the app each frame when scopes are open.
    let bins: Vec<f32> = ctx
        .data(|d| d.get_temp::<Vec<f32>>(egui::Id::new("ke1_spectrum_db")))
        .unwrap_or_default();
    ui.label(
        RichText::new("Audio spectrum (master bus, dBFS)")
            .small()
            .color(muted(ui)),
    );
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 200.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_rgb(7, 7, 11));
    if bins.is_empty() {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No audio — play the sequence",
            egui::FontId::proportional(12.0),
            muted(ui),
        );
        return;
    }
    let n = bins.len().max(1) as f32;
    let bar_w = rect.width() / n;
    // Map -96..0 dB into full height.
    let floor = -96.0f32;
    for (i, &db) in bins.iter().enumerate() {
        let t = ((db - floor) / -floor).clamp(0.0, 1.0);
        let h = t * rect.height();
        let x = rect.left() + i as f32 * bar_w;
        let r = Rect::from_min_max(
            pos2(x, rect.bottom() - h),
            pos2(x + bar_w.max(1.0) - 0.5, rect.bottom()),
        );
        painter.rect_filled(r, 0.0, Color32::from_rgb(0x6E, 0xA0, 0xE0));
    }
}

/// K-E1 vectorscope guides: I/Q axes (skin-tone on I), 75% / 100% boxes, and
/// the outer chroma circle. Angles are NTSC-derived in degrees from +Cb.
fn draw_vectorscope_guides(ui: &Ui, painter: &egui::Painter, rect: Rect) {
    let center = rect.center();
    let r100 = rect.width() * 0.45;
    let r75 = r100 * 0.75;
    let stroke_soft = Stroke::new(0.5, border(ui));
    let stroke_i = Stroke::new(1.2, Color32::from_rgb(0xE8, 0xB0, 0x70)); // warm skin
    let stroke_q = Stroke::new(1.0, Color32::from_rgb(0x70, 0xB0, 0xE8)); // cool Q

    // 100% outer circle + 75% broadcast-safe box (square inscribed at 0.75 radius).
    // A square inscribed in a circle of radius r has side 2r/√2, so the factor is
    // exactly FRAC_1_SQRT_2 — spelled out rather than as a 0.7071 literal, which
    // clippy::approx_constant rejects (deny-by-default, and CI's lint job is
    // blocking).
    const INSCRIBED: f32 = std::f32::consts::FRAC_1_SQRT_2;
    painter.circle_stroke(center, r100, stroke_soft);
    let box75 = Rect::from_center_size(center, vec2(r75 * 2.0 * INSCRIBED, r75 * 2.0 * INSCRIBED));
    painter.rect_stroke(
        box75,
        0.0,
        Stroke::new(0.8, Color32::from_rgb(0x90, 0x90, 0x70)),
    );
    // Fainter 100% box for the full legal box.
    let box100 =
        Rect::from_center_size(center, vec2(r100 * 2.0 * INSCRIBED, r100 * 2.0 * INSCRIBED));
    painter.rect_stroke(box100, 0.0, stroke_soft);

    // I-line (≈123°) — skin tones; Q-line is perpendicular (≈33°).
    let i_ang = 123.0_f32.to_radians();
    let q_ang = 33.0_f32.to_radians();
    let i_dir = vec2(i_ang.cos(), -i_ang.sin());
    let q_dir = vec2(q_ang.cos(), -q_ang.sin());
    painter.line_segment([center - i_dir * r100, center + i_dir * r100], stroke_i);
    painter.line_segment([center - q_dir * r100, center + q_dir * r100], stroke_q);
    // Tiny labels near the rim.
    let i_label = center + i_dir * (r100 * 0.92);
    let q_label = center + q_dir * (r100 * 0.92);
    painter.text(
        i_label,
        egui::Align2::CENTER_CENTER,
        "I",
        egui::FontId::proportional(10.0),
        stroke_i.color,
    );
    painter.text(
        q_label,
        egui::Align2::CENTER_CENTER,
        "Q",
        egui::FontId::proportional(10.0),
        stroke_q.color,
    );
    // 75% label on the box corner.
    painter.text(
        box75.right_top() + vec2(-2.0, 2.0),
        egui::Align2::RIGHT_TOP,
        "75%",
        egui::FontId::proportional(9.0),
        muted(ui),
    );
}

/// Upload a scope image and paint it square, with an optional overlay.
fn scope_image(
    ui: &mut Ui,
    name: &str,
    w: usize,
    h: usize,
    pixels: Vec<Color32>,
    overlay: Option<fn(&Ui, &egui::Painter, Rect)>,
) {
    let image = egui::ColorImage {
        size: [w, h],
        pixels,
    };
    let handle = ui.ctx().load_texture(name, image, TextureOptions::LINEAR);
    let side = ui.available_width().min(ui.available_height()).max(160.0);
    let (rect, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, Color32::from_rgb(7, 7, 11));
    painter.image(
        handle.id(),
        rect,
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );
    if let Some(f) = overlay {
        f(ui, &painter, rect);
    }
    painter.rect_stroke(rect, 3.0, Stroke::new(1.0, border(ui)));
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests — pure mapping / hit-test / seeding logic.
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── K-E2 scope tap ──────────────────────────────────────────────────────

    /// The panel's request follows the selection in `Clip` mode and is pinned in
    /// `Program` mode; with nothing selected there is no clip to scope, so the
    /// request is the program (13 §10.2's "never blank").
    #[test]
    fn tap_request_follows_selection_only_in_clip_mode() {
        let clip = ClipId::new();
        assert_eq!(
            requested_tap(TapMode::Clip, &[clip]),
            ScopeTapPoint::Clip(clip)
        );
        assert_eq!(requested_tap(TapMode::Clip, &[]), ScopeTapPoint::Program);
        assert_eq!(
            requested_tap(TapMode::Program, &[clip]),
            ScopeTapPoint::Program,
            "Program mode ignores the selection"
        );
    }

    /// The "Scoping:" line names what the engine ACTUALLY tapped. When a clip tap
    /// was requested but the engine fell back, the label must say so rather than
    /// keep claiming the clip's name — the failure mode is a colourist trusting a
    /// program reading they think is a clip reading.
    #[test]
    fn tap_label_reports_the_fallback_rather_than_the_request() {
        let doc = Document::new("scopes", 16.0, 16.0);
        let clip = ClipId::new();
        let fell_back = scope_tap_label(&doc, ScopeTapPoint::Clip(clip), ScopeTapPoint::Program);
        assert!(
            fell_back.starts_with("Program") && fell_back.contains("not under the playhead"),
            "a fallback must be visible in the label, got {fell_back:?}"
        );
        assert_eq!(
            scope_tap_label(&doc, ScopeTapPoint::Program, ScopeTapPoint::Program),
            "Program",
            "an honest program request carries no fallback note"
        );
    }

    #[test]
    fn chroma_round_trips_through_wheel() {
        let delta = [0.2f32, -0.1, -0.1];
        let xy = deltas_to_chroma_xy(delta);
        let back = chroma_to_deltas(xy);
        let luma = (delta[0] + delta[1] + delta[2]) / 3.0;
        for c in 0..3 {
            assert!((back[c] - (delta[c] - luma)).abs() < 1e-4, "channel {c}");
        }
        assert!((back[0] + back[1] + back[2]).abs() < 1e-4, "no luma shift");
    }

    #[test]
    fn neutral_and_pure_luma_have_no_chroma() {
        assert!(deltas_to_chroma_xy([0.0, 0.0, 0.0]).length() < 1e-6);
        assert!(deltas_to_chroma_xy([0.3, 0.3, 0.3]).length() < 1e-5);
    }

    #[test]
    fn two_consecutive_nudges_move_two_steps() {
        // The regression the focus-lock fixes: before it, egui stole focus off the
        // plot after the first Arrow, so the second nudge silently did nothing.
        // The extracted logic proves two nudges compose to 2*step.
        let mut pts = vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)];
        let step = 0.005;
        pts[1] = nudge_point(&pts, 1, 0.0, 1.0, step);
        pts[1] = nudge_point(&pts, 1, 0.0, 1.0, step);
        assert!((pts[1].1 - (0.5 + 2.0 * step)).abs() < 1e-6);
        // A middle point's x stays strictly inside its neighbours; a horizontal
        // nudge moves it too.
        let moved = nudge_point(&pts, 1, 1.0, 0.0, step);
        assert!(moved.0 > 0.5 && moved.0 < 1.0);
    }

    #[test]
    fn endpoints_keep_pinned_x() {
        let pts = vec![(0.0, 0.2), (0.5, 0.5), (1.0, 0.8)];
        // Endpoint x never moves regardless of dx.
        assert_eq!(nudge_point(&pts, 0, 1.0, 0.0, 0.05).0, 0.0);
        assert_eq!(nudge_point(&pts, 2, -1.0, 0.0, 0.05).0, 1.0);
    }

    #[test]
    fn curve_screen_round_trip() {
        let rect = Rect::from_min_max(pos2(10.0, 20.0), pos2(210.0, 120.0));
        for &(x, y) in &[(0.0f32, 0.0f32), (0.5, 0.5), (1.0, 1.0), (0.25, 0.8)] {
            let s = curve_to_screen((x, y), rect);
            let (rx, ry) = screen_to_curve(s, rect);
            assert!((rx - x).abs() < 1e-3, "x {x}");
            assert!((ry - y).abs() < 1e-3, "y {y}");
        }
    }

    #[test]
    fn curve_y_is_flipped() {
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0));
        assert!(curve_to_screen((0.0, 1.0), rect).y < curve_to_screen((0.0, 0.0), rect).y);
    }

    #[test]
    fn nearest_point_picks_within_threshold() {
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0));
        let pts = vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)];
        let to_screen = |p: (f32, f32)| curve_to_screen(p, rect);
        let mid = to_screen((0.5, 0.5));
        assert_eq!(nearest_point(&pts, &to_screen, mid, 8.0), Some(1));
        assert_eq!(nearest_point(&pts, &to_screen, pos2(48.0, 90.0), 4.0), None);
    }

    #[test]
    fn insert_sorted_keeps_order() {
        let mut pts = vec![(0.0, 0.0), (1.0, 1.0)];
        assert_eq!(insert_sorted(&mut pts, (0.4, 0.6)), 1);
        assert_eq!(pts, vec![(0.0, 0.0), (0.4, 0.6), (1.0, 1.0)]);
        // 0.9 sorts before the 1.0 endpoint → index 2.
        assert_eq!(insert_sorted(&mut pts, (0.9, 0.2)), 2);
        assert_eq!(pts, vec![(0.0, 0.0), (0.4, 0.6), (0.9, 0.2), (1.0, 1.0)]);
    }

    #[test]
    fn rgb_hsl_known_values() {
        let (h, s, l) = rgb_to_hsl(1.0, 0.0, 0.0);
        assert!(h.abs() < 1e-3 || (h - 1.0).abs() < 1e-3);
        assert!((s - 1.0).abs() < 1e-3);
        assert!((l - 0.5).abs() < 1e-3);
        let (_, s2, l2) = rgb_to_hsl(0.5, 0.5, 0.5);
        assert!(s2 < 1e-3);
        assert!((l2 - 0.5).abs() < 1e-3);
    }

    #[test]
    fn qualifier_seed_brackets_center_and_clamps() {
        let (hr, sr, lr) = seed_qualifier(0.5, 0.5, 0.5);
        assert!(hr[0] <= 0.5 && hr[1] >= 0.5);
        assert!(sr[0] <= 0.5 && sr[1] >= 0.5);
        assert!(lr[0] <= 0.5 && lr[1] >= 0.5);
        let (_, sr2, _) = seed_qualifier(0.5, 0.95, 0.5);
        assert!(sr2[1] <= 1.0, "upper clamps to 1.0");
    }
}
