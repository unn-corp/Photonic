//! `DrawerGroup::Captions` panel (04 §4.1) — caption track list, cue text/timing
//! editor, style panel (track→cue→word cascade), karaoke preset picker,
//! "Auto-caption" (mock transcription provider), and a TTS mini-panel.
//! Interior owned by 06-captions-ai.md. The cue under edit lives on
//! [`super::VideoPanelUi::caption_edit_cue`].
//!
//! ## Wiring (CAP-019 parity)
//! Every mutation is built as one or more [`TimelineCmd`]s using the same
//! pure `photonic_core::timeline::ops`/`CaptionCmd` constructors the MCP
//! caption tools use (`photonic-mcp/src/handlers/video.rs`), then handed to
//! the app via `ctx.action = Some(PanelAction::CaptionEditBatch(cmds))` — a
//! small, additive `PanelAction::CaptionEditBatch(Vec<TimelineCmd>)` variant
//! (`panels/mod.rs`) with one matching arm in `app/panel_actions.rs`, mirroring
//! `MediaImportDialog`'s shape (a `PropPanelCtx`-based drawer carries `doc: &
//! Document` for reads but no `&mut CommandHistory`, so the drawer builds the
//! already-validated `TimelineCmd`(s) itself and hands them up as one undo
//! step). No direct `doc` mutation happens in this module.
//!
//! ## Async jobs without new `PhotonicApp` fields
//! "Auto-caption" and the TTS generator run the real, committed
//! `photonic_video::captions` providers (`MockTranscriptionProvider` /
//! `MockTtsProvider` — the deterministic providers CI is allowed to depend
//! on, same as the MCP `auto_caption`/`generate_voiceover` tools' `"mock"`
//! path) on a background thread, exactly like the media-pool import worker
//! (`panels/media_pool.rs`). Since this panel's territory is this file alone
//! (no new `PhotonicApp` session field), the job's `Receiver` is parked in
//! egui's per-widget temp memory (`ui.data_mut`) behind `Arc<Mutex<_>>` (the
//! `Clone` egui's temp map requires) and polled once per frame, requesting a
//! repaint while pending — the same non-continuous-repaint discipline
//! `app/mod.rs` uses for its other background checks.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

use egui::{Color32, RichText, Ui};
use egui_phosphor::regular as ph;

use photonic_core::timeline::{
    ops, CaptionAnim, CaptionBackground, CaptionCmd, CaptionCue, CaptionStyle, CaptionTrack,
    CaptionWord, Clip, ClipId, ClipSource, CueId, KaraokeMode, KaraokeStyle, MediaAsset, Sequence,
    SequenceId, StyleTarget, Tick, TimelineCmd, Track, TrackId, TICKS_PER_SECOND,
};
use photonic_core::Color;
use photonic_video::captions::{
    group_words_into_cues, CancelToken, GroupingParams, MockTranscriptionProvider, MockTtsProvider,
    TranscribedWord, TranscriptionProvider, TranscriptionRequest, TtsProvider, TtsRequest,
    VoiceDescriptor,
};

use crate::color_popup::ColorPopup;
use crate::panels::{PanelAction, PropPanelCtx};

// ── Pure helpers (unit-tested below) ────────────────────────────────────────

/// Whole+fractional seconds, UI-edge only (never fed back into the data
/// model as anything but a fresh [`Tick`]).
fn tick_from_seconds_f64(secs: f64) -> Tick {
    Tick((secs.max(0.0) * TICKS_PER_SECOND as f64).round() as i64)
}

/// `M:SS.d` readout, mirrors `media_pool::format_duration`.
fn format_tc(t: Tick) -> String {
    let total_ds = (t.0.max(0) * 10) / TICKS_PER_SECOND;
    let m = total_ds / 600;
    let s = (total_ds % 600) / 10;
    let d = total_ds % 10;
    format!("{m}:{s:02}.{d}")
}

/// Style cascade resolution: word override → cue override → track style
/// (01 §7, 06 §4). Pure so it's unit-testable independent of egui/doc state.
fn resolve_style(
    track: &CaptionTrack,
    cue: Option<&CaptionCue>,
    word: Option<usize>,
) -> CaptionStyle {
    if let (Some(c), Some(wi)) = (cue, word) {
        if let Some(w) = c.words.get(wi) {
            if let Some(s) = &w.style_override {
                return s.clone();
            }
        }
    }
    if let Some(c) = cue {
        if let Some(s) = &c.style_override {
            return s.clone();
        }
    }
    track.style.clone()
}

/// The raw override stored at exactly `target` (not cascade-resolved) — the
/// `old` value a `SetStyle` command must record. `Track` always has a
/// concrete style (never "unset").
fn raw_style_override(track: &CaptionTrack, target: &StyleTarget) -> Option<CaptionStyle> {
    match target {
        StyleTarget::Track => Some(track.style.clone()),
        StyleTarget::Cue(id) => track
            .cues
            .iter()
            .find(|c| c.id == *id)
            .and_then(|c| c.style_override.clone()),
        StyleTarget::Word(id, wi) => track
            .cues
            .iter()
            .find(|c| c.id == *id)
            .and_then(|c| c.words.get(*wi))
            .and_then(|w| w.style_override.clone()),
    }
}

/// Whole-style looks for social velocity (proposal 213). Distinct from karaoke
/// *mode* chips — these rewrite font/background/highlight for Clean / Karaoke /
/// Social delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptionLook {
    /// White text, light black stroke, no box — broadcast-clean.
    Clean,
    /// Karaoke word-pop with yellow active highlight.
    Karaoke,
    /// Large bold + semi-opaque pill background — CapCut/TikTok social default.
    Social,
}

const CAPTION_LOOKS: &[(CaptionLook, &str)] = &[
    (CaptionLook::Clean, "Clean"),
    (CaptionLook::Karaoke, "Karaoke"),
    (CaptionLook::Social, "Social"),
];

/// Build a full [`CaptionStyle`] for a look preset (pure — unit-tested).
fn caption_look_style(look: CaptionLook) -> CaptionStyle {
    match look {
        CaptionLook::Clean => CaptionStyle {
            font_family: "sans-serif".into(),
            font_size: 42.0,
            weight: 600,
            fill: Color::new(1.0, 1.0, 1.0, 1.0),
            stroke: Some((Color::new(0.0, 0.0, 0.0, 0.85), 1.5)),
            background: None,
            highlight: None,
            position: [0.5, 0.88],
            max_width: 0.85,
            animation: CaptionAnim::None,
        },
        CaptionLook::Karaoke => CaptionStyle {
            font_family: "sans-serif".into(),
            font_size: 52.0,
            weight: 700,
            fill: Color::new(1.0, 1.0, 1.0, 1.0),
            stroke: Some((Color::new(0.0, 0.0, 0.0, 1.0), 2.5)),
            background: None,
            highlight: Some(KaraokeStyle {
                mode: KaraokeMode::WordPop,
                active_color: Color::new(1.0, 0.85, 0.15, 1.0),
                inactive_color: Color::new(0.92, 0.92, 0.92, 1.0),
            }),
            position: [0.5, 0.82],
            max_width: 0.9,
            animation: CaptionAnim::FadeWords,
        },
        CaptionLook::Social => CaptionStyle {
            font_family: "sans-serif".into(),
            font_size: 56.0,
            weight: 800,
            fill: Color::new(1.0, 1.0, 1.0, 1.0),
            stroke: None,
            background: Some(CaptionBackground {
                color: Color::new(0.0, 0.0, 0.0, 0.55),
                corner_radius: 10.0,
                padding: 10.0,
            }),
            highlight: None,
            position: [0.5, 0.78],
            max_width: 0.88,
            animation: CaptionAnim::SlideUp,
        },
    }
}

