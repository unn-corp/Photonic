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
    color::{
        ColorDigest, InputColorInterpretation, InputMatrix, InputSignalRange, NativeChromaLocation,
        NativeInputColorInterpretation, NativeInputStandard, SequenceColorConfig,
    },
    ops, AssetId, AssetKind, AssetSource, CdlParams, ClipId, ClipLook, Grade, GradeGraph,
    GradeGraphNode, GradeMask, GradeOp, GradeOpId, GradeOpKind, GradeOpParams, LutInterp,
    QualifierKey, SequenceId, TrackId, VfxOwner, WindowShape, MAX_QUALIFIER_KEYS,
};
use photonic_render::scopes::ScopeScale;

use photonic_video::graph::ScopeTapPoint;

use super::param_expr;
use photonic_video::session::EngineFrame;

use crate::panels::{eyedropper_btn, EyedropperTarget, PanelAction, QualifierSampleMode};

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

/// Chronological neighbours on the selected clip's own video track. Sorting
/// here tolerates old projects whose clip vector is not in timeline order.
fn grade_clip_neighbors(
    sequence: &photonic_core::timeline::Sequence,
    track_id: TrackId,
    clip_id: ClipId,
) -> Option<(Option<ClipId>, Option<ClipId>, usize, usize)> {
    let track = sequence.video_tracks.iter().find(|t| t.id == track_id)?;
    let mut ordered: Vec<_> = track
        .clips
        .iter()
        .enumerate()
        .map(|(index, clip)| (clip.start, index, clip.id))
        .collect();
    ordered.sort_by_key(|(start, index, _)| (*start, *index));
    let position = ordered.iter().position(|(_, _, id)| *id == clip_id)?;
    Some((
        position.checked_sub(1).map(|index| ordered[index].2),
        ordered.get(position + 1).map(|entry| entry.2),
        position,
        ordered.len(),
    ))
}

fn draw_grade_navigation(
    ui: &mut Ui,
    doc: &Document,
    actions: &mut Vec<PanelAction>,
    seq: SequenceId,
    track: TrackId,
    clip: ClipId,
) {
    let Some((previous, next, position, total)) = doc
        .timeline
        .as_ref()
        .and_then(|p| p.sequences.get(&seq))
        .and_then(|s| grade_clip_neighbors(s, track, clip))
    else {
        return;
    };
    ui.horizontal(|ui| {
        if ui
            .add_enabled(previous.is_some(), egui::Button::new("← Previous clip"))
            .clicked()
        {
            actions.push(PanelAction::SelectGradeClip {
                clip: previous.unwrap(),
            });
        }
        ui.label(format!("{} / {}", position + 1, total));
        if ui
            .add_enabled(next.is_some(), egui::Button::new("Next clip →"))
            .clicked()
        {
            actions.push(PanelAction::SelectGradeClip {
                clip: next.unwrap(),
            });
        }
    });
}

/// Shared session key used by Color Controls and the program-monitor window
/// overlay. A clip selection gets its own scope choice without document edits.
pub(crate) fn active_grade_scope_id(clip: ClipId) -> egui::Id {
    egui::Id::new(("active_grade_scope", clip))
}

fn shared_look_editor_id(clip: ClipId) -> egui::Id {
    egui::Id::new(("edit_shared_look", clip))
}

fn advanced_controls_id() -> egui::Id {
    egui::Id::new("advanced_color_controls")
}

fn grade_multi_selection_id(owner: VfxOwner) -> egui::Id {
    egui::Id::new(("grade_multi_selection", owner))
}

/// Update selection without mutating the grade. Shift selects an ordered span;
/// Command/Ctrl toggles one corrector; a plain click chooses just one.
fn select_grade_ops(
    ids: &[GradeOpId],
    selected: &mut Vec<GradeOpId>,
    primary: &mut Option<GradeOpId>,
    clicked: GradeOpId,
    command: bool,
    shift: bool,
) {
    if shift {
        let clicked_index = ids.iter().position(|id| *id == clicked);
        let anchor_index = primary.and_then(|id| ids.iter().position(|candidate| *candidate == id));
        if let (Some(clicked_index), Some(anchor_index)) = (clicked_index, anchor_index) {
            let (start, end) = if anchor_index <= clicked_index {
                (anchor_index, clicked_index)
            } else {
                (clicked_index, anchor_index)
            };
            *selected = ids[start..=end].to_vec();
        } else {
            *selected = vec![clicked];
        }
    } else if command {
        if let Some(index) = selected.iter().position(|id| *id == clicked) {
            selected.remove(index);
        } else {
            selected.push(clicked);
        }
    } else {
        *selected = vec![clicked];
    }
    *primary = selected.last().copied();
}

fn set_selected_grade_ops_enabled(grade: &mut Grade, selected: &[GradeOpId], enabled: bool) {
    for op in &mut grade.ops {
        if selected.contains(&op.id) {
            op.enabled = enabled;
        }
    }
}

fn remove_selected_grade_ops(grade: &mut Grade, selected: &[GradeOpId]) {
    grade.ops.retain(|op| !selected.contains(&op.id));
}

fn input_color_detail(finding: &photonic_video::color::ColorInputFinding) -> &str {
    let detail = if finding.interpretation == "unresolved" {
        finding
            .diagnostic
            .as_deref()
            .or(finding.color_space.as_deref())
    } else {
        finding
            .color_space
            .as_deref()
            .or(finding.diagnostic.as_deref())
    };
    detail.unwrap_or("unknown")
}

#[derive(Clone)]
struct InputColorDraft {
    baseline: Option<InputColorInterpretation>,
    color_space: String,
    range: InputSignalRange,
    matrix: InputMatrix,
}

impl InputColorDraft {
    fn from_current(current: Option<&InputColorInterpretation>) -> Self {
        Self {
            baseline: current.cloned(),
            color_space: current.map_or_else(String::new, |value| value.color_space.clone()),
            range: current.map_or(InputSignalRange::FromMetadata, |value| value.range),
            matrix: current.map_or(InputMatrix::FromMetadata, |value| value.matrix),
        }
    }

    fn interpretation(&self, config_sha256: ColorDigest) -> InputColorInterpretation {
        InputColorInterpretation {
            config_sha256,
            color_space: self.color_space.trim().to_owned(),
            range: self.range,
            matrix: self.matrix,
        }
    }
}

#[derive(Clone, Copy)]
enum InputColorEditTarget {
    Asset(AssetId),
    Clip(ClipId),
}

fn draw_input_color_editor(
    ui: &mut Ui,
    doc: &mut Document,
    history: &mut CommandHistory,
    sequence: SequenceId,
    track: TrackId,
    target: InputColorEditTarget,
    current: Option<InputColorInterpretation>,
    digest: ColorDigest,
    locked: bool,
) -> bool {
    let key = match target {
        InputColorEditTarget::Asset(id) => egui::Id::new(("asset_input_color_draft", id)),
        InputColorEditTarget::Clip(id) => egui::Id::new(("clip_input_color_draft", id)),
    };
    let mut draft = ui
        .data(|data| data.get_temp::<InputColorDraft>(key))
        .unwrap_or_else(|| InputColorDraft::from_current(current.as_ref()));
    if draft.baseline != current {
        draft = InputColorDraft::from_current(current.as_ref());
    }
    ui.add_enabled_ui(!locked, |ui| {
        ui.horizontal(|ui| {
            ui.label("OCIO space");
            ui.text_edit_singleline(&mut draft.color_space);
        });
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt((key, "range"))
                .selected_text(match draft.range {
                    InputSignalRange::FromMetadata => "Range: metadata",
                    InputSignalRange::Full => "Range: full",
                    InputSignalRange::Limited => "Range: limited",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut draft.range,
                        InputSignalRange::FromMetadata,
                        "Metadata",
                    );
                    ui.selectable_value(&mut draft.range, InputSignalRange::Full, "Full");
                    ui.selectable_value(&mut draft.range, InputSignalRange::Limited, "Limited");
                });
            egui::ComboBox::from_id_salt((key, "matrix"))
                .selected_text(match draft.matrix {
                    InputMatrix::FromMetadata => "Matrix: metadata",
                    InputMatrix::Rgb => "Matrix: RGB",
                    InputMatrix::Bt601 => "Matrix: BT.601",
                    InputMatrix::Bt709 => "Matrix: BT.709",
                    InputMatrix::Bt2020NonConstant => "Matrix: BT.2020 NCL",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut draft.matrix, InputMatrix::FromMetadata, "Metadata");
                    ui.selectable_value(&mut draft.matrix, InputMatrix::Rgb, "RGB");
                    ui.selectable_value(&mut draft.matrix, InputMatrix::Bt601, "BT.601");
                    ui.selectable_value(&mut draft.matrix, InputMatrix::Bt709, "BT.709");
                    ui.selectable_value(
                        &mut draft.matrix,
                        InputMatrix::Bt2020NonConstant,
                        "BT.2020 NCL",
                    );
                });
        });
    });
    ui.data_mut(|data| data.insert_temp(key, draft.clone()));
    let proposed = draft.interpretation(digest);
    let can_apply = !locked && proposed.validate().is_ok() && current.as_ref() != Some(&proposed);
    let mut edit = None;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(can_apply, egui::Button::new("Apply interpretation"))
            .clicked()
        {
            edit = Some(Some(proposed));
        }
        if ui
            .add_enabled(
                !locked && current.is_some(),
                egui::Button::new("Clear override"),
            )
            .clicked()
        {
            edit = Some(None);
        }
    });
    if locked {
        ui.label("Clip track is locked.");
    }
    let Some(new) = edit else { return false };
    let Some(project) = doc.timeline.as_ref() else {
        return false;
    };
    let command = match target {
        InputColorEditTarget::Asset(id) => ops::set_asset_input_color(project, id, new),
        InputColorEditTarget::Clip(id) => {
            ops::set_clip_input_color(project, sequence, track, id, new)
        }
    };
    if let Ok(command) = command {
        history.execute_discrete(Command::Timeline(command), doc);
        ui.data_mut(|data| data.remove::<InputColorDraft>(key));
        return true;
    }
    false
}

#[derive(Clone)]
struct NativeInputDraft {
    hlg_peak_nits: u32,
    reference_white_nits: u32,
    baseline: Option<NativeInputColorInterpretation>,
    standard: NativeInputStandard,
    range: InputSignalRange,
    chroma_location: Option<NativeChromaLocation>,
}

impl NativeInputDraft {
    fn from_current(current: Option<&NativeInputColorInterpretation>) -> Self {
        Self {
            hlg_peak_nits: current.and_then(|v| v.hlg_peak_nits).unwrap_or(1000),
            reference_white_nits: current.and_then(|v| v.reference_white_nits).unwrap_or(203),
            baseline: current.cloned(),
            standard: current.map_or(NativeInputStandard::Bt709Scene, |v| v.standard),
            range: current.map_or(InputSignalRange::Limited, |v| v.range),
            chroma_location: current.and_then(|v| v.chroma_location),
        }
    }

    fn interpretation(&self) -> NativeInputColorInterpretation {
        NativeInputColorInterpretation {
            hlg_peak_nits: (self.standard == NativeInputStandard::Bt2100HlgScene)
                .then_some(self.hlg_peak_nits),
            reference_white_nits: matches!(
                self.standard,
                NativeInputStandard::Bt2100PqDisplay | NativeInputStandard::Bt2100HlgScene
            )
            .then_some(self.reference_white_nits),
            version: 1,
            standard: self.standard,
            range: self.range,
            matrix: match self.standard {
                NativeInputStandard::SrgbDisplay => InputMatrix::Rgb,
                NativeInputStandard::Bt709Scene => InputMatrix::Bt709,
                NativeInputStandard::Bt2020Scene
                | NativeInputStandard::Bt2100PqDisplay
                | NativeInputStandard::Bt2100HlgScene => InputMatrix::Bt2020NonConstant,
            },
            chroma_location: self.chroma_location,
        }
    }
}

fn draw_native_input_color_editor(
    ui: &mut Ui,
    doc: &mut Document,
    history: &mut CommandHistory,
    sequence: SequenceId,
    track: TrackId,
    target: InputColorEditTarget,
    current: Option<NativeInputColorInterpretation>,
    locked: bool,
) -> bool {
    let key = match target {
        InputColorEditTarget::Asset(id) => egui::Id::new(("native_asset_input_draft", id)),
        InputColorEditTarget::Clip(id) => egui::Id::new(("native_clip_input_draft", id)),
    };
    let mut draft = ui
        .data(|data| data.get_temp::<NativeInputDraft>(key))
        .unwrap_or_else(|| NativeInputDraft::from_current(current.as_ref()));
    if draft.baseline != current {
        draft = NativeInputDraft::from_current(current.as_ref());
    }
    ui.add_enabled_ui(!locked, |ui| {
        egui::ComboBox::from_id_salt((key, "standard"))
            .width(ui.available_width().min(200.0))
            .selected_text(match draft.standard {
                NativeInputStandard::SrgbDisplay => "sRGB display still",
                NativeInputStandard::Bt709Scene => "BT.709 scene",
                NativeInputStandard::Bt2020Scene => "BT.2020 scene",
                NativeInputStandard::Bt2100PqDisplay => "BT.2100 PQ display",
                NativeInputStandard::Bt2100HlgScene => "BT.2100 HLG scene",
            })
            .show_ui(ui, |ui| {
                if ui
                    .selectable_value(
                        &mut draft.standard,
                        NativeInputStandard::SrgbDisplay,
                        "sRGB display still",
                    )
                    .clicked()
                {
                    draft.range = InputSignalRange::Full;
                    draft.chroma_location = None;
                }
                ui.selectable_value(
                    &mut draft.standard,
                    NativeInputStandard::Bt709Scene,
                    "BT.709 scene",
                );
                ui.selectable_value(
                    &mut draft.standard,
                    NativeInputStandard::Bt2020Scene,
                    "BT.2020 scene",
                );
                ui.selectable_value(
                    &mut draft.standard,
                    NativeInputStandard::Bt2100HlgScene,
                    "BT.2100 HLG scene",
                );
                ui.selectable_value(
                    &mut draft.standard,
                    NativeInputStandard::Bt2100PqDisplay,
                    "BT.2100 PQ display",
                );
            });
        egui::ComboBox::from_id_salt((key, "range"))
            .width(ui.available_width().min(200.0))
            .selected_text(match draft.range {
                InputSignalRange::Full => "Full range",
                InputSignalRange::Limited => "Limited range",
                InputSignalRange::FromMetadata => "Choose range",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut draft.range, InputSignalRange::Full, "Full");
                ui.selectable_value(&mut draft.range, InputSignalRange::Limited, "Limited");
            });
        egui::ComboBox::from_id_salt((key, "chroma_location"))
            .width(ui.available_width().min(200.0))
            .selected_text(match draft.chroma_location {
                None => "Choose 4:2:0 siting",
                Some(NativeChromaLocation::Left) => "Chroma left",
                Some(NativeChromaLocation::Center) => "Chroma center",
                Some(NativeChromaLocation::TopLeft) => "Chroma top-left",
                Some(NativeChromaLocation::Top) => "Chroma top",
                Some(NativeChromaLocation::BottomLeft) => "Chroma bottom-left",
                Some(NativeChromaLocation::Bottom) => "Chroma bottom",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut draft.chroma_location, None, "Unspecified");
                for (value, label) in [
                    (NativeChromaLocation::Left, "Left"),
                    (NativeChromaLocation::Center, "Center"),
                    (NativeChromaLocation::TopLeft, "Top-left"),
                    (NativeChromaLocation::Top, "Top"),
                    (NativeChromaLocation::BottomLeft, "Bottom-left"),
                    (NativeChromaLocation::Bottom, "Bottom"),
                ] {
                    ui.selectable_value(&mut draft.chroma_location, Some(value), label);
                }
            });
    });
    if matches!(
        draft.standard,
        NativeInputStandard::Bt2100PqDisplay | NativeInputStandard::Bt2100HlgScene
    ) {
        ui.add_enabled_ui(!locked, |ui| {
            ui.add(
                egui::DragValue::new(&mut draft.reference_white_nits)
                    .range(1..=10000)
                    .prefix("Reference white ")
                    .suffix(" nits"),
            );
            if draft.standard == NativeInputStandard::Bt2100HlgScene {
                ui.add(egui::DragValue::new(&mut draft.hlg_peak_nits).range(400..=2000).prefix("HLG reference peak ").suffix(" nits"));
                ui.label("Scene light anchored to the selected reference white and peak. Display contrast is applied by the output transform.");
            } else {
                ui.label("Absolute display light normalized into ACEScg. This does not undo a camera look.");
            }
        });
    }
    ui.data_mut(|data| data.insert_temp(key, draft.clone()));
    let proposed = draft.interpretation();
    let kind = doc.timeline.as_ref().and_then(|project| {
        let asset = match target {
            InputColorEditTarget::Asset(id) => Some(id),
            InputColorEditTarget::Clip(id) => project
                .sequences
                .get(&sequence)?
                .track(track)?
                .clips
                .iter()
                .find(|c| c.id == id)?
                .source
                .asset(),
        }?;
        project.media.assets.get(&asset).map(|a| a.kind)
    });
    let validation = kind.map_or_else(
        || Err("source asset is missing".into()),
        |kind| proposed.validate_asset_kind(kind),
    );
    if let Err(error) = &validation {
        ui.colored_label(ui.visuals().warn_fg_color, error);
    }
    let mut edit = None;
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                !locked && validation.is_ok() && current.as_ref() != Some(&proposed),
                egui::Button::new("Apply interpretation"),
            )
            .clicked()
        {
            edit = Some(Some(proposed));
        }
        if ui
            .add_enabled(
                !locked && current.is_some(),
                egui::Button::new("Clear override"),
            )
            .clicked()
        {
            edit = Some(None);
        }
    });
    if locked {
        ui.label("Interpretation is locked by a source user.");
    }
    let Some(new) = edit else { return false };
    let Some(project) = doc.timeline.as_ref() else {
        return false;
    };
    let command = match target {
        InputColorEditTarget::Asset(id) => ops::set_asset_native_input_color(project, id, new),
        InputColorEditTarget::Clip(id) => {
            ops::set_clip_native_input_color(project, sequence, track, id, new)
        }
    };
    match command {
        Ok(command) => {
            history.execute_discrete(Command::Timeline(command), doc);
            ui.data_mut(|data| data.remove::<NativeInputDraft>(key));
            return true;
        }
        Err(error) => {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("Input edit rejected: {error}"),
            );
        }
    }
    false
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

