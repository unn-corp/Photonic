//! Read-only paused or bounded live paired native preview measurements.
use super::*;
use photonic_render::scene_measurement::{FrameMeasurements, MAX_PAIRED_PIXELS};
use photonic_video::graph::{ir::FrameColorEncoding, ScopeTapPoint};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    document: uuid::Uuid,
    sequence: SequenceId,
    time: Tick,
    revision: u64,
    generation: u64,
    hash: u128,
    size: (u32, u32),
    quality: photonic_video::PreviewQuality,
    proxy: photonic_video::ProxyMode,
}
impl Key {
    fn same_context(self, other: Self) -> bool {
        self.document == other.document
            && self.sequence == other.sequence
            && self.revision == other.revision
            && self.generation == other.generation
            && self.size == other.size
            && self.quality == other.quality
            && self.proxy == other.proxy
    }
}
const LIVE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
const MAX_LIVE_PIXELS: u64 = 2_073_600;
fn live_dimensions_supported(size: (u32, u32)) -> bool {
    size.0 > 0 && size.1 > 0 && u64::from(size.0) * u64::from(size.1) <= MAX_LIVE_PIXELS
}
struct Job {
    key: Key,
    epoch: u64,
    receiver: std::sync::mpsc::Receiver<Result<FrameMeasurements, String>>,
}
struct Preview {
    key: Key,
    values: FrameMeasurements,
    texture: egui::TextureHandle,
}
#[derive(Default)]
pub(super) struct State {
    pub open: bool,
    pub requested: bool,
    pub live: bool,
    last_started: Option<std::time::Instant>,
    target: Option<(uuid::Uuid, SequenceId)>,
    epoch: u64,
    was_playing: bool,
    job: Option<Job>,
    preview: Option<Preview>,
    status: String,
}
impl State {
    fn close(&mut self) {
        if self.open {
            self.epoch = self.epoch.wrapping_add(1);
        }
        self.open = false;
        self.live = false;
        self.last_started = None;
        self.was_playing = false;
        self.requested = false;
        self.preview = None;
        self.target = None;
        // Retain at most one in-flight worker until it drains, including after
        // close/reopen. An old completion cannot satisfy a newer capture epoch.
    }
    fn request(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.requested = true;
        self.preview = None;
        self.status = "Waiting for the paused preview...".into();
    }
    fn poll(&mut self, ctx: &egui::Context, current: Option<Key>) {
        self.poll_with_mode(ctx, current, false);
    }
    fn live_due(&self, now: std::time::Instant, current: Key) -> bool {
        self.live
            && self.job.is_none()
            && self
                .last_started
                .is_none_or(|last| now.saturating_duration_since(last) >= LIVE_INTERVAL)
            && self.preview.as_ref().is_none_or(|p| p.key != current)
    }
    fn set_live(&mut self, live: bool) {
        if self.live != live {
            self.live = live;
            self.epoch = self.epoch.wrapping_add(1);
            self.preview = None;
            self.requested = false;
            self.last_started = None;
            self.status = if live {
                "Live sampling armed; play to update."
            } else {
                "Live sampling off. Capture a paused preview."
            }
            .into();
        }
    }
    fn poll_with_mode(&mut self, ctx: &egui::Context, current: Option<Key>, live_playing: bool) {
        let accepts = |key: Key| {
            current.is_some_and(|now| {
                if live_playing {
                    key.same_context(now)
                } else {
                    key == now
                }
            })
        };
        if self.preview.as_ref().is_some_and(|p| !accepts(p.key)) {
            self.preview = None;
            self.status = "Frame changed. Capture again.".into();
        }
        let Some(job) = self.job.as_ref() else {
            return;
        };
        match job.receiver.try_recv() {
            Ok(result) => {
                let job = self.job.take().unwrap();
                if !self.open || self.epoch != job.epoch || !accepts(job.key) {
                    self.status = "Capture discarded because its context changed.".into();
                    return;
                }
                match result {
                    Ok(mut values) => {
                        let rgba = std::mem::take(&mut values.display_rgba);
                        let texture = ctx.load_texture(
                            "native-output-measurement",
                            egui::ColorImage::from_rgba_unmultiplied(
                                [job.key.size.0 as usize, job.key.size.1 as usize],
                                &rgba,
                            ),
                            egui::TextureOptions::LINEAR,
                        );
                        self.preview = Some(Preview {
                            key: job.key,
                            values,
                            texture,
                        });
                        self.status.clear();
                    }
                    Err(error) => self.status = format!("Measurement failed: {error}"),
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(50))
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.job = None;
                self.status = "Measurement worker stopped.".into();
            }
        }
    }
}
fn native_sequence(doc: &Document, sequence: SequenceId) -> bool {
    doc.timeline
        .as_ref()
        .filter(|p| p.active_sequence == Some(sequence))
        .and_then(|p| p.sequences.get(&sequence))
        .is_some_and(|s| !s.color.is_legacy())
}
fn formatted_channel(value: Option<[f64; 3]>, channel: usize) -> String {
    value
        .map(|v| {
            let n = v[channel];
            if n.abs() >= 10000.0 || (n != 0.0 && n.abs() < 0.0001) {
                format!("{n:.3e}")
            } else {
                format!("{n:.4}")
            }
        })
        .unwrap_or_else(|| "-".into())
}