/// A karaoke highlight preset — sensible active/inactive colors seeded from
/// the style's current fill so the inactive-word color still matches the
/// surrounding caption text (06 §3.7 karaoke presets).
fn karaoke_preset(mode: KaraokeMode, current_fill: Color) -> KaraokeStyle {
    KaraokeStyle {
        mode,
        active_color: Color::new(1.0, 0.82, 0.2, 1.0),
        inactive_color: current_fill,
    }
}

/// Target `[start, end)` for auto-caption / TTS placement: the union of
/// selected clips if any, else the sequence's work range, else the full
/// content span. Pure placement logic — distinct from the engine's own
/// hosted-provider offset mapping (06 §3.4), which is out of GUI scope.
fn resolve_target_span(
    selected_clip_spans: &[(Tick, Tick)],
    work_range: Option<(Tick, Tick)>,
    content_end: Tick,
) -> (Tick, Tick) {
    if !selected_clip_spans.is_empty() {
        let start = selected_clip_spans.iter().map(|(s, _)| *s).min().unwrap();
        let end = selected_clip_spans.iter().map(|(_, e)| *e).max().unwrap();
        return (start, end);
    }
    if let Some(range) = work_range {
        return range;
    }
    (Tick::ZERO, content_end)
}

/// `(start, end)` spans of the given clip ids, searched across every
/// video/audio track of `seq`.
fn selected_clip_spans(seq: &Sequence, ids: &[ClipId]) -> Vec<(Tick, Tick)> {
    seq.video_tracks
        .iter()
        .chain(seq.audio_tracks.iter())
        .flat_map(|t| t.clips.iter())
        .filter(|c| ids.contains(&c.id))
        .map(|c| (c.start, c.end()))
        .collect()
}

/// The append point (end of the last clip, or zero) for a track — where a
/// newly generated voiceover clip lands.
fn track_append_point(track: &Track) -> Tick {
    track
        .clips
        .iter()
        .map(|c| c.end())
        .max()
        .unwrap_or(Tick::ZERO)
}

/// Word index to split a cue at, given a UI slider value clamped to a valid
/// interior boundary (`SplitCue` requires `0 < idx < words.len()`).
fn clamp_split_index(cue: &CaptionCue, wanted: usize) -> Option<usize> {
    if cue.words.len() < 2 {
        return None;
    }
    Some(wanted.clamp(1, cue.words.len() - 1))
}

// ── egui temp-memory keys (this panel's session state; no `PhotonicApp` field) ─

