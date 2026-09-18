//! Transcript drawer: derived captions, local filler preview, and shared core
//! plans. All session state is disposable egui data; no transcript is persisted.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::{Arc, Mutex};

use egui::{Color32, Id, RichText, Ui};
use photonic_core::timeline::{
    transcript::{self, TranscriptError, TranscriptProjection, TranscriptTokenRef},
    CaptionTrack, ClipId, Sequence, SequenceId, Tick, TimelineCmd, TimelineProject, TrackId,
};
use photonic_core::Document;

use crate::panels::{PanelAction, PropPanelCtx};

/// Session-only commands queued by the shared command registry. The remove
/// command is explicitly the ripple-timeline action shown in this drawer.
#[derive(Clone, Copy, Debug)]
pub(crate) enum TranscriptCommand {
    RemoveSelection,
    FindFillers,
}

pub(crate) fn request_command(ctx: &egui::Context, command: TranscriptCommand) {
    ctx.data_mut(|data| data.insert_temp(Id::new("transcript_pending_command"), command));
}

fn track_choice_id(document: &Document, sequence: SequenceId) -> Id {
    Id::new(("transcript_track", document.id, sequence))
}

fn session_id(document: &Document, sequence: SequenceId, track: TrackId) -> Id {
    Id::new(("transcript_session", document.id, sequence, track))
}

#[derive(Clone)]
struct WordSelection {
    anchor: TranscriptTokenRef,
    focus: TranscriptTokenRef,
}

impl WordSelection {
    fn indices(&self, view: &TranscriptProjection) -> Option<Range<usize>> {
        let anchor = view.resolve(&self.anchor).ok()?;
        let focus = view.resolve(&self.focus).ok()?;
        Some(anchor.min(focus)..anchor.max(focus) + 1)
    }
}

#[derive(Clone, Debug)]
enum TranscriptRow {
    Cue(Tick),
    Words(Range<usize>),
}

#[derive(Default)]
struct TranscriptSession {
    source: Option<CaptionTrack>,
    projection: Arc<TranscriptProjection>,
    rows: Arc<Vec<TranscriptRow>>,
    layout_width: f32,
    selection: Option<WordSelection>,
    editor_word: Option<TranscriptTokenRef>,
    editor_text: String,
    hovered: Option<usize>,
    lexicon: String,
    fillers: Vec<TranscriptTokenRef>,
    filler_types: String,
    excluded: HashSet<(photonic_core::timeline::CueId, usize)>,
    previewed: bool,
    status: Option<String>,
}

impl TranscriptSession {
    fn selected_words(&self) -> Option<&[TranscriptTokenRef]> {
        Some(&self.projection.tokens[self.selection.as_ref()?.indices(&self.projection)?])
    }

    fn refresh(
        &mut self,
        project: &TimelineProject,
        sequence: SequenceId,
        track: &CaptionTrack,
    ) -> Result<(), TranscriptError> {
        if self.source.as_ref() == Some(track) {
            return Ok(());
        }
        self.projection = Arc::new(transcript::get_transcript(project, sequence, track.id)?);
        self.source = Some(track.clone());
        self.rows = Arc::default();
        self.layout_width = 0.0;
        self.hovered = None;
        if self
            .selection
            .as_ref()
            .is_some_and(|selection| selection.indices(&self.projection).is_none())
        {
            self.selection = None;
        }
        if self
            .editor_word
            .as_ref()
            .is_some_and(|word| self.projection.resolve(word).is_err())
        {
            self.editor_word = None;
        }
        if self
            .fillers
            .iter()
            .any(|word| self.projection.resolve(word).is_err())
        {
            self.fillers.clear();
            self.excluded.clear();
            self.previewed = false;
            self.status = Some("Transcript changed; preview filler words again.".into());
        }
        Ok(())
    }

