//! GUI ↔ `photonic_video::VideoEngine` bridge (Wire phase of 02 §1 / 03 §5).
//!
//! Three jobs live here, all host-side so `photonic-video` stays untouched:
//!
//! 1. **Lock-flavor bridge.** The app's live `Document`/`CommandHistory` sit
//!    under `tokio::sync::Mutex` (shared with the MCP server), but
//!    `VideoEngine::open_session` snapshots through `std::sync::Mutex`. The
//!    bridge owns a std-mutexed *mirror* pair; [`EngineBridge::sync_document`]
//!    copies `doc.timeline` into the mirror whenever the real history revision
//!    moves and bumps the mirror history's revision (`CommandHistory::reset` —
//!    documented to bump `revision`, and the mirror's stacks are always empty
//!    so it is otherwise a no-op). The engine thread then re-snapshots exactly
//!    as if it were watching the real document.
//!
//! 2. **Presentation.** [`EngineBridge::present_latest`] runs the normative
//!    `EngineFrame`→screen present pass (03 §5,
//!    `photonic_render::video::VideoPresenter`) into an intermediate
//!    `Rgba8UnormSrgb` texture registered with egui as a native texture; the
//!    monitor paints it with a UV rect cropped to the frame's logical size
//!    (the engine's frame textures are pool-bucket padded — see the facade
//!    notes in `photonic_video::session`).
//!
//! 3. **Desired-state reconciliation.** Transport methods only mutate GUI
//!    intent (`monitor_playing`, playhead, loop toggle, proxy mode);
//!    `PhotonicApp::drive_playback` (in `app/monitor.rs`) diffs intent against
//!    what was last sent, via the `set_*`/`seek`/`step` methods here, and
//!    emits the minimal `EngineCmd` stream (Play/Pause/Seek/SetLoop/
//!    SetActiveSequence/SetProxyMode). JKL shuttle (reverse / >1× speed) has
//!    no engine-side primitive yet, so it scrubs via coalesced `Seek`s while
//!    the engine stays paused (documented seam: reverse/ramped playback with
//!    audio belongs to the speed-maps story, P8).
//!
//! Timeline clip thumbnails and audio waveform strips are a documented seam:
//! the engine exposes no per-clip decoded-frame or PCM access yet (its
//! `EngineFrame` is program-out only), so wiring them waits for the decode-
//! pool / waveform-pyramid follow-up rather than spawning ad-hoc ffmpeg runs
//! per visible clip here.
//!
//! The master-output meter beside the program monitor (`app/monitor.rs`,
//! NLE-parity Gap G-4) is another documented seam: [`EngineBridge::master_level`]
//! is the accessor the monitor polls, but it returns `None` today because
//! neither `EngineSession` nor `EngineStatus` (`photonic_video::session`)
//! surface a level yet — see that method's doc for the exact shape of the
//! fix (out of this story's territory, `app/{monitor.rs,engine.rs}` only).

use photonic_core::document::Document;
use photonic_core::history::CommandHistory;
use photonic_core::timeline::{SequenceId, Tick};
use photonic_render::video::{PresentChannel, VideoPresenter};
use photonic_video::{
    EngineCmd, EngineSession, EngineStatus, PreviewQuality, PreviewTarget, ProxyMode, VideoEngine,
};
use std::sync::{Arc, Mutex as StdMutex};

/// The registered egui texture for the current engine frame.
pub(crate) struct MonitorTexture {
    pub id: egui::TextureId,
    /// Physical (pool-bucket-padded) size of the presented texture.
    pub physical: (u32, u32),
}

/// Master-bus output level, `[L, R]` linear amplitude, as
/// [`EngineBridge::master_level`] would report it once the engine surfaces
/// one — see that method's doc for the seam this type is shaped to close.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(crate) struct MasterLevel {
    pub peak: [f32; 2],
    pub rms: [f32; 2],
}

/// Identity of the last-presented frame, so an unchanged frame isn't
/// re-presented every GUI frame.
type FrameKey = (Tick, SequenceId, usize);