fn id(name: &str) -> egui::Id {
    egui::Id::new(("caption_editor", name))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StyleScope {
    Track,
    Cue,
    Word,
}

// ── Entry point ──────────────────────────────────────────────────────────────

/// Left-rail Captions drawer.
pub(crate) fn draw_caption_editor(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let Some(project) = ctx.doc.timeline.as_ref() else {
        ui.label("No timeline project yet — enter video mode to create one.");
        return;
    };
    let Some(seq_id) = project.active_sequence else {
        ui.label("No active sequence.");
        return;
    };
    let Some(seq) = project.sequences.get(&seq_id) else {
        return;
    };

    draw_track_selector(ui, ctx, seq_id, seq);
    ui.add_space(4.0);

    let selected_track = ui.data(|d| d.get_temp::<TrackId>(id("selected_track")));
    let Some(track) =
        selected_track.and_then(|tid| seq.caption_tracks.iter().find(|t| t.id == tid))
    else {
        ui.label(RichText::new("No caption track selected.").weak());
        draw_auto_caption(ui, ctx, seq_id, seq, None);
        ui.add_space(4.0);
        draw_tts_panel(ui, ctx, seq_id, seq);
        return;
    };
    let track_id = track.id;

    ui.separator();
    if ctx.matches("cues") {
        draw_cue_list(ui, ctx, track_id, track);
    }

    // Proposal 213: look chips always visible (AS-1 doesn't require opening Style).
    ui.add_space(6.0);
    draw_caption_look_chips(ui, ctx, track_id, track);

    ui.add_space(4.0);
    egui::CollapsingHeader::new("Style")
        .id_salt("caption_style_section")
        .default_open(true)
        .open(ctx.forced_open)
        .show(ui, |ui| {
            draw_style_editor(ui, ctx, track_id, track);
        });

    ui.add_space(6.0);
    egui::CollapsingHeader::new("Auto-caption")
        .id_salt("caption_auto_section")
        .default_open(false)
        .open(ctx.forced_open)
        .show(ui, |ui| {
            draw_auto_caption(ui, ctx, seq_id, seq, Some(track_id));
        });

    ui.add_space(6.0);
    egui::CollapsingHeader::new("Text-to-speech")
        .id_salt("caption_tts_section")
        .default_open(false)
        .open(ctx.forced_open)
        .show(ui, |ui| {
            draw_tts_panel(ui, ctx, seq_id, seq);
        });
}

// ── Track selector ───────────────────────────────────────────────────────────

fn draw_track_selector(ui: &mut Ui, ctx: &mut PropPanelCtx, seq_id: SequenceId, seq: &Sequence) {
    let stored = ui.data(|d| d.get_temp::<TrackId>(id("selected_track")));
    let current = stored
        .filter(|tid| seq.caption_tracks.iter().any(|t| t.id == *tid))
        .or_else(|| seq.caption_tracks.first().map(|t| t.id));
    if current != stored {
        if let Some(c) = current {
            ui.data_mut(|d| d.insert_temp(id("selected_track"), c));
        }
    }

    ui.horizontal(|ui| {
        ui.label("Track:");
        let selected_name = current
            .and_then(|tid| seq.caption_tracks.iter().find(|t| t.id == tid))
            .map(|t| t.name.clone())
            .unwrap_or_else(|| "—".to_string());
        egui::ComboBox::from_id_salt("caption_track_select")
            .selected_text(selected_name)
            .show_ui(ui, |ui| {
                for t in &seq.caption_tracks {
                    if ui
                        .selectable_label(current == Some(t.id), &t.name)
                        .clicked()
                    {
                        ui.data_mut(|d| d.insert_temp(id("selected_track"), t.id));
                    }
                }
            });
        if let Some(tid) = current {
            if ui
                .small_button(ph::TRASH)
                .on_hover_text("Remove this caption track")
                .clicked()
            {
                let inserted_ids: Vec<CueId> = seq
                    .caption_tracks
                    .iter()
                    .find(|t| t.id == tid)
                    .map(|t| t.cues.iter().map(|c| c.id).collect())
                    .unwrap_or_default();
                let cmd = TimelineCmd::CaptionEdit(CaptionCmd::UndoBulkInsert {
                    track: tid,
                    inserted_ids,
                    restored: Vec::new(),
                    remove_track: true,
                });
                ctx.action = Some(PanelAction::CaptionEditBatch(vec![cmd]));
                ui.data_mut(|d| d.remove::<TrackId>(id("selected_track")));
            }
        }
    });

    let new_name_id = id("new_track_name");
    ui.horizontal(|ui| {
        let mut buf: String = ui.data(|d| d.get_temp(new_name_id).unwrap_or_default());
        ui.add(
            egui::TextEdit::singleline(&mut buf)
                .hint_text("New track name…")
                .desired_width(120.0),
        );
        let can = !buf.trim().is_empty();
        if ui
            .add_enabled(can, egui::Button::new(format!("{} Add track", ph::PLUS)))
            .clicked()
        {
            let track = CaptionTrack::new(buf.trim());
            let new_id = track.id;
            let cmd = TimelineCmd::CaptionEdit(CaptionCmd::BulkInsertCues {
                track: new_id,
                cues: Vec::new(),
                replace_range: None,
                replaced: Vec::new(),
                created_track: Some(Box::new(track)),
            });
            let _ = seq_id; // active sequence is where BulkInsertCues creates the track
            ctx.action = Some(PanelAction::CaptionEditBatch(vec![cmd]));
            ui.data_mut(|d| {
                d.insert_temp(new_name_id, String::new());
                d.insert_temp(id("selected_track"), new_id);
            });
        } else {
            ui.data_mut(|d| d.insert_temp(new_name_id, buf));
        }
    });
}

// ── Cue list + inline text/timing edit ──────────────────────────────────────

fn draw_cue_list(ui: &mut Ui, ctx: &mut PropPanelCtx, track_id: TrackId, track: &CaptionTrack) {
    if track.cues.is_empty() {
        ui.label(RichText::new("No cues yet — use Auto-caption below.").weak());
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("caption_cue_scroll")
        .max_height(260.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for cue in &track.cues {
                draw_cue_row(ui, ctx, track_id, track, cue);
            }
        });
}

fn draw_cue_row(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    track_id: TrackId,
    track: &CaptionTrack,
    cue: &CaptionCue,
) {
    let editing = *ctx.video.caption_edit_cue == Some(cue.id);
    let text_buf_id = id("cue_text").with(cue.id);

    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{}–{}", format_tc(cue.start), format_tc(cue.end))).weak());
        let cue_text = cue.text();
        let row_label = if cue_text.is_empty() {
            "…"
        } else {
            cue_text.as_str()
        };
        let resp = ui.selectable_label(editing, row_label);
        if resp.clicked() {
            *ctx.video.caption_edit_cue = if editing { None } else { Some(cue.id) };
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button(ph::TRASH)
                .on_hover_text("Delete cue")
                .clicked()
            {
                // A `BulkInsertCues` that replaces exactly this cue's span with
                // nothing removes it (retain keeps cues outside [s,e)); the
                // core has no dedicated single-cue-remove command (06 §3.6).
                let cmd = TimelineCmd::CaptionEdit(CaptionCmd::BulkInsertCues {
                    track: track_id,
                    cues: Vec::new(),
                    replace_range: Some((cue.start, cue.end)),
                    replaced: vec![cue.clone()],
                    created_track: None,
                });
                ctx.action = Some(PanelAction::CaptionEditBatch(vec![cmd]));
                if editing {
                    *ctx.video.caption_edit_cue = None;
                }
            }
        });
    });

    if !editing {
        return;
    }

    ui.indent(("cap_cue_detail", cue.id), |ui| {
        // ── Inline text edit (commit on Enter/blur, Escape cancels) ─────────
        let mut buf: String = ui
            .data(|d| d.get_temp(text_buf_id))
            .unwrap_or_else(|| cue.text());
        let resp = ui.add(egui::TextEdit::multiline(&mut buf).desired_rows(1));
        if resp.lost_focus() {
            let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if !escaped && buf != cue.text() {
                let new_words = retext_cue_words(cue, &buf);
                let cmd = TimelineCmd::CaptionEdit(CaptionCmd::SetCueText {
                    track: track_id,
                    cue: cue.id,
                    old_words: cue.words.clone(),
                    new_words,
                });
                ctx.action = Some(PanelAction::CaptionEditBatch(vec![cmd]));
            }
            ui.data_mut(|d| d.remove::<String>(text_buf_id));
        } else {
            ui.data_mut(|d| d.insert_temp(text_buf_id, buf));
        }

        // ── Retime by drag (cue-level) ───────────────────────────────────────
        // `cue.start`/`cue.end` (read from `ctx.doc`) stay constant for the
        // whole gesture — nothing commits mid-drag — so re-deriving the
        // starting seconds fresh every frame is equivalent to persisting it;
        // egui's `DragValue` tracks the accumulated delta itself. Commit only
        // at the gesture boundary (`drag_stopped`/`lost_focus`): `CaptionCmd`
        // has no `TimelineCmd::coalesce` arm (unlike `TrimClip`/`MoveClip`),
        // so committing every changed frame via the coalescing path would
        // still emit one undo entry per tick — gating here is the only way
        // to get one undo step per drag gesture.
        let mut start_s = cue.start.as_seconds_f64();
        let mut end_s = cue.end.as_seconds_f64();
        let mut gesture_done = false;
        ui.horizontal(|ui| {
            ui.label("Start:");
            let r1 = ui.add(egui::DragValue::new(&mut start_s).speed(0.02).suffix("s"));
            ui.label("End:");
            let r2 = ui.add(egui::DragValue::new(&mut end_s).speed(0.02).suffix("s"));
            gesture_done =
                r1.drag_stopped() || r1.lost_focus() || r2.drag_stopped() || r2.lost_focus();
        });
        let new_start = tick_from_seconds_f64(start_s);
        let new_end = tick_from_seconds_f64(end_s);
        if gesture_done && (new_start, new_end) != (cue.start, cue.end) && new_end > new_start {
            let cmd = TimelineCmd::CaptionEdit(CaptionCmd::RetimeCue {
                track: track_id,
                cue: cue.id,
                old: (cue.start, cue.end),
                new: (new_start, new_end),
            });
            ctx.action = Some(PanelAction::CaptionEditBatch(vec![cmd]));
        }

        // ── Word-level timing + split/merge ──────────────────────────────────
        draw_word_editor(ui, ctx, track_id, track, cue);
    });
}

