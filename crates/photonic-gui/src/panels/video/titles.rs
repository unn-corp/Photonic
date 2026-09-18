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
//! drag-to-timeline-position (see point 1), and the
//! `list_title_templates`/`insert_title_template` MCP tools — those need the
//! `VectorDoc`/`AssetSource::EmbeddedVector` path and touch the render + MCP
//! territories too.
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
use egui::{Color32, RichText, Ui};
use egui_phosphor::regular as ph;
use photonic_core::timeline::clip::TextClipContent;
use photonic_core::timeline::{
    ops, CaptionBackground, CaptionStyle, Clip, ClipId, ClipSource, SequenceId, Tick, TimelineCmd,
    TimelineProject, TrackId,
};
use photonic_core::Color;

const MUTED: Color32 = Color32::from_rgb(0x7A, 0x7A, 0x9A); // `secondary`
const SECTION: Color32 = Color32::from_rgb(0x50, 0x50, 0x6E); // section-header dim-muted

/// One starter title preset. 05 §4b's shipped set is `VectorDoc`-templated
/// with entrance/exit keyframes; this is the plain-`ClipSource::Text` subset
/// of it (see module doc's scope cut) — three placements that cover the
/// common cases (bottom-anchored name/role, dead-center hero text, a boxed
/// card).
struct TitlePreset {
    name: &'static str,
    glyph: &'static str,
    description: &'static str,
    sample_text: &'static str,
    duration_secs: i64,
    style: fn() -> CaptionStyle,
}

fn lower_third_style() -> CaptionStyle {
    CaptionStyle {
        font_size: 40.0,
        position: [0.07, 0.84],
        max_width: 0.5,
        stroke: None,
        background: Some(CaptionBackground {
            color: Color::new(0.0, 0.0, 0.0, 0.55),
            corner_radius: 4.0,
            padding: 12.0,
        }),
        ..CaptionStyle::default()
    }
}

fn centered_title_style() -> CaptionStyle {
    CaptionStyle {
        font_size: 96.0,
        weight: 800,
        position: [0.5, 0.5],
        max_width: 0.8,
        ..CaptionStyle::default()
    }
}

fn caption_card_style() -> CaptionStyle {
    CaptionStyle {
        font_size: 34.0,
        weight: 600,
        position: [0.5, 0.5],
        max_width: 0.6,
        stroke: None,
        background: Some(CaptionBackground {
            color: Color::new(0.08, 0.08, 0.12, 0.85),
            corner_radius: 10.0,
            padding: 16.0,
        }),
        ..CaptionStyle::default()
    }
}

const PRESETS: &[TitlePreset] = &[
    TitlePreset {
        name: "Lower Third",
        glyph: ph::TEXT_ALIGN_LEFT,
        description: "Name / role bar anchored bottom-left — the classic interview caption.",
        sample_text: "Name Here\nRole / Title",
        duration_secs: 5,
        style: lower_third_style,
    },
    TitlePreset {
        name: "Centered Title",
        glyph: ph::TEXT_AA,
        description: "Large hero text, dead center — cold opens and chapter cards.",
        sample_text: "Title Goes Here",
        duration_secs: 4,
        style: centered_title_style,
    },
    TitlePreset {
        name: "Caption Card",
        glyph: ph::CARDS,
        description: "Boxed text card for callouts, quotes, and CTAs.",
        sample_text: "Caption text goes here.",
        duration_secs: 4,
        style: caption_card_style,
    },
];

/// Left-rail Titles drawer: the starter-preset browser plus the selected
/// Text clip's basic editor.
pub(crate) fn draw_titles(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let Some(project) = ctx.doc.timeline.as_ref() else {
        ui.label(RichText::new("No video project yet.").color(MUTED));
        return;
    };

    let mut action: Option<PanelAction> = None;
    let playhead = ctx.video.playhead;

    section_header(ui, "STARTER TITLES");
    if project.active_sequence.is_none() {
        ui.label(
            RichText::new("Add a sequence first to place a title.")
                .color(MUTED)
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
    draw_text_editor(ui, ctx.video.selection, project, &mut action);

    if action.is_some() {
        ctx.action = action;
    }
}

/// A small, muted section heading (mirrors `effects_browser.rs`'s
/// `section_header` idiom — DESIGN.md's `#50506E` "dim-muted" token).
fn section_header(ui: &mut Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text).small().color(SECTION));
    ui.add_space(2.0);
}

fn draw_preset_row(
    ui: &mut Ui,
    project: &TimelineProject,
    preset: &TitlePreset,
    playhead: Tick,
    action: &mut Option<PanelAction>,
) {
    let label = format!("{}  {}", preset.glyph, preset.name);
    let resp = ui
        .add(egui::SelectableLabel::new(false, label))
        .on_hover_text(format!(
            "{} — click to insert a {}s title at the playhead",
            preset.description, preset.duration_secs
        ));
    if resp.clicked() {
        if let Some(cmd) = insert_preset_at_playhead(project, preset, playhead) {
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
/// insert (no video track, no room anywhere) is a silent no-op, matching
/// `ops_bridge.rs`'s own documented convention for invalid gestures.
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
                .color(MUTED)
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
                    .speed(0.01),
            )
            .changed();
        changed |= ui
            .add(
                egui::DragValue::new(&mut style.position[1])
                    .range(0.0..=1.0)
                    .speed(0.01),
            )
            .changed();
    });

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
            true,
        );
    }
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