/// GUI-side handle to a running engine session plus everything the monitor
/// needs to feed and reflect it. `None` on `PhotonicApp` means "no engine"
/// (tests, GPU-less hosts) and every caller falls back to the pre-P3
/// wall-clock placeholder paths.
pub struct EngineBridge {
    /// Kept alive for the session's lifetime (owns the shared `GpuContext`).
    #[allow(dead_code)]
    engine: VideoEngine,
    pub(crate) session: EngineSession,

    // ── Snapshot mirror (lock-flavor bridge) ────────────────────────────────
    last_synced_revision: Option<u64>,
    last_synced_document: Option<uuid::Uuid>,
    live_snapshot: Option<photonic_video::RenderSnapshot>,
    trim_candidate: Option<(Vec<photonic_core::timeline::TimelineCmd>, u64)>,
    waiting_snapshot: Option<u64>,

    // ── Presentation ────────────────────────────────────────────────────────
    presenter: Option<VideoPresenter>,
    target: Option<PresentTarget>,
    /// K-B5 clean (no clip looks) present target, when compare is on.
    compare_target: Option<PresentTarget>,
    presented: Option<FrameKey>,
    pub(crate) monitor_tex: Option<MonitorTexture>,
    /// K-B5 clean side texture (bypassed clip looks).
    pub(crate) compare_tex: Option<MonitorTexture>,
    /// The frame the monitor texture currently shows (time + sequence), for
    /// the buffering heuristic.
    pub(crate) presented_frame: Option<(Tick, SequenceId)>,
    /// Logical size of the frame currently in `monitor_tex`; unlike the active
    /// sequence format this also remains correct for an asset source peek.
    pub(crate) presented_logical_size: Option<(u32, u32)>,
    /// K-B17: colour vs alpha-as-luminance present channel.
    pub(crate) present_channel: PresentChannel,
    /// K-B5: dual clean/graded compare requested.
    pub(crate) compare_effects: bool,
    /// K-B5: vertical split fraction (0=all clean left, 1=all graded).
    pub(crate) compare_split: f32,

    // ── Reconciler state (last values actually sent to the engine) ──────────
    trim_stills: Option<TrimStills>,
    retired_trim_textures: Vec<egui::TextureId>,
    sent_playing: Option<bool>,
    sent_loop: Option<Option<(Tick, Tick)>>,
    sent_sequence: Option<SequenceId>,
    sent_preview_cache_dir: Option<std::path::PathBuf>,
    sent_proxy: Option<ProxyMode>,
    sent_preview_quality: Option<PreviewQuality>,
    sent_preview_target: Option<PreviewTarget>,
    sent_compare_effects: Option<bool>,
    pending_source_seek: Option<(photonic_core::timeline::AssetId, Tick)>,
    /// Playhead value the GUI and engine last agreed on — a differing
    /// `self.playhead` means the *user* moved it (ruler scrub, Home/End,
    /// marker jump) and a `Seek` must be sent.
    pub(crate) agreed_playhead: Option<Tick>,
    /// GUI-side proxy-mode intent (media pool toggle).
    pub(crate) proxy_mode: ProxyMode,
    /// Draft (default) / Full interactive quality (24 §4).
    pub(crate) preview_quality: PreviewQuality,
    /// Desired single-monitor target; play-wins enforced by engine (24 §3).
    pub(crate) preview_target: PreviewTarget,
}

struct TrimStills {
    requests: [(photonic_core::timeline::AssetId, Tick); 2],
    revision: u64,
    slots: [Option<TrimStill>; 2],
}
struct TrimStill {
    _texture: wgpu::Texture,
    id: egui::TextureId,
    physical: (u32, u32),
    logical: (u32, u32),
}

struct PresentTarget {
    #[allow(dead_code)]
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
    egui_id: Option<egui::TextureId>,
}

impl EngineBridge {
    /// Build a bridge sharing the windowed renderer's wgpu device/queue
    /// (02 §1 "GUI mode shares the winit GpuContext"). This is the one-line
    /// construction hosts call right after the renderer exists.
    pub fn from_renderer(renderer: &photonic_render::PhotonicRenderer) -> Self {
        let gpu = photonic_video::GpuContext::new(renderer.device_arc(), renderer.queue_arc());
        EngineBridge::new(VideoEngine::new(gpu))
    }