/// Rebuild a cue's word list from an edited plain-text string, keeping the
/// cue's own `[start, end)` span and distributing the new words proportionally
/// across it (same fallback the providers use when they have no per-word
/// timing, 06 §2.2) — word count/text can change freely via inline edit.
fn retext_cue_words(cue: &CaptionCue, new_text: &str) -> Vec<CaptionWord> {
    let words: Vec<&str> = new_text.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let span = (cue.end - cue.start).0.max(1);
    let per = span / words.len() as i64;
    words
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let s = cue.start + Tick(per * i as i64);
            let e = if i + 1 == words.len() {
                cue.end
            } else {
                cue.start + Tick(per * (i as i64 + 1))
            };
            CaptionWord::new(*w, s, e)
        })
        .collect()
}

fn draw_word_editor(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    track_id: TrackId,
    track: &CaptionTrack,
    cue: &CaptionCue,
) {
    if cue.words.is_empty() {
        return;
    }
    ui.add_space(2.0);
    ui.label(
        RichText::new("WORDS")
            .small()
            .color(crate::theme::section_header_color(ui)),
    );
    let sel_word_id = id("word_sel").with(cue.id);
    let mut selected_word: Option<usize> = ui.data(|d| d.get_temp(sel_word_id));

    egui::Grid::new(("cap_words_grid", cue.id))
        .num_columns(4)
        .spacing([4.0, 2.0])
        .show(ui, |ui| {
            for (i, w) in cue.words.iter().enumerate() {
                let is_sel = selected_word == Some(i);
                if ui.selectable_label(is_sel, &w.text).clicked() {
                    selected_word = if is_sel { None } else { Some(i) };
                }
                let mut ws = w.start.as_seconds_f64();
                let mut we = w.end.as_seconds_f64();
                let r1 = ui.add(
                    egui::DragValue::new(&mut ws)
                        .speed(0.01)
                        .suffix("s")
                        .fixed_decimals(2),
                );
                let r2 = ui.add(
                    egui::DragValue::new(&mut we)
                        .speed(0.01)
                        .suffix("s")
                        .fixed_decimals(2),
                );
                // Commit only at the gesture boundary — see the cue-level
                // retime comment above (`CaptionCmd` has no coalesce arm).
                let gesture_done =
                    r1.drag_stopped() || r1.lost_focus() || r2.drag_stopped() || r2.lost_focus();
                let new_start = tick_from_seconds_f64(ws);
                let new_end = tick_from_seconds_f64(we);
                if gesture_done && (new_start, new_end) != (w.start, w.end) && new_end > new_start {
                    let cmd = TimelineCmd::CaptionEdit(CaptionCmd::RetimeWord {
                        track: track_id,
                        cue: cue.id,
                        word: i,
                        old: (w.start, w.end),
                        new: (new_start, new_end),
                    });
                    ctx.action = Some(PanelAction::CaptionEditBatch(vec![cmd]));
                }
                ui.end_row();
            }
        });
    ui.data_mut(|d| d.insert_temp(sel_word_id, selected_word));

    // ── Split at the selected word boundary ─────────────────────────────────
    ui.horizontal(|ui| {
        let can_split = selected_word
            .and_then(|wi| clamp_split_index(cue, wi))
            .is_some();
        if ui
            .add_enabled(can_split, egui::Button::new("Split here"))
            .on_hover_text("Split the cue before the selected word")
            .clicked()
        {
            if let Some(idx) = selected_word.and_then(|wi| clamp_split_index(cue, wi)) {
                let cmd = TimelineCmd::CaptionEdit(CaptionCmd::SplitCue {
                    track: track_id,
                    cue: cue.id,
                    at_word_index: idx,
                    new_cue_id: CueId::new(),
                });
                ctx.action = Some(PanelAction::CaptionEditBatch(vec![cmd]));
            }
        }
        if let Some(next) = next_cue(track, cue.id) {
            if ui
                .button("Merge with next")
                .on_hover_text(format!("Merge with \"{}\"", next.text()))
                .clicked()
            {
                let cmd = TimelineCmd::CaptionEdit(CaptionCmd::MergeCues {
                    track: track_id,
                    a: cue.id,
                    b: next.id,
                    old_a: Box::new(cue.clone()),
                    old_b: Box::new(next.clone()),
                });
                ctx.action = Some(PanelAction::CaptionEditBatch(vec![cmd]));
            }
        }
    });
}

fn next_cue(track: &CaptionTrack, cue: CueId) -> Option<&CaptionCue> {
    let idx = track.cues.iter().position(|c| c.id == cue)?;
    track.cues.get(idx + 1)
}

// ── Look chips (proposal 213) ───────────────────────────────────────────────

/// Apply a whole-style look to the **track** (one undo step via SetStyle).
fn draw_caption_look_chips(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    track_id: TrackId,
    track: &CaptionTrack,
) {
    ui.label(RichText::new("Looks").small().weak());
    ui.horizontal_wrapped(|ui| {
        for &(look, label) in CAPTION_LOOKS {
            let style = caption_look_style(look);
            let active = track.style == style;
            let resp = ui
                .selectable_label(active, label)
                .on_hover_text(match look {
                    CaptionLook::Clean => "White text, light stroke — clean social / broadcast",
                    CaptionLook::Karaoke => "Word-pop karaoke highlight with yellow active word",
                    CaptionLook::Social => "Bold pill background — CapCut/TikTok-style social",
                });
            if resp.clicked() {
                let old = Some(track.style.clone());
                let cmd = TimelineCmd::CaptionEdit(CaptionCmd::SetStyle {
                    track: track_id,
                    target: StyleTarget::Track,
                    old: old.map(Box::new),
                    new: Some(Box::new(style)),
                });
                ctx.action = Some(PanelAction::CaptionEditBatch(vec![cmd]));
            }
        }
    });
}

// ── Style editor (track → cue → word cascade, 06 §4) ────────────────────────