fn current_grade_scoped(doc: &Document, owner: VfxOwner) -> Grade {
    doc.timeline
        .as_ref()
        .and_then(|project| ops::scope_grade(project, owner).ok().flatten().cloned())
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
    discrete: bool,
) {
    let new = if new.ops.is_empty() && !new.bypass && new.graph.is_none() {
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
    if discrete {
        history.execute_discrete(Command::Timeline(cmd), doc);
    } else {
        history.execute(Command::Timeline(cmd), doc);
    }
}

fn commit_grade_scoped(
    doc: &mut Document,
    history: &mut CommandHistory,
    owner: VfxOwner,
    new: Grade,
    discrete: bool,
) {
    let new = if new.ops.is_empty() && !new.bypass && new.graph.is_none() {
        None
    } else {
        Some(new)
    };
    let Ok(cmd) = doc
        .timeline
        .as_ref()
        .ok_or(())
        .and_then(|project| ops::set_grade_scoped(project, owner, new).map_err(|_| ()))
    else {
        return;
    };
    if discrete {
        history.execute_discrete(Command::Timeline(cmd), doc);
    } else {
        history.execute(Command::Timeline(cmd), doc);
    }
}

fn commit_shared_look_grade(
    doc: &mut Document,
    history: &mut CommandHistory,
    id: photonic_core::timeline::SharedLookId,
    new: Grade,
    discrete: bool,
) {
    let Some(project) = doc.timeline.as_ref() else {
        return;
    };
    let Some(look) = project.shared_looks.get(&id) else {
        return;
    };
    let Ok(cmd) = ops::update_shared_look(project, id, &look.name, new) else {
        return;
    };
    if discrete {
        history.execute_discrete(Command::Timeline(cmd), doc);
    } else {
        history.execute(Command::Timeline(cmd), doc);
    }
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
        GradeOpKind::LinearOffset => "Linear Offset",
        GradeOpKind::PrinterLights => "Printer Lights",
        GradeOpKind::HighlightRolloff => "Highlight Roll-off",
        GradeOpKind::SaturationVibrance => "Saturation & Vibrance",
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
        GradeOpKind::LinearOffset => GradeOpParams::LinearOffset { rgb: [0.0; 3] },
        GradeOpKind::PrinterLights => GradeOpParams::PrinterLights { points: [0.0; 3] },
        GradeOpKind::HighlightRolloff => GradeOpParams::HighlightRolloff {
            knee: 1.0,
            strength: 0.0,
        },
        GradeOpKind::SaturationVibrance => GradeOpParams::SaturationVibrance {
            saturation: 1.0,
            vibrance: 0.0,
        },
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
            hue_vs_luma: Vec::new(),
            luma_vs_sat: Vec::new(),
            sat_vs_sat: Vec::new(),
        },
        GradeOpKind::HslQualifier => GradeOpParams::HslQualifier {
            hue: [0.0, 1.0],
            sat: [0.0, 1.0],
            lum: [0.0, 1.0],
            softness: 0.1,
            correction: CdlParams::identity(),
            keys: Vec::new(),
            matte_levels: [0.0, 0.0],
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
const ALL_KINDS: [GradeOpKind; 12] = [
    GradeOpKind::WhiteBalance,
    GradeOpKind::Exposure,
    GradeOpKind::LinearOffset,
    GradeOpKind::PrinterLights,
    GradeOpKind::HighlightRolloff,
    GradeOpKind::SaturationVibrance,
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

    draw_grade_navigation(ui, doc, actions, seq, track, clip);

    let asset = doc.timeline.as_ref().and_then(|project| {
        let clip = project
            .sequences
            .get(&seq)?
            .track(track)?
            .clips
            .iter()
            .find(|candidate| candidate.id == clip)?;
        match &clip.source {
            photonic_core::timeline::ClipSource::Asset { asset }
            | photonic_core::timeline::ClipSource::Vector { asset } => Some(*asset),
            _ => None,
        }
    });
    let groups = doc
        .timeline
        .as_ref()
        .and_then(|project| project.sequences.get(&seq))
        .and_then(|sequence| {
            let group = sequence
                .track(track)?
                .clips
                .iter()
                .find(|candidate| candidate.id == clip)?
                .group?;
            Some(sequence.group_chain(group))
        })
        .unwrap_or_default();
    let scope_id = active_grade_scope_id(clip);
    let mut owner = ui
        .data(|data| data.get_temp::<VfxOwner>(scope_id))
        .unwrap_or(VfxOwner::Clip(clip));
    if matches!(owner, VfxOwner::Asset(_)) && asset.is_none() {
        owner = VfxOwner::Clip(clip);
    }
    if matches!(owner, VfxOwner::GroupPre(id) | VfxOwner::GroupPost(id) if !groups.contains(&id)) {
        owner = VfxOwner::Clip(clip);
    }
    ui.horizontal(|ui| {
        ui.label("Grade scope");
        egui::ComboBox::from_id_salt(("grade_scope", clip))
            .selected_text(match owner {
                VfxOwner::Clip(_) => "Clip",
                VfxOwner::Track(_) => "Track",
                VfxOwner::Master(_) => "Sequence master",
                VfxOwner::Asset(_) => "Source asset",
                VfxOwner::GroupPre(_) => "Group pre",
                VfxOwner::GroupPost(_) => "Group post",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut owner, VfxOwner::Clip(clip), "Clip · this shot");
                if let Some(asset) = asset {
                    ui.selectable_value(
                        &mut owner,
                        VfxOwner::Asset(asset),
                        "Source asset · all uses",
                    );
                }
                ui.selectable_value(&mut owner, VfxOwner::Track(track), "Track · all clips");
                for (depth, group) in groups.iter().rev().enumerate() {
                    let label = if depth == 0 { "Group" } else { "Parent group" };
                    ui.selectable_value(
                        &mut owner,
                        VfxOwner::GroupPre(*group),
                        format!("{label} pre · shared balance"),
                    );
                    ui.selectable_value(
                        &mut owner,
                        VfxOwner::GroupPost(*group),
                        format!("{label} post · shared look"),
                    );
                }
                ui.selectable_value(
                    &mut owner,
                    VfxOwner::Master(seq),
                    "Sequence master · entire output",
                );
            });
    });
    if owner
        != ui
            .data(|data| data.get_temp::<VfxOwner>(scope_id))
            .unwrap_or(VfxOwner::Clip(clip))
    {
        *selected_op = None;
    }
    ui.data_mut(|data| data.insert_temp(scope_id, owner));

    if let Some(project) = doc.timeline.as_ref() {
        if let Some(sequence) = project.sequences.get(&seq) {
            if let Some(finding) = sequence
                .track(track)
                .and_then(|t| t.clips.iter().find(|c| c.id == clip))
                .and_then(|c| photonic_video::color::inspect_input(project, sequence, c))
            {
                let color = if finding.interpretation == "unresolved" {
                    ui.visuals().warn_fg_color
                } else {
                    muted(ui)
                };
                let detail = input_color_detail(&finding);
                ui.colored_label(
                    color,
                    format!("Input color: {} · {detail}", finding.interpretation),
                );
            }
            if let Some(diagnostic) = photonic_video::color::inspect(sequence).diagnostic {
                if matches!(&sequence.color, SequenceColorConfig::NativeManaged(config) if config.validate().is_ok())
                {
                    ui.label(
                        RichText::new("Native SDR · ProRes MOV delivery")
                            .small()
                            .color(muted(ui)),
                    )
                    .on_hover_text(diagnostic);
                } else {
                    ui.colored_label(ui.visuals().warn_fg_color, diagnostic);
                }
            }
        }
    }
    if doc
        .timeline
        .as_ref()
        .and_then(|project| project.sequences.get(&seq))
        .is_some_and(|sequence| sequence.color.is_legacy())
        && ui
            .button("Create native managed-color draft")
            .on_hover_text(
                "Copies this sequence for Photonic-owned color work. The original stays active; qualified video tracks can preview and export as full-resolution ProRes MOV with BT.709 output.",
            )
            .clicked()
    {
        if let Some(project) = doc.timeline.as_ref() {
            if let Ok(command) = ops::convert_sequence_native_color(
                project,
                seq,
                photonic_core::timeline::color::NativeManagedColorConfig::sdr_draft(),
            ) {
                history.execute_discrete(Command::Timeline(command), doc);
                return;
            }
        }
    }

    let input_edit = doc.timeline.as_ref().and_then(|project| {
        let sequence = project.sequences.get(&seq)?;
        let SequenceColorConfig::Managed(config) = &sequence.color else {
            return None;
        };
        let selected_track = sequence.track(track)?;
        let selected_clip = selected_track
            .clips
            .iter()
            .find(|candidate| candidate.id == clip)?;
        let asset_id = match selected_clip.source {
            photonic_core::timeline::ClipSource::Asset { asset }
            | photonic_core::timeline::ClipSource::Vector { asset } => asset,
            _ => return None,
        };
        let source = project.media.assets.get(&asset_id)?;
        Some((
            config.ocio.sha256.clone(),
            asset_id,
            source.input_color.clone(),
            selected_clip.input_color.clone(),
            selected_track.locked,
        ))
    });
    if let Some((digest, asset_id, asset_input, clip_input, track_locked)) = input_edit {
        let changed = ui.collapsing("Source input interpretation", |ui| {
            ui.label("Asset interpretation · shared by every use");
            if draw_input_color_editor(
                ui,
                doc,
                history,
                seq,
                track,
                InputColorEditTarget::Asset(asset_id),
                asset_input,
                digest.clone(),
                track_locked,
            ) {
                return true;
            }
            ui.separator();
            ui.label("Clip override · this shot only");
            draw_input_color_editor(
                ui,
                doc,
                history,
                seq,
                track,
                InputColorEditTarget::Clip(clip),
                clip_input,
                digest,
                track_locked,
            )
        });
        if changed.body_returned == Some(true) {
            return;
        }
    }

    let native_input_edit = doc.timeline.as_ref().and_then(|project| {
        let sequence = project.sequences.get(&seq)?;
        if !matches!(&sequence.color, SequenceColorConfig::NativeManaged(_)) {
            return None;
        }
        let selected_track = sequence.track(track)?;
        let selected_clip = selected_track
            .clips
            .iter()
            .find(|candidate| candidate.id == clip)?;
        let asset_id = match selected_clip.source {
            photonic_core::timeline::ClipSource::Asset { asset } => asset,
            _ => return None,
        };
        let source = project.media.assets.get(&asset_id)?;
        if !matches!(source.kind, AssetKind::Video | AssetKind::Image) {
            return None;
        }
        Some((
            asset_id,
            source.native_input_color.clone(),
            selected_clip.native_input_color.clone(),
            selected_track.locked,
            ops::grade_scope_locked(project, VfxOwner::Asset(asset_id)),
        ))
    });
    if let Some((asset_id, asset_input, clip_input, track_locked, asset_locked)) = native_input_edit
    {
        let changed = ui.collapsing("Native source interpretation", |ui| {
            ui.label("Asset interpretation · shared by every use");
            if draw_native_input_color_editor(
                ui,
                doc,
                history,
                seq,
                track,
                InputColorEditTarget::Asset(asset_id),
                asset_input,
                asset_locked,
            ) {
                return true;
            }
            ui.separator();
            ui.label("Clip override · this shot only");
            draw_native_input_color_editor(
                ui,
                doc,
                history,
                seq,
                track,
                InputColorEditTarget::Clip(clip),
                clip_input,
                track_locked,
            )
        });
        if changed.body_returned == Some(true) {
            return;
        }
    }

    // Session view state only: both views operate on the same Grade. Default
    // to the existing full controls so older workspaces retain their layout.
    let mode_id = advanced_controls_id();
    let mut advanced = ui.data(|data| data.get_temp::<bool>(mode_id).unwrap_or(true));
    ui.horizontal(|ui| {
        ui.label("Controls");
        ui.selectable_value(&mut advanced, false, "Creator");
        ui.selectable_value(&mut advanced, true, "Advanced");
    });
    ui.data_mut(|data| data.insert_temp(mode_id, advanced));

    if doc.timeline.as_ref().is_some_and(|project| {
        ops::grade_scope_locked(project, owner) || ops::scope_grade(project, owner).is_err()
    }) {
        ui.label("This grade scope is locked or unavailable.");
        ui.disable();
    }

    if advanced && matches!(owner, VfxOwner::Clip(_)) {
        if draw_shared_looks(ui, doc, history, seq, track, clip, selected_op) {
            return;
        }
        let editing_shared = ui
            .data(|d| d.get_temp::<bool>(shared_look_editor_id(clip)))
            .unwrap_or(false);
        if !editing_shared && draw_grade_versions(ui, doc, history, seq, track, clip) {
            return; // reload the selected grade from the edited document next frame
        }
        if !editing_shared && draw_grade_copy(ui, doc, history, seq, track, clip) {
            return;
        }
    }

    let shared_id = if advanced
        && matches!(owner, VfxOwner::Clip(_))
        && ui
            .data(|d| d.get_temp::<bool>(shared_look_editor_id(clip)))
            .unwrap_or(false)
    {
        doc.timeline
            .as_ref()
            .and_then(|project| project.sequences.get(&seq))
            .and_then(|sequence| sequence.track(track))
            .and_then(|track| track.clips.iter().find(|candidate| candidate.id == clip))
            .and_then(|clip| match clip.look {
                Some(ClipLook::Shared(id)) => Some(id),
                _ => None,
            })
    } else {
        None
    };
    if let Some(id) = shared_id {
        let locked = doc.timeline.as_ref().is_some_and(|project| {
            project
                .sequences
                .values()
                .flat_map(|sequence| sequence.tracks())
                .filter(|track| track.locked)
                .flat_map(|track| &track.clips)
                .any(|candidate| candidate.look == Some(ClipLook::Shared(id)))
        });
        if locked {
            ui.label("This shared look is linked to a locked track.");
            ui.disable();
        }
    }

    // Read-only snapshot up front so the later `&mut doc` commit doesn't collide
    // with the media-pool borrow.
    let luts = lut_assets(doc);
    let orig = shared_id
        .and_then(|id| {
            doc.timeline
                .as_ref()?
                .shared_looks
                .get(&id)
                .map(|look| look.grade.clone())
        })
        .unwrap_or_else(|| current_grade_scoped(doc, owner));
    let mut g = orig.clone();
    let mut graph_active = g.graph.is_some();
    let mut topology_changed = false;
    let multi_id = grade_multi_selection_id(owner);
    let mut selected_ops: Vec<GradeOpId> =
        ui.data(|data| data.get_temp(multi_id).unwrap_or_default());
    selected_ops.retain(|id| g.ops.iter().any(|op| op.id == *id));
    if selected_ops.is_empty() && selected_op.is_some_and(|id| g.ops.iter().any(|op| op.id == id)) {
        selected_ops.push(selected_op.unwrap());
    }
    if !selected_ops.is_empty() && !selected_op.is_some_and(|id| selected_ops.contains(&id)) {
        *selected_op = selected_ops.last().copied();
    }

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

    if advanced && graph_active {
        ui.label(
            RichText::new("Graph grade · route correctors and mix parallel branches below.")
                .color(muted(ui)),
        );
        if g.graph.as_ref() == Some(&photonic_core::timeline::GradeGraph::from_stack(&g.ops))
            && ui.button("Return to ordered stack").clicked()
        {
            g.graph = None;
            graph_active = false;
            topology_changed = true;
        }
    } else if advanced && !g.ops.is_empty() && ui.button("Convert stack to grade graph").clicked() {
        g.convert_to_graph();
        graph_active = true;
        topology_changed = true;
    }

    // ── Quick grade strip (proposal 213 AS-1) — Exposure / Contrast / Sat ────
    // Temporary identity controls are persisted only when actually adjusted.
    let native = doc
        .timeline
        .as_ref()
        .and_then(|project| project.sequences.get(&seq))
        .is_some_and(|sequence| {
            matches!(
                sequence.color,
                photonic_core::timeline::color::SequenceColorConfig::NativeManaged(_)
            )
        });
    section_header(ui, "QUICK GRADE");
    {
        let saturation_kind = if native {
            GradeOpKind::SaturationVibrance
        } else {
            GradeOpKind::Cdl
        };
        for (kind, label, range) in [
            (GradeOpKind::Exposure, "Exposure", -3.0..=3.0),
            (
                GradeOpKind::Contrast,
                "Contrast",
                if native { -4.0..=4.0 } else { -1.0..=1.0 },
            ),
            (saturation_kind, "Saturation", 0.0..=2.0),
        ] {
            let index = g.ops.iter().position(|op| op.kind == kind);
            let mut op = index
                .map(|i| g.ops[i].clone())
                .unwrap_or_else(|| default_op(kind, &luts));
            let value = match &mut op.params.base {
                GradeOpParams::Exposure { stops } => stops,
                GradeOpParams::Contrast { amount, .. } => amount,
                GradeOpParams::Cdl { sat, .. } => sat,
                GradeOpParams::SaturationVibrance { saturation, .. } => saturation,
                _ => continue,
            };
            let mut changed = false;
            labelled(ui, label, |ui| {
                changed = ui
                    .add_enabled(
                        !graph_active,
                        egui::Slider::new(value, range)
                            .clamping(egui::SliderClamping::Edits)
                            .step_by(0.01),
                    )
                    .changed();
            });
            if changed {
                match index {
                    Some(i) => g.ops[i] = op,
                    None => g.ops.push(op),
                }
            }
        }
        if advanced {
            ui.label(
                RichText::new("Full wheels / curves / LUT below")
                    .small()
                    .color(muted(ui)),
            );
        }
    }

    if !advanced {
        if graph_active {
            ui.label("This grade uses a graph. Switch to Advanced to edit its correctors.");
        } else if !g.ops.is_empty() {
            ui.label(
                RichText::new(format!(
                    "{} corrector{} in this grade · Advanced shows the full stack",
                    g.ops.len(),
                    if g.ops.len() == 1 { "" } else { "s" }
                ))
                .small()
                .color(muted(ui)),
            );
        }
        if g != orig {
            let discrete = g.bypass != orig.bypass;
            if let Some(id) = shared_id {
                commit_shared_look_grade(doc, history, id, g, discrete);
            } else {
                commit_grade_scoped(doc, history, owner, g, discrete);
            }
        }
        return;
    }

    let quick_grade = g.clone();
    ui.add_space(4.0);
    ui.separator();

    // ── Primary-corrector tabs: browsing tools never inserts a corrector ─────
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
            let active = *tab == t && *selected_op == existing;
            if ui
                .selectable_label(active, kind_label(kind))
                .on_hover_text("View controls; a corrector is added when you adjust them")
                .clicked()
            {
                *tab = t;
                *selected_op = existing;
                selected_ops = existing.into_iter().collect();
            }
        }
    });

    ui.add_space(4.0);

    // ── Add-corrector menu (full catalog) ────────────────────────────────────
    ui.add_enabled_ui(!graph_active, |ui| {
        ui.menu_button(format!("{} Add corrector", ph::PLUS), |ui| {
            for kind in ALL_KINDS {
                if ui.button(kind_label(kind)).clicked() {
                    let op = default_op(kind, &luts);
                    *selected_op = Some(op.id);
                    selected_ops = vec![op.id];
                    g.ops.push(op);
                    ui.close_menu();
                }
            }
        })
    });

    ui.add_space(4.0);
    section_header(ui, "GRADE STACK");
    if g.ops.is_empty() {
        ui.label(RichText::new("No correctors yet — add one above.").color(muted(ui)));
    } else {
        ui.label(
            RichText::new("Ctrl/⌘ click to select several · Shift click for a range")
                .small()
                .color(muted(ui)),
        );
    }

    // ── Op stack: enable / select / reorder / remove ─────────────────────────
    let mut move_up: Option<usize> = None;
    let mut move_down: Option<usize> = None;
    let mut remove: Option<usize> = None;
    let n = g.ops.len();
    let op_ids: Vec<_> = g.ops.iter().map(|op| op.id).collect();
    for (i, op) in g.ops.iter_mut().enumerate() {
        let is_selected = selected_ops.contains(&op.id);
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
                let modifiers = ui.input(|input| input.modifiers);
                select_grade_ops(
                    &op_ids,
                    &mut selected_ops,
                    selected_op,
                    op.id,
                    modifiers.command,
                    modifiers.shift,
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!graph_active, egui::Button::new(ph::X).small())
                    .on_hover_text("Remove")
                    .clicked()
                {
                    remove = Some(i);
                }
                if ui
                    .add_enabled(
                        !graph_active && i + 1 < n,
                        egui::Button::new(ph::CARET_DOWN).small(),
                    )
                    .on_hover_text("Move down")
                    .clicked()
                {
                    move_down = Some(i);
                }
                if ui
                    .add_enabled(
                        !graph_active && i > 0,
                        egui::Button::new(ph::CARET_UP).small(),
                    )
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
        selected_ops.retain(|id| *id != removed.id);
        if *selected_op == Some(removed.id) {
            *selected_op = selected_ops.last().copied();
        }
    }

    if selected_ops.len() > 1 {
        ui.horizontal(|ui| {
            ui.label(format!("{} selected", selected_ops.len()));
            if ui.button("Enable").clicked() {
                set_selected_grade_ops_enabled(&mut g, &selected_ops, true);
            }
            if ui.button("Bypass").clicked() {
                set_selected_grade_ops_enabled(&mut g, &selected_ops, false);
            }
            if ui
                .add_enabled(!graph_active, egui::Button::new("Remove selected"))
                .on_disabled_hover_text("Remove graph correctors from the graph editor")
                .clicked()
            {
                remove_selected_grade_ops(&mut g, &selected_ops);
                selected_ops.clear();
                *selected_op = None;
            }
        });
    }
    if graph_active {
        let before_graph_selection = *selected_op;
        topology_changed |= draw_grade_graph_editor(
            ui,
            &mut g,
            &luts,
            selected_op,
            egui::Id::new(("grade_graph_canvas", owner, shared_id)),
        );
        if *selected_op != before_graph_selection {
            selected_ops = (*selected_op).into_iter().collect();
        }
    }

    let mut discrete = topology_changed || g != quick_grade || g.bypass != orig.bypass;

    // ── Selected-op editor ───────────────────────────────────────────────────
    ui.separator();
    match selected_op
        .filter(|_| selected_ops.len() <= 1)
        .and_then(|sel| g.ops.iter().position(|o| o.id == sel))
    {
        Some(idx) => {
            let op = g.ops[idx].id;
            let input_id = ui.id().with(("grade_input_node", op));
            let mut graph_node = ui
                .data(|data| data.get_temp::<Option<u32>>(input_id).flatten())
                .filter(|node| g.has_grade_input(op, Some(*node)));
            let previous_input = graph_node;
            if native && graph_active {
                let graph = g.graph.as_ref().unwrap();
                egui::ComboBox::from_id_salt(input_id)
                    .selected_text(
                        graph_node
                            .map(|node| format!("Input · {}", graph_node_label(graph, &g, node)))
                            .unwrap_or_else(|| {
                                if g.has_unambiguous_corrector_input(op) {
                                    "Image corrector input".into()
                                } else {
                                    "Choose graph input".into()
                                }
                            }),
                    )
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut graph_node,
                            None,
                            "Image corrector input (automatic)",
                        );
                        for &node in graph.nodes.keys() {
                            if g.has_grade_input(op, Some(node)) {
                                ui.selectable_value(
                                    &mut graph_node,
                                    Some(node),
                                    graph_node_label(graph, &g, node),
                                );
                            }
                        }
                    });
                ui.label(
                    RichText::new("Picking and matte preview use the selected node's input.")
                        .small()
                        .color(muted(ui)),
                );
            }
            if graph_node != previous_input {
                actions.push(PanelAction::GradeInputSelectionChanged {
                    seq,
                    track,
                    clip,
                    op,
                    graph_node,
                });
            }
            ui.data_mut(|data| data.insert_temp(input_id, graph_node));
            let qualified_input = !graph_active || (native && g.has_grade_input(op, graph_node));
            discrete |= draw_op_editor(
                ui,
                &mut g.ops[idx],
                &luts,
                actions,
                seq,
                track,
                clip,
                matches!(owner, VfxOwner::Clip(_)) && shared_id.is_none(),
                qualified_input,
                graph_node,
                native,
            );
        }
        None => {
            if selected_ops.len() > 1 {
                ui.label(
                    "Use Enable or Bypass above, or select one corrector to edit its controls.",
                );
            } else if graph_active {
                ui.label(
                    RichText::new("Select an existing graph corrector to edit it.")
                        .color(muted(ui)),
                );
            } else {
                let mut draft = default_op(tab_kind(*tab), &luts);
                let identity = draft.clone();
                let action_count = actions.len();
                discrete |= draw_op_editor(
                    ui,
                    &mut draft,
                    &luts,
                    actions,
                    seq,
                    track,
                    clip,
                    matches!(owner, VfxOwner::Clip(_)) && shared_id.is_none(),
                    !graph_active,
                    None,
                    native,
                );
                if draft != identity || actions.len() != action_count {
                    discrete |= actions.len() != action_count;
                    *selected_op = Some(draft.id);
                    selected_ops = vec![draft.id];
                    g.ops.push(draft);
                }
            }
        }
    }
    ui.data_mut(|data| data.insert_temp(multi_id, selected_ops));

    // ── Commit one SetGrade if anything changed this frame ───────────────────
    if g != orig {
        if let Some(id) = shared_id {
            commit_shared_look_grade(doc, history, id, g, discrete);
        } else {
            commit_grade_scoped(doc, history, owner, g, discrete);
        }
    }
}