    fn find_fillers(&mut self) {
        let lexicon: Vec<_> = self
            .lexicon
            .split(',')
            .map(|word| word.trim().to_owned())
            .filter(|word| !word.is_empty())
            .collect();
        self.fillers = transcript::find_filler_words(&self.projection, &lexicon);
        let mut types: Vec<_> = self
            .fillers
            .iter()
            .map(|word| {
                word.text
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase()
            })
            .collect();
        types.sort();
        types.dedup();
        self.filler_types = types.join(", ");
        self.excluded.clear();
        self.previewed = true;
        self.status = None;
    }
}

/// The transcript's current range for timeline cross-highlighting. Stale
/// selections disappear until the drawer refreshes from the changed captions.
pub(crate) fn selected_range(ctx: &egui::Context, document: &Document) -> Option<(Tick, Tick)> {
    let project = document.timeline.as_ref()?;
    let sequence_id = project.active_sequence?;
    let sequence = project.sequences.get(&sequence_id)?;
    let track_id =
        ctx.data(|data| data.get_temp::<TrackId>(track_choice_id(document, sequence_id)))?;
    let track = sequence
        .caption_tracks
        .iter()
        .find(|track| track.id == track_id)?;
    let session = ctx.data(|data| {
        data.get_temp::<Arc<Mutex<TranscriptSession>>>(session_id(document, sequence_id, track_id))
    })?;
    let state = session.lock().ok()?;
    if state.source.as_ref() != Some(track) {
        return None;
    }
    transcript::token_range(state.selected_words()?, sequence.frame_rate).ok()
}