fn draw_style_editor(ui: &mut Ui, ctx: &mut PropPanelCtx, track_id: TrackId, track: &CaptionTrack) {
    let editing_cue = *ctx.video.caption_edit_cue;
    let cue = editing_cue.and_then(|cid| track.cues.iter().find(|c| c.id == cid));
    let selected_word: Option<usize> = cue.and_then(|c| {
        ui.data(|d| d.get_temp::<Option<usize>>(id("word_sel").with(c.id)))
            .flatten()
    });

    let stored_scope = ui.data(|d| d.get_temp::<StyleScope>(id("style_scope")));
    let mut scope = stored_scope.unwrap_or(StyleScope::Track);
    if scope == StyleScope::Word && (cue.is_none() || selected_word.is_none()) {
        scope = StyleScope::Cue;
    }
    if scope == StyleScope::Cue && cue.is_none() {
        scope = StyleScope::Track;
    }

    ui.horizontal(|ui| {
        if ui
            .selectable_label(scope == StyleScope::Track, "Track")
            .clicked()
        {
            scope = StyleScope::Track;
        }
        if ui
            .add_enabled(
                cue.is_some(),
                egui::SelectableLabel::new(scope == StyleScope::Cue, "Cue"),
            )
            .clicked()
        {
            scope = StyleScope::Cue;
        }
        if ui
            .add_enabled(
                cue.is_some() && selected_word.is_some(),
                egui::SelectableLabel::new(scope == StyleScope::Word, "Word"),
            )
            .clicked()
        {
            scope = StyleScope::Word;
        }
    });
    ui.data_mut(|d| d.insert_temp(id("style_scope"), scope));

    let target = match scope {
        StyleScope::Track => StyleTarget::Track,
        StyleScope::Cue => StyleTarget::Cue(cue.unwrap().id),
        StyleScope::Word => StyleTarget::Word(cue.unwrap().id, selected_word.unwrap()),
    };
    let base = match scope {
        StyleScope::Track => resolve_style(track, None, None),
        StyleScope::Cue => resolve_style(track, cue, None),
        StyleScope::Word => resolve_style(track, cue, selected_word),
    };

    // Draft persists across frames (temp memory) so multi-field edits
    // accumulate before an explicit Apply — undo-spam guard, same discipline
    // `ops_bridge` uses for drag commits (preview locally, commit once).
    let draft_key = id("style_draft");
    let draft_target_key = id("style_draft_target");
    let target_sig = format!("{target:?}");
    let stored_sig: Option<String> = ui.data(|d| d.get_temp(draft_target_key));
    let mut draft: CaptionStyle = if stored_sig.as_deref() == Some(target_sig.as_str()) {
        ui.data(|d| d.get_temp(draft_key))
            .unwrap_or_else(|| base.clone())
    } else {
        base.clone()
    };

    egui::Grid::new("cap_style_grid")
        .num_columns(2)
        .spacing([6.0, 4.0])
        .show(ui, |ui| {
            ui.label("Font:");
            ui.text_edit_singleline(&mut draft.font_family);
            ui.end_row();

            ui.label("Size:");
            ui.add(
                egui::DragValue::new(&mut draft.font_size)
                    .range(8.0..=200.0)
                    .speed(0.5),
            );
            ui.end_row();

            ui.label("Weight:");
            ui.add(
                egui::DragValue::new(&mut draft.weight)
                    .range(100..=900)
                    .speed(10.0),
            );
            ui.end_row();

            ui.label("Fill:");
            ColorPopup::swatch_color(ui, &mut draft.fill);
            ui.end_row();

            ui.label("Stroke:");
            ui.horizontal(|ui| {
                let mut has_stroke = draft.stroke.is_some();
                ui.checkbox(&mut has_stroke, "");
                if has_stroke {
                    let (mut c, mut w) = draft.stroke.unwrap_or((Color::BLACK, 2.0));
                    ColorPopup::swatch_color(ui, &mut c);
                    ui.add(egui::DragValue::new(&mut w).range(0.0..=20.0).speed(0.1));
                    draft.stroke = Some((c, w));
                } else {
                    draft.stroke = None;
                }
            });
            ui.end_row();

            ui.label("Background:");
            ui.horizontal(|ui| {
                let mut has_bg = draft.background.is_some();
                ui.checkbox(&mut has_bg, "");
                if has_bg {
                    let mut bg = draft.background.unwrap_or(CaptionBackground {
                        color: Color::new(0.0, 0.0, 0.0, 0.6),
                        corner_radius: 4.0,
                        padding: 6.0,
                    });
                    ColorPopup::swatch_color(ui, &mut bg.color);
                    ui.label("radius");
                    ui.add(egui::DragValue::new(&mut bg.corner_radius).range(0.0..=40.0));
                    ui.label("pad");
                    ui.add(egui::DragValue::new(&mut bg.padding).range(0.0..=40.0));
                    draft.background = Some(bg);
                } else {
                    draft.background = None;
                }
            });
            ui.end_row();

            ui.label("Position:");
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut draft.position[0])
                        .range(0.0..=1.0)
                        .speed(0.01),
                );
                ui.add(
                    egui::DragValue::new(&mut draft.position[1])
                        .range(0.0..=1.0)
                        .speed(0.01),
                );
            });
            ui.end_row();

            ui.label("Max width:");
            ui.add(
                egui::DragValue::new(&mut draft.max_width)
                    .range(0.05..=1.0)
                    .speed(0.01),
            );
            ui.end_row();

            ui.label("Animation:");
            egui::ComboBox::from_id_salt("cap_style_anim")
                .selected_text(anim_label(draft.animation))
                .show_ui(ui, |ui| {
                    for a in [
                        CaptionAnim::None,
                        CaptionAnim::FadeWords,
                        CaptionAnim::SlideUp,
                        CaptionAnim::Typewriter,
                    ] {
                        ui.selectable_value(&mut draft.animation, a, anim_label(a));
                    }
                });
            ui.end_row();

            ui.label("Karaoke:");
            ui.horizontal(|ui| {
                let mut has_hl = draft.highlight.is_some();
                ui.checkbox(&mut has_hl, "");
                if has_hl {
                    let mut hl = draft
                        .highlight
                        .unwrap_or_else(|| karaoke_preset(KaraokeMode::FillSweep, draft.fill));
                    egui::ComboBox::from_id_salt("cap_karaoke_mode")
                        .selected_text(karaoke_mode_label(hl.mode))
                        .show_ui(ui, |ui| {
                            for m in [
                                KaraokeMode::FillSweep,
                                KaraokeMode::WordPop,
                                KaraokeMode::Underline,
                            ] {
                                ui.selectable_value(&mut hl.mode, m, karaoke_mode_label(m));
                            }
                        });
                    ColorPopup::swatch_color(ui, &mut hl.active_color);
                    ColorPopup::swatch_color(ui, &mut hl.inactive_color);
                    draft.highlight = Some(hl);
                } else {
                    draft.highlight = None;
                }
            });
            ui.end_row();
        });

    // ── Look chips (also in draft for cue/word scope) ───────────────────────
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Looks:").weak());
        for &(look, label) in CAPTION_LOOKS {
            let style = caption_look_style(look);
            if ui
                .selectable_label(draft == style, label)
                .on_hover_text("Apply full style look to this draft (Apply to commit)")
                .clicked()
            {
                draft = style;
            }
        }
    });

    // ── Karaoke mode chips (highlight only) ─────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(RichText::new("Karaoke mode:").weak());
        for (label, mode) in [
            ("Fill sweep", KaraokeMode::FillSweep),
            ("Word pop", KaraokeMode::WordPop),
            ("Underline", KaraokeMode::Underline),
        ] {
            if ui.small_button(label).clicked() {
                draft.highlight = Some(karaoke_preset(mode, draft.fill));
            }
        }
    });

    ui.data_mut(|d| {
        d.insert_temp(draft_key, draft.clone());
        d.insert_temp(draft_target_key, target_sig);
    });

    let changed = draft != base;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(changed, egui::Button::new("Apply"))
            .clicked()
        {
            let old = raw_style_override(track, &target);
            let cmd = TimelineCmd::CaptionEdit(CaptionCmd::SetStyle {
                track: track_id,
                target: target.clone(),
                old: old.map(Box::new),
                new: Some(Box::new(draft.clone())),
            });
            ctx.action = Some(PanelAction::CaptionEditBatch(vec![cmd]));
            ui.data_mut(|d| d.remove::<CaptionStyle>(draft_key));
        }
        if ui
            .add_enabled(changed, egui::Button::new("Revert"))
            .clicked()
        {
            ui.data_mut(|d| d.insert_temp(draft_key, base.clone()));
        }
        if !matches!(target, StyleTarget::Track) {
            let has_override = raw_style_override(track, &target).is_some();
            if ui
                .add_enabled(has_override, egui::Button::new("Clear override"))
                .clicked()
            {
                let old = raw_style_override(track, &target);
                let cmd = TimelineCmd::CaptionEdit(CaptionCmd::SetStyle {
                    track: track_id,
                    target: target.clone(),
                    old: old.map(Box::new),
                    new: None,
                });
                ctx.action = Some(PanelAction::CaptionEditBatch(vec![cmd]));
                ui.data_mut(|d| d.remove::<CaptionStyle>(draft_key));
            }
        }
    });
}