fn graph_node_label(graph: &GradeGraph, grade: &Grade, id: u32) -> String {
    match graph.nodes.get(&id) {
        Some(GradeGraphNode::Input) => format!("{id} · Grade input"),
        Some(GradeGraphNode::Corrector { op, label, .. }) => {
            let kind = grade
                .ops
                .iter()
                .find(|candidate| candidate.id == *op)
                .map(|candidate| kind_label(candidate.kind))
                .unwrap_or("Missing corrector");
            if label.is_empty() {
                format!("{id} · {kind}")
            } else {
                format!("{id} · {label}")
            }
        }
        Some(GradeGraphNode::LayerMixer { label, .. }) => {
            if label.is_empty() {
                format!("{id} · Layer mixer")
            } else {
                format!("{id} · {label}")
            }
        }
        Some(GradeGraphNode::QualifierMatte { label, .. }) => format!(
            "{id} · {}",
            if label.is_empty() {
                "Qualifier key"
            } else {
                label
            }
        ),
        Some(GradeGraphNode::MatteRefine { label, .. }) => format!(
            "{id} · {}",
            if label.is_empty() {
                "Refine matte"
            } else {
                label
            }
        ),
        Some(GradeGraphNode::KeyMixer { label, .. }) => format!(
            "{id} · {}",
            if label.is_empty() { "Key mixer" } else { label }
        ),
        Some(GradeGraphNode::MatteApply { label, .. }) => format!(
            "{id} · {}",
            if label.is_empty() {
                "Apply matte"
            } else {
                label
            }
        ),
        Some(GradeGraphNode::Output { .. }) => format!("{id} · Grade output"),
        None => format!("{id} · Missing node"),
    }
}

fn graph_input_picker(
    ui: &mut Ui,
    grade: &Grade,
    graph: &GradeGraph,
    target: u32,
    port: &'static str,
    selected: &mut u32,
) -> bool {
    let before = *selected;
    let expected = graph
        .nodes
        .get(&target)
        .and_then(|node| {
            node.input_ports()
                .into_iter()
                .flatten()
                .find(|input| input.label == port)
        })
        .map(|input| input.kind);
    egui::ComboBox::from_id_salt(("grade_graph_port", target, port))
        .selected_text(graph_node_label(graph, grade, *selected))
        .show_ui(ui, |ui| {
            for (&id, node) in &graph.nodes {
                if id != target
                    && !matches!(node, GradeGraphNode::Output { .. })
                    && expected.is_some_and(|kind| node.output_type() == kind)
                {
                    ui.selectable_value(selected, id, graph_node_label(graph, grade, id));
                }
            }
        });
    before != *selected
}

/// Place each image node to the right of its deepest input. Parallel branches
/// share a column; disconnected alternatives remain visible for reuse.
fn graph_canvas_layers(graph: &GradeGraph) -> std::collections::BTreeMap<u32, (usize, usize)> {
    fn depth(
        graph: &GradeGraph,
        id: u32,
        cache: &mut std::collections::HashMap<u32, usize>,
        visiting: &mut std::collections::HashSet<u32>,
    ) -> usize {
        if let Some(&cached) = cache.get(&id) {
            return cached;
        }
        if !visiting.insert(id) {
            return 0; // malformed loaded graph: keep the canvas bounded
        }
        let inputs: Vec<u32> = graph
            .nodes
            .get(&id)
            .map(|node| {
                node.input_ports()
                    .into_iter()
                    .flatten()
                    .map(|port| port.node)
                    .collect()
            })
            .unwrap_or_default();
        let layer = if inputs.is_empty() {
            0
        } else {
            1 + inputs
                .into_iter()
                .map(|input| depth(graph, input, cache, visiting))
                .max()
                .unwrap_or(0)
        };
        visiting.remove(&id);
        cache.insert(id, layer.min(graph.nodes.len()));
        layer.min(graph.nodes.len())
    }

    let mut cache = std::collections::HashMap::new();
    let mut rows = std::collections::HashMap::new();
    graph
        .nodes
        .keys()
        .map(|&id| {
            let layer = depth(graph, id, &mut cache, &mut std::collections::HashSet::new());
            let row = rows.entry(layer).or_insert(0);
            let placed = (layer, *row);
            *row += 1;
            (id, placed)
        })
        .collect()
}

fn graph_image_inputs(node: &GradeGraphNode) -> Vec<(&'static str, u32, f32)> {
    let ports: Vec<_> = node.input_ports().into_iter().flatten().collect();
    let count = ports.len();
    ports
        .into_iter()
        .enumerate()
        .map(|(index, port)| {
            (
                port.label,
                port.node,
                (index + 1) as f32 / (count + 1) as f32,
            )
        })
        .collect()
}

fn set_graph_image_input(graph: &mut GradeGraph, target: u32, port: &str, source: u32) -> bool {
    let Some(source_kind) = graph.nodes.get(&source).map(GradeGraphNode::output_type) else {
        return false;
    };
    let Some(node) = graph.nodes.get_mut(&target) else {
        return false;
    };
    let Some(input) = node
        .input_ports()
        .into_iter()
        .flatten()
        .find(|input| input.label == port)
    else {
        return false;
    };
    if input.node == source || input.kind != source_kind {
        return false;
    }
    node.set_input(port, source).is_ok()
}

/// Click an output dot, then an input dot to route an image connection. The
/// caller validates the candidate graph before committing it to history.
fn draw_grade_graph_canvas(
    ui: &mut Ui,
    graph: &GradeGraph,
    grade: &Grade,
    candidate: &mut GradeGraph,
    selected_op: &mut Option<GradeOpId>,
    canvas_id: egui::Id,
) -> bool {
    let layers = graph_canvas_layers(graph);
    let columns = layers.values().map(|(layer, _)| *layer).max().unwrap_or(0) + 1;
    let rows = layers.values().map(|(_, row)| *row).max().unwrap_or(0) + 1;
    let canvas_size = vec2(
        (columns as f32 * 180.0 + 24.0).max(320.0),
        (rows as f32 * 86.0 + 24.0).max(150.0),
    );
    let pending_id = canvas_id.with("pending_output");
    let mut pending: Option<u32> =
        ui.data(|data| data.get_temp::<Option<u32>>(pending_id).flatten());
    if pending.is_some_and(|id| !graph.nodes.contains_key(&id) || id == graph.output) {
        pending = None;
    }
    let mut changed = false;
    egui::ScrollArea::both().max_height(310.0).show(ui, |ui| {
        let (canvas, _) = ui.allocate_exact_size(canvas_size, Sense::hover());
        let painter = ui.painter_at(canvas);
        painter.rect_filled(canvas, 5.0, surface_widget(ui));
        let rects: std::collections::BTreeMap<u32, Rect> = layers
            .iter()
            .map(|(&id, &(layer, row))| {
                (
                    id,
                    Rect::from_min_size(
                        canvas.min + vec2(20.0 + layer as f32 * 180.0, 20.0 + row as f32 * 86.0),
                        vec2(146.0, 58.0),
                    ),
                )
            })
            .collect();
        for (&id, node) in &graph.nodes {
            let Some(&target) = rects.get(&id) else {
                continue;
            };
            for (_, source_id, fraction) in graph_image_inputs(node) {
                let Some(&source) = rects.get(&source_id) else {
                    continue;
                };
                let start = pos2(source.right(), source.center().y);
                let end = pos2(target.left(), target.top() + target.height() * fraction);
                let mid = (start.x + end.x) * 0.5;
                painter.line_segment([start, pos2(mid, start.y)], Stroke::new(1.5, muted(ui)));
                painter.line_segment(
                    [pos2(mid, start.y), pos2(mid, end.y)],
                    Stroke::new(1.5, muted(ui)),
                );
                painter.line_segment([pos2(mid, end.y), end], Stroke::new(1.5, muted(ui)));
            }
        }
        for (&id, node) in &graph.nodes {
            let Some(&card) = rects.get(&id) else {
                continue;
            };
            let selected = match node {
                GradeGraphNode::Corrector { op, .. } => *selected_op == Some(*op),
                _ => false,
            };
            painter.rect_filled(card, 5.0, ui.visuals().widgets.inactive.bg_fill);
            painter.rect_stroke(
                card,
                5.0,
                Stroke::new(
                    if selected { 2.0 } else { 1.0 },
                    if selected { accent(ui) } else { border(ui) },
                ),
            );
            let label = graph_node_label(graph, grade, id);
            painter.text(
                card.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(12.0),
                on_surface(ui),
            );
            if let GradeGraphNode::Corrector { op, .. }
            | GradeGraphNode::QualifierMatte { op, .. } = node
            {
                if ui
                    .interact(
                        card.shrink2(vec2(12.0, 0.0)),
                        canvas_id.with(("node", id)),
                        Sense::click(),
                    )
                    .clicked()
                {
                    *selected_op = Some(*op);
                }
            }
            if !matches!(node, GradeGraphNode::Output { .. }) {
                let center = pos2(card.right(), card.center().y);
                let hit = Rect::from_center_size(center, vec2(17.0, 17.0));
                if ui
                    .interact(hit, canvas_id.with(("out", id)), Sense::click())
                    .on_hover_text("Choose this output as the routing source")
                    .clicked()
                {
                    pending = Some(id);
                }
                painter.circle_filled(
                    center,
                    5.0,
                    if pending == Some(id) {
                        accent(ui)
                    } else if node.output_type()
                        == photonic_core::timeline::GradeGraphPortType::Matte
                    {
                        egui::Color32::from_rgb(74, 185, 165)
                    } else {
                        on_surface(ui)
                    },
                );
            }
            for (port, _, fraction) in graph_image_inputs(node) {
                let center = pos2(card.left(), card.top() + card.height() * fraction);
                let hit = Rect::from_center_size(center, vec2(17.0, 17.0));
                if ui
                    .interact(hit, canvas_id.with(("in", id, port)), Sense::click())
                    .on_hover_text(format!("Route {port} input from the selected output"))
                    .clicked()
                {
                    if let Some(source) = pending.take() {
                        changed |= set_graph_image_input(candidate, id, port, source);
                    }
                }
                let matte_port = node.input_ports().into_iter().flatten().any(|input| {
                    input.label == port
                        && input.kind == photonic_core::timeline::GradeGraphPortType::Matte
                });
                painter.circle_filled(
                    center,
                    5.0,
                    if matte_port {
                        egui::Color32::from_rgb(74, 185, 165)
                    } else {
                        on_surface(ui)
                    },
                );
            }
        }
    });
    if pending.is_some() {
        ui.label(
            RichText::new("Choose a compatible input dot to complete the connection")
                .small()
                .color(muted(ui)),
        );
    }
    ui.data_mut(|data| data.insert_temp(pending_id, pending));
    changed
}