    /// Open a session against fresh mirror state. The mirror starts empty; the
    /// first [`Self::sync_document`] populates it.
    pub fn new(engine: VideoEngine) -> Self {
        let mirror_doc = Arc::new(StdMutex::new(Document::new("engine-mirror", 1.0, 1.0)));
        let mirror_history = Arc::new(StdMutex::new(CommandHistory::new(1)));
        let session = engine.open_session(Arc::clone(&mirror_doc), Arc::clone(&mirror_history));
        EngineBridge {
            engine,
            session,
            last_synced_revision: None,
            last_synced_document: None,
            live_snapshot: None,
            trim_candidate: None,
            waiting_snapshot: None,
            presenter: None,
            target: None,
            compare_target: None,
            presented: None,
            monitor_tex: None,
            compare_tex: None,
            presented_frame: None,
            presented_logical_size: None,
            present_channel: PresentChannel::Color,
            compare_effects: false,
            compare_split: 0.5,
            trim_stills: None,
            retired_trim_textures: Vec::new(),
            sent_playing: None,
            sent_loop: None,
            sent_sequence: None,
            sent_preview_cache_dir: None,
            sent_proxy: None,
            sent_preview_quality: None,
            sent_preview_target: None,
            sent_compare_effects: None,
            pending_source_seek: None,
            agreed_playhead: None,
            proxy_mode: ProxyMode::Auto,
            preview_quality: PreviewQuality::Draft,
            preview_target: PreviewTarget::default(),
        }
    }

    /// Wait-free engine status for this frame.
    pub fn status(&self) -> Arc<EngineStatus> {
        self.session.status()
    }

    /// Shared wgpu device for background export workers (K-F1 render queue).
    pub fn gpu(&self) -> &photonic_video::GpuContext {
        self.engine.gpu()
    }

    /// The engine's real master-bus output level (NLE-parity Gap G-4: the
    /// slim stereo peak+RMS meter beside the program monitor,
    /// `app/monitor.rs::draw_master_meter`). Linear peak/RMS amplitude per
    /// channel `[L, R]` — the same unit
    /// [`photonic_video::audio::StereoMeter::peak`]/`::rms` publish, so a
    /// caller converts to dB itself (exactly like
    /// `panels/video/audio_mixer.rs`'s master strip does).
    ///
    /// G-4 closed: reads `EngineStatus.master_level` published from the mixer
    /// feeder's live `StereoMeter`. `None` while paused / no audio device —
    /// callers render the silence floor honestly (no fabricated motion).
    pub(crate) fn master_level(&self) -> Option<MasterLevel> {
        self.session.status().master_level.map(|m| MasterLevel {
            peak: m.peak,
            rms: m.rms,
        })
    }

    /// The raw session handle (command sends, frame polls) for hosts/tests.
    pub fn session(&self) -> &EngineSession {
        &self.session
    }

    /// Physical size + egui id of the registered monitor texture, if any.
    pub fn monitor_tex_info(&self) -> Option<((u32, u32), egui::TextureId)> {
        self.monitor_tex.as_ref().map(|t| (t.physical, t.id))
    }

    /// Copy `doc.timeline` into the engine-visible mirror when the real
    /// history revision moved. Contention on either mirror lock (the engine
    /// thread snapshots with `try_lock` and holds only briefly) just retries
    /// next frame — `last_synced_revision` is only advanced on success.
    pub fn sync_document(&mut self, doc: &Document, history: &CommandHistory) {
        let rev = history.revision();
        if self.last_synced_revision == Some(rev) && self.last_synced_document == Some(doc.id) {
            return;
        }
        let snapshot = photonic_video::RenderSnapshot::from_document(doc, rev);
        let generation = self.session.publish_snapshot(snapshot.clone());
        self.live_snapshot = Some(snapshot);
        if self.trim_candidate.take().is_some() {
            self.waiting_snapshot = Some(generation);
            self.set_playing(false);
        }
        self.last_synced_revision = Some(rev);
        self.last_synced_document = Some(doc.id);
    }

    // ── Reconciliation ───────────────────────────────────────────────────────

    /// D-12: ask the engine to analyze `clip` and warm its stabilization cache
    /// (22 §6.5).
    ///
    /// Not reconciled like the `set_*` senders below: this is an explicit user
    /// action, and re-running it on unchanged input is exactly what the
    /// Reanalyze button is for.
    pub(crate) fn send_analyze_stabilization(&self, clip: photonic_core::timeline::ClipId) {
        self.session.send(EngineCmd::AnalyzeStabilization { clip });
    }

