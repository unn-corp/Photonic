//! `DrawerGroup::Titles` panel (04 §4.1) — G-12 minimal
//! (17-nle-parity-round2.md §G-12 / 05-import-export.md §4b).
//!
//! G-12 is rated **Larger** in the round-2 gap list — a mini-spec-sized
//! feature spanning `core-timeline` (a text/graphics `ClipSource`), the
//! engine (title-clip render), and `panels-video` (the editor UI), plus a
//! full "starter title-template library" (05 §4b: `VectorDoc` snippets with
//! pre-authored entrance/exit `PropertyTrack` keyframes) and Pin-To
//! responsive-position reframing / protected intro-outro trims. This panel
//! ships the slice that fits inside `panels-video` alone, over the plain
//! `ClipSource::Text` (styled text via the `CaptionStyle` cascade, rendered
//! by the engine's `TextGen` op) that core already landed:
//!
//! 1. A **starter title browser**: a handful of built-in presets (lower
//!    third / centered title / caption card — 05 §4b's shipped-set naming,
//!    the plain-text subset of it) that insert a `Text` clip at the playhead
//!    on the first video track with room, on click — the load-bearing
//!    interaction (same precedent as `effects_browser.rs`'s
//!    double-click-to-apply "not a fallback afterthought" note). Real
//!    drag-and-drop onto a specific timeline-lane position would need a drop
//!    target inside `app/timeline/clips.rs` (out of this panel's territory,
//!    and — unlike `effects_browser::EffectDrag`, which drops onto the Clip
//!    Inspector's effects stack — there's no other non-timeline drop target
//!    a title preset could sensibly land on), so it's left as a reported gap
//!    rather than an inert `dnd_drag_source` no one consumes.
//! 2. A **basic Text-clip editor**: text / font size / fill color / position
//!    for the timeline-selected clip, when it's a `Text` clip.
//!
//! **Not** in this cut (residual G-12 work, tracked in 17): `VectorDoc`
//! template presets with pre-authored entrance/exit keyframes, Pin-To
//! responsive-position reframing, protected intro/outro trims, real
//! drag-to-timeline-position (see point 1). The starter text presets are shared
//! with MCP.
//!
//! Every mutation is built as a real `photonic_core::timeline::ops::*` call
//! (reading `ctx.doc`, never mutating it) and handed to the app via
//! [`crate::panels::PanelAction::ClipEditDiscrete`]/`ClipEditCoalesced` — the
//! same `ops_bridge`-at-the-drawer-boundary idiom every other `PropPanelCtx`
//! panel uses (`clip_inspector.rs`, `effects_browser.rs`). The one addition:
//! `PropPanelCtx` otherwise carries no live playhead
//! (`keyframe_editor.rs`'s `no_live_playhead` note), so inserting "at the
//! playhead" for real (not a stubbed position) needed the actual value —
//! threaded read-only via [`super::VideoPanelUi::playhead`], tagged
//! `[titles]` there and wired at both `VideoPanelUi` construction sites in
//! `app/mod.rs`.

use crate::panels::{PanelAction, PropPanelCtx};
use egui::{RichText, Ui};
use egui_phosphor::regular as ph;
use photonic_core::timeline::clip::TextClipContent;
use photonic_core::timeline::title_presets::{TitlePreset, TITLE_PRESETS as PRESETS};
use photonic_core::timeline::{
    ops, Clip, ClipId, ClipSource, SequenceId, Tick, TimelineCmd, TimelineProject, TrackId,
};

/// Left-rail Titles drawer: the starter-preset browser plus the selected
/// Text clip's basic editor.
pub(crate) fn draw_titles(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let Some(project) = ctx.doc.timeline.as_ref() else {
        ui.label(
            RichText::new("No video project yet.").color(crate::theme::section_header_color(ui)),
        );
        return;
    };

    let mut action: Option<PanelAction> = None;
    let playhead = ctx.video.playhead;

    section_header(ui, "STARTER TITLES");
    if project.active_sequence.is_none() {
        ui.label(
            RichText::new("Add a sequence first to place a title.")
                .color(crate::theme::section_header_color(ui))
                .small(),
        );
    } else {
        for preset in PRESETS.iter() {
            if ctx.matches(preset.name) {
                draw_preset_row(ui, project, preset, playhead, &mut action);
            }
        }
    }

    ui.add_space(8.0);
    section_header(ui, "EDIT TITLE");
    ui.push_id(ctx.doc.id, |ui| {
        draw_text_editor(ui, ctx.video.selection, project, &mut action)
    });

    if action.is_some() {
        ctx.action = action;
    }
}