/// Edit image connections and mixer strength in the same whole-grade history
/// path used by every other Color control. Candidate graphs are validated
/// before assignment, so a cycle or dangling port never enters the document.
fn draw_grade_graph_editor(
    ui: &mut Ui,
    grade: &mut Grade,
    luts: &[(AssetId, String)],
    selected_op: &mut Option<GradeOpId>,
    canvas_id: egui::Id,
) -> bool {
    section_header(ui, "GRADE GRAPH");
    let mut structural = false;
    ui.horizontal(|ui| {
        for (parallel, title) in [(false, "Add serial node"), (true, "Add parallel node")] {
            ui.menu_button(title, |ui| {
                for kind in ALL_KINDS {
                    if ui.button(kind_label(kind)).clicked() {
                        let op = default_op(kind, luts);
                        if grade.add_graph_corrector(op.clone(), parallel).is_ok() {
                            *selected_op = Some(op.id);
                            structural = true;
                        }
                        ui.close_menu();
                    }
                }
            });
        }
    });
    ui.menu_button("Add key utility", |ui| {
        if let Some(graph) = grade.graph.clone() {
            for (&id, node) in &graph.nodes {
                if let GradeGraphNode::Corrector { input, op, .. } = node {
                    if grade.ops.iter().any(|candidate| {
                        candidate.id == *op && candidate.kind == GradeOpKind::HslQualifier
                    }) && ui
                        .button(format!("Key from {}", graph_node_label(&graph, grade, id)))
                        .clicked()
                    {
                        structural |= grade
                            .add_graph_utility(GradeGraphNode::QualifierMatte {
                                input: *input,
                                op: *op,
                                label: String::new(),
                            })
                            .is_ok();
                        ui.close_menu();
                    }
                }
            }
            let matte = graph
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.output_type() == photonic_core::timeline::GradeGraphPortType::Matte
                })
                .map(|(&id, _)| id);
            if let Some(matte) = matte {
                if ui.button("Refine matte").clicked() {
                    structural |= grade
                        .add_graph_utility(GradeGraphNode::MatteRefine {
                            input: matte,
                            refinement: Default::default(),
                            label: String::new(),
                        })
                        .is_ok();
                    ui.close_menu();
                }
                if ui.button("Key mixer").clicked() {
                    structural |= grade
                        .add_graph_utility(GradeGraphNode::KeyMixer {
                            top: matte,
                            bottom: matte,
                            mode: photonic_core::timeline::GradeKeyMixMode::Union,
                            label: String::new(),
                        })
                        .is_ok();
                    ui.close_menu();
                }
                if ui.button("Apply matte to output").clicked() {
                    let original = graph
                        .nodes
                        .iter()
                        .find(|(_, node)| matches!(node, GradeGraphNode::Input))
                        .map(|(&id, _)| id);
                    if let (Some(original), Some(GradeGraphNode::Output { input: corrected })) =
                        (original, graph.nodes.get(&graph.output))
                    {
                        structural |= grade
                            .add_graph_utility(GradeGraphNode::MatteApply {
                                original,
                                corrected: *corrected,
                                matte,
                                label: String::new(),
                            })
                            .is_ok();
                    }
                    ui.close_menu();
                }
            } else {
                ui.label("Add a qualifier key before mixing or applying mattes.");
            }
        }
    });
    let Some(snapshot) = grade.graph.clone() else {
        return structural;
    };
    let mut candidate = snapshot.clone();
    let expanded_id = canvas_id.with("expanded_graph");
    let mut expanded = ui
        .data(|data| data.get_temp::<bool>(expanded_id))
        .unwrap_or(false);
    if ui
        .small_button("Expand graph")
        .on_hover_text("Open a wider graph canvas for routing image and matte branches")
        .clicked()
    {
        expanded = true;
    }
    if expanded {
        let columns = graph_canvas_layers(&snapshot)
            .values()
            .map(|(column, _)| *column)
            .max()
            .unwrap_or(0)
            + 1;
        let width = (columns as f32 * 180.0 + 48.0)
            .max(980.0)
            .min((ui.ctx().screen_rect().width() - 96.0).max(320.0));
        egui::Window::new("Grade graph").id(expanded_id).default_size(vec2(width,430.0)).resizable(true).open(&mut expanded).show(ui.ctx(),|ui| {
            ui.label("Image ports are white. Matte ports are green. Choose an output, then a compatible input.");
            structural |= draw_grade_graph_canvas(ui,&snapshot,grade,&mut candidate,selected_op,canvas_id.with("expanded_canvas"));
        });
    }
    ui.data_mut(|data| data.insert_temp(expanded_id, expanded));
    structural |=
        draw_grade_graph_canvas(ui, &snapshot, grade, &mut candidate, selected_op, canvas_id);
    let mut remove_node = None;
    for (&id, node) in &snapshot.nodes {
        let mut edited = node.clone();
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(graph_node_label(&snapshot, grade, id));
                if let GradeGraphNode::Corrector { op, .. }
                | GradeGraphNode::QualifierMatte { op, .. } = node
                {
                    if ui.small_button("Edit").clicked() {
                        *selected_op = Some(*op);
                    }
                }
                if matches!(
                    node,
                    GradeGraphNode::Corrector { .. }
                        | GradeGraphNode::LayerMixer { .. }
                        | GradeGraphNode::QualifierMatte { .. }
                        | GradeGraphNode::KeyMixer { .. }
                        | GradeGraphNode::MatteApply { .. }
                        | GradeGraphNode::MatteRefine { .. }
                ) && ui
                    .small_button("Remove")
                    .on_hover_text(
                        "Remove this corrector, or remove this mixer and keep its bottom branch",
                    )
                    .clicked()
                {
                    remove_node = Some(id);
                }
            });
            match &mut edited {
                GradeGraphNode::Input => {}
                GradeGraphNode::Corrector { input, label, .. }
                | GradeGraphNode::QualifierMatte { input, label, .. } => {
                    ui.horizontal(|ui| {
                        ui.label("Image in");
                        structural |= graph_input_picker(ui, grade, &snapshot, id, "image", input);
                    });
                    ui.add(egui::TextEdit::singleline(label).hint_text("Node label"));
                }
                GradeGraphNode::LayerMixer {
                    top,
                    bottom,
                    opacity,
                    label,
                } => {
                    ui.horizontal(|ui| {
                        ui.label("Top");
                        structural |= graph_input_picker(ui, grade, &snapshot, id, "top", top);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Bottom");
                        structural |=
                            graph_input_picker(ui, grade, &snapshot, id, "bottom", bottom);
                    });
                    ui.add(egui::Slider::new(opacity, 0.0..=1.0).text("Top strength"));
                    ui.add(egui::TextEdit::singleline(label).hint_text("Mixer label"));
                }
                GradeGraphNode::MatteRefine { input, refinement, label } => {
                    ui.horizontal(|ui| { ui.label("Matte in"); structural |= graph_input_picker(ui,grade,&snapshot,id,"matte",input); });
                    ui.checkbox(&mut refinement.denoise,"Remove isolated pixels").on_hover_text("3×3 median at the current processing resolution. Full-quality export uses final resolution.");
                    let mut grow=refinement.grow*100.0;
                    if ui.add(egui::Slider::new(&mut grow,-2.0..=2.0).text("Grow / shrink (%)")).on_hover_text("Fraction of the frame's shorter dimension. Positive expands and negative shrinks using a square neighborhood.").changed() { refinement.grow=grow/100.0; }
                    let mut blur=refinement.blur*100.0;
                    if ui.add(egui::Slider::new(&mut blur,0.0..=2.0).text("Blur (%)")).on_hover_text("Gaussian sigma as a percentage of the frame's shorter dimension.").changed() { refinement.blur=blur/100.0; }
                    let white=refinement.matte_levels[1];
                    ui.add(egui::Slider::new(&mut refinement.matte_levels[0],0.0..=(1.0-white-0.001).max(0.0)).text("Clean black"));
                    let black=refinement.matte_levels[0];
                    ui.add(egui::Slider::new(&mut refinement.matte_levels[1],0.0..=(1.0-black-0.001).max(0.0)).text("Clean white"));
                    ui.add(egui::TextEdit::singleline(label).hint_text("Refinement label"));
                }
                GradeGraphNode::KeyMixer {
                    top,
                    bottom,
                    mode,
                    label,
                } => {
                    ui.horizontal(|ui| {
                        ui.label("First key");
                        structural |= graph_input_picker(ui, grade, &snapshot, id, "top", top);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Second key");
                        structural |=
                            graph_input_picker(ui, grade, &snapshot, id, "bottom", bottom);
                    });
                    egui::ComboBox::from_id_salt(("key_mix_mode", id))
                        .selected_text(format!("{mode:?}"))
                        .show_ui(ui, |ui| {
                            for value in [
                                photonic_core::timeline::GradeKeyMixMode::Union,
                                photonic_core::timeline::GradeKeyMixMode::Intersect,
                                photonic_core::timeline::GradeKeyMixMode::Subtract,
                                photonic_core::timeline::GradeKeyMixMode::Multiply,
                            ] {
                                ui.selectable_value(mode, value, format!("{value:?}"));
                            }
                        });
                    ui.add(egui::TextEdit::singleline(label).hint_text("Key mixer label"));
                }
                GradeGraphNode::MatteApply {
                    original,
                    corrected,
                    matte,
                    label,
                } => {
                    for (port, input) in [
                        ("original", original),
                        ("corrected", corrected),
                        ("matte", matte),
                    ] {
                        ui.horizontal(|ui| {
                            ui.label(port);
                            structural |= graph_input_picker(ui, grade, &snapshot, id, port, input);
                        });
                    }
                    ui.add(egui::TextEdit::singleline(label).hint_text("Masked correction label"));
                }
                GradeGraphNode::Output { input } => {
                    ui.horizontal(|ui| {
                        ui.label("Image in");
                        structural |= graph_input_picker(ui, grade, &snapshot, id, "image", input);
                    });
                }
            }
        });
        if edited != *node {
            candidate.nodes.insert(id, edited);
        }
    }
    if candidate != snapshot {
        match candidate.validate(&grade.ops) {
            Ok(()) => grade.graph = Some(candidate),
            Err(error) => {
                ui.colored_label(ui.visuals().warn_fg_color, error);
                structural = false;
            }
        }
    }
    if let Some(id) = remove_node {
        match grade.remove_graph_node(id) {
            Ok(()) => {
                if selected_op
                    .is_some_and(|op| !grade.ops.iter().any(|candidate| candidate.id == op))
                {
                    *selected_op = None;
                }
                structural = true;
            }
            Err(error) => {
                ui.colored_label(ui.visuals().warn_fg_color, error);
            }
        }
    }
    structural
}

/// Project looks run after the ordinary clip grade. Keeping that stage separate
/// lets a shot detach from a shared look without changing its current output.
fn draw_shared_looks(
    ui: &mut Ui,
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    clip: ClipId,
    selected_op: &mut Option<GradeOpId>,
) -> bool {
    enum Action {
        Create(String),
        Link(photonic_core::timeline::SharedLookId),
        Detach,
        Update(photonic_core::timeline::SharedLookId),
        Remove(photonic_core::timeline::SharedLookId),
    }
    let Some(project) = doc.timeline.as_ref() else {
        return false;
    };
    let Some(current) = project
        .sequences
        .get(&seq)
        .and_then(|s| s.track(track))
        .and_then(|t| t.clips.iter().find(|c| c.id == clip))
    else {
        return false;
    };
    let mut looks: Vec<_> = project
        .shared_looks
        .values()
        .map(|look| (look.id, look.name.clone()))
        .collect();
    looks.sort_by(|a, b| a.1.cmp(&b.1));
    let selected_id = ui.id().with(("selected_shared_look", clip));
    let mut selected = ui
        .data(|d| d.get_temp::<photonic_core::timeline::SharedLookId>(selected_id))
        .or_else(|| looks.first().map(|look| look.0));
    let name_id = ui.id().with(("new_shared_look_name", clip));
    let mut name = ui
        .data(|d| d.get_temp::<String>(name_id))
        .unwrap_or_else(|| "Shared look".into());
    let linked = match current.look.as_ref() {
        Some(ClipLook::Shared(id)) => Some(*id),
        _ => None,
    };
    let mut action = None;
    section_header(ui, "SHARED LOOK");
    ui.horizontal_wrapped(|ui| {
        ui.label(match linked {
            Some(id) => project
                .shared_looks
                .get(&id)
                .map_or("Missing linked look", |look| look.name.as_str()),
            None if matches!(current.look, Some(ClipLook::Local(_))) => "Independent look",
            None => "No linked look",
        });
        egui::ComboBox::from_id_salt(("shared_look_picker", clip))
            .selected_text(
                selected
                    .and_then(|id| {
                        looks
                            .iter()
                            .find(|look| look.0 == id)
                            .map(|look| look.1.as_str())
                    })
                    .unwrap_or("Choose look"),
            )
            .show_ui(ui, |ui| {
                for (id, label) in &looks {
                    ui.selectable_value(&mut selected, Some(*id), label);
                }
            });
        if let Some(id) = selected {
            if ui
                .add_enabled(linked != Some(id), egui::Button::new("Link to shot"))
                .clicked()
            {
                action = Some(Action::Link(id));
            }
        }
        if linked.is_some() && ui.button("Make independent").clicked() {
            action = Some(Action::Detach);
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut name)
                .desired_width(120.0)
                .hint_text("Look name"),
        );
        if ui.button("Create empty").clicked() {
            action = Some(Action::Create(name.clone()));
        }
        if let Some(id) = linked {
            if ui.button("Replace look from clip grade").clicked() {
                action = Some(Action::Update(id));
            }
        }
        if let Some(id) = selected {
            let in_use = project
                .sequences
                .values()
                .flat_map(|sequence| sequence.tracks())
                .flat_map(|track| &track.clips)
                .any(|candidate| candidate.look == Some(ClipLook::Shared(id)));
            if ui
                .add_enabled(!in_use, egui::Button::new("Delete unused look"))
                .clicked()
            {
                action = Some(Action::Remove(id));
            }
        }
    });
    if let Some(id) = linked {
        let affected = project
            .sequences
            .values()
            .flat_map(|sequence| sequence.tracks())
            .flat_map(|track| &track.clips)
            .filter(|candidate| candidate.look == Some(ClipLook::Shared(id)))
            .count();
        ui.label(format!(
            "Editing this look affects {affected} linked shot(s)."
        ));
        let key = shared_look_editor_id(clip);
        let mut editing = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(false);
        if ui
            .checkbox(&mut editing, "Edit shared look correctors")
            .changed()
        {
            *selected_op = None;
        }
        ui.data_mut(|d| d.insert_temp(key, editing));
    }
    ui.data_mut(|d| {
        if let Some(id) = selected {
            d.insert_temp(selected_id, id);
        }
        d.insert_temp(name_id, name);
    });
    let Some(action) = action else { return false };
    let result: Result<Command, photonic_core::timeline::EditError> = match action {
        Action::Create(name) => {
            ops::create_shared_look(project, &name, Grade::default()).map(Command::Timeline)
        }
        Action::Link(id) => {
            ops::link_shared_look(project, seq, track, clip, id).map(Command::Timeline)
        }
        Action::Detach => {
            ops::make_shared_look_independent(project, seq, track, clip).map(Command::Timeline)
        }
        Action::Remove(id) => ops::remove_shared_look(project, id).map(Command::Timeline),
        Action::Update(id) => (|| {
            let Some(grade) = current.grade.clone() else {
                return Err(photonic_core::timeline::EditError::InvalidSharedLook(
                    "add a clip grade first".into(),
                ));
            };
            let existing = project
                .shared_looks
                .get(&id)
                .ok_or(photonic_core::timeline::EditError::NoSharedLook(id))?;
            let look_cmd = ops::update_shared_look(project, id, &existing.name, grade)?;
            let mut replacement = current.clone();
            replacement.grade = None;
            replacement.active_grade_version = None;
            let clip_cmd = ops::set_clip_prop(project, seq, track, replacement)?;
            Ok(Command::Batch(vec![
                Command::Timeline(look_cmd),
                Command::Timeline(clip_cmd),
            ]))
        })(),
    };
    match result {
        Ok(command) => {
            history.execute_discrete(command, doc);
            true
        }
        Err(error) => {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("Shared look edit rejected: {error}"),
            );
            false
        }
    }
}

/// Named clip looks are structural edits and therefore stay separate from a
/// continuous wheel or curve gesture in history.
fn draw_grade_versions(
    ui: &mut Ui,
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    clip: ClipId,
) -> bool {
    enum Action {
        Add(String),
        Activate(uuid::Uuid),
        Rename(uuid::Uuid, String),
        Remove(uuid::Uuid),
    }
    let Some(current) = doc
        .timeline
        .as_ref()
        .and_then(|p| p.sequences.get(&seq))
        .and_then(|s| s.track(track))
        .and_then(|t| t.clips.iter().find(|c| c.id == clip))
    else {
        return false;
    };
    let versions: Vec<_> = current
        .grade_versions
        .iter()
        .map(|v| (v.id, v.name.clone()))
        .collect();
    let active = current.active_grade_version;
    let active_label = versions
        .iter()
        .find(|(id, _)| Some(*id) == active)
        .map(|(_, name)| name.as_str())
        .unwrap_or("Unsaved grade");
    let name_id = ui.id().with(("grade_version_name", clip));
    let mut name = ui
        .data(|d| d.get_temp::<String>(name_id))
        .unwrap_or_else(|| format!("Version {}", versions.len() + 1));
    let mut action = None;
    section_header(ui, "GRADE VERSIONS");
    ui.horizontal_wrapped(|ui| {
        egui::ComboBox::from_id_salt(("grade_versions", clip))
            .selected_text(active_label)
            .show_ui(ui, |ui| {
                for (id, label) in &versions {
                    if ui.selectable_label(Some(*id) == active, label).clicked()
                        && Some(*id) != active
                    {
                        action = Some(Action::Activate(*id));
                    }
                }
            });
        ui.add(
            egui::TextEdit::singleline(&mut name)
                .desired_width(110.0)
                .hint_text("Look name"),
        );
        if ui.button("Save new").clicked() {
            action = Some(Action::Add(name.clone()));
        }
        if let Some(id) = active {
            if ui.button("Rename").clicked() {
                action = Some(Action::Rename(id, name.clone()));
            }
            if ui.button("Delete").clicked() {
                action = Some(Action::Remove(id));
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(name_id, name));
    let Some(action) = action else {
        return false;
    };
    let Some(project) = doc.timeline.as_ref() else {
        return false;
    };
    let command = match &action {
        Action::Add(name) => ops::add_grade_version(project, seq, track, clip, name),
        Action::Activate(id) => ops::activate_grade_version(project, seq, track, clip, *id),
        Action::Rename(id, name) => ops::rename_grade_version(project, seq, track, clip, *id, name),
        Action::Remove(id) => ops::remove_grade_version(project, seq, track, clip, *id),
    };
    match command {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), doc);
            if matches!(action, Action::Add(_)) {
                ui.data_mut(|d| d.remove::<String>(name_id));
            }
            true
        }
        Err(error) => {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("Version edit rejected: {error}"),
            );
            false
        }
    }
}