    /// Send `cmd` kinds only when the desired value changed since last send.
    pub(crate) fn set_playing(&mut self, playing: bool) {
        if playing {
            if let Some(generation) = self.waiting_snapshot {
                if self.status().snapshot_generation != generation {
                    return;
                }
                self.waiting_snapshot = None;
            }
        }
        if self.sent_playing != Some(playing) {
            if self.session.send(if playing {
                EngineCmd::Play
            } else {
                EngineCmd::Pause
            }) {
                self.sent_playing = Some(playing);
            }
        }
    }

    pub(crate) fn set_loop(&mut self, range: Option<(Tick, Tick)>) {
        if self.sent_loop != Some(range) {
            if self.session.send(EngineCmd::SetLoop(range)) {
                self.sent_loop = Some(range);
            }
        }
    }

    pub(crate) fn set_active_sequence(&mut self, seq: Option<SequenceId>) {
        if let Some(seq) = seq {
            if self.sent_sequence != Some(seq) {
                if self.session.send(EngineCmd::SetActiveSequence(seq)) {
                    self.sent_sequence = Some(seq);
                }
            }
        }
    }

    pub(crate) fn set_preview_cache_dir(&mut self, path: std::path::PathBuf) -> bool {
        if self.sent_preview_cache_dir.as_ref() == Some(&path) {
            return true;
        }
        if self
            .session
            .send(EngineCmd::SetPreviewCacheDir { path: path.clone() })
        {
            self.sent_preview_cache_dir = Some(path);
            true
        } else {
            false
        }
    }
    pub(crate) fn prepare_full_preview(&mut self) -> bool {
        self.preview_quality = PreviewQuality::Full;
        self.proxy_mode = ProxyMode::ForceOriginal;
        self.apply_preview_quality();
        self.apply_proxy_mode();
        self.sent_preview_quality == Some(self.preview_quality)
            && self.sent_proxy == Some(self.proxy_mode)
    }

    pub(crate) fn apply_proxy_mode(&mut self) {
        if self.sent_proxy != Some(self.proxy_mode) {
            if self.session.send(EngineCmd::SetProxyMode(self.proxy_mode)) {
                self.sent_proxy = Some(self.proxy_mode);
            }
        }
    }

    pub(crate) fn apply_preview_quality(&mut self) {
        if self.sent_preview_quality != Some(self.preview_quality) {
            if self
                .session
                .send(EngineCmd::SetPreviewQuality(self.preview_quality))
            {
                self.sent_preview_quality = Some(self.preview_quality);
            }
        }
    }

    pub(crate) fn apply_preview_target(&mut self) {
        if let Some((asset, time)) = self.pending_source_seek {
            if self.session.send(EngineCmd::SeekSource { asset, time }) {
                self.pending_source_seek = None;
            }
        }
        if self.sent_compare_effects != Some(self.compare_effects)
            && self
                .session
                .send(EngineCmd::SetCompareEffects(self.compare_effects))
        {
            self.sent_compare_effects = Some(self.compare_effects);
        }
        if self.sent_preview_target.as_ref() != Some(&self.preview_target) {
            if self
                .session
                .send(EngineCmd::SetPreviewTarget(self.preview_target.clone()))
            {
                self.sent_preview_target = Some(self.preview_target.clone());
            }
        }
    }

    /// Peek a media-pool asset on the single monitor (24 §3). No-op while
    /// playing (engine enforces play-wins).
    pub(crate) fn peek_asset(&mut self, asset: photonic_core::timeline::AssetId, time: Tick) {
        self.preview_target = PreviewTarget::Asset {
            asset,
            source_time: time,
        };
        self.apply_preview_target();
    }

    /// Seek within a source peek (source clock; G-10 / 24 §3.3).
    pub(crate) fn seek_source(&mut self, asset: photonic_core::timeline::AssetId, time: Tick) {
        self.preview_target = PreviewTarget::Asset {
            asset,
            source_time: time,
        };
        self.pending_source_seek = Some((asset, time));
        self.apply_preview_target();
    }