fn anim_label(a: CaptionAnim) -> &'static str {
    match a {
        CaptionAnim::None => "None",
        CaptionAnim::FadeWords => "Fade words",
        CaptionAnim::SlideUp => "Slide up",
        CaptionAnim::Typewriter => "Typewriter",
    }
}

fn karaoke_mode_label(m: KaraokeMode) -> &'static str {
    match m {
        KaraokeMode::FillSweep => "Fill sweep",
        KaraokeMode::WordPop => "Word pop",
        KaraokeMode::Underline => "Underline",
    }
}

// ── Auto-caption (mock transcription provider job) ──────────────────────────

type AutoCaptionRx = Arc<Mutex<mpsc::Receiver<Result<Vec<TimelineCmd>, String>>>>;

fn draw_auto_caption(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    _seq_id: SequenceId,
    seq: &Sequence,
    current_track: Option<TrackId>,
) {
    let span = resolve_target_span(
        &selected_clip_spans(seq, ctx.video.selection),
        seq.work_range,
        seq.content_end(),
    );
    ui.label(
        RichText::new(format!(
            "Target range: {} – {}",
            format_tc(span.0),
            format_tc(span.1)
        ))
        .weak()
        .small(),
    );

    let transcript_id = id("mock_transcript");
    let mut transcript: String = ui.data(|d| d.get_temp(transcript_id).unwrap_or_default());
    ui.add(
        egui::TextEdit::multiline(&mut transcript)
            .hint_text("Paste/type the transcript to auto-caption (deterministic mock provider)…")
            .desired_rows(3),
    );
    ui.data_mut(|d| d.insert_temp(transcript_id, transcript.clone()));

    let job_key = id("auto_job");
    let running = ui.data(|d| d.get_temp::<AutoCaptionRx>(job_key)).is_some();
    let error_id = id("auto_error");

    ui.horizontal(|ui| {
        let can_go = !running && !transcript.trim().is_empty() && span.1 > span.0;
        if ui
            .add_enabled(
                can_go,
                egui::Button::new(format!("{} Auto-caption", ph::SPARKLE)),
            )
            .clicked()
        {
            ui.data_mut(|d| d.remove::<String>(error_id));
            let (track_id, created_track) = match current_track {
                Some(t) => (t, None),
                None => {
                    let t = CaptionTrack::new("Captions");
                    (t.id, Some(Box::new(t)))
                }
            };
            let (tx, rx) = mpsc::channel();
            let text = transcript.clone();
            let (start, end) = span;
            std::thread::spawn(move || {
                let provider = MockTranscriptionProvider::fixture(&text, start, end);
                let (progress_tx, _progress_rx) = crossbeam_channel::unbounded();
                let out = provider
                    .transcribe(
                        TranscriptionRequest {
                            audio_path: PathBuf::from("mock.wav"),
                            language_hint: None,
                            model: None,
                        },
                        progress_tx,
                        CancelToken::new(),
                    )
                    .map(|r| {
                        let cues = group_words_into_cues(&r.words, &GroupingParams::default());
                        vec![TimelineCmd::CaptionEdit(CaptionCmd::BulkInsertCues {
                            track: track_id,
                            cues,
                            replace_range: None,
                            replaced: Vec::new(),
                            created_track,
                        })]
                    })
                    .map_err(|e| e.to_string());
                let _ = tx.send(out);
            });
            ui.data_mut(|d| d.insert_temp(job_key, Arc::new(Mutex::new(rx))));
        }
        if running {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(RichText::new("Transcribing…").weak());
            ui.ctx().request_repaint();
        }
    });

    if let Some(err) = ui.data(|d| d.get_temp::<String>(error_id)) {
        ui.colored_label(Color32::from_rgb(248, 113, 113), err);
    }

    if let Some(rx) = ui.data(|d| d.get_temp::<AutoCaptionRx>(job_key)) {
        let outcome = rx.lock().ok().and_then(|r| r.try_recv().ok());
        if let Some(outcome) = outcome {
            ui.data_mut(|d| d.remove::<AutoCaptionRx>(job_key));
            match outcome {
                Ok(cmds) => {
                    ctx.action = Some(PanelAction::CaptionEditBatch(cmds));
                    ui.data_mut(|d| d.insert_temp(transcript_id, String::new()));
                }
                Err(e) => ui.data_mut(|d| d.insert_temp(error_id, e)),
            }
        }
    }
}

// ── Text-to-speech mini-panel (script → generate → clip, 06 §6) ─────────────

struct TtsOutcome {
    wav_path: PathBuf,
    duration: Tick,
    word_timings: Option<Vec<TranscribedWord>>,
}

type TtsRx = Arc<Mutex<mpsc::Receiver<Result<TtsOutcome, String>>>>;