/// Copy from another shot in the sequence through the same validated core op
/// used by MCP. Source selection is transient UI state; only pressing an edit
/// button creates a history entry.
fn draw_grade_copy(
    ui: &mut Ui,
    doc: &mut Document,
    history: &mut CommandHistory,
    seq: SequenceId,
    track: TrackId,
    clip: ClipId,
) -> bool {
    let Some(sequence) = doc.timeline.as_ref().and_then(|p| p.sequences.get(&seq)) else {
        return false;
    };
    let sources: Vec<_> = sequence
        .video_tracks
        .iter()
        .flat_map(|t| {
            t.clips.iter().filter(move |c| c.id != clip).map(move |c| {
                let label = if c.name.is_empty() {
                    format!("{} · {}", t.name, c.id)
                } else {
                    format!("{} · {}", t.name, c.name)
                };
                let ops = c
                    .grade
                    .as_ref()
                    .map(|g| {
                        g.ops
                            .iter()
                            .enumerate()
                            .map(|(i, op)| (op.id, format!("{} · {}", i + 1, kind_label(op.kind))))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                (t.id, c.id, label, ops)
            })
        })
        .collect();
    if sources.is_empty() {
        return false;
    }
    let source_id = ui.id().with(("copy_grade_source", clip));
    let mut selected = ui
        .data(|d| d.get_temp::<ClipId>(source_id))
        .filter(|id| sources.iter().any(|(_, source, _, _)| source == id))
        .unwrap_or(sources[0].1);
    let mut action: Option<(TrackId, ClipId, Option<GradeOpId>, bool)> = None;
    ui.collapsing("COPY FROM SHOT", |ui| {
        let label = sources
            .iter()
            .find(|(_, id, _, _)| *id == selected)
            .map(|(_, _, label, _)| label.as_str())
            .unwrap_or("Select shot");
        egui::ComboBox::from_id_salt(("copy_grade_from", clip))
            .selected_text(label)
            .show_ui(ui, |ui| {
                for (_, id, label, _) in &sources {
                    ui.selectable_value(&mut selected, *id, label);
                }
            });
        if let Some((source_track, source_clip, _, ops)) =
            sources.iter().find(|(_, id, _, _)| *id == selected)
        {
            if ui.button("Replace grade").clicked() {
                action = Some((*source_track, *source_clip, None, false));
            }
            for (op_id, label) in ops {
                if ui.button(format!("Append {label}")).clicked() {
                    action = Some((*source_track, *source_clip, Some(*op_id), true));
                }
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(source_id, selected));
    let Some((source_track, source_clip, op_id, append)) = action else {
        return false;
    };
    let Some(project) = doc.timeline.as_ref() else {
        return false;
    };
    let selected_ops = op_id.map(|id| [id]);
    match ops::copy_grade_correctors(
        project,
        (seq, source_track, source_clip),
        (seq, track, clip),
        selected_ops.as_ref().map(|ids| ids.as_slice()),
        append,
    ) {
        Ok(cmd) => {
            history.execute_discrete(Command::Timeline(cmd), doc);
            true
        }
        Err(error) => {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("Grade copy rejected: {error}"),
            );
            false
        }
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
    can_sample: bool,
    qualified_input: bool,
    graph_node: Option<u32>,
    native: bool,
) -> bool {
    section_header(ui, kind_label(op.kind));
    let op_id = op.id;
    let mut structural_change = false;
    match &mut op.params.base {
        GradeOpParams::Exposure { stops } => {
            labelled(ui, "Stops", |ui| {
                ui.add(
                    egui::Slider::new(stops, -6.0..=6.0)
                        .clamping(egui::SliderClamping::Edits)
                        .step_by(0.01),
                );
            });
        }
        GradeOpParams::LinearOffset { rgb } => {
            ui.label(
                RichText::new("Additive scene-linear RGB")
                    .small()
                    .color(muted(ui)),
            );
            for (label, channel) in ["Red", "Green", "Blue"].into_iter().zip(rgb.iter_mut()) {
                labelled(ui, label, |ui| {
                    ui.add(
                        egui::Slider::new(channel, -2.0..=2.0)
                            .clamping(egui::SliderClamping::Edits)
                            .step_by(0.001),
                    );
                });
            }
        }
        GradeOpParams::PrinterLights { points } => {
            ui.label(
                RichText::new("Scene-linear channel trim · 12 points = one stop")
                    .small()
                    .color(muted(ui)),
            );
            for (label, channel) in ["Red", "Green", "Blue"].into_iter().zip(points.iter_mut()) {
                labelled(ui, label, |ui| {
                    ui.add(
                        egui::Slider::new(channel, -48.0..=48.0)
                            .clamping(egui::SliderClamping::Edits)
                            .step_by(0.1),
                    );
                });
            }
        }
        GradeOpParams::HighlightRolloff { knee, strength } => {
            ui.label(RichText::new("Scene-linear highlight compression; hue ratios and negative detail are preserved.").small().color(muted(ui)));
            labelled(ui, "Knee", |ui| {
                ui.add(
                    egui::Slider::new(knee, 0.0..=8.0)
                        .clamping(egui::SliderClamping::Edits)
                        .step_by(0.01),
                );
            });
            labelled(ui, "Strength", |ui| {
                ui.add(
                    egui::Slider::new(strength, 0.0..=8.0)
                        .clamping(egui::SliderClamping::Edits)
                        .step_by(0.01),
                );
            });
        }
        GradeOpParams::SaturationVibrance {
            saturation,
            vibrance,
        } => {
            ui.label(
                RichText::new(
                    "Scene-linear colorfulness in the sequence working space; vibrance favors less saturated colors.",
                )
                .small()
                .color(muted(ui)),
            );
            labelled(ui, "Saturation", |ui| {
                ui.add(
                    egui::Slider::new(saturation, 0.0..=2.0)
                        .clamping(egui::SliderClamping::Edits)
                        .step_by(0.01),
                );
            });
            labelled(ui, "Vibrance", |ui| {
                ui.add(
                    egui::Slider::new(vibrance, -1.0..=1.0)
                        .clamping(egui::SliderClamping::Edits)
                        .step_by(0.01),
                );
            });
        }
        GradeOpParams::Contrast { pivot, amount } => {
            if native {
                ui.label(
                    RichText::new(
                        "Pivot is an ACEScct code value. +1 doubles slope; −1 halves it.",
                    )
                    .small()
                    .color(ui.visuals().weak_text_color()),
                );
            }
            labelled(ui, if native { "Log₂ slope" } else { "Amount" }, |ui| {
                ui.add(
                    egui::Slider::new(amount, if native { -4.0..=4.0 } else { -1.0..=1.0 })
                        .clamping(egui::SliderClamping::Edits)
                        .step_by(0.001),
                );
            });
            labelled(ui, "Pivot", |ui| {
                ui.add(
                    egui::Slider::new(pivot, 0.0..=1.0)
                        .clamping(egui::SliderClamping::Edits)
                        .step_by(0.001),
                );
            });
        }
        GradeOpParams::WhiteBalance { temp, tint } => {
            if native {
                ui.label("Normalized temperature/tint trim in scene-linear ACEScg. Neutral is zero; these controls do not represent Kelvin.");
            }
            labelled(ui, "Temp", |ui| {
                ui.add(
                    egui::Slider::new(temp, -1.0..=1.0)
                        .clamping(egui::SliderClamping::Edits)
                        .step_by(0.001),
                );
            });
            labelled(ui, "Tint", |ui| {
                ui.add(
                    egui::Slider::new(tint, -1.0..=1.0)
                        .clamping(egui::SliderClamping::Edits)
                        .step_by(0.001),
                );
            });
        }
        GradeOpParams::Cdl {
            slope,
            offset,
            power,
            sat,
        } => {
            if native {
                ui.label(RichText::new("ACEScct SOP/saturation, no clamp. Negative SOP passes through power; saturation uses CDL Rec.709 weights.").small().color(muted(ui)));
            }
            cdl_editor(ui, slope, offset, power, sat);
        }
        GradeOpParams::Wheels {
            lift,
            gamma,
            gain,
            sat,
        } => {
            if native {
                ui.label(RichText::new("Log wheels in ACEScct: slope = gain − lift, offset = lift, power = 1/gamma. Saturation follows CDL.").small().color(muted(ui)));
            }
            ui.horizontal(|ui| {
                wheel_dial(ui, "Lift", lift, 0.0);
                wheel_dial(ui, "Gamma", gamma, 1.0);
                wheel_dial(ui, "Gain", gain, 1.0);
            });
            labelled(ui, "Saturation", |ui| {
                ui.add(
                    egui::Slider::new(sat, 0.0..=2.0)
                        .clamping(egui::SliderClamping::Edits)
                        .step_by(0.001),
                );
            });
        }
        GradeOpParams::Curves {
            master,
            red,
            green,
            blue,
            hue_vs_hue,
            hue_vs_sat,
            hue_vs_luma,
            luma_vs_sat,
            sat_vs_sat,
        } => curves_editor(
            ui,
            [
                master,
                red,
                green,
                blue,
                hue_vs_hue,
                hue_vs_sat,
                hue_vs_luma,
                luma_vs_sat,
                sat_vs_sat,
            ],
            actions,
            seq,
            track,
            clip,
            op_id,
            graph_node,
            can_sample && (!native || qualified_input),
            native,
        ),
        GradeOpParams::HslQualifier {
            hue,
            sat,
            lum,
            softness,
            correction,
            keys,
            matte_levels,
        } => {
            structural_change |= qualifier_editor(
                ui,
                hue,
                sat,
                lum,
                softness,
                correction,
                keys,
                matte_levels,
                actions,
                seq,
                track,
                clip,
                op_id,
                graph_node,
                can_sample && (!native || qualified_input),
                qualified_input,
                native,
            )
        }
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

    structural_change | draw_window_mask_editor(ui, &mut op.mask)
}

fn default_window(shape: WindowShape) -> GradeMask {
    GradeMask::PowerWindow {
        shape,
        center: [0.5, 0.5],
        size: [0.25, 0.25],
        rotation: 0.0,
        softness: 0.1,
        invert: false,
    }
}

/// Returns whether a structural mask change needs a discrete history step.
/// Numeric edits flow through the caller's normal gesture-coalesced SetGrade.
fn draw_window_mask_editor(ui: &mut Ui, mask: &mut Option<GradeMask>) -> bool {
    ui.add_space(4.0);
    section_header(ui, "POWER WINDOW");
    if mask.is_none() {
        let mut shape = None;
        ui.horizontal(|ui| {
            if ui.button("Add ellipse").clicked() {
                shape = Some(WindowShape::Ellipse);
            }
            if ui.button("Add rectangle").clicked() {
                shape = Some(WindowShape::Rectangle);
            }
            if ui.button("Add gradient").clicked() {
                shape = Some(WindowShape::Gradient);
            }
        });
        if let Some(shape) = shape {
            *mask = Some(default_window(shape));
            return true;
        }
        ui.label(RichText::new("Full frame").small().color(muted(ui)));
        return false;
    }
    if ui.small_button("Clear mask").clicked() {
        *mask = None;
        return true;
    }
    let Some(GradeMask::PowerWindow {
        shape,
        center,
        size,
        rotation,
        softness,
        invert,
    }) = mask.as_mut()
    else {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            "Roto mask is unresolved here; this corrector is bypassed until resolved or disabled.",
        );
        return false;
    };
    let mut discrete = false;
    ui.horizontal(|ui| {
        discrete |= ui
            .selectable_value(shape, WindowShape::Ellipse, "Ellipse")
            .changed();
        discrete |= ui
            .selectable_value(shape, WindowShape::Rectangle, "Rectangle")
            .changed();
        discrete |= ui
            .selectable_value(shape, WindowShape::Gradient, "Gradient")
            .changed();
    });
    for (label, is_center, index) in [
        ("Center X", true, 0),
        ("Center Y", true, 1),
        ("Width", false, 0),
        ("Height", false, 1),
    ] {
        if *shape == WindowShape::Gradient && !is_center && index == 0 {
            continue;
        }
        let label = if *shape == WindowShape::Gradient && !is_center {
            "Falloff"
        } else {
            label
        };
        let value = if is_center {
            &mut center[index]
        } else {
            &mut size[index]
        };
        labelled(ui, label, |ui| {
            ui.add(
                egui::Slider::new(value, 0.0..=1.0)
                    .clamping(egui::SliderClamping::Edits)
                    .step_by(0.001),
            );
        });
    }
    let mut degrees = rotation.to_degrees();
    labelled(ui, "Rotation", |ui| {
        if ui
            .add(
                egui::Slider::new(&mut degrees, -180.0..=180.0)
                    .suffix("°")
                    .clamping(egui::SliderClamping::Edits)
                    .step_by(0.1),
            )
            .changed()
        {
            *rotation = degrees.to_radians();
        }
    });
    if *shape != WindowShape::Gradient {
        labelled(ui, "Feather", |ui| {
            ui.add(
                egui::Slider::new(softness, 0.0..=1.0)
                    .clamping(egui::SliderClamping::Edits)
                    .step_by(0.001),
            );
        });
    }
    discrete |= ui.checkbox(invert, "Invert window").changed();
    discrete
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
        ui.add(
            egui::Slider::new(sat, 0.0..=2.0)
                .clamping(egui::SliderClamping::Edits)
                .step_by(0.001),
        );
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
        ui.add(
            egui::Slider::new(intensity, 0.0..=1.0)
                .clamping(egui::SliderClamping::Edits)
                .step_by(0.01),
        );
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
    channels: [&mut Vec<(f32, f32)>; 9],
    actions: &mut Vec<PanelAction>,
    seq: SequenceId,
    track: TrackId,
    clip: ClipId,
    op: GradeOpId,
    graph_node: Option<u32>,
    can_sample: bool,
    native: bool,
) {
    let tab_id = ui.id().with("curve_ch");
    let mut ch: usize = ui.data(|d| d.get_temp::<usize>(tab_id).unwrap_or(0)).min(8);
    if native {
        ui.label(
            RichText::new(
                "ACEScct curves. Secondary curves use bounded AP1 log coordinates and preserve extended-channel residuals.",
            )
            .small()
            .color(muted(ui)),
        );
    }
    ui.horizontal_wrapped(|ui| {
        for (i, (name, help)) in [
            ("RGB", "Master RGB curve"),
            ("R", "Red channel"),
            ("G", "Green channel"),
            ("B", "Blue channel"),
            ("H-H", "Hue versus hue shift"),
            ("H-S", "Hue versus saturation"),
            ("H-L", "Hue versus Rec.709 luma offset"),
            ("L-S", "Rec.709 luma versus saturation"),
            ("S-S", "Saturation versus saturation"),
        ]
        .iter()
        .enumerate()
        {
            if ui
                .add(egui::SelectableLabel::new(ch == i, *name))
                .on_hover_text(if native && i == 6 {
                    "Hue versus AP1 log luma offset"
                } else if native && i == 7 {
                    "AP1 log luma versus saturation"
                } else {
                    *help
                })
                .clicked()
            {
                ch = i;
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(tab_id, ch));
    if ch >= 4 {
        ui.label(
            RichText::new("Midline is neutral; empty curves are disabled.")
                .small()
                .color(muted(ui)),
        );
    }

    let points = &mut *channels[ch];
    // Empty hue curves mean "disabled" in the renderer. Editing the display
    // directly would materialize a non-neutral curve merely by opening a tab.
    // Show neutral virtual points and persist them only after a real edit.
    let mut visible = if points.is_empty() {
        neutral_curve_points(ch)
    } else {
        points.clone()
    };
    let before = visible.clone();
    if !native {
        let sample_id = ui.id().with("curve_sample_color");
        let mut sample: [f32; 4] = ui.data(|d| {
            d.get_temp::<[f32; 4]>(sample_id)
                .unwrap_or([0.6, 0.4, 0.3, 1.0])
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Sample colour").small().color(muted(ui)));
            crate::color_popup::ColorPopup::swatch_f32(ui, &mut sample);
            if ui.add_enabled_ui(can_sample, eyedropper_btn).inner {
                actions.push(PanelAction::StartEyedropper(EyedropperTarget::GradeCurve {
                    graph_node,
                    seq,
                    track,
                    clip,
                    op,
                    channel: ch,
                }));
            }
            if ui
                .button("Add anchor")
                .on_hover_text(
                    "Place an anchor at this colour's channel, hue, luma, or saturation value",
                )
                .clicked()
            {
                add_curve_sample_anchor(&mut visible, ch, swatch_to_working(sample));
            }
        });
        ui.data_mut(|d| d.insert_temp(sample_id, sample));
    } else {
        ui.horizontal(|ui| {
            if ui.add_enabled_ui(can_sample, eyedropper_btn).inner {
                actions.push(PanelAction::StartEyedropper(EyedropperTarget::GradeCurve {
                    graph_node,
                    seq,
                    track,
                    clip,
                    op,
                    channel: ch,
                }));
            }
            ui.label(
                RichText::new("Sample input before this curve")
                    .small()
                    .color(muted(ui)),
            );
        });
    }

    curve_plot(ui, &mut visible, ch);
    if visible != before {
        *points = visible;
    }
}

pub(crate) fn add_curve_sample_anchor(
    points: &mut Vec<(f32, f32)>,
    channel: usize,
    rgb: [f32; 3],
) -> bool {
    add_curve_sample_anchor_in_domain(points, channel, rgb, false)
}

pub(crate) fn add_curve_sample_anchor_in_domain(
    points: &mut Vec<(f32, f32)>,
    channel: usize,
    rgb: [f32; 3],
    native: bool,
) -> bool {
    let (hue, saturation, _) = rgb_to_hsl(rgb[0], rgb[1], rgb[2]);
    let luma = if native {
        rgb[0] * 0.27222872 + rgb[1] * 0.67408174 + rgb[2] * 0.053689517
    } else {
        photonic_render::grade::luma709(rgb)
    };
    let x = match channel {
        0 | 7 => luma,
        1..=3 => rgb[channel - 1],
        4..=6 => hue,
        8 => saturation,
        _ => return false,
    };
    if !x.is_finite() || !(0.005..=0.995).contains(&x) {
        return false;
    }
    if points.iter().any(|point| (point.0 - x).abs() < 0.005) {
        return false;
    }
    let y = photonic_render::grade::sample_lut256(&photonic_render::grade::curve_lut(points), x);
    insert_sorted(points, (x, y));
    true
}

fn swatch_to_working(rgba: [f32; 4]) -> [f32; 3] {
    [rgba[0], rgba[1], rgba[2]].map(photonic_video::graph::ops::srgb_to_linear)
}

pub(crate) fn neutral_curve_points(channel: usize) -> Vec<(f32, f32)> {
    if channel >= 4 {
        vec![(0.0, 0.5), (1.0, 0.5)]
    } else {
        vec![(0.0, 0.0), (1.0, 1.0)]
    }
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
    keys: &mut Vec<QualifierKey>,
    matte_levels: &mut [f32; 2],
    actions: &mut Vec<PanelAction>,
    seq: SequenceId,
    track: TrackId,
    clip: ClipId,
    op: GradeOpId,
    graph_node: Option<u32>,
    can_sample: bool,
    qualified_input: bool,
    native: bool,
) -> bool {
    let mut structural_change = false;
    if native {
        ui.label("Key ranges use HSL of bounded ACEScct/AP1 coordinates. Correction uses log CDL. Picking and matte preview use the input before this correction.");
    }
    ui.horizontal(|ui| {
        // Eyedropper — extends the app-wide EyedropperTarget (07 §5 / 13 §9.3):
        // the next canvas click seeds this qualifier centre from the sampled colour.
        if ui.add_enabled_ui(can_sample, eyedropper_btn).inner {
            actions.push(PanelAction::StartEyedropper(
                EyedropperTarget::GradeQualifier {
                    graph_node,
                    seq,
                    track,
                    clip,
                    op,
                    mode: QualifierSampleMode::Replace,
                },
            ));
        }
        ui.label(
            RichText::new("Set key from monitor")
                .small()
                .color(muted(ui)),
        );
        if ui
            .add_enabled(
                can_sample && keys.len() < MAX_QUALIFIER_KEYS,
                egui::Button::new("+ Pick"),
            )
            .on_hover_text("Add a separate HSL key from a monitor sample")
            .clicked()
        {
            actions.push(PanelAction::StartEyedropper(
                EyedropperTarget::GradeQualifier {
                    graph_node,
                    seq,
                    track,
                    clip,
                    op,
                    mode: QualifierSampleMode::Add,
                },
            ));
        }
        if ui
            .add_enabled(
                can_sample && keys.len() < MAX_QUALIFIER_KEYS,
                egui::Button::new("− Pick"),
            )
            .on_hover_text("Subtract a sampled HSL region from the key")
            .clicked()
        {
            actions.push(PanelAction::StartEyedropper(
                EyedropperTarget::GradeQualifier {
                    graph_node,
                    seq,
                    track,
                    clip,
                    op,
                    mode: QualifierSampleMode::Subtract,
                },
            ));
        }
    });

    if !native {
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
                let [r, g, b] = swatch_to_working(seed);
                let (h, s, l) = rgb_to_hsl(r, g, b);
                let (nh, ns, nl) = seed_qualifier(h, s, l);
                *hue = nh;
                *sat = ns;
                *lum = nl;
                keys.clear();
            }
        });
        ui.data_mut(|d| d.insert_temp(seed_id, seed));
    }

    // Hue is circular. Allow a gate to extend beyond 0/1 so a picked red near
    // the seam also includes nearby reds on its other side. The renderer
    // evaluates the hue and its ±1 aliases for this representation.
    range_gate_with_bounds(ui, "Hue", hue, -1.0..=2.0);
    range_gate(ui, "Sat", sat);
    range_gate(ui, "Lum", lum);
    labelled(ui, "Softness", |ui| {
        ui.add(
            egui::Slider::new(softness, 0.0..=1.0)
                .clamping(egui::SliderClamping::Edits)
                .step_by(0.01),
        );
    });

    if !keys.is_empty() {
        section_header(ui, "SAMPLED KEY REGIONS");
        let mut remove = None;
        for (index, key) in keys.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{} {}",
                    if key.mode == photonic_core::timeline::QualifierKeyMode::Add {
                        "+"
                    } else {
                        "−"
                    },
                    index + 1
                ));
                if ui.small_button("Remove").clicked() {
                    remove = Some(index);
                }
            });
            range_gate_with_bounds(ui, "Hue", &mut key.hue, -1.0..=2.0);
            range_gate(ui, "Sat", &mut key.sat);
            range_gate(ui, "Lum", &mut key.lum);
            labelled(ui, "Softness", |ui| {
                ui.add(egui::Slider::new(&mut key.softness, 0.0..=1.0));
            });
        }
        if let Some(index) = remove {
            keys.remove(index);
            structural_change = true;
        }
    }

    section_header(ui, "MATTE LEVELS");
    ui.label(
        RichText::new("Trim weak key values and fill strong values. Neutral at zero.")
            .small()
            .color(muted(ui)),
    );
    labelled(ui, "Clean black", |ui| {
        ui.add(egui::Slider::new(&mut matte_levels[0], 0.0..=0.49).step_by(0.01));
    });
    labelled(ui, "Clean white", |ui| {
        ui.add(egui::Slider::new(&mut matte_levels[1], 0.0..=0.49).step_by(0.01));
    });

    let target = (seq, clip, op, graph_node);
    let active = ui.data(|data| {
        data.get_temp::<Option<(SequenceId, ClipId, GradeOpId, Option<u32>)>>(egui::Id::new(
            "qualifier_matte_target",
        ))
        .flatten()
    }) == Some(target);
    if ui
        .add_enabled(
            can_sample && qualified_input,
            egui::SelectableLabel::new(active, "Highlight matte"),
        )
        .on_disabled_hover_text("Matte preview requires an unambiguous clip corrector input; graph inspection requires Native Managed")
        .clicked()
    {
        actions.push(PanelAction::ToggleQualifierMatte {
            seq,
            track,
            clip,
            op,
            graph_node,
        });
    }

    ui.add_space(4.0);
    section_header(ui, "SECONDARY CORRECTION (CDL)");
    cdl_editor(
        ui,
        &mut correction.slope,
        &mut correction.offset,
        &mut correction.power,
        &mut correction.sat,
    );
    structural_change
}

