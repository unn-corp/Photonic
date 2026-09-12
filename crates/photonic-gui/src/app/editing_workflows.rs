//! Session-only source/precision/navigation workflows over shared core planners.
use super::*;
use crate::panels::video::source_monitor::SourceCommand;
use photonic_core::timeline::{
    precision_trim::{self, TrimMode, TrimTarget},
    ClipSource, ClipTiming, TimelineCmd,
};

#[derive(Clone)]
pub(crate) struct PrecisionTrimSession {
    document: uuid::Uuid,
    revision: u64,
    pub target: TrimTarget,
    mode: TrimMode,
    frames: i64,
    return_target: Option<photonic_video::PreviewTarget>,
    return_quality: Option<photonic_video::PreviewQuality>,
    return_proxy: Option<photonic_video::ProxyMode>,
    return_playhead: Tick,
    loop_playing: bool,
}

#[derive(Clone)]
pub(crate) struct SequenceViewState {
    playhead: Tick,
    view: timeline::layout::TimelineView,
    selection: Vec<ClipId>,
}

pub(crate) fn candidate_document(doc: &Document, commands: &[TimelineCmd]) -> Document {
    let mut candidate = doc.clone();
    for command in commands {
        command.apply(&mut candidate);
    }
    candidate
}

impl PhotonicApp {
    pub(crate) fn video_audition_source(&mut self, doc: &Document) {
        let range = self.source_marks.armed_asset.and_then(|asset| {
            let media = doc.timeline.as_ref()?.media.assets.get(&asset)?;
            self.source_marks
                .audition_range(media)
                .map(|(start, end)| (asset, start, end))
        });
        if let Some((asset, start, end)) = range {
            self.source_panel_command(doc, SourceCommand::Audition { asset, start, end });
        } else {
            self.file_status = Some("Choose a ready source with a positive In/Out range.".into());
        }
    }
    pub(crate) fn source_panel_command(&mut self, doc: &Document, command: SourceCommand) {
        if matches!(
            command,
            SourceCommand::Audition { .. } | SourceCommand::Program
        ) {
            self.finish_precision_trim();
        }
        let Some(bridge) = self.engine.as_mut() else {
            self.file_status = Some("Source audition needs the video engine.".into());
            return;
        };
        let accepted = match command {
            SourceCommand::Audition { asset, start, end } => {
                if !doc
                    .timeline
                    .as_ref()
                    .is_some_and(|project| project.media.assets.contains_key(&asset))
                {
                    return;
                }
                self.monitor_playing = false;
                let Some(sequence) = doc
                    .timeline
                    .as_ref()
                    .and_then(|project| project.active_sequence)
                else {
                    return;
                };
                bridge.audition_source(sequence, self.playhead, asset, start, end)
            }
            SourceCommand::Stop => bridge
                .session()
                .send(photonic_video::EngineCmd::StopSourceAudition),
            SourceCommand::Program => {
                if let Some(sequence) = doc.timeline.as_ref().and_then(|p| p.active_sequence) {
                    bridge.peek_sequence(sequence);
                }
                true
            }
        };
        if !accepted {
            self.file_status =
                Some("Engine command queue is busy; try the source action again.".into());
        }
    }