    pub(crate) fn audition_source(
        &mut self,
        sequence: SequenceId,
        playhead: Tick,
        asset: photonic_core::timeline::AssetId,
        start: Tick,
        end: Tick,
    ) -> bool {
        self.set_active_sequence(Some(sequence));
        self.set_playing(false);
        if self.sent_sequence != Some(sequence) || self.sent_playing != Some(false) {
            return false;
        }
        if self.agreed_playhead != Some(playhead) {
            self.seek(playhead);
        }
        if self.agreed_playhead != Some(playhead) {
            return false;
        }
        if !self
            .session
            .send(EngineCmd::AuditionSource { asset, start, end })
        {
            return false;
        }
        self.pending_source_seek = None;
        self.preview_target = PreviewTarget::Asset {
            asset,
            source_time: start,
        };
        self.sent_preview_target = Some(self.preview_target.clone());
        true
    }

    /// Follow an engine-owned audition clock without sending a new target
    /// command (which would cancel that audition).
    pub(crate) fn follow_source_audition(
        &mut self,
        asset: photonic_core::timeline::AssetId,
        time: Tick,
    ) {
        self.preview_target = PreviewTarget::Asset {
            asset,
            source_time: time,
        };
        self.sent_preview_target = Some(self.preview_target.clone());
    }

    /// True when the single monitor is showing a source peek.
    pub(crate) fn preview_is_asset(&self) -> bool {
        matches!(self.preview_target, PreviewTarget::Asset { .. })
    }

    /// Return the monitor to sequence program view.
    pub(crate) fn peek_sequence(&mut self, sequence: SequenceId) {
        self.pending_source_seek = None;
        self.preview_target = PreviewTarget::Sequence { sequence };
        self.apply_preview_target();
    }

    /// Seek and record agreement so the scrub detector stays quiet.
    pub(crate) fn seek(&mut self, to: Tick) {
        if self.session.send(EngineCmd::Seek(to)) {
            self.agreed_playhead = Some(to);
        }
    }

    /// Live scrub target while the playhead is being dragged: decodes a cheap
    /// keyframe preview. Records agreement like `seek`; the drag-release settle
    /// sends a real `seek` to land the exact frame.
    pub(crate) fn scrub_seek(&mut self, to: Tick) {
        if self.session.send(EngineCmd::ScrubSeek(to)) {
            self.agreed_playhead = Some(to);
        }
    }

    /// Exact-frame step; the engine pauses itself (02 §4). The caller updates
    /// its optimistic local playhead and then records agreement via
    /// [`Self::note_agreed`].
    pub(crate) fn step(&mut self, frames: i32) -> bool {
        if self.session.send(EngineCmd::Step(frames)) {
            self.sent_playing = Some(false);
            true
        } else {
            false
        }
    }

    pub(crate) fn note_agreed(&mut self, playhead: Tick) {
        self.agreed_playhead = Some(playhead);
    }

    /// Publish an isolated candidate. Wait for its generation before starting
    /// audio, since snapshot publication and the command mailbox are separate.
    pub(crate) fn preview_trim_candidate(
        &mut self,
        doc: &Document,
        history: &CommandHistory,
        commands: &[photonic_core::timeline::TimelineCmd],
    ) -> bool {
        self.sync_document(doc, history);
        if self
            .trim_candidate
            .as_ref()
            .is_some_and(|(old, _)| old == commands)
        {
            return false;
        }
        self.set_playing(false);
        let candidate = super::editing_workflows::candidate_document(doc, commands);
        let generation =
            self.session
                .publish_snapshot(photonic_video::RenderSnapshot::from_document(
                    &candidate,
                    history.revision(),
                ));
        self.trim_candidate = Some((commands.to_vec(), generation));
        self.waiting_snapshot = Some(generation);
        true
    }
    pub(crate) fn restore_trim_candidate(&mut self) {
        if self.trim_candidate.take().is_some() {
            self.set_playing(false);
            if let Some(snapshot) = &self.live_snapshot {
                self.waiting_snapshot = Some(self.session.publish_snapshot(snapshot.clone()));
            }
        }
    }