fn draw_tts_panel(ui: &mut Ui, ctx: &mut PropPanelCtx, seq_id: SequenceId, seq: &Sequence) {
    if seq.audio_tracks.is_empty() {
        ui.label(RichText::new("Add an audio track first (Media Pool / timeline).").weak());
        return;
    }

    let script_id = id("tts_script");
    let mut script: String = ui.data(|d| d.get_temp(script_id).unwrap_or_default());
    ui.add(
        egui::TextEdit::multiline(&mut script)
            .hint_text("Voiceover script…")
            .desired_rows(3),
    );
    ui.data_mut(|d| d.insert_temp(script_id, script.clone()));

    let voices = MockTtsProvider::default().voices().unwrap_or_default();
    let voice_id = id("tts_voice");
    let mut voice: String = ui
        .data(|d| d.get_temp(voice_id))
        .unwrap_or_else(|| voices.first().map(|v| v.id.clone()).unwrap_or_default());
    let track_id_key = id("tts_track");
    let mut dest_track: TrackId = ui
        .data(|d| d.get_temp(track_id_key))
        .filter(|t| seq.audio_tracks.iter().any(|tr| tr.id == *t))
        .unwrap_or(seq.audio_tracks[0].id);
    let also_caption_id = id("tts_also_caption");
    let mut also_caption: bool = ui.data(|d| d.get_temp(also_caption_id).unwrap_or(true));

    ui.horizontal(|ui| {
        ui.label("Voice:");
        egui::ComboBox::from_id_salt("tts_voice_select")
            .selected_text(voice_label(&voices, &voice))
            .show_ui(ui, |ui| {
                for v in &voices {
                    ui.selectable_value(&mut voice, v.id.clone(), &v.name);
                }
            });
    });
    ui.horizontal(|ui| {
        ui.label("Onto track:");
        let track_name = seq
            .audio_tracks
            .iter()
            .find(|t| t.id == dest_track)
            .map(|t| t.name.clone())
            .unwrap_or_default();
        egui::ComboBox::from_id_salt("tts_track_select")
            .selected_text(track_name)
            .show_ui(ui, |ui| {
                for t in &seq.audio_tracks {
                    ui.selectable_value(&mut dest_track, t.id, &t.name);
                }
            });
    });
    ui.checkbox(&mut also_caption, "Also add word-timed captions");

    ui.data_mut(|d| {
        d.insert_temp(voice_id, voice.clone());
        d.insert_temp(track_id_key, dest_track);
        d.insert_temp(also_caption_id, also_caption);
    });

    let job_key = id("tts_job");
    let running = ui.data(|d| d.get_temp::<TtsRx>(job_key)).is_some();
    let error_id = id("tts_error");

    let can_go = !running && !script.trim().is_empty() && !voice.is_empty();
    if ui
        .add_enabled(
            can_go,
            egui::Button::new(format!("{} Generate", ph::WAVEFORM)),
        )
        .clicked()
    {
        ui.data_mut(|d| d.remove::<String>(error_id));
        let (tx, rx) = mpsc::channel();
        let text = script.clone();
        let voice_for_job = voice.clone();
        std::thread::spawn(move || {
            let (progress_tx, _progress_rx) = crossbeam_channel::unbounded();
            let out = MockTtsProvider::default()
                .synthesize(
                    TtsRequest {
                        text,
                        voice: voice_for_job,
                        params: HashMap::new(),
                    },
                    progress_tx,
                    CancelToken::new(),
                )
                .map_err(|e| e.to_string())
                .and_then(|r| {
                    let duration_secs = photonic_video::captions::wav::read_wav_info(&r.audio)
                        .map(|i| i.duration_secs())
                        .unwrap_or(0.0);
                    let duration = tick_from_seconds_f64(duration_secs).max(Tick(1));
                    let cache_dir = photonic_video::media::proxy::proxy_cache_dir(None);
                    std::fs::create_dir_all(&cache_dir).map_err(|e| e.to_string())?;
                    let wav_path = cache_dir.join(format!("tts-{}.wav", uuid::Uuid::new_v4()));
                    std::fs::write(&wav_path, &r.audio).map_err(|e| e.to_string())?;
                    Ok(TtsOutcome {
                        wav_path,
                        duration,
                        word_timings: r.word_timings,
                    })
                });
            let _ = tx.send(out);
        });
        ui.data_mut(|d| d.insert_temp(job_key, Arc::new(Mutex::new(rx))));
    }
    if running {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(RichText::new("Synthesizing…").weak());
        });
        ui.ctx().request_repaint();
    }
    if let Some(err) = ui.data(|d| d.get_temp::<String>(error_id)) {
        ui.colored_label(Color32::from_rgb(248, 113, 113), err);
    }

    if let Some(rx) = ui.data(|d| d.get_temp::<TtsRx>(job_key)) {
        let outcome = rx.lock().ok().and_then(|r| r.try_recv().ok());
        if let Some(outcome) = outcome {
            ui.data_mut(|d| d.remove::<TtsRx>(job_key));
            match outcome {
                Ok(out) => {
                    if let Some(project) = ctx.doc.timeline.as_ref() {
                        if let Some(cmds) =
                            build_tts_place_commands(project, seq_id, dest_track, out, also_caption)
                        {
                            ctx.action = Some(PanelAction::CaptionEditBatch(cmds));
                            ui.data_mut(|d| d.insert_temp(script_id, String::new()));
                        } else {
                            ui.data_mut(|d| {
                                d.insert_temp(
                                    error_id,
                                    "could not place voiceover clip (track missing?)".to_string(),
                                )
                            });
                        }
                    }
                }
                Err(e) => ui.data_mut(|d| d.insert_temp(error_id, e)),
            }
        }
    }
}

fn voice_label(voices: &[VoiceDescriptor], id: &str) -> String {
    voices
        .iter()
        .find(|v| v.id == id)
        .map(|v| v.name.clone())
        .unwrap_or_else(|| "Select voice…".to_string())
}