    pub(crate) fn toggle_precision_trim(&mut self, doc: &Document, history: &CommandHistory) {
        if self.precision_trim.is_some() {
            self.finish_precision_trim();
            return;
        }
        let Some(project) = doc.timeline.as_ref() else {
            return;
        };
        let Some(sequence) = project.active_sequence else {
            return;
        };
        match precision_trim::resolve_target(
            project,
            sequence,
            self.timeline_selection.first().copied(),
            self.playhead,
        )
        .and_then(|target| {
            precision_trim::plan_trim(project, target, TrimMode::Roll, Tick::ZERO).map(|_| target)
        }) {
            Ok(target) => {
                self.video_pause();
                self.timeline_grab = None;
                self.precision_trim = Some(PrecisionTrimSession {
                    document: doc.id,
                    revision: history.revision(),
                    target,
                    mode: TrimMode::Roll,
                    frames: 0,
                    return_target: self.engine.as_ref().map(|e| e.preview_target.clone()),
                    return_quality: self.engine.as_ref().map(|e| e.preview_quality),
                    return_proxy: self.engine.as_ref().map(|e| e.proxy_mode),
                    return_playhead: self.playhead,
                    loop_playing: false,
                });
            }
            Err(error) => {
                self.file_status = Some(format!(
                    "Precision trim needs an unlocked adjacent cut: {error}"
                ))
            }
        }
    }
    pub(crate) fn finish_precision_trim(&mut self) {
        if let Some(session) = self.precision_trim.take() {
            self.monitor_playing = false;
            self.playhead = session.return_playhead;
            if let Some(engine) = self.engine.as_mut() {
                engine.restore_trim_candidate();
                engine.seek(self.playhead);
                engine.set_trim_stills(None);
                engine.set_playing(false);
                if let Some(target) = session.return_target {
                    engine.preview_target = target;
                    engine.apply_preview_target();
                }
                if let Some(quality) = session.return_quality {
                    engine.preview_quality = quality;
                    engine.apply_preview_quality();
                }
                if let Some(proxy) = session.return_proxy {
                    engine.proxy_mode = proxy;
                    engine.apply_proxy_mode();
                }
            }
        }
    }
    pub(crate) fn precision_keyboard(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) -> bool {
        if self.precision_trim.is_none() {
            return false;
        }
        if ctx.wants_keyboard_input() {
            return true;
        }
        if self.binding_pressed(ctx, "video.play_pause") {
            self.finish_precision_trim();
            self.video_play_pause();
            return true;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.finish_precision_trim();
            return true;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.commit_precision_trim(doc, history);
            return true;
        }
        if self.binding_pressed(ctx, "video.precision_trim") {
            self.finish_precision_trim();
            return true;
        }
        let Some(session) = self.precision_trim.as_mut() else {
            return true;
        };
        if ctx.input(|i| i.key_pressed(egui::Key::Tab)) {
            session.mode = session.mode.next();
            session.frames = 0;
        }
        let step = if ctx.input(|i| i.modifiers.shift) {
            5
        } else {
            1
        };
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
            session.frames = session.frames.saturating_sub(step);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
            session.frames = session.frames.saturating_add(step);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::K)) {
            session.loop_playing = false;
            self.monitor_playing = false;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::J) || i.key_pressed(egui::Key::L)) {
            session.loop_playing = true;
        }
        true
    }
    fn precision_plan(
        &self,
        doc: &Document,
        history: &CommandHistory,
    ) -> Result<Vec<TimelineCmd>, String> {
        let session = self
            .precision_trim
            .as_ref()
            .ok_or("Precision mode closed")?;
        if session.document != doc.id || session.revision != history.revision() {
            return Err("The timeline changed. Reopen precision trim for the current cut.".into());
        }
        let project = doc.timeline.as_ref().ok_or("No timeline")?;
        if project.active_sequence != Some(session.target.sequence) {
            return Err("The active sequence changed.".into());
        }
        let seq = project
            .sequences
            .get(&session.target.sequence)
            .ok_or("The sequence was removed")?;
        let delta = seq
            .frame_rate
            .ticks_per_frame()
            .0
            .checked_mul(session.frames)
            .map(Tick)
            .ok_or("Trim delta is too large")?;
        precision_trim::plan_trim(project, session.target, session.mode, delta)
            .map_err(|error| format!("Cannot apply this trim: {error}"))
    }
    fn commit_precision_trim(&mut self, doc: &mut Document, history: &mut CommandHistory) {
        match self.precision_plan(doc, history) {
            Ok(commands) => {
                if !commands.is_empty() {
                    history.execute_discrete(
                        photonic_core::history::Command::Batch(
                            commands
                                .into_iter()
                                .map(photonic_core::history::Command::Timeline)
                                .collect(),
                        ),
                        doc,
                    );
                }
                if let Some(engine) = self.engine.as_mut() {
                    engine.sync_document(doc, history);
                }
                self.finish_precision_trim();
            }
            Err(error) => self.file_status = Some(error),
        }
    }
    pub(crate) fn draw_precision_trim(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        let Some(session) = self.precision_trim.clone() else {
            return;
        };
        if session.document != doc.id
            || session.revision != history.revision()
            || doc.timeline.as_ref().and_then(|p| p.active_sequence)
                != Some(session.target.sequence)
        {
            if let Some(engine) = self.engine.as_mut() {
                engine.sync_document(doc, history);
            }
            self.finish_precision_trim();
            self.file_status = Some("Precision trim closed because its timeline changed.".into());
            return;
        }
        let plan = self.precision_plan(doc, history);
        let mut mode = session.mode;
        let mut frames = session.frames;
        let mut apply = false;
        let mut cancel = false;
        let mut loop_playing = session.loop_playing;
        let mut labels = [
            String::from("Outgoing source unavailable"),
            String::from("Incoming source unavailable"),
        ];
        let mut requests = Vec::new();
        if let Some(sequence) = doc
            .timeline
            .as_ref()
            .and_then(|p| p.sequences.get(&session.target.sequence))
        {
            if let Some(track) = sequence.track(session.target.track) {
                for (index, id) in [session.target.outgoing, session.target.incoming]
                    .into_iter()
                    .enumerate()
                {
                    if let Some(clip) = track.clips.iter().find(|c| c.id == id) {
                        let timing = plan
                            .as_ref()
                            .ok()
                            .and_then(|commands| {
                                commands.iter().find_map(|command| match command {
                                    TimelineCmd::RollEdit { changes, .. }
                                    | TimelineCmd::RippleEdit { changes, .. } => changes
                                        .iter()
                                        .find(|(cid, _, _)| *cid == id)
                                        .map(|(_, _, new)| *new),
                                    _ => None,
                                })
                            })
                            .unwrap_or_else(|| ClipTiming::of(clip));
                        let at = if index == 0 {
                            timing.source_in.saturating_add(
                                clip.speed.source_delta(Tick(
                                    timing
                                        .duration
                                        .0
                                        .saturating_sub(sequence.frame_rate.ticks_per_frame().0)
                                        .max(0),
                                )),
                            )
                        } else {
                            timing.source_in
                        };
                        labels[index] = format!(
                            "{} · {:.3} s",
                            if index == 0 {
                                "Outgoing last frame"
                            } else {
                                "Incoming first frame"
                            },
                            at.as_seconds_f64()
                        );
                        if let ClipSource::Asset { asset } = clip.source {
                            let rate = doc
                                .timeline
                                .as_ref()
                                .and_then(|p| p.media.assets.get(&asset))
                                .and_then(|m| m.probe.as_ref())
                                .and_then(|p| p.video.as_ref())
                                .map_or(sequence.frame_rate, |v| v.frame_rate);
                            requests.push((asset, rate.snap(at)));
                        }
                    }
                }
            }
        }
        if let Some(engine) = self.engine.as_mut() {
            if session.loop_playing && plan.is_ok() {
                engine.set_trim_stills(None);
                let commands = plan.as_ref().expect("checked");
                let changed = engine.preview_trim_candidate(doc, history, commands);
                let original_cut = doc
                    .timeline
                    .as_ref()
                    .and_then(|p| p.sequences.get(&session.target.sequence))
                    .and_then(|s| s.track(session.target.track))
                    .and_then(|t| t.clips.iter().find(|c| c.id == session.target.incoming))
                    .map_or(self.playhead, |c| c.start);
                let cut = commands
                    .iter()
                    .find_map(|command| match command {
                        TimelineCmd::RollEdit { changes, .. }
                        | TimelineCmd::RippleEdit { changes, .. } => changes
                            .iter()
                            .find(|(id, _, _)| *id == session.target.incoming)
                            .map(|(_, _, new)| new.start),
                        _ => None,
                    })
                    .unwrap_or(original_cut);
                engine.peek_sequence(session.target.sequence);
                let start = cut.saturating_sub(Tick::from_seconds(1)).max(Tick::ZERO);
                engine.set_loop(Some((start, cut.saturating_add(Tick::from_seconds(1)))));
                if changed {
                    engine.seek(start);
                }
                engine.set_playing(true);
                ctx.request_repaint();
            } else {
                engine.restore_trim_candidate();
                engine.set_playing(false);
                engine.prepare_full_preview();
                engine.set_trim_stills(
                    requests
                        .try_into()
                        .ok()
                        .map(|requests| (requests, session.revision)),
                );
            }
        }
        let images = self
            .engine
            .as_ref()
            .map(|e| e.trim_still_images())
            .unwrap_or([None, None]);
        egui::Window::new("Precision trim").id(egui::Id::new("precision_trim_window")).default_width(640.).resizable(true).show(ctx,|ui|{
            ui.columns(2,|columns|for index in 0..2 {
                if columns[index].selectable_label((index==0&&matches!(mode,TrimMode::RippleOutgoing|TrimMode::SlipOutgoing))||(index==1&&matches!(mode,TrimMode::RippleIncoming|TrimMode::SlipIncoming)),&labels[index]).clicked() {
                    mode=if index==0{TrimMode::RippleOutgoing}else{TrimMode::RippleIncoming};frames=0;
                }
                if let Some((id,logical,physical))=images[index] {
                    let width=columns[index].available_width().min(480.);
                    columns[index].add(egui::Image::new((id,egui::vec2(width,width*logical.1 as f32/logical.0.max(1) as f32))).uv(super::engine::padded_uv(logical,physical)));
                } else {columns[index].allocate_ui(egui::vec2(columns[index].available_width(),100.),|ui|{ui.centered_and_justified(|ui|{ui.label("Preparing exact source frame…");});});}
            });
            let (boundary,response)=ui.allocate_exact_size(egui::vec2(ui.available_width(),20.),egui::Sense::drag());
            ui.painter().text(boundary.center(),egui::Align2::CENTER_CENTER,"↔ Drag shared boundary to roll",egui::FontId::proportional(12.),ui.visuals().text_color());
            if response.dragged(){mode=TrimMode::Roll;frames=frames.saturating_add(response.drag_delta().x.round() as i64);}
            egui::ComboBox::from_id_salt("precision_mode").selected_text(mode.label()).show_ui(ui,|ui|for option in TrimMode::ALL{ui.selectable_value(&mut mode,option,option.label());});
            ui.horizontal(|ui|{
                if ui.button("−1").clicked(){frames=frames.saturating_sub(1);}
                ui.add(egui::DragValue::new(&mut frames).prefix("Delta ").suffix(" frames"));
                if ui.button("+1").clicked(){frames=frames.saturating_add(1);}
                ui.checkbox(&mut loop_playing,"Loop candidate cut · J/L");
            });
            if let Err(error)=&plan {ui.colored_label(ui.visuals().error_fg_color,error);}
            ui.small("Tab cycles edit side/mode · ←/→ one frame · Shift five frames · Enter applies · Esc cancels · K stops loop");
            ui.horizontal(|ui|{apply=ui.add_enabled(plan.is_ok(),egui::Button::new("Apply trim")).clicked();cancel=ui.button("Cancel").clicked();});
        });
        if let Some(session) = self.precision_trim.as_mut() {
            session.mode = mode;
            session.frames = frames;
            session.loop_playing = loop_playing;
        }
        if apply {
            self.commit_precision_trim(doc, history);
        } else if cancel {
            self.finish_precision_trim();
        }
        if images.iter().any(Option::is_none) {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    pub(crate) fn video_preview_action(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
        action: &str,
    ) {
        use photonic_core::timeline::{ops, sequence::PreviewZone};
        use photonic_video::EngineCmd;
        let Some(project) = doc.timeline.as_ref() else {
            return;
        };
        let Some(sequence) = project
            .active_sequence
            .and_then(|id| project.sequences.get(&id))
        else {
            return;
        };
        let seq_id = sequence.id;
        let range = sequence.work_range.filter(|(a, b)| b > a);
        if matches!(action, "video.render_preview" | "video.stop_preview_render") {
            if action == "video.render_preview" {
                let cache_dir = self
                    .current_file
                    .as_deref()
                    .map(photonic_video::media::cache_dir_for_project)
                    .unwrap_or_else(|| {
                        std::env::temp_dir()
                            .join("photonic-preview-cache")
                            .join(doc.id.to_string())
                    });
                if !self.engine.as_mut().is_some_and(|engine| {
                    engine.set_preview_cache_dir(cache_dir) && engine.prepare_full_preview()
                }) {
                    self.file_status=Some("Preview render waits for the engine to accept Full/original mode; try again.".into());
                    return;
                }
            }
            let command = if action == "video.render_preview" {
                EngineCmd::RenderPreview {
                    sequence: seq_id,
                    range,
                }
            } else {
                EngineCmd::CancelPreview { sequence: seq_id }
            };
            if !self
                .engine
                .as_ref()
                .is_some_and(|engine| engine.session().send(command))
            {
                self.file_status =
                    Some("Preview render could not start: engine unavailable or busy.".into());
            }
            return;
        }
        let mut zones = sequence.preview_zones.clone();
        match action {
            "video.add_preview_zone" => {
                let Some((start, end)) = range else {
                    self.file_status =
                        Some("Set sequence In and Out before adding a preview zone.".into());
                    return;
                };
                zones.push(PreviewZone { start, end });
            }
            "video.remove_preview_zone" => {
                let Some((start, end)) = range else {
                    self.file_status =
                        Some("Set sequence In and Out before removing a preview zone.".into());
                    return;
                };
                zones = zones
                    .into_iter()
                    .flat_map(|zone| {
                        if zone.end <= start || zone.start >= end {
                            return vec![zone];
                        }
                        let mut remains = Vec::new();
                        if zone.start < start {
                            remains.push(PreviewZone {
                                start: zone.start,
                                end: start,
                            });
                        }
                        if zone.end > end {
                            remains.push(PreviewZone {
                                start: end,
                                end: zone.end,
                            });
                        }
                        remains
                    })
                    .collect();
            }
            "video.remove_all_preview_zones" => zones.clear(),
            _ => return,
        }
        if zones == sequence.preview_zones {
            return;
        }
        match ops::set_preview_zones(project, seq_id, &zones) {
            Ok(command) => {
                history.execute_discrete(photonic_core::history::Command::Timeline(command), doc)
            }
            Err(error) => {
                self.file_status = Some(format!("Preview zone refused: {error}"));
                return;
            }
        }
        if action != "video.add_preview_zone" {
            let clear = EngineCmd::ClearPreview {
                sequence: seq_id,
                range: if action == "video.remove_all_preview_zones" {
                    None
                } else {
                    range
                },
            };
            if !self
                .engine
                .as_ref()
                .is_some_and(|engine| engine.session().send(clear))
            {
                self.file_status =
                    Some("Preview zone updated; engine cache clear will need retry.".into());
            }
        }
    }

    /// Remember independent views by document and sequence, filtering stale
    /// selections on restore. Reconciles tab clicks, undo, and nested entry.
    pub(crate) fn sync_sequence_view(&mut self, doc: &Document) {
        let current = doc
            .timeline
            .as_ref()
            .and_then(|p| p.active_sequence)
            .map(|sequence| (doc.id, sequence));
        if self.view_sequence == current {
            return;
        }
        if self.view_sequence.is_none() {
            self.view_sequence = current;
            return;
        }
        self.finish_precision_trim();
        if let Some(old) = self.view_sequence {
            if self.sequence_views.len() >= 128 && !self.sequence_views.contains_key(&old) {
                if let Some(key) = self.sequence_views.keys().next().copied() {
                    self.sequence_views.remove(&key);
                }
            }
            self.sequence_views.insert(
                old,
                SequenceViewState {
                    playhead: self.playhead,
                    view: self.timeline_view,
                    selection: self.timeline_selection.clone(),
                },
            );
            if old.0 != doc.id {
                self.nested_sequence_breadcrumbs.clear();
            }
        }
        self.view_sequence = current;
        let restored = current
            .and_then(|key| self.sequence_views.get(&key))
            .cloned();
        self.playhead = restored.as_ref().map_or(Tick::ZERO, |v| v.playhead);
        self.timeline_view = restored.as_ref().map_or_else(Default::default, |v| v.view);
        self.timeline_selection = restored.map_or_else(Vec::new, |v| v.selection);
        self.timeline_grab = None;
        if let Some(engine) = self.engine.as_mut() {
            engine.restore_trim_candidate();
        }
        self.precision_trim = None;
        self.target_video_track = None;
        self.target_audio_track = None;
        if let Some(sequence) = doc
            .timeline
            .as_ref()
            .and_then(|p| p.active_sequence.and_then(|id| p.sequences.get(&id)))
        {
            self.playhead = self
                .playhead
                .clamp(Tick::ZERO, sequence.content_end().max(Tick::ZERO));
            self.timeline_selection.retain(|id| {
                sequence
                    .tracks()
                    .any(|t| t.clips.iter().any(|c| c.id == *id))
            });
            self.nested_sequence_breadcrumbs.retain(|id| {
                doc.timeline
                    .as_ref()
                    .is_some_and(|p| p.sequences.contains_key(id))
            });
            if let Some(engine) = self.engine.as_mut() {
                engine.set_trim_stills(None);
                engine.peek_sequence(sequence.id);
                engine.seek(self.playhead);
            }
        }
        self.monitor_playing = false;
    }
    pub(crate) fn enter_nested_sequence(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        let Some(project) = doc.timeline.as_ref() else {
            return;
        };
        let Some(parent) = project.active_sequence else {
            return;
        };
        let Some(sequence) = project.sequences.get(&parent) else {
            return;
        };
        let nested = self
            .timeline_selection
            .iter()
            .find_map(|id| {
                sequence
                    .tracks()
                    .flat_map(|t| &t.clips)
                    .find(|c| c.id == *id)
            })
            .and_then(|clip| match clip.source {
                ClipSource::NestedSequence { sequence } => Some(sequence),
                _ => None,
            });
        let Some(child) = nested else {
            self.file_status = Some("Select a nested sequence clip first.".into());
            return;
        };
        if child == parent
            || self.nested_sequence_breadcrumbs.contains(&child)
            || !project.sequences.contains_key(&child)
        {
            self.file_status = Some("Cannot enter a missing or recursive nested sequence.".into());
            return;
        }
        let command = photonic_core::timeline::ops::set_active_sequence(project, Some(child));
        self.nested_sequence_breadcrumbs.push(parent);
        history.execute_discrete(photonic_core::history::Command::Timeline(command), doc);
        self.sync_sequence_view(doc);
    }
    pub(crate) fn leave_nested_sequence(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        let Some(project) = doc.timeline.as_ref() else {
            return;
        };
        while let Some(parent) = self.nested_sequence_breadcrumbs.pop() {
            if project.sequences.contains_key(&parent) {
                let command =
                    photonic_core::timeline::ops::set_active_sequence(project, Some(parent));
                history.execute_discrete(photonic_core::history::Command::Timeline(command), doc);
                self.sync_sequence_view(doc);
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::{
        AssetId, Clip, FrameRate, Sequence, TimelineProject, Track, TrackKind,
    };
    fn fixture() -> (Document, ClipId, ClipId) {
        let mut doc = Document::new("precision", 1., 1.);
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("main", FrameRate::FPS_30, 1920, 1080);
        let mut track = Track::new(TrackKind::Video, "video");
        let left = Clip::new(
            ClipSource::Asset {
                asset: AssetId::new(),
            },
            Tick::ZERO,
            Tick::from_seconds(2),
        );
        let mut right = Clip::new(
            left.source.clone(),
            Tick::from_seconds(2),
            Tick::from_seconds(2),
        );
        right.source_in = Tick::from_seconds(1);
        let ids = (left.id, right.id);
        track.clips = vec![left, right];
        seq.video_tracks.push(track);
        let id = project.insert_sequence(seq);
        project.active_sequence = Some(id);
        doc.timeline = Some(project);
        (doc, ids.0, ids.1)
    }
    #[test]
    fn precision_candidate_shared_plan_cancel_and_one_undo() {
        let (mut doc, left, _) = fixture();
        let original = doc.clone();
        let mut history = CommandHistory::new(8);
        let revision = history.revision();
        let mut app = PhotonicApp::default();
        app.timeline_selection = vec![left];
        app.playhead = Tick::from_seconds(1);
        app.toggle_precision_trim(&doc, &history);
        app.precision_trim.as_mut().unwrap().frames = 3;
        let plan = app.precision_plan(&doc, &history).unwrap();
        let state = app.precision_trim.as_ref().unwrap();
        let shared = precision_trim::plan_trim(
            doc.timeline.as_ref().unwrap(),
            state.target,
            state.mode,
            Tick(3 * FrameRate::FPS_30.ticks_per_frame().0),
        )
        .unwrap();
        assert_eq!(plan, shared);
        let candidate = candidate_document(&doc, &plan);
        assert_ne!(candidate.timeline, doc.timeline);
        assert_eq!(doc.timeline, original.timeline);
        assert_eq!(history.revision(), revision);
        app.finish_precision_trim();
        assert_eq!(app.playhead, Tick::from_seconds(1));
        assert_eq!(history.revision(), revision);
        assert_eq!(doc.timeline, original.timeline);
        app.toggle_precision_trim(&doc, &history);
        app.precision_trim.as_mut().unwrap().frames = 3;
        app.commit_precision_trim(&mut doc, &mut history);
        assert_ne!(doc.timeline, original.timeline);
        history.undo(&mut doc);
        assert_eq!(doc.timeline, original.timeline);
    }
    #[test]
    fn precision_stale_revision_refuses_without_writing_candidate() {
        let (mut doc, left, _) = fixture();
        let mut history = CommandHistory::new(8);
        let mut app = PhotonicApp::default();
        app.timeline_selection = vec![left];
        app.toggle_precision_trim(&doc, &history);
        history.reset();
        let original = doc.timeline.clone();
        app.commit_precision_trim(&mut doc, &mut history);
        assert_eq!(doc.timeline, original);
        assert!(app.file_status.as_ref().unwrap().contains("changed"));
    }
    #[test]
    fn nested_navigation_restores_independent_view_and_filters_stale_selection() {
        let (mut doc, _, _) = fixture();
        let project = doc.timeline.as_mut().unwrap();
        let parent = project.active_sequence.unwrap();
        let child = project.insert_sequence(Sequence::new("child", FrameRate::FPS_30, 1920, 1080));
        let clip = Clip::new(
            ClipSource::NestedSequence { sequence: child },
            Tick::from_seconds(4),
            Tick::from_seconds(1),
        );
        let id = clip.id;
        project.sequences.get_mut(&parent).unwrap().video_tracks[0]
            .clips
            .push(clip);
        let mut app = PhotonicApp::default();
        let mut history = CommandHistory::new(8);
        app.timeline_selection = vec![id];
        app.playhead = Tick::from_seconds(4);
        app.timeline_view.scroll_ticks = Tick::from_seconds(2);
        app.sync_sequence_view(&doc);
        app.enter_nested_sequence(&mut doc, &mut history);
        assert_eq!(doc.timeline.as_ref().unwrap().active_sequence, Some(child));
        assert!(app.timeline_selection.is_empty());
        assert_eq!(app.playhead, Tick::ZERO);
        app.leave_nested_sequence(&mut doc, &mut history);
        assert_eq!(app.timeline_selection, vec![id]);
        assert_eq!(app.playhead, Tick::from_seconds(4));
        assert_eq!(app.timeline_view.scroll_ticks, Tick::from_seconds(2));
        app.enter_nested_sequence(&mut doc, &mut history);
        doc.timeline
            .as_mut()
            .unwrap()
            .sequences
            .get_mut(&parent)
            .unwrap()
            .video_tracks[0]
            .clips
            .retain(|c| c.id != id);
        app.leave_nested_sequence(&mut doc, &mut history);
        assert!(app.timeline_selection.is_empty());
    }
}