    pub(crate) fn set_trim_stills(
        &mut self,
        requests: Option<([(photonic_core::timeline::AssetId, Tick); 2], u64)>,
    ) {
        if self.trim_stills.as_ref().map(|s| (s.requests, s.revision)) == requests {
            return;
        }
        if let Some(old) = self.trim_stills.take() {
            self.retired_trim_textures
                .extend(old.slots.into_iter().flatten().map(|slot| slot.id));
        }
        if let Some((requests, revision)) = requests {
            self.trim_stills = Some(TrimStills {
                requests,
                revision,
                slots: [None, None],
            });
            self.peek_asset(requests[0].0, requests[0].1);
        }
    }
    pub(crate) fn trim_still_images(
        &self,
    ) -> [Option<(egui::TextureId, (u32, u32), (u32, u32))>; 2] {
        std::array::from_fn(|index| {
            self.trim_stills.as_ref()?.slots[index]
                .as_ref()
                .map(|slot| (slot.id, slot.logical, slot.physical))
        })
    }

    // ── Presentation (03 §5) ─────────────────────────────────────────────────

    /// Present the newest `EngineFrame` (if it changed) into the egui-visible
    /// intermediate texture. Called from the host render loop *before*
    /// `egui::Context::run`, so the registered texture is valid for this
    /// frame's paint pass.
    pub fn present_latest(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        egui_renderer: &mut egui_wgpu::Renderer,
    ) {
        for id in self.retired_trim_textures.drain(..) {
            egui_renderer.free_texture(&id);
        }
        let Some(frame) = self.session.latest_frame() else {
            return;
        };
        // Include the present channel so toggling alpha view re-presents the
        // same frame through the alpha pipeline (K-B17).
        let clean_ptr = frame
            .compare_clean
            .as_ref()
            .map(|t| Arc::as_ptr(t) as usize)
            .unwrap_or(0);
        let key: FrameKey = (
            frame.time,
            frame.sequence,
            Arc::as_ptr(&frame.texture) as usize
                ^ (self.present_channel as usize).wrapping_mul(0x9e37_79b9)
                ^ clean_ptr.wrapping_mul(0x85eb_ca6b)
                ^ (self.compare_effects as usize)
                ^ (frame.logical_size.0 as usize).wrapping_mul(0x27d4_eb2d)
                ^ (frame.logical_size.1 as usize).wrapping_mul(0x1656_67b1),
        );
        let capture = self.trim_stills.as_ref().and_then(|stills| {
            if frame.doc_revision != stills.revision {
                return None;
            }
            stills
                .requests
                .iter()
                .enumerate()
                .find_map(|(index, (asset, time))| {
                    (stills.slots[index].is_none()
                        && frame.preview_asset == Some(*asset)
                        && frame.time == *time)
                        .then_some(index)
                })
        });
        if self.presented == Some(key) && capture.is_none() {
            return;
        }
        let size = (frame.texture.width(), frame.texture.height());
        self.ensure_target(device, egui_renderer, size, false);
        if frame.compare_clean.is_some() {
            self.ensure_target(device, egui_renderer, size, true);
        } else {
            self.compare_tex = None;
            if let Some(old) = self.compare_target.take() {
                if let Some(id) = old.egui_id {
                    egui_renderer.free_texture(&id);
                }
            }
        }
        if self.presenter.is_none() {
            self.presenter = Some(VideoPresenter::new(
                device,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            ));
        }
        let channel = self.present_channel;
        let src_view = frame.texture.create_view(&Default::default());
        let clean_view = frame
            .compare_clean
            .as_ref()
            .map(|t| t.create_view(&Default::default()));
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("engine_frame_present"),
        });
        {
            let presenter = self.presenter.as_ref().expect("presenter set above");
            let target = self.target.as_ref().expect("ensure_target sets target");
            presenter.present_engine_frame_channel(
                device,
                &mut encoder,
                &src_view,
                &target.view,
                channel,
            );
            if let (Some(cv), Some(ct)) = (clean_view.as_ref(), self.compare_target.as_ref()) {
                presenter.present_engine_frame_channel(device, &mut encoder, cv, &ct.view, channel);
            }
        }
        if let Some(index) = capture {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("precision_trim_still"),
                size: wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            encoder.copy_texture_to_texture(
                wgpu::ImageCopyTexture {
                    texture: &self.target.as_ref().expect("present target").texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyTexture {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
            );
            let view = texture.create_view(&Default::default());
            let id = egui_renderer.register_native_texture(device, &view, wgpu::FilterMode::Linear);
            if let Some(stills) = self.trim_stills.as_mut() {
                stills.slots[index] = Some(TrimStill {
                    _texture: texture,
                    id,
                    physical: size,
                    logical: frame.logical_size,
                });
                if let Some(next) = stills.slots.iter().position(Option::is_none) {
                    let (asset, time) = stills.requests[next];
                    self.peek_asset(asset, time);
                }
            }
        }
        queue.submit([encoder.finish()]);

        self.presented = Some(key);
        self.presented_frame = Some((frame.time, frame.sequence));
        self.presented_logical_size = Some(frame.logical_size);
    }

    /// Toggle alpha-as-luminance present (K-B17). Forces the next
    /// [`present_latest`] to re-encode the current frame.
    pub fn toggle_alpha_view(&mut self) {
        self.present_channel = match self.present_channel {
            PresentChannel::Color => PresentChannel::Alpha,
            PresentChannel::Alpha => PresentChannel::Color,
        };
        self.presented = None; // force re-present
    }

    pub fn alpha_view(&self) -> bool {
        self.present_channel == PresentChannel::Alpha
    }

    /// K-B5: current compare-effects view flag (43 UI-path tests).
    pub fn compare_effects(&self) -> bool {
        self.compare_effects
    }

    /// K-B5: toggle effect-compare split; sends view-state to the engine.
    pub fn toggle_compare_effects(&mut self) {
        self.compare_effects = !self.compare_effects;
        self.apply_preview_target();
        self.presented = None;
    }

    /// (Re)create the intermediate target + egui registration when the
    /// physical frame size changes. `for_compare` selects the clean-side target.
    fn ensure_target(
        &mut self,
        device: &wgpu::Device,
        egui_renderer: &mut egui_wgpu::Renderer,
        size: (u32, u32),
        for_compare: bool,
    ) {
        let slot = if for_compare {
            &mut self.compare_target
        } else {
            &mut self.target
        };
        if slot.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        if let Some(old) = slot.take() {
            if let Some(id) = old.egui_id {
                egui_renderer.free_texture(&id);
            }
        }
        let label = if for_compare {
            "engine_monitor_compare_target"
        } else {
            "engine_monitor_target"
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size.0.max(1),
                height: size.1.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // The present pass writes linear values and hardware encodes them;
            // egui then decodes the native texture before its gamma-space
            // window pass, avoiding a double sRGB transform.
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let egui_id =
            egui_renderer.register_native_texture(device, &view, wgpu::FilterMode::Linear);
        let mon = MonitorTexture {
            id: egui_id,
            physical: size,
        };
        if for_compare {
            self.compare_tex = Some(mon);
        } else {
            self.monitor_tex = Some(mon);
        }
        *slot = Some(PresentTarget {
            texture,
            view,
            size,
            egui_id: Some(egui_id),
        });
    }
}

/// UV rect cropping a pool-bucket-padded engine texture down to the logical
/// (sequence-format) content that sits at its top-left (facade note: physical
/// size is the pool's 64 px bucket, logical content is format-sized).
pub(crate) fn padded_uv(logical: (u32, u32), physical: (u32, u32)) -> egui::Rect {
    let (lw, lh) = (logical.0.max(1) as f32, logical.1.max(1) as f32);
    let (pw, ph) = (physical.0.max(1) as f32, physical.1.max(1) as f32);
    egui::Rect::from_min_max(
        egui::pos2(0.0, 0.0),
        egui::pos2((lw / pw).min(1.0), (lh / ph).min(1.0)),
    )
}

/// Buffering heuristic: playing, but the frame on screen lags the engine
/// playhead by more than `threshold_frames` — show the monitor spinner.
pub(crate) fn is_buffering(
    playing: bool,
    playhead: Tick,
    presented: Option<Tick>,
    ticks_per_frame: i64,
    threshold_frames: i64,
) -> bool {
    if !playing {
        return false;
    }
    let Some(shown) = presented else {
        return true; // playing with nothing on screen yet
    };
    (playhead.0 - shown.0).abs() > ticks_per_frame.max(1) * threshold_frames
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precision_candidate_snapshot_restores_live_revision_and_playhead() {
        use photonic_core::timeline::{
            precision_trim, Clip, ClipSource, FrameRate, Sequence, TimelineProject, Track,
            TrackKind,
        };
        let Some(engine) = VideoEngine::headless() else {
            eprintln!("GPU unavailable; precision snapshot integration skipped");
            return;
        };
        let mut bridge = EngineBridge::new(engine);
        let mut doc = Document::new("precision", 1., 1.);
        let mut project = TimelineProject::new();
        let mut sequence = Sequence::new("test", FrameRate::FPS_30, 32, 32);
        let mut track = Track::new(TrackKind::Video, "color");
        let left = Clip::new(
            ClipSource::SolidColor {
                color: photonic_core::Color::default(),
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
        let target = precision_trim::TrimTarget {
            sequence: sequence.id,
            track: track.id,
            outgoing: left.id,
            incoming: right.id,
        };
        track.clips = vec![left, right];
        sequence.video_tracks.push(track);
        project.insert_sequence(sequence);
        project.active_sequence = Some(target.sequence);
        doc.timeline = Some(project);
        let original = doc.timeline.clone();
        let mut history = CommandHistory::new(8);
        let playhead = Tick::from_seconds(1);
        bridge.sync_document(&doc, &history);
        bridge.set_active_sequence(Some(target.sequence));
        bridge.seek(playhead);
        let commands = precision_trim::plan_trim(
            doc.timeline.as_ref().unwrap(),
            target,
            precision_trim::TrimMode::Roll,
            FrameRate::FPS_30.ticks_per_frame(),
        )
        .unwrap();
        bridge.preview_trim_candidate(&doc, &history, &commands);
        let candidate_generation = bridge.trim_candidate.as_ref().unwrap().1;
        let wait = |bridge: &EngineBridge, generation: u64| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while bridge.status().snapshot_generation != generation
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(bridge.status().snapshot_generation, generation);
        };
        wait(&bridge, candidate_generation);
        assert_eq!(doc.timeline, original);
        bridge.restore_trim_candidate();
        let restored_generation = bridge.waiting_snapshot.unwrap();
        bridge.seek(playhead);
        wait(&bridge, restored_generation);
        assert_eq!(bridge.status().doc_revision, history.revision());
        assert_eq!(bridge.status().playhead, playhead);
        bridge.preview_trim_candidate(&doc, &history, &commands);
        history.reset();
        bridge.sync_document(&doc, &history);
        assert!(bridge.trim_candidate.is_none());
        bridge.restore_trim_candidate();
        let live_generation = bridge.waiting_snapshot.unwrap();
        wait(&bridge, live_generation);
        assert_eq!(bridge.status().doc_revision, history.revision());
        assert_eq!(doc.timeline, original);
    }

    #[test]
    fn padded_uv_crops_bucket_padding() {
        // 320x180 logical in a 320x192 bucket: u full, v cropped.
        let uv = padded_uv((320, 180), (320, 192));
        assert_eq!(uv.min, egui::pos2(0.0, 0.0));
        assert!((uv.max.x - 1.0).abs() < f32::EPSILON);
        assert!((uv.max.y - 180.0 / 192.0).abs() < 1e-6);
    }

    #[test]
    fn padded_uv_exact_fit_is_full_texture() {
        let uv = padded_uv((1920, 1080), (1920, 1080));
        assert_eq!(uv.max, egui::pos2(1.0, 1.0));
    }

    #[test]
    fn padded_uv_never_exceeds_one_or_divides_by_zero() {
        let uv = padded_uv((100, 100), (64, 0));
        assert!(uv.max.x <= 1.0 && uv.max.y <= 1.0);
    }

    #[test]
    fn buffering_only_while_playing_and_lagging() {
        let tpf = 1000;
        // Not playing → never buffering.
        assert!(!is_buffering(false, Tick(9000), None, tpf, 4));
        // Playing with no frame yet → buffering.
        assert!(is_buffering(true, Tick(0), None, tpf, 4));
        // Small lag (≤ 4 frames) → fine.
        assert!(!is_buffering(true, Tick(4000), Some(Tick(0)), tpf, 4));
        // Large lag → buffering.
        assert!(is_buffering(true, Tick(9000), Some(Tick(0)), tpf, 4));
    }
}