/// A min/max range as two clamped drag values keeping `lo <= hi`.
fn range_gate(ui: &mut Ui, label: &str, range: &mut [f32; 2]) {
    range_gate_with_bounds(ui, label, range, 0.0..=1.0);
}

fn range_gate_with_bounds(
    ui: &mut Ui,
    label: &str,
    range: &mut [f32; 2],
    bounds: std::ops::RangeInclusive<f32>,
) {
    let mut changed = false;
    labelled(ui, label, |ui| {
        changed |= ui
            .add(
                egui::DragValue::new(&mut range[0])
                    .speed(0.005)
                    .range(bounds.clone())
                    .clamp_existing_to_range(false),
            )
            .changed();
        ui.label("–");
        changed |= ui
            .add(
                egui::DragValue::new(&mut range[1])
                    .speed(0.005)
                    .range(bounds)
                    .clamp_existing_to_range(false),
            )
            .changed();
    });
    if changed && range[0] > range[1] {
        range.swap(0, 1);
    }
}

/// Seed a qualifier's hue/sat/lum gates around a sampled HSL colour with a
/// sensible default half-width. Hue may extend beyond `[0,1]` at the circular
/// seam; the renderer evaluates aliases, while saturation/luma remain clamped.
pub(crate) fn seed_qualifier(h: f32, s: f32, l: f32) -> ([f32; 2], [f32; 2], [f32; 2]) {
    let hw = |v: f32, w: f32| [(v - w).max(0.0), (v + w).min(1.0)];
    ([h - 0.06, h + 0.06], hw(s, 0.20), hw(l, 0.20))
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
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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
        ScopeTapPoint::GradeGraphMatteOutput { clip, node } => format!("{} · matte node {node}", clip_name(doc, clip)),
        ScopeTapPoint::Clip(clip) => clip_name(doc, clip),
        ScopeTapPoint::ClipPreGrade(clip) => format!("{} · pre-grade", clip_name(doc, clip)),
        ScopeTapPoint::NativeGraphQualifierInput { clip, .. }
        | ScopeTapPoint::NativeGraphCurveInput { clip, .. }
        | ScopeTapPoint::NativeQualifierInput { clip, .. }
        | ScopeTapPoint::NativeCurveInput { clip, .. } => {
            format!("{} · before key", clip_name(doc, clip))
        }
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
/// `CaptionOverlay` in Legacy SDR. Native Managed's Program tap is after its
/// sRGB display transform, while its clip tap remains scene-linear. Returns
/// the tap the panel wants so the caller can hand it to the engine.
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
    settings: &mut crate::preferences::ScopeWindowPreferences,
) -> ScopeTapPoint {
    settings.normalize();
    let mut is_open = *open;
    let mut mode = settings.tap;
    let mut scale = settings.scale;
    let want = requested_tap(mode, selection);
    let got = frame.map(|f| f.scope_tap_point).unwrap_or(want);

    let was_docked = settings.docked;
    let floating_rect = settings.rect;
    let dock_height = settings.dock_height;
    let mut close_requested = false;
    let content = |ui: &mut Ui| {
        ui.horizontal_wrapped(|ui| {
            if ui
                .selectable_label(settings.docked, "Dock")
                .on_hover_text("Reserve a resizable scope tray below the program monitor")
                .clicked()
            {
                settings.docked = true;
            }
            if ui.selectable_label(!settings.docked, "Float").clicked() {
                settings.docked = false;
            }
            if was_docked {
                egui::ComboBox::from_id_salt("docked_scope_kind")
                    .selected_text(scope_kind_name(*kind))
                    .show_ui(ui, |ui| {
                        for choice in [
                            ScopeKind::Waveform,
                            ScopeKind::Parade,
                            ScopeKind::RgbWaveform,
                            ScopeKind::Vectorscope,
                            ScopeKind::Histogram,
                            ScopeKind::AudioSpectrum,
                        ] {
                            ui.selectable_value(kind, choice, scope_kind_name(choice));
                        }
                    });
                egui::ComboBox::from_id_salt("docked_scope_tap")
                    .selected_text(match mode {
                        TapMode::Clip => "Clip",
                        TapMode::Program => "Program",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut mode, TapMode::Clip, "Clip");
                        ui.selectable_value(&mut mode, TapMode::Program, "Program");
                    });
                if matches!(
                    *kind,
                    ScopeKind::Waveform | ScopeKind::Parade | ScopeKind::RgbWaveform
                ) {
                    let display_signal = frame.is_some_and(|f| {
                        f.scope_tap_encoding
                            == Some(photonic_video::graph::ir::FrameColorEncoding::SrgbDisplay)
                    });
                    egui::ComboBox::from_id_salt("docked_scope_scale")
                        .selected_text(if display_signal || scale == ScopeScale::Full {
                            "Full 0–100%"
                        } else {
                            "Video legal"
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut scale, ScopeScale::Full, "Full 0–100%");
                            ui.add_enabled_ui(!display_signal, |ui| {
                                ui.selectable_value(
                                    &mut scale,
                                    ScopeScale::VideoLegal,
                                    "Video legal 16–235",
                                );
                            });
                        });
                }
                if ui
                    .add_enabled(settings.additional.len() < 3, egui::Button::new("+ Scope"))
                    .clicked()
                {
                    settings.add_scope();
                }
            }
            if was_docked && ui.button("Close").clicked() {
                close_requested = true;
            }
        });

        if was_docked {
            draw_scope_signal(
                ui,
                ctx,
                device,
                queue,
                frame,
                doc,
                selection,
                mode,
                *kind,
                scale,
                got,
                &mut settings.vectorscope_601,
                &mut settings.vectorscope_targets,
            );
            return;
        }

        ui.horizontal_wrapped(|ui| {
            for (k, name) in [
                (ScopeKind::Waveform, "Waveform"),
                (ScopeKind::Parade, "Parade"),
                (ScopeKind::RgbWaveform, "RGB Wave"),
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
                    "Scope the sequence; Native Managed measures after its display transform",
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
        if matches!(
            *kind,
            ScopeKind::Waveform | ScopeKind::Parade | ScopeKind::RgbWaveform
        ) {
            let display_signal = frame.is_some_and(|frame| {
                frame.scope_tap_encoding
                    == Some(photonic_video::graph::ir::FrameColorEncoding::SrgbDisplay)
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Scale").small().color(muted(ui)));
                ui.selectable_value(&mut scale, ScopeScale::Full, "Full 0–100%");
                ui.add_enabled_ui(!display_signal, |ui| {
                    ui.selectable_value(&mut scale, ScopeScale::VideoLegal, "Video legal 16–235");
                });
            });
        }
        if ui
            .add_enabled(settings.additional.len() < 3, egui::Button::new("+ Scope"))
            .on_hover_text("Open another scope over the same measurement tap")
            .clicked()
        {
            settings.add_scope();
        }
        draw_scope_signal(
            ui,
            ctx,
            device,
            queue,
            frame,
            doc,
            selection,
            mode,
            *kind,
            scale,
            got,
            &mut settings.vectorscope_601,
            &mut settings.vectorscope_targets,
        );
    };
    let shown_rect = if was_docked {
        Some(
            egui::TopBottomPanel::bottom("scopes_dock")
                .resizable(true)
                .default_height(dock_height)
                .min_height(160.0)
                .max_height((ctx.screen_rect().height() * 0.5).clamp(160.0, 560.0))
                .show(ctx, content)
                .response
                .rect,
        )
    } else {
        let mut window = egui::Window::new("Scopes")
            .open(&mut is_open)
            .resizable(true)
            .default_size([320.0, 360.0]);
        if let Some([left, top, width, height]) =
            restored_scope_rect(floating_rect, ctx.screen_rect())
        {
            window = window
                .default_pos([left, top])
                .default_size([width, height]);
        }
        window.show(ctx, content).map(|shown| shown.response.rect)
    };
    if let Some(rect) = shown_rect {
        if ctx.input(|input| input.pointer.any_released()) {
            if was_docked {
                settings.dock_height = rect.height().clamp(160.0, 560.0);
            } else {
                settings.rect = Some([rect.left(), rect.top(), rect.width(), rect.height()]);
            }
        }
    }
    if close_requested {
        is_open = false;
    }
    let mut closed = Vec::new();
    for extra in &mut settings.additional {
        let mut extra_open = true;
        let mut window = egui::Window::new(format!("Scope {}", extra.id))
            .open(&mut extra_open)
            .resizable(true)
            .default_pos([
                370.0 + extra.id as f32 * 20.0,
                72.0 + extra.id as f32 * 20.0,
            ])
            .default_size([350.0, 320.0]);
        if let Some([left, top, width, height]) = restored_scope_rect(extra.rect, ctx.screen_rect())
        {
            window = window
                .default_pos([left, top])
                .default_size([width, height]);
        }
        let shown = window.show(ctx, |ui| {
            egui::ComboBox::from_id_salt("scope_kind")
                .selected_text(scope_kind_name(extra.kind))
                .show_ui(ui, |ui| {
                    for kind in [
                        ScopeKind::Waveform,
                        ScopeKind::Parade,
                        ScopeKind::RgbWaveform,
                        ScopeKind::Vectorscope,
                        ScopeKind::Histogram,
                        ScopeKind::AudioSpectrum,
                    ] {
                        ui.selectable_value(&mut extra.kind, kind, scope_kind_name(kind));
                    }
                });
            if matches!(
                extra.kind,
                ScopeKind::Waveform | ScopeKind::Parade | ScopeKind::RgbWaveform
            ) {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut extra.scale, ScopeScale::Full, "Full");
                    ui.selectable_value(&mut extra.scale, ScopeScale::VideoLegal, "Video legal");
                });
            }
            draw_scope_signal(
                ui,
                ctx,
                device,
                queue,
                frame,
                doc,
                selection,
                mode,
                extra.kind,
                extra.scale,
                got,
                &mut extra.vectorscope_601,
                &mut extra.vectorscope_targets,
            );
        });
        if let Some(shown) = shown {
            if ctx.input(|input| input.pointer.any_released()) {
                let rect = shown.response.rect;
                extra.rect = Some([rect.left(), rect.top(), rect.width(), rect.height()]);
            }
        }
        if !extra_open {
            closed.push(extra.id);
        }
    }
    settings
        .additional
        .retain(|extra| !closed.contains(&extra.id));
    settings.kind = *kind;
    settings.tap = mode;
    settings.scale = scale;
    settings.open = is_open;
    *open = is_open;
    requested_tap(mode, selection)
}

fn scope_kind_name(kind: ScopeKind) -> &'static str {
    match kind {
        ScopeKind::Waveform => "Waveform",
        ScopeKind::Parade => "RGB Parade",
        ScopeKind::RgbWaveform => "RGB Wave",
        ScopeKind::Vectorscope => "Vectorscope",
        ScopeKind::Histogram => "Histogram",
        ScopeKind::AudioSpectrum => "Spectrum",
    }
}

/// Restore a saved floating panel onto the current monitor, including when its
/// former monitor was unplugged or the desktop resolution became smaller.
fn restored_scope_rect(saved: Option<[f32; 4]>, screen: egui::Rect) -> Option<[f32; 4]> {
    let [left, top, width, height] = saved?;
    if ![left, top, width, height]
        .iter()
        .all(|value| value.is_finite())
        || width < 160.0
        || height < 160.0
        || screen.width() <= 0.0
        || screen.height() <= 0.0
    {
        return None;
    }
    let width = width.min(screen.width());
    let height = height.min(screen.height());
    Some([
        left.clamp(screen.left(), screen.right() - width),
        top.clamp(screen.top(), screen.bottom() - height),
        width,
        height,
    ])
}

#[allow(clippy::too_many_arguments)]
fn draw_scope_signal(
    ui: &mut Ui,
    ctx: &egui::Context,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: Option<&EngineFrame>,
    doc: &Document,
    selection: &[ClipId],
    mode: TapMode,
    kind: ScopeKind,
    scale: ScopeScale,
    got: ScopeTapPoint,
    vectorscope_601: &mut bool,
    vectorscope_targets: &mut VectorscopeTargets,
) {
    let want = requested_tap(mode, selection);
    ui.label(
        RichText::new(format!("Scoping: {}", scope_tap_label(doc, want, got)))
            .small()
            .color(muted(ui)),
    );
    if let Some(frame) = frame {
        let interpretation = match frame.scope_tap_encoding {
            Some(photonic_video::graph::ir::FrameColorEncoding::LegacyLinearRec709) => {
                "Legacy SDR / BT.709 signal"
            }
            Some(photonic_video::graph::ir::FrameColorEncoding::SceneLinearAcescg) => {
                "scene-linear ACEScg"
            }
            Some(photonic_video::graph::ir::FrameColorEncoding::Acescct) => {
                "ACEScct grading coordinates"
            }
            Some(photonic_video::graph::ir::FrameColorEncoding::SrgbDisplay) => {
                "sRGB display signal"
            }
            Some(photonic_video::graph::ir::FrameColorEncoding::Bt709Video) => {
                "BT.709 video signal"
            }
            Some(photonic_video::graph::ir::FrameColorEncoding::MatteWeight) => "matte weight",
            None => "unresolved tap interpretation",
        };
        ui.label(format!(
            "Tick {} · revision {} · {interpretation}",
            frame.time.0, frame.doc_revision,
        ));
        for error in &frame.color_errors {
            ui.colored_label(ui.visuals().warn_fg_color, error);
        }
        for error in &frame.grading_errors {
            ui.colored_label(ui.visuals().warn_fg_color, error.to_string());
        }
    }
    ui.separator();
    if kind == ScopeKind::AudioSpectrum {
        draw_audio_spectrum(ui, ctx);
        return;
    }
    if frame.is_some_and(|frame| {
        !matches!(
            frame.scope_tap_encoding,
            Some(
                photonic_video::graph::ir::FrameColorEncoding::LegacyLinearRec709
                    | photonic_video::graph::ir::FrameColorEncoding::SrgbDisplay
            )
        )
    }) {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(if frame.is_some_and(|frame| frame.output_encoding == photonic_video::graph::ir::FrameColorEncoding::SrgbDisplay) {
                    "Scene-linear clip scopes are not yet qualified. Select Program to measure the sRGB display signal."
                } else {
                    "This tap needs a qualified color-specific scope scale."
                })
                    .color(muted(ui)),
            );
        });
        return;
    }
    let Some(tap) = frame.and_then(|frame| frame.scope_tap.as_ref()) else {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No signal — play or seek the sequence.").color(muted(ui)));
        });
        return;
    };
    // The tap texture is bucket-padded; scopes measure only the logical frame.
    let (tex, w, h) = (tap.texture.as_ref(), tap.width, tap.height);
    let frame = frame.expect("scope tap belongs to a frame");
    let srgb_display = frame.scope_tap_encoding
        == Some(photonic_video::graph::ir::FrameColorEncoding::SrgbDisplay);
    if srgb_display {
        ui.label(
            "sRGB display components · full-range scope levels; video-legal scale does not apply",
        );
        if kind == ScopeKind::Vectorscope {
            ui.label("Chroma uses the selected luma coefficients; this is not a calibrated BT.709 video signal.");
        }
    }
    let scale = if srgb_display {
        ScopeScale::Full
    } else {
        scale
    };
    ctx.data_mut(|data| {
        data.insert_temp(
            egui::Id::new("scope_frame_stamp"),
            ScopeFrameStamp {
                revision: frame.doc_revision,
                generation: frame.snapshot_generation,
                sequence: frame.sequence,
                time: frame.time,
                content: frame.content_hash,
                output_encoding: frame.output_encoding,
                tap_encoding: frame.scope_tap_encoding,
                tap: frame.scope_tap_point,
                requested: want,
            },
        )
    });
    match kind {
        ScopeKind::Histogram => draw_histogram(ui, device, queue, tex, w, h),
        ScopeKind::Parade => draw_parade(ui, device, queue, tex, (w, h), false, scale),
        ScopeKind::RgbWaveform => draw_parade(ui, device, queue, tex, (w, h), true, scale),
        ScopeKind::Waveform => draw_waveform(ui, device, queue, tex, w, h, scale),
        ScopeKind::Vectorscope => draw_vectorscope(
            ui,
            device,
            queue,
            tex,
            w,
            h,
            vectorscope_601,
            vectorscope_targets,
        ),
        ScopeKind::AudioSpectrum => unreachable!(),
    }
}

#[derive(Clone, PartialEq)]
struct ScopeFrameStamp {
    revision: u64,
    generation: u64,
    sequence: SequenceId,
    time: photonic_core::timeline::Tick,
    content: photonic_video::graph::ir::ContentHash,
    output_encoding: photonic_video::graph::ir::FrameColorEncoding,
    tap_encoding: Option<photonic_video::graph::ir::FrameColorEncoding>,
    tap: ScopeTapPoint,
    requested: ScopeTapPoint,
}

impl ScopeFrameStamp {
    fn same_context(&self, other: &Self) -> bool {
        self.revision == other.revision
            && self.generation == other.generation
            && self.sequence == other.sequence
            && self.tap == other.tap
            && self.requested == other.requested
            && self.output_encoding == other.output_encoding
            && self.tap_encoding == other.tap_encoding
    }
}

#[derive(Default)]
struct ScopeAnalysisState {
    signature: Option<(String, (u32, u32), u32)>,
    pending: Option<(ScopeFrameStamp, photonic_render::scopes::ScopeReadback)>,
    measured: Option<(ScopeFrameStamp, Vec<u32>)>,
}