fn section_header(ui: &mut Ui, text: &str) {
    crate::theme::section_header(ui, text);
}

fn draw_preset_row(
    ui: &mut Ui,
    project: &TimelineProject,
    preset: &TitlePreset,
    playhead: Tick,
    action: &mut Option<PanelAction>,
) {
    let label = format!("{}  {}", ph::TEXT_AA, preset.name);
    let command = insert_preset_at_playhead(project, preset, playhead);
    let resp = ui
        .add_enabled(command.is_some(), egui::SelectableLabel::new(false, label))
        .on_hover_text(format!(
            "{} — click to insert a {}s title at the playhead",
            preset.description, preset.duration_secs
        ))
        .on_disabled_hover_text(
            "Add an unlocked video or text track with enough free space at the playhead.",
        );
    if resp.clicked() {
        if let Some(cmd) = command {
            *action = Some(PanelAction::ClipEditDiscrete(cmd));
        }
    }
}

/// Build `preset`'s content and find the `InsertClip` command on the first
/// video track (top to bottom) that has room at `playhead` — the click/drag
/// "load-bearing, no-drag" insert path (mirrors `MediaInsertAtPlayhead`'s
/// asset analogue, `ops_bridge::insert_asset_at_first_fit`, reimplemented
/// here purely over `&TimelineProject` since this panel has no
/// `&mut CommandHistory` to call that helper with — same `PropPanelCtx`
/// boundary every other left-rail video drawer works within). A rejected
/// insert disables the preset and explains how to make room.
fn insert_preset_at_playhead(
    project: &TimelineProject,
    preset: &TitlePreset,
    playhead: Tick,
) -> Option<TimelineCmd> {
    let seq_id = project.active_sequence?;
    let seq = project.sequences.get(&seq_id)?;
    let content = TextClipContent {
        text: preset.sample_text.to_string(),
        style: (preset.style)(),
    };
    let duration = Tick::from_seconds(preset.duration_secs);
    // Prefer a dedicated Text track (add one via "+ Text" in the timeline);
    // fall back to any video track with room so titles still work without one.
    use photonic_core::timeline::TrackKind;
    let text_first = seq
        .video_tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Text)
        .chain(
            seq.video_tracks
                .iter()
                .filter(|t| t.kind == TrackKind::Video),
        );
    for track in text_first {
        if let Ok(cmd) = ops::add_text_clip(
            project,
            seq_id,
            track.id,
            playhead,
            duration,
            content.clone(),
        ) {
            return Some(cmd);
        }
    }
    None
}

// ── Text-clip editor (basic: text / size / color / position) ──────────────

/// Resolve the first `selection` entry that is a `Text` clip, if any. Scans
/// every track (`ClipId`s carry no track hint) — same O(clips) lookup
/// `clip_inspector.rs`'s `locate_clip` uses, duplicated locally per that
/// file's own stated convention (each panel stays independently buildable).
fn selected_text_clip<'p>(
    selection: &[ClipId],
    project: &'p TimelineProject,
) -> Option<(SequenceId, TrackId, &'p Clip, &'p TextClipContent)> {
    for &clip_id in selection {
        for (seq_id, seq) in &project.sequences {
            for track in seq.tracks() {
                if let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) {
                    if let ClipSource::Text { content } = &clip.source {
                        return Some((*seq_id, track.id, clip, content));
                    }
                }
            }
        }
    }
    None
}