/// Left-rail Transcript drawer. Media edits target the selected dialogue
/// clip's linked group plus sync-lock, as resolved by the shared core helper.
pub(crate) fn draw_transcript(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    // Consume requests even when there is no transcript, so a command cannot
    // linger and later edit a different sequence or document.
    let pending = ui.data_mut(|data| {
        let id = Id::new("transcript_pending_command");
        let command = data.get_temp::<TranscriptCommand>(id);
        data.remove::<TranscriptCommand>(id);
        command
    });
    let Some(project) = ctx.doc.timeline.as_ref() else {
        ui.label("No video project yet.");
        return;
    };
    let Some(sequence_id) = project.active_sequence else {
        ui.label("No active sequence.");
        return;
    };
    let Some(sequence) = project.sequences.get(&sequence_id) else {
        return;
    };
    if sequence.caption_tracks.is_empty() {
        ui.label("No transcript yet. Generate or import word-timed captions in Captions.");
        return;
    }
    *ctx.video.transcript_panel_open = true;
    let choice_id = track_choice_id(ctx.doc, sequence_id);
    let remembered = ui.data(|data| data.get_temp::<TrackId>(choice_id));
    let mut chosen =
        remembered.filter(|id| sequence.caption_tracks.iter().any(|track| track.id == *id));
    if sequence.caption_tracks.len() == 1 {
        chosen = Some(sequence.caption_tracks[0].id);
    }
    egui::ComboBox::from_id_salt(choice_id)
        .selected_text(
            chosen
                .and_then(|id| sequence.caption_tracks.iter().find(|track| track.id == id))
                .map_or("Choose caption track", |track| track.name.as_str()),
        )
        .show_ui(ui, |ui| {
            for track in &sequence.caption_tracks {
                ui.selectable_value(&mut chosen, Some(track.id), &track.name);
            }
        });
    let Some(track_id) = chosen else {
        ui.label("Choose the caption track for this transcript.");
        return;
    };
    ui.data_mut(|data| data.insert_temp(choice_id, track_id));
    let Some(track) = sequence
        .caption_tracks
        .iter()
        .find(|track| track.id == track_id)
    else {
        return;
    };
    let cache_id = session_id(ctx.doc, sequence_id, track_id);
    let cached = ui.data(|data| data.get_temp::<Arc<Mutex<TranscriptSession>>>(cache_id));
    let session = cached.unwrap_or_else(|| {
        Arc::new(Mutex::new(TranscriptSession {
            lexicon: transcript::DEFAULT_FILLER_WORDS.join(", "),
            ..TranscriptSession::default()
        }))
    });
    ui.data_mut(|data| data.insert_temp(cache_id, session.clone()));
    let Ok(mut state) = session.lock() else {
        ui.data_mut(|data| data.remove::<Arc<Mutex<TranscriptSession>>>(cache_id));
        ui.label("Refreshing transcript session.");
        return;
    };
    if let Err(error) = state.refresh(project, sequence_id, track) {
        ui.label(error.to_string());
        return;
    }

    let scope = transcript::dialogue_track_scope(project, sequence_id, ctx.video.selection);
    match &scope {
        Ok(tracks) => {
            let names = tracks
                .iter()
                .filter_map(|id| sequence.track(*id))
                .map(|track| track.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            ui.small(format!("Timeline targets: {names}"));
        }
        Err(reason) => {
            ui.small(reason.to_string());
        }
    }
    ui.small("Click a word to select and seek. Shift-click extends. Hover previews; double-click edits wording.");
    if matches!(pending, Some(TranscriptCommand::FindFillers)) {
        state.find_fillers();
    }
    let selection_view = state.projection.clone();
    let selected = state
        .selection
        .as_ref()
        .and_then(|selection| selection.indices(&selection_view))
        .map(|indices| &selection_view.tokens[indices])
        .unwrap_or_default();
    let mut text_only = false;
    let mut ripple = matches!(pending, Some(TranscriptCommand::RemoveSelection));
    let mut lift = false;
    ui.horizontal_wrapped(|ui| {
        text_only = ui.add_enabled(!selected.is_empty(), egui::Button::new("Delete text only")).clicked();
        ripple |= ui.add_enabled(!selected.is_empty() && scope.is_ok(), egui::Button::new("Ripple timeline")).on_hover_text("Remove the selected word interval from the listed tracks and captions, then close the gap. One undo step.").clicked();
        lift = ui.add_enabled(!selected.is_empty() && scope.is_ok(), egui::Button::new("Lift timeline")).on_hover_text("Remove the selected interval and captions, leaving a gap.").clicked();
    });
    if text_only {
        let plan = transcript::delete_transcript_text(project, sequence_id, track_id, selected);
        finish_plan(
            ctx,
            &mut state,
            plan,
            "Deleted selected transcript wording.",
        );
    } else if ripple || lift {
        let plan = scope.as_ref().map_err(Clone::clone).and_then(|tracks| {
            let range = transcript::token_range(selected, sequence.frame_rate)?;
            transcript::delete_transcript_range(
                project,
                sequence_id,
                track_id,
                tracks,
                range,
                ripple,
            )
        });
        finish_plan(
            ctx,
            &mut state,
            plan,
            if ripple {
                "Ripple-deleted selected interval."
            } else {
                "Lifted selected interval."
            },
        );
    }
    draw_word_editor(ui, ctx, project, sequence_id, track_id, &mut state);
    draw_filler_preview(
        ui,
        ctx,
        project,
        sequence_id,
        track_id,
        scope.as_deref(),
        &mut state,
    );
    if let Some(status) = &state.status {
        ui.label(status);
    }
    let view = state.projection.clone();
    if view.adjusted_word_count > 0 || view.has_overlapping_words {
        ui.colored_label(Color32::YELLOW, "Caption timings need correction before timeline deletion. Original wording and timing are preserved.");
    }
    if view.tokens.is_empty() {
        ui.label("This caption track has no timed words.");
        return;
    }
    ui.separator();
    ui.small(format!("{} words", view.tokens.len()));
    let row_height = ui
        .spacing()
        .interact_size
        .y
        .max(ui.text_style_height(&egui::TextStyle::Body));
    let width = (ui.available_width() - 12.0).max(40.0);
    if state.rows.is_empty() || (state.layout_width - width).abs() > 1.0 {
        state.rows = Arc::new(layout_rows(ui, &view, width));
        state.layout_width = width;
    }
    let rows = state.rows.clone();
    let selection_indices = state
        .selection
        .as_ref()
        .and_then(|selection| selection.indices(&view));
    let active = view.active_at(ctx.video.playhead);
    let clip_spans = selected_clip_spans(sequence, ctx.video.selection);
    let previous_hover = state.hovered;
    let mut hovered = None;
    let output = egui::ScrollArea::vertical()
        .id_salt(("transcript_words", track_id))
        .max_height((ui.clip_rect().bottom() - ui.cursor().top()).max(120.0))
        .vertical_scroll_offset(*ctx.video.transcript_scroll)
        .auto_shrink([false, false])
        .show_rows(ui, row_height, rows.len(), |ui, visible| {
            for row in &rows[visible] {
                match row {
                    TranscriptRow::Cue(at) => {
                        ui.add_sized(
                            [ui.available_width(), row_height],
                            egui::Label::new(
                                RichText::new(format!("{:07.2}s", at.as_seconds_f64()))
                                    .small()
                                    .weak(),
                            ),
                        );
                    }
                    TranscriptRow::Words(indices) => {
                        ui.horizontal(|ui| {
                            ui.set_height(row_height);
                            for index in indices.clone() {
                                let word = &view.tokens[index];
                                let selected = selection_indices
                                    .as_ref()
                                    .is_some_and(|range| range.contains(&index));
                                let in_clip = clip_spans
                                    .iter()
                                    .any(|(start, end)| *start < word.end && *end > word.start);
                                let mut label = RichText::new(display_word(&word.text));
                                if active == Some(index) {
                                    label = label.strong().color(ui.visuals().hyperlink_color);
                                } else if in_clip {
                                    label = label.underline();
                                }
                                let response =
                                    ui.selectable_label(selected, label).on_hover_text(format!(
                                        "{:.3}–{:.3}s",
                                        word.start.as_seconds_f64(),
                                        word.end.as_seconds_f64()
                                    ));
                                if response.hovered() {
                                    hovered = Some(index);
                                    if previous_hover != Some(index) && ctx.action.is_none() {
                                        ctx.action =
                                            Some(PanelAction::SeekPlayhead { at: word.start });
                                    }
                                }
                                if response.clicked() {
                                    let anchor = if ui.input(|input| input.modifiers.shift) {
                                        state
                                            .selection
                                            .as_ref()
                                            .map(|selection| selection.anchor.clone())
                                            .unwrap_or_else(|| word.clone())
                                    } else {
                                        word.clone()
                                    };
                                    state.selection = Some(WordSelection {
                                        anchor,
                                        focus: word.clone(),
                                    });
                                    if ctx.action.is_none() {
                                        ctx.action =
                                            Some(PanelAction::SeekPlayhead { at: word.start });
                                    }
                                }
                                if response.double_clicked() {
                                    state.editor_word = Some(word.clone());
                                    state.editor_text = word.text.clone();
                                }
                            }
                        });
                    }
                }
            }
        });
    *ctx.video.transcript_scroll = output.state.offset.y;
    state.hovered = hovered;
}

fn finish_plan(
    ctx: &mut PropPanelCtx,
    state: &mut TranscriptSession,
    plan: Result<Vec<TimelineCmd>, TranscriptError>,
    message: &str,
) {
    match plan {
        Ok(commands) => {
            if !commands.is_empty() {
                ctx.action = Some(PanelAction::CaptionEditBatch(commands));
                state.selection = None;
                state.editor_word = None;
                state.fillers.clear();
                state.excluded.clear();
                state.previewed = false;
            }
            state.status = Some(message.into());
        }
        Err(error) => state.status = Some(error.to_string()),
    }
}

fn draw_word_editor(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    project: &TimelineProject,
    sequence: SequenceId,
    track: TrackId,
    state: &mut TranscriptSession,
) {
    let Some(word) = state.editor_word.clone() else {
        return;
    };
    ui.label("Edit wording (timing stays fixed)");
    ui.text_edit_singleline(&mut state.editor_text);
    ui.horizontal(|ui| {
        if ui.button("Save wording").clicked() {
            let plan = state.projection.resolve(&word).and_then(|_| {
                transcript::edit_transcript_word(
                    project,
                    sequence,
                    track,
                    word.cue,
                    word.word_index,
                    state.editor_text.clone(),
                )
            });
            finish_plan(ctx, state, plan, "Updated transcript wording.");
        }
        if ui.button("Cancel").clicked() {
            state.editor_word = None;
        }
    });
}

fn draw_filler_preview(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    project: &TimelineProject,
    sequence: SequenceId,
    track: TrackId,
    scope: Result<&[TrackId], &TranscriptError>,
    state: &mut TranscriptSession,
) {
    egui::CollapsingHeader::new("Filler words")
        .default_open(true)
        .show(ui, |ui| {
            ui.label("Local lexicon (comma separated)");
            ui.text_edit_singleline(&mut state.lexicon);
            if ui.button("Find fillers").clicked() {
                state.find_fillers();
            }
            if !state.previewed {
                return;
            }
            ui.small(format!(
                "{} matches; uncheck any to keep.",
                state.fillers.len()
            ));
            if !state.filler_types.is_empty() {
                ui.small(format!("Types: {}", state.filler_types));
            }
            let row_height = ui.spacing().interact_size.y;
            egui::ScrollArea::vertical()
                .id_salt(("transcript_fillers", track))
                .max_height(140.0)
                .show_rows(ui, row_height, state.fillers.len(), |ui, visible| {
                    for index in visible {
                        let word = &state.fillers[index];
                        let key = (word.cue, word.word_index);
                        let mut included = !state.excluded.contains(&key);
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut included, "").changed() {
                                if included {
                                    state.excluded.remove(&key);
                                } else {
                                    state.excluded.insert(key);
                                }
                            }
                            if ui
                                .link(format!(
                                    "{} · {:.2}s",
                                    display_word(&word.text),
                                    word.start.as_seconds_f64()
                                ))
                                .clicked()
                                && ctx.action.is_none()
                            {
                                ctx.action = Some(PanelAction::SeekPlayhead { at: word.start });
                            }
                        });
                    }
                });
            let included_count = state.fillers.len().saturating_sub(state.excluded.len());
            if ui
                .add_enabled(
                    included_count > 0 && scope.is_ok(),
                    egui::Button::new(format!("Ripple-remove {included_count} included fillers")),
                )
                .clicked()
            {
                let included: Vec<_> = state
                    .fillers
                    .iter()
                    .filter(|word| !state.excluded.contains(&(word.cue, word.word_index)))
                    .cloned()
                    .collect();
                let plan = scope.map_err(Clone::clone).and_then(|targets| {
                    transcript::remove_filler_words(
                        project, sequence, track, targets, &included, true,
                    )
                });
                finish_plan(ctx, state, plan, "Ripple-removed included filler words.");
            }
        });
}