impl PhotonicApp {
    pub(super) fn open_program_measurement(
        &mut self,
        ctx: &egui::Context,
        doc: &Document,
        sequence: SequenceId,
    ) {
        if !native_sequence(doc, sequence) {
            return;
        }
        self.program_measurement.open = true;
        self.program_measurement.target = Some((doc.id, sequence));
        self.request_program_measurement(ctx);
    }
    fn request_program_measurement(&mut self, ctx: &egui::Context) {
        self.grade_matte_target = None;
        self.grade_matte_preview = None;
        self.qualifier_matte_target = None;
        self.qualifier_matte_preview = None;
        self.eyedropper.cancel();
        if let Some(job) = self.video_eyedropper_job.as_mut() {
            job.canceled = true;
        }
        ctx.data_mut(|data| {
            data.insert_temp(
                egui::Id::new("qualifier_matte_target"),
                None::<(SequenceId, ClipId, GradeOpId, Option<u32>)>,
            )
        });
        self.program_measurement.request();
    }
    pub(super) fn update_program_measurement(
        &mut self,
        ctx: &egui::Context,
        doc: &Document,
        history: &CommandHistory,
    ) {
        if self.mode != AppMode::Video
            || self
                .program_measurement
                .target
                .is_some_and(|(id, seq)| id != doc.id || !native_sequence(doc, seq))
        {
            self.program_measurement.close();
        }
        if !self.program_measurement.open {
            self.program_measurement.poll(ctx, None);
            return;
        }
        let was_playing = self.program_measurement.was_playing;
        self.program_measurement.was_playing = self.monitor_playing;
        let live_playing = self.program_measurement.live && self.monitor_playing;
        let target = self.program_measurement.target;
        let frame = self.engine.as_ref().and_then(|b| b.session.latest_frame());
        let current = frame.as_ref().and_then(|frame| {
            let (id, seq) = target?;
            let presented = self.engine.as_ref()?.presented_frame;
            if (self.monitor_playing && !live_playing)
                || id != doc.id
                || frame.sequence != seq
                || frame.doc_revision != history.revision()
                // Paused capture must match the visible frame exactly. Live
                // sampling may read a newer immutable engine frame before the
                // GUI presents it; the UI labels its captured timestamp.
                || (!live_playing && (presented != Some((frame.time, frame.sequence))
                    || doc.timeline.as_ref().and_then(|p| p.sequences.get(&seq))
                        .is_none_or(|sequence| sequence.frame_rate.snap(self.playhead) != frame.time)))
                || frame.inspection_request_id.is_some()
                || frame.preview_asset.is_some()
                || frame.cached_preview
                || frame.output_encoding != FrameColorEncoding::SrgbDisplay
                || !frame.color_errors.is_empty()
                || !frame.grading_errors.is_empty()
            {
                return None;
            }
            Some(Key {
                document: id,
                sequence: seq,
                time: frame.time,
                revision: frame.doc_revision,
                generation: frame.snapshot_generation,
                hash: frame.content_hash.0,
                size: frame.logical_size,
                quality: frame.preview_quality,
                proxy: frame.proxy_mode,
            })
        });
        self.program_measurement
            .poll_with_mode(ctx, current, live_playing);
        if self.monitor_playing && !live_playing {
            self.program_measurement.status = "Pause playback to measure a frame.".into();
            return;
        }
        if was_playing && !self.monitor_playing {
            self.program_measurement.status =
                "Playback paused. Capture the current preview.".into();
        }
        if live_playing {
            ctx.request_repaint_after(LIVE_INTERVAL);
            if let Some(key) = current {
                if self
                    .program_measurement
                    .live_due(std::time::Instant::now(), key)
                {
                    self.program_measurement.requested = true;
                }
            }
        }
        if !self.program_measurement.requested || self.program_measurement.job.is_some() {
            return;
        }
        let (Some(key), Some(frame)) = (current, frame) else {
            self.program_measurement.status =
                "Waiting for a qualified, currently presented native frame.".into();
            return;
        };
        if live_playing && !live_dimensions_supported(key.size) {
            self.program_measurement.requested = false;
            self.program_measurement.status = "Live measurements are limited to 2,073,600 pixels. Use Draft or pause for a Full capture.".into();
            return;
        }
        if frame.scope_tap_point != ScopeTapPoint::NativeSceneProgram
            || frame.scope_tap_encoding != Some(FrameColorEncoding::SceneLinearAcescg)
        {
            return;
        }
        let Some(tap) = frame.scope_tap.as_ref() else {
            return;
        };
        if key.size != (tap.width, tap.height)
            || key.size.0 == 0
            || key.size.1 == 0
            || u64::from(key.size.0) * u64::from(key.size.1) > MAX_PAIRED_PIXELS as u64
        {
            self.program_measurement.requested = false;
            self.program_measurement.status="Paired preview readback is limited to 16,777,216 pixels with equal stage dimensions.".into();
            return;
        }
        let bridge = self.engine.as_ref().unwrap();
        let gpu = photonic_video::GpuContext::new(
            Arc::clone(bridge.gpu().device()),
            Arc::clone(bridge.gpu().queue()),
        );
        let scene = Arc::clone(&tap.texture);
        let display = Arc::clone(&frame.texture);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let a = photonic_video::graph::eval::read_texture_rgba16f(
                &gpu, &scene, key.size.0, key.size.1,
            );
            let b = photonic_video::graph::eval::read_texture_rgba16f(
                &gpu, &display, key.size.0, key.size.1,
            );
            let _ = sender.send(
                photonic_render::scene_measurement::measure_pair(&a, &b, key.size.0, key.size.1)
                    .map_err(str::to_owned),
            );
        });
        self.program_measurement.last_started = Some(std::time::Instant::now());
        self.program_measurement.job = Some(Job {
            key,
            epoch: self.program_measurement.epoch,
            receiver,
        });
        self.program_measurement.requested = false;
        self.program_measurement.status = "Measuring the captured preview...".into();
        // Release the requested special tap once texture Arcs are retained.
        // Normal scopes resume while the single bounded readback worker runs.
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
    pub(super) fn draw_program_measurement(&mut self, ctx: &egui::Context) {
        if !self.program_measurement.open {
            return;
        }
        let mut open = true;
        let mut capture = false;
        let mut live = self.program_measurement.live;
        let mut live_changed = false;
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        egui::Window::new("Native output measurements").id(egui::Id::new("native_output_measurements"))
            .open(&mut open).collapsible(false).resizable(true).default_size(egui::vec2(700.0,540.0))
            .min_width(570.0).vscroll(true).show(ctx,|ui| {
                ui.label("Paired scene and SDR-display measurements from one rendered frame.");
                live_changed = ui.checkbox(&mut live, "Live during playback (up to 4 samples/sec)").changed();
                if live { ui.small("One worker; latest completed sample may lag playback. Live limit: 1920 x 1080 pixels. Pause for exact-frame capture."); }
                ui.label("Preview resolution and requested proxy mode are shown below. Use Full/original for final checks.");
                capture=ui.add_enabled(!self.monitor_playing && self.program_measurement.job.is_none(),egui::Button::new("Capture current preview")).clicked();
                if !self.program_measurement.status.is_empty() {ui.label(&self.program_measurement.status);}
                if let Some(preview)=self.program_measurement.preview.as_ref() {
                    let k=preview.key;let m=&preview.values;
                    ui.horizontal(|ui| {
                        ui.add(egui::Image::new((preview.texture.id(),egui::vec2(k.size.0 as f32,k.size.1 as f32)*(160.0/k.size.0 as f32).min(100.0/k.size.1 as f32))));
                        ui.vertical(|ui| {
                            ui.label(format!("{} x {} | {:?} | {:.3} seconds",k.size.0,k.size.1,k.quality,k.time.as_seconds_f64()));
                            ui.label(format!("Requested proxy mode: {:?}",k.proxy));
                            ui.label(format!("Covered {} | transparent {}",m.covered_pixels,m.transparent_pixels));
                            ui.label(format!("Alpha {:.4} .. {:.4}",m.alpha_min,m.alpha_max));
                        });
                    });
                    ui.separator();
                    let number=formatted_channel;
                    egui::Grid::new("native_output_rgb_values").striped(true).show(ui,|ui| {
                        for label in ["RGB","AP1 min","AP1 mean","AP1 max","sRGB min","sRGB mean","sRGB max"] {ui.strong(label);}ui.end_row();
                        for (c,label) in ["R","G","B"].into_iter().enumerate() {
                            ui.label(label);
                            for a in [m.scene_rgb.min,m.scene_rgb.mean,m.scene_rgb.max,m.display_rgb.min,m.display_rgb.mean,m.display_rgb.max] {ui.monospace(number(a,c));}ui.end_row();
                        }
                    });
                    ui.label(format!("AP1 below 0: {:?} | above 1: {:?} (R/G/B counts)",m.scene_rgb.below_zero,m.scene_rgb.above_one));
                    ui.label(format!("sRGB below 0: {:?} | above 1: {:?}",m.display_rgb.below_zero,m.display_rgb.above_one));
                    ui.separator();ui.strong("Relative AP1 luminance: stops from 0.18");
                    let h=&m.scene_luminance_stops;
                    let (rect,_)=ui.allocate_exact_size(egui::vec2(ui.available_width(),112.0),egui::Sense::hover());
                    let plot=egui::Rect::from_min_max(rect.min,egui::pos2(rect.right(),rect.bottom()-20.0));
                    let maximum=h.counts.iter().copied().max().unwrap_or(1).max(1) as f32;
                    let painter=ui.painter();painter.rect_filled(plot,0.0_f32,egui::Color32::from_gray(25));
                    for (i,n) in h.counts.iter().enumerate() {
                        let x=plot.left()+i as f32*plot.width()/h.counts.len() as f32;
                        let width=plot.width()/h.counts.len() as f32;
                        let bar=egui::Rect::from_min_max(egui::pos2(x,plot.bottom()-plot.height()*(*n as f32/maximum)),egui::pos2(x+width,plot.bottom()));
                        painter.rect_filled(bar,0.0_f32,egui::Color32::from_rgb(115,170,225));
                    }
                    for (i,label) in ["-12","-6","0","+6","+12"].into_iter().enumerate() {
                        let align=if i==0 {egui::Align2::LEFT_TOP}else if i==4 {egui::Align2::RIGHT_TOP}else{egui::Align2::CENTER_TOP};
                        painter.text(egui::pos2(plot.left()+i as f32*plot.width()/4.0,plot.bottom()+3.0),align,label,egui::FontId::proportional(11.0),ui.visuals().text_color());
                    }
                    ui.label(format!("Nonpositive: {} | below -12: {} | at/above +12: {}",h.nonpositive,h.below_range,h.above_range));
                    ui.small("Unclipped straight values, equally weighted covered pixels. Relative working values, not physical nits, video IRE or calibrated HDR monitoring.");
                }
            });
        if !open || escape {
            self.program_measurement.close();
        } else if live_changed {
            // Cancel competing inspection tools before switching capture mode.
            self.request_program_measurement(ctx);
            self.program_measurement.set_live(live);
        } else if capture {
            self.request_program_measurement(ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::color::{NativeManagedColorConfig, SequenceColorConfig};
    use photonic_core::timeline::{
        Clip, ClipSource, FrameRate, Grade, GradeOp, GradeOpKind, GradeOpParams, Sequence,
        TimelineProject, Track, TrackKind,
    };
    fn key() -> Key {
        Key {
            document: uuid::Uuid::new_v4(),
            sequence: SequenceId::new(),
            time: Tick::ZERO,
            revision: 0,
            generation: 1,
            hash: 1,
            size: (2, 1),
            quality: photonic_video::PreviewQuality::Full,
            proxy: photonic_video::ProxyMode::ForceOriginal,
        }
    }
    fn values() -> FrameMeasurements {
        photonic_render::scene_measurement::measure_pair(
            &[[0.1, 0.1, 0.1, 1.0]; 2],
            &[[0.3, 0.3, 0.3, 1.0]; 2],
            2,
            1,
        )
        .unwrap()
    }
    #[test]
    fn extreme_readouts_remain_compact_and_missing_signal_is_explicit() {
        assert_eq!(formatted_channel(None, 0), "-");
        for v in [0.0, -0.2731, 65504.0, 1e20, 1e-12] {
            assert!(formatted_channel(Some([v; 3]), 0).len() <= 12);
        }
    }

    #[test]
    fn closed_and_reopened_capture_epochs_reject_old_worker_results() {
        let ctx = egui::Context::default();
        let k = key();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut state = State {
            open: true,
            epoch: 1,
            job: Some(Job {
                key: k,
                epoch: 1,
                receiver: rx,
            }),
            ..Default::default()
        };
        state.close();
        assert!(state.job.is_some());
        state.open = true;
        state.request();
        assert!(state.job.is_some());
        tx.send(Ok(values())).unwrap();
        state.poll(&ctx, Some(k));
        assert!(state.preview.is_none());
        assert!(state.job.is_none());
        assert!(state.requested);
    }
    #[test]
    fn changed_frame_and_close_remove_visible_measurements() {
        let ctx = egui::Context::default();
        let k = key();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut state = State {
            open: true,
            epoch: 1,
            job: Some(Job {
                key: k,
                epoch: 1,
                receiver: rx,
            }),
            ..Default::default()
        };
        tx.send(Ok(values())).unwrap();
        state.poll(&ctx, Some(k));
        assert!(state.preview.is_some());
        let mut changed = k;
        changed.time = Tick(1);
        state.poll(&ctx, Some(changed));
        assert!(state.preview.is_none());
        state.close();
        assert!(!state.open);
        assert!(!state.requested);
    }
    #[test]
    fn live_results_allow_time_progress_but_reject_every_context_change() {
        let ctx = egui::Context::default();
        let original = key();
        let mut later = original;
        later.time = Tick(10);
        later.hash = 999;
        let (tx, rx) = std::sync::mpsc::channel();
        let mut state = State {
            open: true,
            live: true,
            epoch: 1,
            job: Some(Job {
                key: original,
                epoch: 1,
                receiver: rx,
            }),
            ..Default::default()
        };
        tx.send(Ok(values())).unwrap();
        state.poll_with_mode(&ctx, Some(later), true);
        assert_eq!(
            state.preview.as_ref().unwrap().key,
            original,
            "display captured time, not current time"
        );
        let changes = [
            Key {
                document: uuid::Uuid::new_v4(),
                ..later
            },
            Key {
                sequence: SequenceId::new(),
                ..later
            },
            Key {
                revision: 1,
                ..later
            },
            Key {
                generation: 2,
                ..later
            },
            Key {
                size: (4, 1),
                ..later
            },
            Key {
                quality: photonic_video::PreviewQuality::Draft,
                ..later
            },
            Key {
                proxy: photonic_video::ProxyMode::Auto,
                ..later
            },
        ];
        for changed in changes {
            let (tx, rx) = std::sync::mpsc::channel();
            state.job = Some(Job {
                key: original,
                epoch: 1,
                receiver: rx,
            });
            tx.send(Ok(values())).unwrap();
            state.poll_with_mode(&ctx, Some(changed), true);
            assert!(
                state.preview.is_none(),
                "stale context accepted: {changed:?}"
            );
        }
        let (tx, rx) = std::sync::mpsc::channel();
        state.job = Some(Job {
            key: original,
            epoch: 1,
            receiver: rx,
        });
        tx.send(Ok(values())).unwrap();
        state.poll_with_mode(&ctx, Some(later), false);
        assert!(
            state.preview.is_none(),
            "paused mode must require the exact frame"
        );
    }

    #[test]
    fn live_pixel_limit_is_explicit_without_downsampling() {
        assert!(live_dimensions_supported((1920, 1080)));
        assert!(live_dimensions_supported((1080, 1920)));
        for size in [
            (0, 1080),
            (1920, 0),
            (1921, 1080),
            (3840, 2160),
            (u32::MAX, u32::MAX),
        ] {
            assert!(!live_dimensions_supported(size));
        }
    }

    #[test]
    fn live_scheduler_is_bounded_and_toggling_rejects_inflight_results() {
        let ctx = egui::Context::default();
        let now = std::time::Instant::now();
        let k = key();
        let mut state = State {
            open: true,
            ..Default::default()
        };
        assert!(!state.live_due(now, k));
        state.set_live(true);
        assert!(state.live_due(now, k));
        state.last_started = Some(now);
        assert!(!state.live_due(now + LIVE_INTERVAL - std::time::Duration::from_nanos(1), k));
        assert!(state.live_due(now + LIVE_INTERVAL, k));
        let (tx, rx) = std::sync::mpsc::channel();
        state.job = Some(Job {
            key: k,
            epoch: state.epoch,
            receiver: rx,
        });
        assert!(!state.live_due(now + LIVE_INTERVAL, k), "one worker only");
        state.set_live(false);
        state.set_live(true);
        tx.send(Ok(values())).unwrap();
        state.poll_with_mode(&ctx, Some(k), true);
        assert!(state.preview.is_none());
        assert!(state.job.is_none());
        state.close();
        assert!(!state.live);
    }

    #[test]
    fn live_native_preview_samples_real_playback_without_editing() {
        let engine =
            photonic_video::VideoEngine::headless().expect("GPU required for live measurements");
        let (doc, sid) = fixture();
        let before = serde_json::to_value(&doc).unwrap();
        let history = CommandHistory::new(20);
        let mut bridge = engine::EngineBridge::new(engine);
        bridge.sync_document(&doc, &history);
        bridge.peek_sequence(sid);
        assert!(bridge.prepare_full_preview());
        bridge.session.send(photonic_video::EngineCmd::SetScopeTap(
            ScopeTapPoint::NativeSceneProgram,
        ));
        bridge
            .session
            .send(photonic_video::EngineCmd::SetLoop(Some((
                Tick::ZERO,
                Tick::from_seconds(1),
            ))));
        bridge.session.send(photonic_video::EngineCmd::Play);
        let mut app = PhotonicApp::default();
        app.mode = AppMode::Video;
        app.engine = Some(bridge);
        app.monitor_playing = true;
        let ctx = egui::Context::default();
        app.open_program_measurement(&ctx, &doc, sid);
        app.program_measurement.set_live(true);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut times = std::collections::BTreeSet::new();
        while times.len() < 3 {
            app.update_program_measurement(&ctx, &doc, &history);
            if let Some(preview) = app.program_measurement.preview.as_ref() {
                times.insert(preview.key.time);
                assert_eq!(preview.values.covered_pixels, 1024);
                assert_eq!(preview.values.scene_rgb.below_zero, [1024, 0, 0]);
                assert_eq!(preview.values.scene_rgb.above_one, [0, 1024, 1024]);
            }
            assert!(
                std::time::Instant::now() < deadline,
                "live samples stalled: {}",
                app.program_measurement.status
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        app.engine
            .as_ref()
            .unwrap()
            .session
            .send(photonic_video::EngineCmd::Pause);
        assert_eq!(history.undo_depth(), 0);
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        app.program_measurement.close();
        assert!(!app.program_measurement.live);
    }

    fn fixture() -> (Document, SequenceId) {
        let mut doc = Document::new("Program measurement", 32.0, 32.0);
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("Native", FrameRate::FPS_30, 32, 32);
        seq.color =
            SequenceColorConfig::NativeManaged(Box::new(NativeManagedColorConfig::sdr_draft()));
        let mut clip = Clip::new(
            ClipSource::SolidColor {
                color: photonic_core::Color::rgb(0.5, 0.5, 0.5),
            },
            Tick::ZERO,
            Tick::from_seconds(1),
        );
        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 3.0 },
        ));
        grade.ops.push(GradeOp::new(
            GradeOpKind::LinearOffset,
            GradeOpParams::LinearOffset {
                rgb: [-2.0, 0.0, 0.0],
            },
        ));
        clip.grade = Some(grade);
        let mut track = Track::new(TrackKind::Video, "Locked");
        track.opacity = 0.5;
        track.locked = true;
        track.clips.push(clip);
        seq.video_tracks.push(track);
        let sid = seq.id;
        project.insert_sequence(seq);
        project.active_sequence = Some(sid);
        doc.timeline = Some(project);
        (doc, sid)
    }
    #[test]
    fn opening_requires_current_native_sequence_and_does_not_edit_document() {
        let (mut doc, sid) = fixture();
        let before = serde_json::to_value(&doc).unwrap();
        let ctx = egui::Context::default();
        let mut app = PhotonicApp::default();
        app.open_program_measurement(&ctx, &doc, sid);
        assert!(app.program_measurement.open);
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        app.program_measurement.close();
        doc.timeline
            .as_mut()
            .unwrap()
            .sequences
            .get_mut(&sid)
            .unwrap()
            .color = Default::default();
        app.open_program_measurement(&ctx, &doc, sid);
        assert!(!app.program_measurement.open);
    }
    #[test]
    fn native_preview_capture_survives_tap_restore_and_remains_readonly() {
        let engine = photonic_video::VideoEngine::headless()
            .expect("GPU required for paired preview UI qualification");
        let (mut doc, sid) = fixture();
        let before = serde_json::to_value(&doc).unwrap();
        let history = CommandHistory::new(20);
        let mut bridge = engine::EngineBridge::new(engine);
        bridge.sync_document(&doc, &history);
        bridge.peek_sequence(sid);
        assert!(bridge.prepare_full_preview());
        bridge.session.send(photonic_video::EngineCmd::SetScopeTap(
            ScopeTapPoint::NativeSceneProgram,
        ));
        bridge.seek(Tick::ZERO);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let frame = loop {
            if let Some(f) = bridge.session.latest_frame() {
                if f.scope_tap_point == ScopeTapPoint::NativeSceneProgram && f.scope_tap.is_some() {
                    break f;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "native scene preview was not produced"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        bridge.presented_frame = Some((frame.time, frame.sequence));
        let ctx = egui::Context::default();
        let mut app = PhotonicApp::default();
        app.mode = AppMode::Video;
        app.engine = Some(bridge);
        app.open_program_measurement(&ctx, &doc, sid);
        app.update_program_measurement(&ctx, &doc, &history);
        assert!(app.program_measurement.job.is_some());
        assert!(!app.program_measurement.requested);
        app.engine
            .as_ref()
            .unwrap()
            .session
            .send(photonic_video::EngineCmd::SetScopeTap(
                ScopeTapPoint::Program,
            ));
        loop {
            let mut restored = false;
            if let Some(f) = app.engine.as_ref().unwrap().session.latest_frame() {
                app.engine.as_mut().unwrap().presented_frame = Some((f.time, f.sequence));
                restored = f.scope_tap_point == ScopeTapPoint::Program;
            }
            app.update_program_measurement(&ctx, &doc, &history);
            if app.program_measurement.preview.is_some() && restored {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "paired preview did not finish: {}",
                app.program_measurement.status
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let preview = app.program_measurement.preview.as_ref().unwrap();
        assert_eq!(preview.values.covered_pixels, 1024);
        assert_eq!(preview.values.alpha_min, 0.5);
        assert_eq!(preview.values.scene_rgb.below_zero, [1024, 0, 0]);
        assert_eq!(preview.values.scene_rgb.above_one, [0, 1024, 1024]);
        assert_eq!(history.undo_depth(), 0);
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        app.playhead = Tick::from_seconds(1);
        app.update_program_measurement(&ctx, &doc, &history);
        assert!(
            app.program_measurement.preview.is_none(),
            "paused measurement must match the GUI playhead"
        );
        app.playhead = Tick::ZERO;
        app.monitor_playing = true;
        app.update_program_measurement(&ctx, &doc, &history);
        assert!(app.program_measurement.preview.is_none());
        app.monitor_playing = false;
        app.update_program_measurement(&ctx, &doc, &history);
        assert!(app.program_measurement.status.contains("paused"));
        doc.id = uuid::Uuid::new_v4();
        app.update_program_measurement(&ctx, &doc, &history);
        assert!(!app.program_measurement.open);
    }
}