fn draw_text_editor(
    ui: &mut Ui,
    selection: &[ClipId],
    project: &TimelineProject,
    action: &mut Option<PanelAction>,
) {
    let Some((seq_id, track_id, clip, content)) = selected_text_clip(selection, project) else {
        ui.label(
            RichText::new("Select a Text clip to edit its title.")
                .color(crate::theme::section_header_color(ui))
                .small(),
        );
        return;
    };
    let clip_id = clip.id;

    ui.label(RichText::new(&clip.name).strong());
    ui.add_space(2.0);

    // ── Text (commit on Enter/blur, Escape cancels — mirrors
    // `caption_editor.rs`'s cue-text idiom; committing per-keystroke would
    // flood undo history with one step per character) ──────────────────────
    let text_buf_id = ui.id().with(("titles_text_buf", clip_id));
    let mut buf: String = ui
        .data(|d| d.get_temp(text_buf_id))
        .unwrap_or_else(|| content.text.clone());
    ui.label("Text:");
    let resp = ui.add(egui::TextEdit::multiline(&mut buf).desired_rows(2));
    if resp.lost_focus() {
        let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if !escaped && buf != content.text {
            let mut new_content = content.clone();
            new_content.text = buf;
            commit_text_edit(
                project,
                seq_id,
                track_id,
                clip_id,
                new_content,
                action,
                false,
            );
        }
        ui.data_mut(|d| d.remove::<String>(text_buf_id));
    } else {
        ui.data_mut(|d| d.insert_temp(text_buf_id, buf));
    }

    // ── Size / color / position (auto-commit + coalesced — mirrors
    // `clip_inspector.rs`'s `transform_row` + `set_clip_coalesced` idiom;
    // `SetClipProp` supports coalescing, so per-frame commits during one drag
    // gesture fold into a single undo step) ─────────────────────────────────
    let mut style = content.style.clone();
    let mut changed = false;

    ui.horizontal(|ui| {
        ui.label("Size:");
        changed |= ui
            .add(
                egui::DragValue::new(&mut style.font_size)
                    .range(8.0..=400.0)
                    .clamp_existing_to_range(false)
                    .speed(0.5),
            )
            .changed();
    });
    ui.horizontal(|ui| {
        ui.label("Color:");
        changed |= crate::color_popup::ColorPopup::swatch_color(ui, &mut style.fill).changed();
    });
    ui.horizontal(|ui| {
        ui.label("Position:");
        changed |= ui
            .add(
                egui::DragValue::new(&mut style.position[0])
                    .range(0.0..=1.0)
                    .clamp_existing_to_range(false)
                    .speed(0.01),
            )
            .changed();
        changed |= ui
            .add(
                egui::DragValue::new(&mut style.position[1])
                    .range(0.0..=1.0)
                    .clamp_existing_to_range(false)
                    .speed(0.01),
            )
            .changed();
    });

    let native = matches!(
        project.sequences[&seq_id].color,
        photonic_core::timeline::color::SequenceColorConfig::NativeManaged(_)
    );
    let mut animation_changed = false;
    if native {
        ui.horizontal(|ui| {
            ui.label("Entrance:");
            for (value, label) in [
                (photonic_core::timeline::CaptionAnim::None, "None"),
                (photonic_core::timeline::CaptionAnim::SlideUp, "Slide up"),
            ] {
                animation_changed |= ui
                    .selectable_value(&mut style.animation, value, label)
                    .changed();
            }
        });
        ui.small("Slide up: whole title and paint, 200ms from clip start.");
        changed |= animation_changed;
    }
    let decorated = photonic_video::graph::compile::native_text_decoration(&style).is_err()
        || style.highlight.is_some()
        || !matches!(
            style.animation,
            photonic_core::timeline::CaptionAnim::None
                | photonic_core::timeline::CaptionAnim::SlideUp
        );
    let mut plain_clicked = false;
    if native && decorated {
        ui.label("This title has unsupported styling. Managed outline ≤8px; box padding/radius ≤128px; title karaoke and word-timed animations are not yet qualified.");
        let candidate = plain_title_command(project, seq_id, track_id, clip_id, content);
        if ui
            .add_enabled(
                candidate.is_some() && action.is_none() && !changed,
                egui::Button::new("Use plain fill (remove decorations)"),
            )
            .clicked()
        {
            if let Some(command) = candidate {
                *action = Some(PanelAction::ClipEditDiscrete(command));
                plain_clicked = true;
            }
        }
    }
    if plain_clicked {
        return;
    }

    if changed {
        let mut new_content = content.clone();
        new_content.style = style;
        commit_text_edit(
            project,
            seq_id,
            track_id,
            clip_id,
            new_content,
            action,
            !animation_changed,
        );
    }
    if native {
        draw_title_paint_editor(ui, project, seq_id, track_id, clip_id, content, action);
    }
}