/// Keep one bounded readback per scope. A previous frame can remain visible
/// during playback, but changes of document or tap invalidate it immediately.
#[allow(clippy::too_many_arguments)]
fn scope_bins(
    ui: &mut Ui,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    shader: &str,
    entry: &str,
    len: usize,
    size: (u32, u32),
    matrix: u32,
) -> Option<Vec<u32>> {
    let stamp = ui
        .ctx()
        .data(|d| d.get_temp::<ScopeFrameStamp>(egui::Id::new("scope_frame_stamp")))?;
    let id = ui.id().with("scope_analysis");
    let shared = ui.ctx().data_mut(|d| {
        d.get_temp_mut_or_default::<std::sync::Arc<std::sync::Mutex<ScopeAnalysisState>>>(id)
            .clone()
    });
    let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
    let signature = (entry.to_owned(), size, matrix);
    if state.signature.as_ref() != Some(&signature) {
        *state = ScopeAnalysisState {
            signature: Some(signature),
            ..Default::default()
        };
    }

    if state
        .pending
        .as_ref()
        .is_some_and(|(s, _)| !s.same_context(&stamp))
    {
        state.pending = None;
    }
    if state
        .measured
        .as_ref()
        .is_some_and(|(s, _)| !s.same_context(&stamp))
    {
        state.measured = None;
    }
    if let Some((pending_stamp, request)) = state.pending.take() {
        match request.poll(device) {
            Some(Ok(bins)) => state.measured = Some((pending_stamp, bins)),
            Some(Err(error)) => {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("Scope readback failed: {error}"),
                );
            }
            None => state.pending = Some((pending_stamp, request)),
        }
    }
    if state.pending.is_none() && state.measured.as_ref().is_none_or(|(s, _)| s != &stamp) {
        let signal = if stamp.tap_encoding
            == Some(photonic_video::graph::ir::FrameColorEncoding::SrgbDisplay)
        {
            photonic_render::scopes::ScopeSignal::SrgbDisplay
        } else {
            photonic_render::scopes::ScopeSignal::LegacyLinearRec709
        };
        state.pending = Some((
            stamp,
            photonic_render::scopes::begin_scope_readback_signal(
                device, queue, tex, shader, entry, len, size, matrix, signal,
            ),
        ));
    }
    if state.pending.is_some() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(16));
    }
    if let Some((measured, bins)) = &state.measured {
        ui.label(format!(
            "Measured tick {} · revision {} · {}",
            measured.time.0,
            measured.revision,
            if measured.tap_encoding
                == Some(photonic_video::graph::ir::FrameColorEncoding::SrgbDisplay)
            {
                "sRGB display components"
            } else {
                "Legacy SDR / BT.709 signal"
            }
        ));
        Some(bins.clone())
    } else {
        ui.label("Measuring…");
        None
    }
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
    let Some(data) = scope_bins(
        ui,
        device,
        queue,
        tex,
        photonic_render::scopes::HISTOGRAM_SHADER,
        "cs_hist",
        1030,
        (logical_w, logical_h),
        0,
    ) else {
        return;
    };
    let h = photonic_render::scopes::Histogram {
        luma: data[0..256].try_into().unwrap(),
        red: data[256..512].try_into().unwrap(),
        green: data[512..768].try_into().unwrap(),
        blue: data[768..1024].try_into().unwrap(),
    };
    let below = &data[1024..1027];
    let above = &data[1027..1030];
    if below.iter().chain(above).any(|&count| count > 0) {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            format!(
                "Outside 0–1 before scope clamp · below R/G/B: {}/{}/{} · above: {}/{}/{} pixels",
                below[0], below[1], below[2], above[0], above[1], above[2]
            ),
        );
    }
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
    logical_size: (u32, u32),
    overlay: bool,
    scale: ScopeScale,
) {
    let (logical_w, logical_h) = logical_size;
    let cols = logical_w.min(tex.width()).max(1) as usize;
    let stride = cols * 256;
    let Some(data) = scope_bins(
        ui,
        device,
        queue,
        tex,
        photonic_render::scopes::PARADE_SHADER,
        "cs_parade",
        stride * 3,
        (logical_w, logical_h),
        0,
    ) else {
        return;
    };
    let channels: [photonic_render::scopes::Waveform; 3] =
        std::array::from_fn(|c| photonic_render::scopes::Waveform {
            width: cols,
            bins: 256,
            data: data[c * stride..(c + 1) * stride].to_vec(),
        });
    if scale == ScopeScale::VideoLegal {
        let counts: Vec<_> = channels
            .iter()
            .map(|channel| channel.video_legal_excursions())
            .collect();
        ui.label(format!(
            "Outside legal R/G/B · below: {}/{}/{} · above: {}/{}/{}",
            counts[0].0, counts[1].0, counts[2].0, counts[0].1, counts[1].1, counts[2].1
        ));
    }
    let colors = [[220., 90., 90.], [90., 200., 110.], [100., 130., 230.]];
    let peak = channels
        .iter()
        .flat_map(|c| &c.data)
        .copied()
        .max()
        .unwrap_or(1)
        .max(1) as f32;
    if overlay {
        let mut rgb = vec![[0u8; 3]; 256 * 256];
        for (channel, waveform) in channels.iter().enumerate() {
            for x in 0..256 {
                let start = x * waveform.width / 256;
                let end = ((x + 1) * waveform.width / 256)
                    .max(start + 1)
                    .min(waveform.width);
                for bin in 0..256 {
                    let count = (start..end)
                        .map(|sx| waveform.count(sx, bin))
                        .max()
                        .unwrap_or(0);
                    if count != 0 {
                        let intensity = ((count as f32 / peak).sqrt() * 230.0) as u8;
                        let cell = &mut rgb[(255 - scale.display_bin(bin)) * 256 + x];
                        cell[channel] = cell[channel].max(intensity);
                    }
                }
            }
        }
        let pixels = rgb
            .into_iter()
            .map(|[r, g, b]| Color32::from_rgb(r.max(7), g.max(7), b.max(11)))
            .collect();
        scope_image(ui, "scope_rgb_waveform", 256, 256, pixels, None);
        return;
    }
    let out_w = 768;
    let out_h = 256;
    let mut pixels = vec![Color32::from_rgb(7, 7, 11); out_w * out_h];
    let mut brightest = vec![0u32; out_w * out_h];
    for (channel, waveform) in channels.iter().enumerate() {
        for x in 0..256 {
            // Aggregate every source column into the display column. Sampling
            // one column would hide narrow highlights on wide source images.
            let start = x * waveform.width / 256;
            let end = ((x + 1) * waveform.width / 256)
                .max(start + 1)
                .min(waveform.width);
            for bin in 0..256 {
                let count = (start..end)
                    .map(|sx| waveform.count(sx, bin))
                    .max()
                    .unwrap_or(0);
                if count != 0 {
                    let alpha = (count as f32 / peak).sqrt();
                    let c = colors[channel];
                    let index = (255 - scale.display_bin(bin)) * out_w + channel * 256 + x;
                    if count > brightest[index] {
                        brightest[index] = count;
                        pixels[index] = Color32::from_rgb(
                            (c[0] * alpha) as u8,
                            (c[1] * alpha) as u8,
                            (c[2] * alpha) as u8,
                        );
                    }
                }
            }
        }
    }
    scope_image(ui, "scope_parade", out_w, out_h, pixels, None);
}