fn display_word(text: &str) -> String {
    text.trim().replace(['\n', '\r', '\t'], " ")
}

fn selected_clip_spans(sequence: &Sequence, selection: &[ClipId]) -> Vec<(Tick, Tick)> {
    sequence
        .tracks()
        .flat_map(|track| &track.clips)
        .filter(|clip| selection.contains(&clip.id))
        .map(|clip| (clip.start, clip.end()))
        .collect()
}

fn layout_rows(ui: &Ui, view: &TranscriptProjection, width: f32) -> Vec<TranscriptRow> {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let word_widths: Vec<_> = view
        .tokens
        .iter()
        .map(|word| {
            ui.painter()
                .layout_no_wrap(display_word(&word.text), font.clone(), Color32::WHITE)
                .size()
                .x
                + 2.0 * ui.spacing().button_padding.x
        })
        .collect();
    wrap_rows(view, &word_widths, width, ui.spacing().item_spacing.x)
}

fn wrap_rows(
    view: &TranscriptProjection,
    widths: &[f32],
    width: f32,
    spacing: f32,
) -> Vec<TranscriptRow> {
    let mut rows = Vec::new();
    let mut start = 0;
    let mut used = 0.0;
    let mut previous_cue = None;
    for (index, word) in view.tokens.iter().enumerate() {
        if previous_cue != Some(word.cue) {
            if start < index {
                rows.push(TranscriptRow::Words(start..index));
            }
            rows.push(TranscriptRow::Cue(word.start));
            start = index;
            used = 0.0;
            previous_cue = Some(word.cue);
        } else if used + spacing + widths[index] > width && start < index {
            rows.push(TranscriptRow::Words(start..index));
            start = index;
            used = 0.0;
        }
        if start < index {
            used += spacing;
        }
        used += widths[index];
    }
    if start < view.tokens.len() {
        rows.push(TranscriptRow::Words(start..view.tokens.len()));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::{
        CaptionCue, CaptionWord, Clip, ClipSource, FrameRate, Track, TrackKind,
    };

    fn fixture(word_count: usize) -> (TimelineProject, SequenceId, TrackId, ClipId) {
        let mut project = TimelineProject::new();
        let mut sequence = Sequence::new("Transcript", FrameRate::FPS_30, 1920, 1080);
        let mut track = Track::new(TrackKind::Audio, "Dialogue");
        let clip = Clip::new(
            ClipSource::Adjustment,
            Tick::ZERO,
            Tick::from_seconds(word_count as i64),
        );
        let clip_id = clip.id;
        track.clips.push(clip);
        sequence.audio_tracks.push(track);
        let mut captions = CaptionTrack::new("Captions");
        captions.cues.push(CaptionCue::new(
            Tick::ZERO,
            Tick::from_seconds(word_count as i64),
            (0..word_count)
                .map(|index| {
                    CaptionWord::new(
                        format!("word{index}"),
                        Tick::from_seconds(index as i64),
                        Tick::from_seconds(index as i64 + 1),
                    )
                })
                .collect(),
        ));
        let caption_id = captions.id;
        sequence.caption_tracks.push(captions);
        let sequence_id = project.insert_sequence(sequence);
        (project, sequence_id, caption_id, clip_id)
    }

    #[test]
    fn shift_selection_maps_identically_to_the_shared_mcp_planner() {
        let (project, sequence, caption, clip) = fixture(4);
        let view = transcript::get_transcript(&project, sequence, caption).unwrap();
        let selection = WordSelection {
            anchor: view.tokens[2].clone(),
            focus: view.tokens[0].clone(),
        };
        let range = selection.indices(&view).unwrap();
        assert_eq!(range, 0..3);
        let scope = transcript::dialogue_track_scope(&project, sequence, &[clip]).unwrap();
        let gui_range =
            transcript::token_range(&view.tokens[range], project.sequences[&sequence].frame_rate)
                .unwrap();
        let gui_plan = transcript::delete_transcript_range(
            &project, sequence, caption, &scope, gui_range, true,
        )
        .unwrap();
        let mcp_plan = transcript::delete_transcript_range(
            &project,
            sequence,
            caption,
            &scope,
            (Tick::ZERO, Tick::from_seconds(3)),
            true,
        )
        .unwrap();
        assert_eq!(gui_plan, mcp_plan);
    }

    #[test]
    fn long_transcript_rows_are_bounded_and_selection_is_disposable() {
        let (project, sequence, caption, _) = fixture(20_000);
        let view = transcript::get_transcript(&project, sequence, caption).unwrap();
        let rows = wrap_rows(&view, &vec![40.0; view.tokens.len()], 200.0, 4.0);
        let rendered: usize = rows
            .iter()
            .filter_map(|row| match row {
                TranscriptRow::Words(range) => Some(range.len()),
                TranscriptRow::Cue(_) => None,
            })
            .sum();
        assert_eq!(rendered, 20_000);
        assert!(rows.iter().all(|row| match row {
            TranscriptRow::Words(range) => range.len() <= 4,
            TranscriptRow::Cue(_) => true,
        }));
        assert_eq!(view.active_at(Tick::from_seconds(19_999)), Some(19_999));
    }
}