#[derive(Clone, PartialEq)]
struct TitlePaintValues {
    stroke: Option<(photonic_core::Color, f32)>,
    background: Option<photonic_core::timeline::CaptionBackground>,
}
impl TitlePaintValues {
    fn from_content(content: &TextClipContent) -> Self {
        Self {
            stroke: content.style.stroke,
            background: content.style.background,
        }
    }
    fn apply(&self, content: &mut TextClipContent) {
        content.style.stroke = self.stroke;
        content.style.background = self.background;
    }
}
#[derive(Clone)]
struct TitlePaintDraft {
    base: TitlePaintValues,
    edited: TitlePaintValues,
}

fn title_paint_command(
    project: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    clip: ClipId,
    content: &TextClipContent,
    paint: &TitlePaintValues,
) -> Option<TimelineCmd> {
    if &TitlePaintValues::from_content(content) == paint {
        return None;
    }
    let mut next = content.clone();
    paint.apply(&mut next);
    photonic_video::graph::compile::native_text_decoration(&next.style).ok()?;
    ops::replace_clip_source(
        project,
        seq,
        track,
        clip,
        ClipSource::Text { content: next },
        None,
    )
    .ok()
}

fn title_paint_fields(ui: &mut Ui, paint: &mut TitlePaintValues) {
    let mut outline = paint.stroke.is_some();
    if ui.checkbox(&mut outline, "Outline").changed() {
        paint.stroke = outline.then_some((photonic_core::Color::BLACK, 2.0));
    }
    if let Some((color, width)) = &mut paint.stroke {
        ui.horizontal(|ui| {
            crate::color_popup::ColorPopup::swatch_color(ui, color);
            ui.label("Width");
            ui.add(
                egui::DragValue::new(width)
                    .range(0.0..=8.0)
                    .clamp_existing_to_range(false)
                    .speed(0.1),
            );
        });
    }
    let mut background = paint.background.is_some();
    if ui.checkbox(&mut background, "Background").changed() {
        paint.background = background.then_some(photonic_core::timeline::CaptionBackground {
            color: photonic_core::Color::new(0.0, 0.0, 0.0, 0.6),
            padding: 8.0,
            corner_radius: 4.0,
        });
    }
    if let Some(background) = &mut paint.background {
        crate::color_popup::ColorPopup::swatch_color(ui, &mut background.color);
        ui.horizontal(|ui| {
            ui.label("Padding");
            ui.add(
                egui::DragValue::new(&mut background.padding)
                    .range(0.0..=128.0)
                    .clamp_existing_to_range(false)
                    .speed(0.5),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Radius");
            ui.add(
                egui::DragValue::new(&mut background.corner_radius)
                    .range(0.0..=128.0)
                    .clamp_existing_to_range(false)
                    .speed(0.5),
            );
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_title_paint_editor(
    ui: &mut Ui,
    project: &TimelineProject,
    seq: SequenceId,
    track: TrackId,
    clip: ClipId,
    content: &TextClipContent,
    action: &mut Option<PanelAction>,
) {
    let key = ui.id().with(("title_paint_draft", seq, clip));
    let base = TitlePaintValues::from_content(content);
    let mut draft = ui
        .data(|d| d.get_temp::<TitlePaintDraft>(key))
        .filter(|d| d.base == base)
        .unwrap_or_else(|| TitlePaintDraft {
            base: base.clone(),
            edited: base.clone(),
        });
    ui.collapsing("Outline and background", |ui| {
        title_paint_fields(ui, &mut draft.edited);
        let mut proposed = content.clone();
        draft.edited.apply(&mut proposed);
        if let Err(error) = photonic_video::graph::compile::native_text_decoration(&proposed.style)
        {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        let command = title_paint_command(project, seq, track, clip, content, &draft.edited);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    action.is_none() && command.is_some(),
                    egui::Button::new("Apply paint"),
                )
                .clicked()
            {
                if let Some(cmd) = command {
                    *action = Some(PanelAction::ClipEditDiscrete(cmd));
                }
            }
            if ui
                .add_enabled(draft.edited != base, egui::Button::new("Revert paint"))
                .clicked()
            {
                draft.edited = base.clone();
            }
        });
        ui.small("Outer outline: up to 8px. Box padding/radius: up to 128px.");
    });
    ui.data_mut(|d| d.insert_temp(key, draft));
}

fn plain_title_command(
    project: &TimelineProject,
    seq_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    content: &TextClipContent,
) -> Option<TimelineCmd> {
    let mut plain = content.clone();
    plain.style.stroke = None;
    plain.style.background = None;
    plain.style.highlight = None;
    plain.style.animation = photonic_core::timeline::CaptionAnim::None;
    if &plain == content {
        return None;
    }
    ops::replace_clip_source(
        project,
        seq_id,
        track_id,
        clip_id,
        ClipSource::Text { content: plain },
        None,
    )
    .ok()
}

fn commit_text_edit(
    project: &TimelineProject,
    seq_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    new_content: TextClipContent,
    action: &mut Option<PanelAction>,
    coalesced: bool,
) {
    if let Ok(cmd) = ops::replace_clip_source(
        project,
        seq_id,
        track_id,
        clip_id,
        ClipSource::Text {
            content: new_content,
        },
        None,
    ) {
        *action = Some(if coalesced {
            PanelAction::ClipEditCoalesced(cmd)
        } else {
            PanelAction::ClipEditDiscrete(cmd)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::{FrameRate, Sequence, Track, TrackKind};

    fn project_with_video_track() -> (TimelineProject, SequenceId, TrackId) {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("s", FrameRate::new(30, 1), 1920, 1080);
        let track = Track::new(TrackKind::Video, "V1");
        let track_id = track.id;
        seq.video_tracks.push(track);
        let seq_id = project.insert_sequence(seq);
        project.active_sequence = Some(seq_id);
        (project, seq_id, track_id)
    }

    #[test]
    fn native_title_slide_view_is_read_only() {
        use photonic_core::timeline::{
            color::{NativeManagedColorConfig, SequenceColorConfig},
            CaptionAnim,
        };
        let (mut project, seq, _) = project_with_video_track();
        project.sequences.get_mut(&seq).unwrap().color =
            SequenceColorConfig::NativeManaged(Box::new(NativeManagedColorConfig::sdr_draft()));
        let mut content = TextClipContent::new("Keep animated title");
        content.style.animation = CaptionAnim::SlideUp;
        let clip = Clip::new(
            ClipSource::Text { content },
            Tick::ZERO,
            Tick::from_seconds(2),
        );
        let id = clip.id;
        project.sequences.get_mut(&seq).unwrap().video_tracks[0]
            .clips
            .push(clip);
        let original = serde_json::to_value(&project).unwrap();
        let ctx = egui::Context::default();
        for _ in 0..3 {
            let mut action = None;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default()
                    .show(ctx, |ui| draw_text_editor(ui, &[id], &project, &mut action));
            });
            assert!(action.is_none());
            assert_eq!(serde_json::to_value(&project).unwrap(), original);
        }
    }
    #[test]
    fn native_title_slide_edit_is_discrete_reversible_and_locked() {
        let (mut project, seq, track) = project_with_video_track();
        let original = TextClipContent::new("Keep title paint");
        let clip = Clip::new(
            ClipSource::Text {
                content: original.clone(),
            },
            Tick::ZERO,
            Tick::from_seconds(2),
        );
        let id = clip.id;
        project.sequences.get_mut(&seq).unwrap().video_tracks[0]
            .clips
            .push(clip);
        let mut next = original.clone();
        next.style.animation = photonic_core::timeline::CaptionAnim::SlideUp;
        let mut action = None;
        commit_text_edit(&project, seq, track, id, next.clone(), &mut action, false);
        let Some(PanelAction::ClipEditDiscrete(command)) = action else {
            panic!("one discrete animation edit")
        };
        let mut doc = photonic_core::Document::new("title slide", 1920.0, 1080.0);
        doc.timeline = Some(project.clone());
        let before = serde_json::to_value(&doc).unwrap();
        let inverse = command.inverse(&doc).unwrap();
        command.apply(&mut doc);
        assert_eq!(
            doc.timeline.as_ref().unwrap().sequences[&seq].video_tracks[0].clips[0].source,
            ClipSource::Text {
                content: next.clone()
            }
        );
        inverse.apply(&mut doc);
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        project.sequences.get_mut(&seq).unwrap().video_tracks[0].locked = true;
        let mut action = None;
        commit_text_edit(&project, seq, track, id, next, &mut action, false);
        assert!(action.is_none());
    }

    #[test]
    fn title_panel_view_does_not_clamp_stored_values_or_emit_an_edit() {
        let (mut project, seq, _) = project_with_video_track();
        let mut content = TextClipContent::new("Read only");
        content.style.font_size = 512.0;
        content.style.position = [1.25, -0.25];
        let clip = Clip::new(
            ClipSource::Text { content },
            Tick::ZERO,
            Tick::from_seconds(2),
        );
        let id = clip.id;
        project.sequences.get_mut(&seq).unwrap().video_tracks[0]
            .clips
            .push(clip);
        let before = serde_json::to_value(&project).unwrap();
        let mut action = None;
        let context = egui::Context::default();
        let _ = context.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default()
                .show(ctx, |ui| draw_text_editor(ui, &[id], &project, &mut action));
        });
        assert!(action.is_none());
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
    }

    #[test]
    fn title_paint_edit_is_atomic_validated_and_preserves_unrelated_content() {
        let (mut project, seq, track) = project_with_video_track();
        let content = TextClipContent::new("Keep title");
        let clip = Clip::new(
            ClipSource::Text {
                content: content.clone(),
            },
            Tick::ZERO,
            Tick::from_seconds(2),
        );
        let id = clip.id;
        project.sequences.get_mut(&seq).unwrap().video_tracks[0]
            .clips
            .push(clip);
        let original = TitlePaintValues::from_content(&content);
        assert!(title_paint_command(&project, seq, track, id, &content, &original).is_none());
        let paint = TitlePaintValues {
            stroke: Some((photonic_core::Color::RED, 3.0)),
            background: Some(photonic_core::timeline::CaptionBackground {
                color: photonic_core::Color::BLACK,
                padding: 6.0,
                corner_radius: 5.0,
            }),
        };
        let cmd = title_paint_command(&project, seq, track, id, &content, &paint).unwrap();
        let mut doc = photonic_core::Document::new("paint", 128.0, 64.0);
        doc.timeline = Some(project.clone());
        let before = serde_json::to_value(&doc).unwrap();
        let inv = cmd.inverse(&doc).unwrap();
        cmd.apply(&mut doc);
        let ClipSource::Text { content: after } =
            &doc.timeline.as_ref().unwrap().sequences[&seq].video_tracks[0].clips[0].source
        else {
            panic!("title")
        };
        let mut expected = content.clone();
        paint.apply(&mut expected);
        assert_eq!(after, &expected);
        inv.apply(&mut doc);
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        let mut invalid = paint.clone();
        invalid.stroke = Some((photonic_core::Color::RED, 9.0));
        assert!(title_paint_command(&project, seq, track, id, &content, &invalid).is_none());
        project.sequences.get_mut(&seq).unwrap().video_tracks[0].locked = true;
        assert!(title_paint_command(&project, seq, track, id, &content, &paint).is_none());
    }
    #[test]
    fn title_paint_view_does_not_clamp_or_commit_stored_values() {
        let ctx = egui::Context::default();
        let mut paint = TitlePaintValues {
            stroke: Some((photonic_core::Color::RED, 20.0)),
            background: Some(photonic_core::timeline::CaptionBackground {
                color: photonic_core::Color::BLACK,
                padding: 200.0,
                corner_radius: 300.0,
            }),
        };
        let before = paint.clone();
        for _ in 0..3 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| title_paint_fields(ui, &mut paint));
            });
            assert!(paint == before);
        }
    }

    #[test]
    fn plain_title_edit_is_discrete_reversible_and_respects_track_lock() {
        let (mut project, seq, track) = project_with_video_track();
        let content = TextClipContent::new("Keep this text");
        let clip = Clip::new(
            ClipSource::Text {
                content: content.clone(),
            },
            Tick::ZERO,
            Tick::from_seconds(2),
        );
        let id = clip.id;
        project.sequences.get_mut(&seq).unwrap().video_tracks[0]
            .clips
            .push(clip);
        let command = plain_title_command(&project, seq, track, id, &content).unwrap();
        let mut doc = photonic_core::Document::new("title", 128.0, 64.0);
        doc.timeline = Some(project.clone());
        let before = serde_json::to_value(&doc).unwrap();
        let undo = command.inverse(&doc).unwrap();
        command.apply(&mut doc);
        let changed = &doc.timeline.as_ref().unwrap().sequences[&seq].video_tracks[0].clips[0];
        let ClipSource::Text { content: plain } = &changed.source else {
            panic!("title source")
        };
        assert_eq!(plain.text, content.text);
        assert_eq!(plain.style.fill, content.style.fill);
        assert!(plain.style.stroke.is_none());
        assert!(
            plain_title_command(doc.timeline.as_ref().unwrap(), seq, track, id, plain).is_none()
        );
        let restored = photonic_core::Document::from_json(&doc.to_json().unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&restored).unwrap(),
            serde_json::to_value(&doc).unwrap()
        );
        undo.apply(&mut doc);
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        project.sequences.get_mut(&seq).unwrap().video_tracks[0].locked = true;
        assert!(plain_title_command(&project, seq, track, id, &content).is_none());
    }

    #[test]
    fn insert_preset_at_playhead_lands_on_the_first_video_track() {
        let (project, seq_id, track_id) = project_with_video_track();
        let preset = &PRESETS[0];
        let cmd = insert_preset_at_playhead(&project, preset, Tick::from_seconds(2))
            .expect("insert should succeed on an empty track");
        match cmd {
            TimelineCmd::InsertClip { seq, track, clip } => {
                assert_eq!(seq, seq_id);
                assert_eq!(track, track_id);
                assert_eq!(clip.start, Tick::from_seconds(2));
                assert_eq!(clip.duration, Tick::from_seconds(preset.duration_secs));
                match &clip.source {
                    ClipSource::Text { content } => {
                        assert_eq!(content.text, preset.sample_text);
                    }
                    other => panic!("expected ClipSource::Text, got {other:?}"),
                }
            }
            other => panic!("expected InsertClip, got {other:?}"),
        }
    }

    #[test]
    fn insert_preset_at_playhead_skips_a_track_with_no_room() {
        let (mut project, seq_id, _blocked_track) = project_with_video_track();
        let open_track = Track::new(TrackKind::Video, "V2");
        let open_track_id = open_track.id;
        {
            let seq = project.sequences.get_mut(&seq_id).unwrap();
            // Block V1 across the whole preset duration.
            seq.video_tracks[0].clips.push(Clip::new(
                ClipSource::Adjustment,
                Tick::ZERO,
                Tick::from_seconds(10),
            ));
            seq.video_tracks.push(open_track);
        }

        let preset = &PRESETS[0];
        let cmd = insert_preset_at_playhead(&project, preset, Tick::ZERO)
            .expect("insert should fall through to V2");
        match cmd {
            TimelineCmd::InsertClip { track, .. } => assert_eq!(track, open_track_id),
            other => panic!("expected InsertClip, got {other:?}"),
        }
    }

    #[test]
    fn insert_preset_at_playhead_is_none_without_an_active_sequence() {
        let project = TimelineProject::new();
        let preset = &PRESETS[0];
        assert!(insert_preset_at_playhead(&project, preset, Tick::ZERO).is_none());
    }

    #[test]
    fn selected_text_clip_finds_the_first_text_clip_in_selection() {
        let (mut project, seq_id, track_id) = project_with_video_track();
        let text_clip = Clip::new(
            ClipSource::Text {
                content: TextClipContent::new("Hello"),
            },
            Tick::ZERO,
            Tick::from_seconds(4),
        );
        let text_clip_id = text_clip.id;
        let adj_clip = Clip::new(
            ClipSource::Adjustment,
            Tick::from_seconds(5),
            Tick::from_seconds(2),
        );
        let adj_clip_id = adj_clip.id;
        {
            let seq = project.sequences.get_mut(&seq_id).unwrap();
            let track = seq
                .video_tracks
                .iter_mut()
                .find(|t| t.id == track_id)
                .unwrap();
            track.clips.push(adj_clip.clone());
            track.clips.push(text_clip.clone());
        }

        // The non-text clip is selected first — it must be skipped in favor
        // of the text clip later in the selection.
        let selection = [adj_clip_id, text_clip_id];
        let found = selected_text_clip(&selection, &project).expect("a text clip is selected");
        assert_eq!(found.2.id, text_clip_id);
        assert_eq!(found.3.text, "Hello");
    }

    #[test]
    fn selected_text_clip_is_none_when_selection_has_no_text_clip() {
        let (mut project, seq_id, track_id) = project_with_video_track();
        let adj_clip = Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick::from_seconds(2));
        let adj_clip_id = adj_clip.id;
        {
            let seq = project.sequences.get_mut(&seq_id).unwrap();
            let track = seq
                .video_tracks
                .iter_mut()
                .find(|t| t.id == track_id)
                .unwrap();
            track.clips.push(adj_clip);
        }
        assert!(selected_text_clip(&[adj_clip_id], &project).is_none());
    }
}
