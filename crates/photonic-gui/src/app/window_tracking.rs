//! Explicit translation analysis proposals, isolated from the user's transport.
use super::*;
use photonic_render::window_tracking::{TrackingConfig, TrackingMethod};
use photonic_video::window_tracking::{
    analyze_window, validate_request, WindowTrackingProposal, WindowTrackingRequest,
};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

#[derive(Clone)]
struct Target {
    document: uuid::Uuid,
    sequence: SequenceId,
    clip: photonic_core::timeline::ClipId,
    op: photonic_core::timeline::GradeOpId,
    rate: photonic_core::timeline::FrameRate,
    start: i64,
    end: i64,
    name: String,
}
struct Job {
    epoch: u64,
    revision: u64,
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicU32>,
    receiver: std::sync::mpsc::Receiver<Result<WindowTrackingProposal, String>>,
}
#[derive(Default)]
pub(super) struct State {
    open: bool,
    epoch: u64,
    target: Option<Target>,
    job: Option<Job>,
    proposal: Option<WindowTrackingProposal>,
    status: String,
    replace: bool,
    partial: bool,
    method: TrackingMethod,
}
impl State {
    fn close(&mut self) {
        self.open = false;
        self.epoch = self.epoch.wrapping_add(1);
        self.proposal = None;
        self.target = None;
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
    fn poll(&mut self, revision: u64) {
        let Some(job) = &self.job else { return };
        if job.revision != revision {
            job.cancel.store(true, Ordering::Relaxed);
        }
        match job.receiver.try_recv() {
            Ok(result) => {
                let job = self.job.take().unwrap();
                if !self.open
                    || job.epoch != self.epoch
                    || job.revision != revision
                    || job.cancel.load(Ordering::Relaxed)
                {
                    self.status = "Analysis cancelled or context changed. Analyze again.".into();
                    return;
                }
                match result {
                    Ok(p) => {
                        self.status = if p.completed {
                            format!(
                                "{} samples ready for review. No edit applied.",
                                p.samples.len()
                            )
                        } else {
                            format!("Stopped after {} samples: {}. Partial result requires explicit approval.",p.samples.len(),p.stop_reason.as_deref().unwrap_or("confidence stop"))
                        };
                        self.proposal = Some(p);
                    }
                    Err(e) => self.status = e,
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.job = None;
                self.status = "Tracking worker stopped without a result.".into();
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
}
impl PhotonicApp {
    pub(super) fn open_window_tracking(
        &mut self,
        doc: &Document,
        clip_id: photonic_core::timeline::ClipId,
        op: photonic_core::timeline::GradeOpId,
    ) {
        let Some(project) = &doc.timeline else { return };
        let Some((sequence, clip)) = project.sequences.values().find_map(|s| {
            s.video_tracks
                .iter()
                .flat_map(|t| &t.clips)
                .find(|c| c.id == clip_id)
                .map(|c| (s, c))
        }) else {
            return;
        };
        self.window_tracking.close();
        let rate = sequence.frame_rate;
        let first = rate.frame_at(clip.start);
        let last = rate.frame_at(Tick(clip.end().0 - 1));
        let start = rate.frame_at(self.playhead).clamp(first, last);
        let end = if start < last {
            (start + 30).min(last)
        } else {
            (start - 30).max(first)
        };
        self.window_tracking.target = Some(Target {
            document: doc.id,
            sequence: sequence.id,
            clip: clip_id,
            op,
            rate,
            start,
            end,
            name: clip.name.clone(),
        });
        self.window_tracking.open = true;
        self.window_tracking.replace = false;
        self.window_tracking.partial = false;
        self.window_tracking.method = TrackingMethod::FixedReference;
        self.window_tracking.status =
            "Choose sequence frame endpoints, then analyze. Center translation only.".into();
    }
    pub(super) fn draw_window_tracking(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        if self.window_tracking.target.as_ref().is_some_and(|t| {
            t.document != doc.id
                || doc.timeline.as_ref().and_then(|p| p.active_sequence) != Some(t.sequence)
        }) {
            self.window_tracking.close();
        }
        self.window_tracking.poll(history.revision());
        if !self.window_tracking.open {
            return;
        }
        if self
            .window_tracking
            .proposal
            .as_ref()
            .is_some_and(|p| p.expected_revision != history.revision())
        {
            self.window_tracking.proposal = None;
            self.window_tracking.status = "Document changed. Analyze again.".into();
        }
        let mut open = true;
        let mut analyze = false;
        let mut apply = false;
        egui::Window::new("Track power window").open(&mut open).default_width(460.0).show(ctx,|ui|{
            let state=&mut self.window_tracking;
            if let Some(t)=state.target.as_mut(){
                ui.label(format!("Clip: {}",if t.name.is_empty(){t.clip.to_string()}else{t.name.clone()}));
                ui.label("Ellipse/rectangle center only. Scale, rotation and perspective are not tracked.");
                ui.add_enabled_ui(state.job.is_none(),|ui|{
                    ui.horizontal(|ui|{ui.label("Start sequence frame");if ui.add(egui::DragValue::new(&mut t.start)).changed(){state.proposal=None;}});
                    ui.horizontal(|ui|{ui.label("End sequence frame");if ui.add(egui::DragValue::new(&mut t.end)).changed(){state.proposal=None;}});
                    let old_method = state.method;
                    egui::ComboBox::from_id_salt("window_tracking_method")
                        .selected_text(match state.method { TrackingMethod::FixedReference => "Fixed reference", TrackingMethod::AdaptiveBidirectional => "Adaptive bidirectional" })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut state.method, TrackingMethod::FixedReference, "Fixed reference");
                            ui.selectable_value(&mut state.method, TrackingMethod::AdaptiveBidirectional, "Adaptive bidirectional");
                        });
                    if state.method != old_method { state.proposal = None; state.status.clear(); }
                    if state.method == TrackingMethod::AdaptiveBidirectional { ui.label("Updates the template each frame. Consistency checks limit, but do not eliminate, drift."); }
                    analyze=ui.button("Analyze window translation").clicked();
                });
            }
            if let Some(job)=&state.job{
                ui.add(egui::ProgressBar::new(job.progress.load(Ordering::Relaxed)as f32/1000.0).show_percentage());
                if ui.button("Cancel analysis").clicked(){job.cancel.store(true,Ordering::Relaxed);state.status="Cancelling analysis...".into();}
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
            ui.label(&state.status);
            if let Some(p)=&state.proposal{
                ui.label(format!("Method: {:?}", p.method));
                if p.method == TrackingMethod::AdaptiveBidirectional {
                    let error = p.samples.iter().filter_map(|s| s.roundtrip_error_pixels).fold(0.0_f64, f64::max);
                    ui.label(format!("Maximum roundtrip error: {error:.3} analysis pixels"));
                }

                if let(Some(first),Some(last))=(p.samples.first(),p.samples.last()){
                    ui.label(format!("Center: ({:.4}, {:.4}) to ({:.4}, {:.4})",first.center[0],first.center[1],last.center[0],last.center[1]));
                    let score=p.samples.iter().map(|s|s.score).fold(1.0_f64,f64::min);
                    ui.label(format!("Minimum correlation: {score:.3}; analysis {} × {}",p.analysis_size[0],p.analysis_size[1]));
                }
                ui.checkbox(&mut state.replace,"Allow replacing existing center keys in this interval");
                ui.label("Outside keys remain; adjacent interpolation can change. Endpoints hold outside the sampled interval.");
                if !p.completed{ui.checkbox(&mut state.partial,"Accept the partial result");}
                apply=ui.add_enabled(p.samples.len()>=2 && (p.completed||state.partial),egui::Button::new("Apply center keys")).clicked();
            }
        });
        if !open {
            self.window_tracking.close();
            return;
        }
        if analyze {
            let Some(t) = self.window_tracking.target.clone() else {
                return;
            };
            let request = WindowTrackingRequest {
                method: self.window_tracking.method,
                clip: t.clip,
                op: t.op,
                start: t.rate.frame_start(t.start),
                end: t.rate.frame_start(t.end),
                step_frames: 1,
                format_index: None,
                config: TrackingConfig::default(),
            };
            if let Err(e) = validate_request(doc, &request) {
                self.window_tracking.status = e;
                return;
            }
            if self.window_tracking.job.is_some() {
                return;
            }
            let Some(bridge) = self.engine.as_ref() else {
                self.window_tracking.status = "Video engine is not available.".into();
                return;
            };
            let gpu = photonic_video::GpuContext::new(
                Arc::clone(bridge.gpu().device()),
                Arc::clone(bridge.gpu().queue()),
            );
            let snapshot = doc.clone();
            let revision = history.revision();
            let cancel = Arc::new(AtomicBool::new(false));
            let worker_cancel = cancel.clone();
            let progress = Arc::new(AtomicU32::new(0));
            let worker_progress = progress.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            let context = ctx.clone();
            match std::thread::Builder::new()
                .name("window-tracking".into())
                .spawn(move || {
                    let result =
                        analyze_window(snapshot, revision, request, gpu, &worker_cancel, |p| {
                            worker_progress.store((p * 1000.0) as u32, Ordering::Relaxed);
                            context.request_repaint();
                        });
                    let _ = tx.send(result);
                    context.request_repaint();
                }) {
                Ok(_) => {
                    self.window_tracking.proposal = None;
                    self.window_tracking.status =
                        "Analyzing isolated frames. Your document and transport are unchanged."
                            .into();
                    self.window_tracking.job = Some(Job {
                        epoch: self.window_tracking.epoch,
                        revision,
                        cancel,
                        progress,
                        receiver: rx,
                    });
                }
                Err(e) => self.window_tracking.status = format!("Could not start tracking: {e}"),
            }
        }
        if apply {
            if let Some(p) = self.window_tracking.proposal.as_ref() {
                match p.apply_command(
                    doc,
                    history.revision(),
                    self.window_tracking.replace,
                    self.window_tracking.partial,
                ) {
                    Ok(command) => {
                        history.execute_discrete(Command::Timeline(command), doc);
                        self.window_tracking.proposal = None;
                        self.window_tracking.status =
                            "Center keys applied. Undo restores the original grade.".into();
                    }
                    Err(e) => self.window_tracking.status = e,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn job(
        epoch: u64,
        revision: u64,
    ) -> (
        Job,
        std::sync::mpsc::Sender<Result<WindowTrackingProposal, String>>,
        Arc<AtomicBool>,
    ) {
        let (tx, receiver) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        (
            Job {
                epoch,
                revision,
                cancel: cancel.clone(),
                progress: Arc::new(AtomicU32::new(0)),
                receiver,
            },
            tx,
            cancel,
        )
    }
    #[test]
    fn close_cancels_and_retains_admission_until_old_worker_drains() {
        let (j, tx, cancel) = job(0, 0);
        let mut state = State::default();
        state.open = true;
        state.job = Some(j);
        state.close();
        assert!(cancel.load(Ordering::Relaxed));
        assert!(state.job.is_some());
        state.open = true;
        tx.send(Err("old failure".into())).unwrap();
        state.poll(0);
        assert!(state.job.is_none());
        assert!(state.proposal.is_none());
        assert!(!state.status.contains("old failure"));
    }
    #[test]
    fn revision_change_cancels_without_applying_or_accepting_result() {
        let (j, tx, cancel) = job(0, 2);
        let mut state = State::default();
        state.open = true;
        state.job = Some(j);
        state.poll(3);
        assert!(cancel.load(Ordering::Relaxed));
        assert!(state.job.is_some());
        tx.send(Err("stale result".into())).unwrap();
        state.poll(3);
        assert!(state.job.is_none());
        assert!(state.proposal.is_none());
        assert!(state.status.contains("context"));
    }
    #[test]
    fn dropping_dialog_state_cooperatively_cancels_its_worker() {
        let (j, _tx, cancel) = job(0, 0);
        let mut state = State::default();
        state.job = Some(j);
        drop(state);
        assert!(cancel.load(Ordering::Relaxed));
    }
    #[test]
    fn opening_and_closing_tracking_does_not_change_document_or_playhead() {
        use photonic_core::timeline::{
            Clip, ClipSource, FrameRate, GradeOpId, Sequence, TimelineProject, Track, TrackKind,
        };
        let mut doc = Document::new("test", 96.0, 72.0);
        let mut p = TimelineProject::new();
        let mut s = Sequence::new("test", FrameRate::FPS_30, 96, 72);
        let mut t = Track::new(TrackKind::Video, "V1");
        let c = Clip::new(
            ClipSource::SolidColor {
                color: photonic_core::Color::rgb(0.2, 0.3, 0.4),
            },
            Tick::from_seconds(1),
            Tick::from_seconds(2),
        );
        let cid = c.id;
        t.clips.push(c);
        s.video_tracks.push(t);
        p.insert_sequence(s);
        doc.timeline = Some(p);
        let before = serde_json::to_value(&doc).unwrap();
        let mut app = PhotonicApp::default();
        app.playhead = Tick::from_seconds(2);
        app.open_window_tracking(&doc, cid, GradeOpId::new());
        assert!(app.window_tracking.open);
        let target = app.window_tracking.target.as_ref().unwrap();
        assert_eq!(target.start, 60);
        assert_eq!(target.end, 89);
        app.window_tracking.close();
        assert_eq!(app.playhead, Tick::from_seconds(2));
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
    }
}