fn draw_waveform(
    ui: &mut Ui,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    logical_w: u32,
    logical_h: u32,
    scale: ScopeScale,
) {
    let cols = logical_w.min(tex.width()).max(1) as usize;
    let Some(data) = scope_bins(
        ui,
        device,
        queue,
        tex,
        photonic_render::scopes::WAVEFORM_SHADER,
        "cs_wave",
        cols * 256,
        (logical_w, logical_h),
        0,
    ) else {
        return;
    };
    let wf = photonic_render::scopes::Waveform {
        width: cols,
        bins: 256,
        data,
    };
    if scale == ScopeScale::VideoLegal {
        let (below, above) = wf.video_legal_excursions();
        ui.label(format!(
            "Outside legal luma · below: {below} · above: {above}"
        ));
    }
    let out_w = 256usize;
    let out_h = wf.bins;
    let mut pixels = vec![Color32::from_rgb(7, 7, 11); out_w * out_h];
    let mut brightest = vec![0u32; out_w * out_h];
    let peak = wf.data.iter().copied().max().unwrap_or(1).max(1) as f32;
    let src_w = wf.width.max(1);
    for ox in 0..out_w {
        let start = ox * src_w / out_w;
        let end = ((ox + 1) * src_w / out_w).max(start + 1).min(src_w);
        for bin in 0..out_h {
            let c = (start..end).map(|sx| wf.count(sx, bin)).max().unwrap_or(0) as f32;
            if c > 0.0 {
                let a = (c / peak).sqrt().clamp(0.0, 1.0);
                let v = ((a * 215.0) as u8).saturating_add(20);
                let row = out_h - 1 - scale.display_bin(bin); // luma high → top
                let index = row * out_w + ox;
                if c as u32 > brightest[index] {
                    brightest[index] = c as u32;
                    pixels[index] = Color32::from_gray(v);
                }
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
    use_601: &mut bool,
    targets: &mut VectorscopeTargets,
) {
    // Both matrix interpretations use the asynchronous GPU scope path.
    ui.horizontal(|ui| {
        ui.label(RichText::new("Matrix").small().color(muted(ui)));
        if ui.selectable_label(!*use_601, "Rec.709").clicked() {
            *use_601 = false;
        }
        if ui.selectable_label(*use_601, "Rec.601").clicked() {
            *use_601 = true;
        }
    });
    ui.horizontal(|ui| {
        ui.label(RichText::new("Targets").small().color(muted(ui)));
        ui.selectable_value(targets, VectorscopeTargets::Pct75, "75%");
        ui.selectable_value(targets, VectorscopeTargets::Pct100, "100%");
    });

    let Some(data) = scope_bins(
        ui,
        device,
        queue,
        tex,
        photonic_render::scopes::VECTORSCOPE_SHADER,
        "cs_vector",
        256 * 256,
        (logical_w, logical_h),
        u32::from(*use_601),
    ) else {
        return;
    };
    let vs = photonic_render::scopes::Vectorscope { size: 256, data };
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
    let matrix = if *use_601 {
        photonic_render::color::Matrix::Bt601
    } else {
        photonic_render::color::Matrix::Bt709
    };
    scope_image(
        ui,
        "scope_vectorscope",
        n,
        n,
        pixels,
        Some(&|ui, painter, rect| draw_vectorscope_guides(ui, painter, rect, matrix, *targets)),
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VectorscopeTargets {
    #[default]
    Pct75,
    Pct100,
}

/// Display the same Cb/Cr bin that the CPU/GPU measurement uses for a
/// reference color patch at the selected signal level.
fn vectorscope_target_pos(
    rect: Rect,
    rgb: [f32; 3],
    matrix: photonic_render::color::Matrix,
) -> Pos2 {
    let (cb, cr) = photonic_render::scopes::cbcr_bins_matrix(rgb, matrix);
    pos2(
        rect.left() + (cb as f32 + 0.5) * rect.width() / 256.0,
        rect.bottom() - (cr as f32 + 0.5) * rect.height() / 256.0,
    )
}

/// Matrix-aware R/Y/G/C/B/M targets plus an approximate I-axis skin guide.
fn draw_vectorscope_guides(
    ui: &Ui,
    painter: &egui::Painter,
    rect: Rect,
    matrix: photonic_render::color::Matrix,
    targets: VectorscopeTargets,
) {
    let center = rect.center();
    let r100 = rect.width() * 0.45;
    let stroke_soft = Stroke::new(0.5, border(ui));
    let stroke_i = Stroke::new(1.2, Color32::from_rgb(0xE8, 0xB0, 0x70)); // warm skin
    let stroke_q = Stroke::new(1.0, Color32::from_rgb(0x70, 0xB0, 0xE8)); // cool Q

    // The outer circle is a visual scale guide; the target locations below
    // are calculated from the actual selected matrix rather than a generic box.
    painter.circle_stroke(center, r100, stroke_soft);

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
    let level = match targets {
        VectorscopeTargets::Pct75 => 0.75,
        VectorscopeTargets::Pct100 => 1.0,
    };
    for (label, channels, color) in [
        ("R", [1.0, 0.0, 0.0], Color32::from_rgb(220, 95, 95)),
        ("Y", [1.0, 1.0, 0.0], Color32::from_rgb(220, 205, 90)),
        ("G", [0.0, 1.0, 0.0], Color32::from_rgb(95, 205, 115)),
        ("C", [0.0, 1.0, 1.0], Color32::from_rgb(95, 205, 215)),
        ("B", [0.0, 0.0, 1.0], Color32::from_rgb(105, 135, 230)),
        ("M", [1.0, 0.0, 1.0], Color32::from_rgb(215, 105, 215)),
    ] {
        let pos = vectorscope_target_pos(rect, channels.map(|channel| channel * level), matrix);
        painter.rect_stroke(
            Rect::from_center_size(pos, vec2(7.0, 7.0)),
            0.0,
            Stroke::new(1.0, color),
        );
        painter.text(
            pos + vec2(5.0, -5.0),
            egui::Align2::LEFT_BOTTOM,
            label,
            egui::FontId::proportional(9.0),
            color,
        );
    }
    // State the selected reference level; target boxes are signal-level
    // references, not a claim that the measured gamut is legal or calibrated.
    painter.text(
        rect.left_top() + vec2(4.0, 4.0),
        egui::Align2::LEFT_TOP,
        match targets {
            VectorscopeTargets::Pct75 => "75% targets",
            VectorscopeTargets::Pct100 => "100% targets",
        },
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
    overlay: Option<&dyn Fn(&Ui, &egui::Painter, Rect)>,
) {
    let image = egui::ColorImage {
        size: [w, h],
        pixels,
    };
    let handle = ui.ctx().load_texture(name, image, TextureOptions::LINEAR);
    let available = ui.available_size();
    let height = available.y.clamp(80.0, 400.0);
    let size = if name == "scope_vectorscope" {
        let side = available.x.min(height).max(80.0);
        vec2(side, side)
    } else {
        vec2(available.x.max(160.0), height)
    };
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
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

    #[test]
    fn saved_scope_window_returns_to_current_screen() {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        assert_eq!(
            restored_scope_rect(Some([1200.0, 900.0, 640.0, 400.0]), screen),
            Some([160.0, 200.0, 640.0, 400.0])
        );
        assert_eq!(
            restored_scope_rect(Some([25.0, 25.0, 1200.0, 700.0]), screen),
            Some([0.0, 0.0, 800.0, 600.0])
        );
        assert_eq!(
            restored_scope_rect(Some([f32::NAN, 0.0, 320.0, 360.0]), screen),
            None
        );
    }

    #[test]
    fn multi_corrector_selection_and_batch_edits_leave_other_ops_alone() {
        let mut grade = Grade::new();
        grade.ops = [0.0, 1.0, 2.0, 3.0]
            .into_iter()
            .map(|stops| GradeOp::new(GradeOpKind::Exposure, GradeOpParams::Exposure { stops }))
            .collect();
        let ids: Vec<_> = grade.ops.iter().map(|op| op.id).collect();
        let mut selected = Vec::new();
        let mut primary = None;
        select_grade_ops(&ids, &mut selected, &mut primary, ids[1], false, false);
        assert_eq!(selected, vec![ids[1]]);
        select_grade_ops(&ids, &mut selected, &mut primary, ids[3], false, true);
        assert_eq!(selected, ids[1..=3]);
        select_grade_ops(&ids, &mut selected, &mut primary, ids[2], true, false);
        assert_eq!(selected, vec![ids[1], ids[3]]);

        set_selected_grade_ops_enabled(&mut grade, &selected, false);
        assert_eq!(
            grade.ops.iter().map(|op| op.enabled).collect::<Vec<_>>(),
            vec![true, false, true, false]
        );
        remove_selected_grade_ops(&mut grade, &selected);
        assert_eq!(
            grade.ops.iter().map(|op| op.id).collect::<Vec<_>>(),
            vec![ids[0], ids[2]]
        );
    }

    #[test]
    fn multi_corrector_batch_is_one_undo_step_and_respects_track_lock() {
        use photonic_core::timeline::{
            Clip, ClipSource, FrameRate, Sequence, Tick, TimelineProject, Track, TrackKind,
        };
        let mut doc = Document::new("batch", 16.0, 16.0);
        let mut project = TimelineProject::new();
        let mut sequence = Sequence::new("cut", FrameRate::FPS_30, 16, 16);
        let seq = sequence.id;
        let mut track = Track::new(TrackKind::Video, "V1");
        let track_id = track.id;
        let mut clip = Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick(100));
        let clip_id = clip.id;
        let mut grade = Grade::new();
        grade.ops = (0..3)
            .map(|index| {
                GradeOp::new(
                    GradeOpKind::Exposure,
                    GradeOpParams::Exposure {
                        stops: index as f32,
                    },
                )
            })
            .collect();
        let ids: Vec<_> = grade.ops.iter().map(|op| op.id).collect();
        clip.grade = Some(grade.clone());
        track.clips.push(clip);
        sequence.video_tracks.push(track);
        project.insert_sequence(sequence);
        doc.timeline = Some(project);
        let mut history = CommandHistory::new(20);

        remove_selected_grade_ops(&mut grade, &ids[..2]);
        commit_grade(&mut doc, &mut history, seq, track_id, clip_id, grade, true);
        assert_eq!(history.revision(), 1);
        let current = &doc.timeline.as_ref().unwrap().sequences[&seq].video_tracks[0].clips[0]
            .grade
            .as_ref()
            .unwrap()
            .ops;
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].id, ids[2]);
        assert!(history.undo(&mut doc));
        assert_eq!(
            doc.timeline.as_ref().unwrap().sequences[&seq].video_tracks[0].clips[0]
                .grade
                .as_ref()
                .unwrap()
                .ops
                .len(),
            3
        );

        doc.timeline
            .as_mut()
            .unwrap()
            .sequences
            .get_mut(&seq)
            .unwrap()
            .video_tracks[0]
            .locked = true;
        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 4.0 },
        ));
        let revision = history.revision();
        commit_grade(&mut doc, &mut history, seq, track_id, clip_id, grade, true);
        assert_eq!(history.revision(), revision);
        assert_eq!(
            doc.timeline.as_ref().unwrap().sequences[&seq].video_tracks[0].clips[0]
                .grade
                .as_ref()
                .unwrap()
                .ops
                .len(),
            3
        );
    }

    #[test]
    fn unresolved_input_color_shows_diagnostic_before_declared_space() {
        let finding = photonic_video::color::ColorInputFinding {
            clip: ClipId::new(),
            asset: AssetId::new(),
            interpretation: "unresolved",
            color_space: Some("ACEScg".into()),
            diagnostic: Some("source metadata lacks signal range".into()),
        };
        assert_eq!(
            input_color_detail(&finding),
            "source metadata lacks signal range"
        );
    }

    #[test]
    fn viewing_input_color_editor_does_not_author_an_override() {
        let mut doc = Document::new("input color", 16.0, 16.0);
        let mut history = CommandHistory::new(100);
        let digest = ColorDigest::try_from("a".repeat(64)).unwrap();
        let asset = AssetId::new();
        let sequence = SequenceId::new();
        let track = TrackId::new();
        let clip = ClipId::new();
        let before = serde_json::to_value(&doc).unwrap();
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                assert!(!draw_input_color_editor(
                    ui,
                    &mut doc,
                    &mut history,
                    sequence,
                    track,
                    InputColorEditTarget::Asset(asset),
                    None,
                    digest.clone(),
                    false,
                ));
                assert!(!draw_input_color_editor(
                    ui,
                    &mut doc,
                    &mut history,
                    sequence,
                    track,
                    InputColorEditTarget::Clip(clip),
                    None,
                    digest.clone(),
                    true,
                ));
            });
        });
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        assert_eq!(history.revision(), 0);
    }

    #[test]
    fn viewing_native_input_editor_does_not_author_an_override() {
        let mut doc = Document::new("native input color", 16.0, 16.0);
        let mut history = CommandHistory::new(100);
        let before = serde_json::to_value(&doc).unwrap();
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                assert!(!draw_native_input_color_editor(
                    ui,
                    &mut doc,
                    &mut history,
                    SequenceId::new(),
                    TrackId::new(),
                    InputColorEditTarget::Asset(AssetId::new()),
                    None,
                    false,
                ));
            });
        });
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        assert_eq!(history.revision(), 0);
    }

    #[test]
    fn viewing_grading_graph_does_not_edit_its_topology() {
        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        ));
        grade.convert_to_graph();
        let before = grade.clone();
        let mut selected = None;
        let ctx = egui::Context::default();
        ctx.data_mut(|data| {
            data.insert_temp(egui::Id::new("test_graph").with("expanded_graph"), true)
        });
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                assert!(!draw_grade_graph_editor(
                    ui,
                    &mut grade,
                    &[],
                    &mut selected,
                    egui::Id::new("test_graph")
                ));
            });
        });
        assert_eq!(grade, before);
        assert_eq!(selected, None);
    }

    #[test]
    fn graph_canvas_routes_clicks_across_separate_frames() {
        let mut grade = Grade::new();
        for stops in [0.5, 1.0] {
            grade.ops.push(GradeOp::new(
                GradeOpKind::Exposure,
                GradeOpParams::Exposure { stops },
            ));
        }
        grade.convert_to_graph();
        let graph = grade.graph.as_ref().unwrap();
        let target=graph.nodes.iter().find(|(_,node)| matches!(node,GradeGraphNode::Corrector { op,.. } if *op==grade.ops[1].id)).map(|(&id,_)|id).unwrap();
        let mut candidate = graph.clone();
        let ctx = egui::Context::default();
        let canvas_id = egui::Id::new("multi_frame_graph");
        let mut frame = |events: Vec<egui::Event>| {
            let mut changed = false;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 600.0))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        changed = draw_grade_graph_canvas(
                            ui,
                            graph,
                            &grade,
                            &mut candidate,
                            &mut None,
                            canvas_id,
                        );
                    });
                },
            );
            changed
        };
        assert!(!frame(vec![]));
        let source = ctx
            .read_response(canvas_id.with(("out", 0u32)))
            .unwrap()
            .rect
            .center();
        let input = ctx
            .read_response(canvas_id.with(("in", target, "image")))
            .unwrap()
            .rect
            .center();
        let click = |pos| {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        assert!(!frame(click(source)));
        assert!(
            !frame(vec![]),
            "a selected source alone must not mutate the graph"
        );
        assert!(
            frame(click(input)),
            "the source selection must survive until the later input click"
        );
        assert!(matches!(
            candidate.nodes[&target],
            GradeGraphNode::Corrector { input: 0, .. }
        ));
        assert_eq!(candidate.validate(&grade.ops), Ok(()));
        assert!(matches!(
            graph.nodes[&target],
            GradeGraphNode::Corrector { input: 1, .. }
        ));
    }

    #[test]
    fn typed_graph_canvas_rejects_cross_type_connections_before_mutating() {
        let mut grade = Grade::new();
        let qualifier = GradeOp::new(
            GradeOpKind::HslQualifier,
            GradeOpParams::HslQualifier {
                hue: [0.0, 1.0],
                sat: [0.0, 1.0],
                lum: [0.0, 1.0],
                softness: 0.0,
                correction: photonic_core::timeline::CdlParams::identity(),
                keys: vec![],
                matte_levels: [0.0, 0.0],
            },
        );
        let op = qualifier.id;
        grade.ops.push(qualifier);
        grade.convert_to_graph();
        let matte = grade
            .add_graph_utility(GradeGraphNode::QualifierMatte {
                input: 0,
                op,
                label: String::new(),
            })
            .unwrap();
        let mixer = grade
            .add_graph_utility(GradeGraphNode::KeyMixer {
                top: matte,
                bottom: matte,
                mode: photonic_core::timeline::GradeKeyMixMode::Union,
                label: String::new(),
            })
            .unwrap();
        let refined = grade
            .add_graph_utility(GradeGraphNode::MatteRefine {
                input: matte,
                refinement: Default::default(),
                label: "Refine".into(),
            })
            .unwrap();
        let graph = grade.graph.as_ref().unwrap();
        let mut candidate = graph.clone();
        assert!(!set_graph_image_input(
            &mut candidate,
            graph.output,
            "image",
            matte
        ));
        assert!(!set_graph_image_input(&mut candidate, mixer, "top", 0));
        assert!(!set_graph_image_input(&mut candidate, refined, "matte", 0));
        assert_eq!(&candidate, graph);
        assert!(set_graph_image_input(
            &mut candidate,
            refined,
            "matte",
            mixer
        ));
        assert_eq!(candidate.validate(&grade.ops), Ok(()));
        assert!(set_graph_image_input(&mut candidate, mixer, "top", refined));
        assert!(candidate.validate(&grade.ops).is_err());
        let layers = graph_canvas_layers(graph);
        assert!(layers[&matte].0 < layers[&mixer].0);
    }

    #[test]
    fn graph_canvas_places_parallel_nodes_and_routes_only_valid_candidates() {
        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 0.5 },
        ));
        grade.convert_to_graph();
        grade
            .add_graph_corrector(
                GradeOp::new(
                    GradeOpKind::Exposure,
                    GradeOpParams::Exposure { stops: 1.0 },
                ),
                true,
            )
            .unwrap();
        let graph = grade.graph.as_ref().unwrap();
        let layout = graph_canvas_layers(graph);
        assert_eq!(layout.len(), graph.nodes.len());
        for (&id, node) in &graph.nodes {
            for (_, source, _) in graph_image_inputs(node) {
                assert!(layout[&source].0 < layout[&id].0);
            }
        }
        let mut candidate = graph.clone();
        assert!(set_graph_image_input(
            &mut candidate,
            graph.output,
            "image",
            0
        ));
        assert!(candidate.validate(&grade.ops).is_ok());
        assert!(set_graph_image_input(
            &mut candidate,
            graph.output,
            "image",
            graph.output
        ));
        assert!(candidate.validate(&grade.ops).is_err());
        assert!(graph.validate(&grade.ops).is_ok());
    }

    #[test]
    fn vectorscope_targets_follow_the_selected_measurement_matrix() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(256.0, 256.0));
        for matrix in [
            photonic_render::color::Matrix::Bt709,
            photonic_render::color::Matrix::Bt601,
        ] {
            let rgb = [0.75, 0.0, 0.0];
            let (cb, cr) = photonic_render::scopes::cbcr_bins_matrix(rgb, matrix);
            let target = vectorscope_target_pos(rect, rgb, matrix);
            assert_eq!(target.x, rect.left() + cb as f32 + 0.5);
            assert_eq!(target.y, rect.bottom() - cr as f32 - 0.5);
        }
        let red709 = vectorscope_target_pos(
            rect,
            [0.75, 0.0, 0.0],
            photonic_render::color::Matrix::Bt709,
        );
        let red601 = vectorscope_target_pos(
            rect,
            [0.75, 0.0, 0.0],
            photonic_render::color::Matrix::Bt601,
        );
        assert_ne!(red709, red601);
    }

    #[test]
    fn viewing_empty_curve_tabs_preserves_disabled_hue_curves() {
        for channel in 0..9 {
            let ctx = egui::Context::default();
            let mut curves: [Vec<(f32, f32)>; 9] = std::array::from_fn(|_| Vec::new());
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.data_mut(|d| d.insert_temp(ui.id().with("curve_ch"), channel));
                    curves_editor(
                        ui,
                        curves.each_mut(),
                        &mut Vec::new(),
                        SequenceId::new(),
                        TrackId::new(),
                        ClipId::new(),
                        GradeOpId::new(),
                        None,
                        false,
                        false,
                    );
                });
            });
            assert!(
                curves.iter().all(Vec::is_empty),
                "viewing curve channel {channel} must not author points"
            );
        }
        assert_eq!(neutral_curve_points(4), vec![(0.0, 0.5), (1.0, 0.5)]);
        assert_eq!(neutral_curve_points(5), vec![(0.0, 0.5), (1.0, 0.5)]);
        assert_eq!(neutral_curve_points(8), vec![(0.0, 0.5), (1.0, 0.5)]);
    }

    #[test]
    fn grade_navigation_uses_timeline_order_on_selected_video_track() {
        use photonic_core::timeline::{
            Clip, ClipSource, FrameRate, Sequence, Tick, Track, TrackKind,
        };
        let mut sequence = Sequence::new("Shots", FrameRate::FPS_30, 1920, 1080);
        let mut track = Track::new(TrackKind::Video, "V1");
        let middle = Clip::new(ClipSource::Adjustment, Tick(100), Tick(100));
        let last = Clip::new(ClipSource::Adjustment, Tick(200), Tick(100));
        let first = Clip::new(ClipSource::Adjustment, Tick(0), Tick(100));
        let (middle_id, last_id, first_id) = (middle.id, last.id, first.id);
        track.clips = vec![middle, last, first];
        let track_id = track.id;
        sequence.video_tracks.push(track);
        assert_eq!(
            grade_clip_neighbors(&sequence, track_id, middle_id),
            Some((Some(first_id), Some(last_id), 1, 3))
        );
        assert_eq!(
            grade_clip_neighbors(&sequence, track_id, first_id),
            Some((None, Some(middle_id), 0, 3))
        );
        assert_eq!(
            grade_clip_neighbors(&sequence, track_id, last_id),
            Some((Some(middle_id), None, 2, 3))
        );
    }

    #[test]
    fn scope_results_are_invalidated_by_revision_or_selection_but_not_playback_time() {
        let stamp = ScopeFrameStamp {
            revision: 1,
            generation: 1,
            sequence: SequenceId::new(),
            time: photonic_core::timeline::Tick(0),
            content: photonic_video::graph::ir::ContentHash(0),
            output_encoding: photonic_video::graph::ir::FrameColorEncoding::LegacyLinearRec709,
            tap_encoding: Some(photonic_video::graph::ir::FrameColorEncoding::LegacyLinearRec709),
            tap: ScopeTapPoint::Program,
            requested: ScopeTapPoint::Program,
        };
        let mut next = stamp.clone();
        next.time = photonic_core::timeline::Tick(100);
        assert!(stamp.same_context(&next));
        next.revision += 1;
        assert!(!stamp.same_context(&next));
        next = stamp.clone();
        next.requested = ScopeTapPoint::Clip(ClipId::new());
        assert!(!stamp.same_context(&next));
        next = stamp.clone();
        next.generation += 1;
        assert!(!stamp.same_context(&next));
        next = stamp.clone();
        next.output_encoding = photonic_video::graph::ir::FrameColorEncoding::SrgbDisplay;
        assert!(!stamp.same_context(&next));
        next = stamp.clone();
        next.tap_encoding = Some(photonic_video::graph::ir::FrameColorEncoding::SceneLinearAcescg);
        assert!(!stamp.same_context(&next));
    }

    #[test]
    fn opening_color_controls_does_not_change_document_or_history() {
        check_color_controls_viewing(false);
    }

    #[test]
    fn opening_native_color_controls_does_not_change_document_or_history() {
        check_color_controls_viewing(true);
    }

    fn check_color_controls_viewing(native: bool) {
        use photonic_core::timeline::{
            Clip, ClipSource, FrameRate, Sequence, Tick, TimelineProject, Track, TrackKind,
        };
        let mut doc = Document::new("grade panel", 100., 100.);
        let mut project = TimelineProject::new();
        let mut sequence = Sequence::new("Test", FrameRate::FPS_30, 16, 16);
        if native {
            sequence.color = photonic_core::timeline::color::SequenceColorConfig::NativeManaged(
                Box::new(photonic_core::timeline::color::NativeManagedColorConfig::sdr_draft()),
            );
        }
        let mut track = Track::new(TrackKind::Video, "V1");
        let clip = Clip::new(
            ClipSource::Asset {
                asset: AssetId::new(),
            },
            Tick(0),
            Tick(100),
        );
        let selection = [clip.id];
        track.clips.push(clip);
        let mut source = Clip::new(
            ClipSource::Asset {
                asset: AssetId::new(),
            },
            Tick(100),
            Tick(100),
        );
        source.grade = Some(Grade {
            ops: vec![GradeOp::new(
                GradeOpKind::Exposure,
                GradeOpParams::Exposure { stops: 1.0 },
            )],
            bypass: false,
            graph: None,
        });
        track.clips.push(source);
        sequence.video_tracks.push(track);
        project.insert_sequence(sequence);
        doc.timeline = Some(project);
        let before = serde_json::to_value(&doc).unwrap();
        let mut history = CommandHistory::new(100);
        let mut selected = None;
        let mut scopes = false;
        let ctx = egui::Context::default();
        for mut tab in [
            ColorPageTab::Wheels,
            ColorPageTab::Curves,
            ColorPageTab::Qualifier,
            ColorPageTab::Lut,
        ] {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    draw_color_controls(
                        ui,
                        &mut doc,
                        &mut history,
                        &mut Vec::new(),
                        &selection,
                        &mut selected,
                        &mut tab,
                        &mut scopes,
                    );
                });
            });
        }
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        assert!(!history.undo(&mut doc));
        ctx.data_mut(|data| data.insert_temp(advanced_controls_id(), false));
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                draw_color_controls(
                    ui,
                    &mut doc,
                    &mut history,
                    &mut Vec::new(),
                    &selection,
                    &mut selected,
                    &mut ColorPageTab::Wheels,
                    &mut scopes,
                );
            });
        });
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        assert_eq!(history.revision(), 0);
        ctx.data_mut(|data| data.insert_temp(advanced_controls_id(), true));
        let (seq, track, clip) = locate_clip(&doc, &selection).unwrap();
        let mut grade = Grade::new();
        let mut exposure = GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 8.0 },
        );
        exposure.mask = Some(GradeMask::PowerWindow {
            shape: WindowShape::Ellipse,
            center: [1.3, -0.2],
            size: [0.25, 0.25],
            rotation: 0.3,
            softness: 0.1,
            invert: false,
        });
        selected = Some(exposure.id);
        grade.ops.push(exposure);
        commit_grade(&mut doc, &mut history, seq, track, clip, grade, true);
        let before = serde_json::to_value(&doc).unwrap();
        let revision = history.revision();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                draw_color_controls(
                    ui,
                    &mut doc,
                    &mut history,
                    &mut Vec::new(),
                    &selection,
                    &mut selected,
                    &mut ColorPageTab::Wheels,
                    &mut scopes,
                );
            });
        });
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        assert_eq!(history.revision(), revision);
        let version =
            ops::add_grade_version(doc.timeline.as_ref().unwrap(), seq, track, clip, "Balance")
                .unwrap();
        history.execute_discrete(Command::Timeline(version), &mut doc);
        let before = serde_json::to_value(&doc).unwrap();
        let revision = history.revision();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                draw_color_controls(
                    ui,
                    &mut doc,
                    &mut history,
                    &mut Vec::new(),
                    &selection,
                    &mut selected,
                    &mut ColorPageTab::Wheels,
                    &mut scopes,
                );
            });
        });
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        assert_eq!(history.revision(), revision);
    }

    #[test]
    fn shared_look_grade_edits_coalesce_and_undo() {
        use photonic_core::timeline::{
            Clip, ClipSource, FrameRate, Sequence, SharedLook, SharedLookId, Tick, TimelineProject,
            Track, TrackKind,
        };
        let mut doc = Document::new("shared grade", 100., 100.);
        let mut project = TimelineProject::new();
        let id = SharedLookId::new();
        project.shared_looks.insert(
            id,
            SharedLook {
                id,
                name: "Scene".into(),
                grade: Grade::default(),
            },
        );
        let mut sequence = Sequence::new("Test", FrameRate::FPS_30, 16, 16);
        let mut track = Track::new(TrackKind::Video, "V1");
        for start in [0, 100] {
            let mut clip = Clip::new(ClipSource::Adjustment, Tick(start), Tick(100));
            clip.look = Some(ClipLook::Shared(id));
            track.clips.push(clip);
        }
        sequence.video_tracks.push(track);
        project.insert_sequence(sequence);
        doc.timeline = Some(project);
        let mut history = CommandHistory::new(100);
        history.begin_coalescing();
        let mut first = Grade::default();
        first.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 0.5 },
        ));
        commit_shared_look_grade(&mut doc, &mut history, id, first.clone(), false);
        let mut second = first;
        second.ops[0].params.base = GradeOpParams::Exposure { stops: 1.0 };
        commit_shared_look_grade(&mut doc, &mut history, id, second.clone(), false);
        history.end_coalescing();
        assert_eq!(
            doc.timeline.as_ref().unwrap().shared_looks[&id].grade,
            second
        );
        assert!(history.undo(&mut doc));
        assert_eq!(
            doc.timeline.as_ref().unwrap().shared_looks[&id].grade,
            Grade::default()
        );
        assert!(!history.undo(&mut doc));
    }

    #[test]
    fn shared_scope_viewing_is_read_only_and_edits_only_the_chosen_grade() {
        use photonic_core::timeline::{
            Clip, ClipSource, FrameRate, GroupKind, GroupNode, Sequence, Tick, TimelineProject,
            Track, TrackKind,
        };
        let mut doc = Document::new("scope panel", 100., 100.);
        let mut project = TimelineProject::new();
        let mut sequence = Sequence::new("Test", FrameRate::FPS_30, 16, 16);
        let mut track = Track::new(TrackKind::Video, "V1");
        let mut clip = Clip::new(ClipSource::Adjustment, Tick(0), Tick(100));
        let (seq_id, track_id, clip_id) = (sequence.id, track.id, clip.id);
        let group = GroupNode::new(GroupKind::Normal);
        let group_id = group.id;
        clip.group = Some(group_id);
        track.clips.push(clip);
        let mut sibling = Clip::new(ClipSource::Adjustment, Tick(100), Tick(100));
        sibling.group = Some(group_id);
        track.clips.push(sibling);
        sequence.video_tracks.push(track);
        sequence.groups.insert(group_id, group);
        project.insert_sequence(sequence);
        doc.timeline = Some(project);
        let mut history = CommandHistory::new(100);
        let before = serde_json::to_value(&doc).unwrap();
        let ctx = egui::Context::default();
        for owner in [
            VfxOwner::Track(track_id),
            VfxOwner::Master(seq_id),
            VfxOwner::GroupPre(group_id),
            VfxOwner::GroupPost(group_id),
        ] {
            let revision = history.revision();
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.data_mut(|data| data.insert_temp(active_grade_scope_id(clip_id), owner));
                    draw_color_controls(
                        ui,
                        &mut doc,
                        &mut history,
                        &mut Vec::new(),
                        &[clip_id],
                        &mut None,
                        &mut ColorPageTab::Wheels,
                        &mut false,
                    );
                });
            });
            assert_eq!(serde_json::to_value(&doc).unwrap(), before);
            assert_eq!(history.revision(), revision);
            let mut grade = Grade::new();
            grade.ops.push(GradeOp::new(
                GradeOpKind::Exposure,
                GradeOpParams::Exposure { stops: 1.0 },
            ));
            commit_grade_scoped(&mut doc, &mut history, owner, grade.clone(), true);
            assert_eq!(current_grade_scoped(&doc, owner), grade);
            assert_eq!(current_grade(&doc, seq_id, track_id, clip_id), Grade::new());
            assert!(history.undo(&mut doc));
            assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        }
    }

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
    fn sampled_curve_anchor_uses_channel_axis_and_existing_output() {
        let rgb = [0.25, 0.5, 0.75];
        let mut red = neutral_curve_points(1);
        assert!(add_curve_sample_anchor(&mut red, 1, rgb));
        assert!((red[1].0 - 0.25).abs() < 1e-6);
        assert!((red[1].1 - 0.25).abs() < 1e-6);
        assert!(!add_curve_sample_anchor(&mut red, 1, rgb));
        assert_eq!(red.len(), 3);

        let mut luma_sat = neutral_curve_points(7);
        assert!(add_curve_sample_anchor(&mut luma_sat, 7, rgb));
        let expected_luma = photonic_render::grade::luma709(rgb);
        assert!((luma_sat[1].0 - expected_luma).abs() < 1e-6);
        assert!((luma_sat[1].1 - 0.5).abs() < 1e-6);

        let mut hue_sat = neutral_curve_points(5);
        assert!(add_curve_sample_anchor(&mut hue_sat, 5, rgb));
        assert!((hue_sat[1].0 - rgb_to_hsl(rgb[0], rgb[1], rgb[2]).0).abs() < 1e-6);
        assert!((hue_sat[1].1 - 0.5).abs() < 1e-6);
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
        let (low_hue, _, _) = seed_qualifier(0.01, 0.8, 0.5);
        let (high_hue, _, _) = seed_qualifier(0.99, 0.8, 0.5);
        assert!(low_hue[0] < 0.0, "red seed crosses zero: {low_hue:?}");
        assert!(high_hue[1] > 1.0, "red seed crosses one: {high_hue:?}");
    }
}