/// Build the final commit batch for a finished TTS job against the *current*
/// (live, freshly re-read) project state — `AddAsset` + `InsertClip`
/// (`ops::*`, overlap-validated) plus an optional word-timed caption bulk
/// insert, mirroring `photonic-mcp`'s `generate_voiceover` handler.
fn build_tts_place_commands(
    project: &photonic_core::timeline::TimelineProject,
    seq_id: SequenceId,
    dest_track: TrackId,
    out: TtsOutcome,
    also_caption: bool,
) -> Option<Vec<TimelineCmd>> {
    let seq = project.sequences.get(&seq_id)?;
    let track = seq.audio_tracks.iter().find(|t| t.id == dest_track)?;
    let start = track_append_point(track);

    let asset = MediaAsset::from_file(photonic_core::timeline::AssetKind::Audio, out.wav_path);
    let asset_id = asset.id;
    let clip = Clip::new(ClipSource::Asset { asset: asset_id }, start, out.duration);

    let insert = ops::insert_clip(project, seq_id, dest_track, clip).ok()?;
    let mut cmds = vec![ops::add_asset(asset), insert];

    if also_caption {
        if let Some(words) = out.word_timings {
            let shifted: Vec<TranscribedWord> = words
                .into_iter()
                .map(|w| TranscribedWord {
                    text: w.text,
                    start: w.start + start,
                    end: w.end + start,
                    confidence: w.confidence,
                })
                .collect();
            let cues = group_words_into_cues(&shifted, &GroupingParams::default());
            if !cues.is_empty() {
                let track = CaptionTrack::new("Voiceover captions");
                let track_id = track.id;
                cmds.push(TimelineCmd::CaptionEdit(CaptionCmd::BulkInsertCues {
                    track: track_id,
                    cues,
                    replace_range: None,
                    replaced: Vec::new(),
                    created_track: Some(Box::new(track)),
                }));
            }
        }
    }
    Some(cmds)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::TrackKind;

    fn word(text: &str, s: i64, e: i64) -> CaptionWord {
        CaptionWord::new(text, Tick(s), Tick(e))
    }

    #[test]
    fn caption_looks_are_distinct_and_stable() {
        let clean = caption_look_style(CaptionLook::Clean);
        let karaoke = caption_look_style(CaptionLook::Karaoke);
        let social = caption_look_style(CaptionLook::Social);
        assert_ne!(clean, karaoke);
        assert_ne!(karaoke, social);
        assert_ne!(clean, social);
        // Karaoke has highlight; Social has background; Clean has neither box nor karaoke.
        assert!(karaoke.highlight.is_some());
        assert!(social.background.is_some());
        assert!(clean.highlight.is_none());
        assert!(clean.background.is_none());
        // Idempotent builders (chips compare for active state).
        assert_eq!(clean, caption_look_style(CaptionLook::Clean));
        assert_eq!(karaoke, caption_look_style(CaptionLook::Karaoke));
        assert_eq!(social, caption_look_style(CaptionLook::Social));
    }

    #[test]
    fn resolve_style_cascades_word_then_cue_then_track() {
        let mut track = CaptionTrack::new("t");
        track.style.font_size = 10.0;
        let mut cue = CaptionCue::new(
            Tick(0),
            Tick(100),
            vec![word("a", 0, 50), word("b", 50, 100)],
        );
        // No overrides anywhere: falls through to track.
        assert_eq!(resolve_style(&track, Some(&cue), Some(1)).font_size, 10.0);

        let mut cue_style = CaptionStyle::default();
        cue_style.font_size = 20.0;
        cue.style_override = Some(cue_style);
        assert_eq!(resolve_style(&track, Some(&cue), Some(1)).font_size, 20.0);

        let mut word_style = CaptionStyle::default();
        word_style.font_size = 30.0;
        cue.words[1].style_override = Some(word_style);
        assert_eq!(resolve_style(&track, Some(&cue), Some(1)).font_size, 30.0);
        // A different word still falls through to the cue override.
        assert_eq!(resolve_style(&track, Some(&cue), Some(0)).font_size, 20.0);

        track.cues.push(cue);
    }

    #[test]
    fn raw_style_override_is_none_until_set() {
        let mut track = CaptionTrack::new("t");
        let cue = CaptionCue::new(Tick(0), Tick(100), vec![word("a", 0, 100)]);
        let cue_id = cue.id;
        track.cues.push(cue);
        assert_eq!(raw_style_override(&track, &StyleTarget::Cue(cue_id)), None);
        assert!(raw_style_override(&track, &StyleTarget::Track).is_some());
    }

    #[test]
    fn resolve_target_span_prefers_selection_then_work_range_then_content() {
        let sel = vec![(Tick(100), Tick(200)), (Tick(50), Tick(150))];
        assert_eq!(
            resolve_target_span(&sel, Some((Tick(0), Tick(9))), Tick(999)),
            (Tick(50), Tick(200))
        );
        assert_eq!(
            resolve_target_span(&[], Some((Tick(10), Tick(20))), Tick(999)),
            (Tick(10), Tick(20))
        );
        assert_eq!(
            resolve_target_span(&[], None, Tick(999)),
            (Tick::ZERO, Tick(999))
        );
    }

    #[test]
    fn clamp_split_index_requires_interior_boundary() {
        let one_word = CaptionCue::new(Tick(0), Tick(100), vec![word("a", 0, 100)]);
        assert_eq!(clamp_split_index(&one_word, 0), None);

        let three = CaptionCue::new(
            Tick(0),
            Tick(300),
            vec![word("a", 0, 100), word("b", 100, 200), word("c", 200, 300)],
        );
        assert_eq!(clamp_split_index(&three, 0), Some(1));
        assert_eq!(clamp_split_index(&three, 5), Some(2));
        assert_eq!(clamp_split_index(&three, 1), Some(1));
    }

    #[test]
    fn retext_cue_words_distributes_across_cue_span() {
        let cue = CaptionCue::new(Tick(0), Tick(TICKS_PER_SECOND * 4), vec![word("old", 0, 1)]);
        let words = retext_cue_words(&cue, "hello there world");
        assert_eq!(words.len(), 3);
        assert_eq!(words[0].text, "hello");
        assert_eq!(words[0].start, Tick(0));
        assert_eq!(words.last().unwrap().end, cue.end);
        // Monotonic, covers the whole span, no gaps.
        for w in &words {
            assert!(w.end > w.start);
        }
        for pair in words.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
    }

    #[test]
    fn retext_cue_words_empty_text_yields_no_words() {
        let cue = CaptionCue::new(Tick(0), Tick(100), vec![word("a", 0, 100)]);
        assert!(retext_cue_words(&cue, "   ").is_empty());
    }

    #[test]
    fn track_append_point_is_end_of_last_clip_or_zero() {
        let empty = Track::new(TrackKind::Audio, "A1");
        assert_eq!(track_append_point(&empty), Tick::ZERO);

        let mut t = Track::new(TrackKind::Audio, "A1");
        let asset = photonic_core::timeline::AssetId::new();
        let c1 = Clip::new(ClipSource::Asset { asset }, Tick(0), Tick(TICKS_PER_SECOND));
        let c2 = Clip::new(
            ClipSource::Asset { asset },
            Tick(TICKS_PER_SECOND * 5),
            Tick(TICKS_PER_SECOND),
        );
        let expected_end = c2.end();
        t.clips.push(c1);
        t.clips.push(c2);
        assert_eq!(track_append_point(&t), expected_end);
    }

    #[test]
    fn karaoke_preset_seeds_inactive_from_current_fill() {
        let fill = Color::new(0.1, 0.2, 0.3, 1.0);
        let preset = karaoke_preset(KaraokeMode::WordPop, fill);
        assert_eq!(preset.mode, KaraokeMode::WordPop);
        assert_eq!(preset.inactive_color, fill);
    }

    #[test]
    fn selected_clip_spans_finds_across_video_and_audio_tracks() {
        let mut seq = Sequence::new("s", photonic_core::timeline::FrameRate::FPS_30, 1920, 1080);
        let mut vt = Track::new(TrackKind::Video, "V1");
        let asset = photonic_core::timeline::AssetId::new();
        let clip = Clip::new(ClipSource::Asset { asset }, Tick(10), Tick(20));
        let cid = clip.id;
        vt.clips.push(clip);
        seq.video_tracks.push(vt);

        let spans = selected_clip_spans(&seq, &[cid]);
        assert_eq!(spans, vec![(Tick(10), Tick(30))]);
        assert!(selected_clip_spans(&seq, &[ClipId::new()]).is_empty());
    }
}
