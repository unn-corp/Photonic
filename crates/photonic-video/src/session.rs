//! The `VideoEngine` facade + `EngineSession` per-open-document runtime
//! (02 §1, normative).
//!
//! Threading model (02 §1):
//! - **Engine thread** (spawned per session) owns the playback state machine
//!   ([`crate::playback`]), graph compile/eval scheduling, and the media
//!   sources. It receives [`EngineCmd`] via a crossbeam channel and publishes
//!   [`EngineFrame`] + [`EngineStatus`] via `arc-swap` — the GUI never blocks
//!   on the engine.
//! - **Audio thread** — the cpal callback inside [`crate::audio::AudioEngine`]
//!   (opened lazily on the first `Play`); it owns the master clock (02 §4).
//! - **Mixer feeder** — a worker thread that renders mixer blocks from the
//!   snapshot's audio tracks into the lock-free ring the callback drains.
//! - **Decode** — P3 floor: the engine thread drives `DecodeSource` seeks
//!   inline (bounded by the sidecar's restart/backoff containment) and pumps
//!   rings via [`crate::playback::prefetch`]; the N-worker decode pool is the
//!   documented follow-up seam.
//!
//! Document access NEVER blocks mid-playback: the engine polls
//! `CommandHistory::revision`/`changes_since` with `try_lock` and re-snapshots
//! the `TimelineProject` (cheap `Clone`, 01) only when the revision moved;
//! contended locks just reuse the last snapshot (02 §1).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use arc_swap::{ArcSwap, ArcSwapOption};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use photonic_core::timeline::{
    AssetKind, AssetSource, Clip, ClipAudio, ClipId, ClipSource, FrameRate, Ratio, Sequence,
    SequenceId, SpeedMap, Tick, TimelineProject, TICKS_PER_SECOND,
};
use photonic_core::{CommandHistory, Document};
use photonic_render::color::{Colorimetry, Matrix, Range};
use photonic_render::video::YuvConverter;
use photonic_render::{ExportBackground, ExportOptions, HeadlessRenderer};

use crate::audio::mixer::PcmSource;
use crate::audio::{
    audio_ring, AudioEngine, ClipVoice, Mixer, RingProducer, TrackVoice, XrunCounters,
    BLOCK_FRAMES, CHANNELS,
};
use crate::contract::{AssetId, VectorRef, VectorStateKey};
use crate::decode::scheduler::{DecodeSource, PtsKind, SourceParams};
use crate::decode::worker::DecodeWorker;
use crate::decode::{PixFmt, SharedRing};
use crate::export::presets::ExportPreset;
use crate::graph::cache::CacheStats;
use crate::graph::compile::{
    compile_asset_peek, compile_full, compile_with_providers, fit_long_edge, CompileDiagnostic,
    DiagSeverity, LutProvider, Quality, ScopeTapPoint, DRAFT_MAX_LONG_EDGE,
};
use crate::graph::eval::{Evaluator, GpuContext, GpuFrame, GpuFrameSource};
use crate::graph::ir::IrOp;
use crate::media::ffmpeg_locate::{locate, FfmpegTools};
use crate::media::keyframe_index::{KeyframeIndex, PtsIndex};
use crate::media::probe::{content_hash, probe_details, ProbeDetails};
use crate::media::proxy_policy::{AdaptiveProxyPolicy, PreviewMediaChoice, PreviewPressure};
use crate::media::stills::{resample_linear_premult, still_target_size, StillCache};
use crate::playback::pcm::{read_pcm_window, TimeWarpPcmSource};
use crate::playback::prefetch::{
    cut_ahead_targets, lru_eviction_victims, CUT_AHEAD_LEAD_FRAMES, MAX_LIVE_SOURCES,
};
use crate::playback::{FfmpegPcmSource, PlaybackController, PresentDecision};

/// While playing, wake often enough for 60–120 Hz present opportunities without
/// spinning at 500 Hz (2 ms). 4 ms ≈ 250 Hz upper bound; PresentDecision still
/// gates actual evaluate work to the sequence frame rate.
const PLAYING_POLL_INTERVAL: Duration = Duration::from_millis(4);
/// Paused: rare doc-snapshot poll; commands still wake immediately via channel.
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(100);

// ── Public command / state types (02 §1) ────────────────────────────────────

/// Proxy media policy for this session (02 §6 — session state, not document).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum ProxyMode {
    /// Engine starts on originals and moves to ready proxies after sustained
    /// preview pressure. This preserves quality on hardware that decodes the
    /// original efficiently, while a missing/pending/failed proxy remains a
    /// correctness-safe fallback to the original (CAP-014).
    #[default]
    Auto,
    ForceProxy,
    ForceOriginal,
}

/// Interactive preview quality (24-preview-media-load §4). Session-only.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum PreviewQuality {
    /// Proxy when allowed + long edge capped at [`DRAFT_MAX_LONG_EDGE`].
    /// Default for scrub/play/selection retarget.
    #[default]
    Draft,
    /// Sequence format size; originals only for decode preference.
    Full,
}

/// What the single central monitor evaluates (24-preview-media-load §3).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PreviewTarget {
    /// Sequence under the shared playhead (default).
    Sequence { sequence: SequenceId },
    /// Media-pool / Match Frame source peek on the same surface.
    Asset { asset: AssetId, source_time: Tick },
}

impl Default for PreviewTarget {
    fn default() -> Self {
        // Sequence id filled in on first present from active sequence.
        PreviewTarget::Sequence {
            sequence: SequenceId::nil(),
        }
    }
}

/// Per-asset import-ladder readiness flags (24 §2 / §8). Derived or published
/// for GUI/MCP; not document state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AssetReadiness {
    pub probe: bool,
    pub poster: bool,
    pub keyframe_index: bool,
    pub proxy_ready: bool,
}

/// Job-level render options (K-F4 / 26 §14) — how *this* run executes, not
/// the output format ([`ExportPreset`]). Defaults keep prior behaviour.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct RenderJobOptions {
    /// Use proxies when available (fast verification renders).
    pub use_proxies: bool,
    /// Evaluate at Draft long-edge (preview resolution) rather than full format.
    pub preview_resolution: bool,
    /// Prefer a hardware encoder when the preset codec has a probed HW twin
    /// (K-F5). Fail-closed: if preferred HW is missing, export errors rather
    /// than silently falling back to software.
    pub prefer_hardware: bool,
    /// Optional raw `key=value` pairs appended to the encoder invocation
    /// (Shotcut-style escape hatch, K-F5).
    pub raw_encoder_args: Vec<String>,
    /// FFmpeg `-preset` speed string when supported (e.g. `"veryfast"`).
    pub encoder_speed: Option<String>,
    /// Burn sequence timecode / frame number into the picture (K-F polish).
    /// Implemented as an ffmpeg `drawtext` filter on the encode path when
    /// tools are available; no-op when drawtext is missing.
    pub burn_in_timecode: bool,
    /// After a successful render, the GUI should offer / perform "add result
    /// to media bin" (K-F polish). The engine records the path; the host acts.
    pub add_to_bin: bool,
    /// Request a two-pass encode. Currently rejected during export preflight;
    /// it is never silently downgraded to a one-pass encode.
    pub two_pass: bool,
    /// Inhibit system sleep while this job runs (K-F polish). Best-effort;
    /// platforms without a known inhibit API no-op.
    pub inhibit_sleep: bool,
}

/// An export request (02 §7). Carried by [`EngineCmd::Export`]: the engine
/// spawns a dedicated worker thread that runs
/// [`crate::export::job::run_export_job`] over a frozen snapshot and publishes
/// progress on [`EngineStatus::export`]. The MCP `export_sequence` tool funnels
/// through the same relocated fn, so there is one render/encode path.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportJob {
    pub sequence: SequenceId,
    pub format_index: usize,
    pub preset: ExportPreset,
    pub output: PathBuf,
    /// `None` = the sequence's work range / full extent.
    pub range: Option<(Tick, Tick)>,
    /// How this render runs (K-F4). Default = full quality, software path.
    pub options: RenderJobOptions,
}

/// Live export progress the GUI polls off [`EngineStatus`] (02 §7). Published
/// wait-free by the engine thread from a background export worker's
/// `ExportEvent` stream; `done` flips true on completion/cancel/failure, and
/// `error` carries the message on failure.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportProgressSnapshot {
    /// Monotonic per-session export counter (distinguishes successive exports).
    pub job: u64,
    pub frame: u64,
    pub total: u64,
    pub fps: f32,
    pub eta: Duration,
    pub done: bool,
    pub error: Option<String>,
}

/// GUI/MCP → engine commands (02 §1).
#[derive(Clone, Debug)]
pub enum EngineCmd {
    /// Sidecar root supplied by the host after save/open; unsaved projects use
    /// a bounded temporary cache. This is session state, never document state.
    SetPreviewCacheDir {
        path: PathBuf,
    },
    SetPreviewProfile(crate::preview::PreviewProfile),
    RenderPreview {
        sequence: SequenceId,
        range: Option<(Tick, Tick)>,
    },
    CancelPreview {
        sequence: SequenceId,
    },
    ClearPreview {
        sequence: SequenceId,
        range: Option<(Tick, Tick)>,
    },
    /// Atomically select one exact paused inspection. Published frames echo the
    /// request id only after every required source is ready.
    InspectFrame {
        request_id: u64,
        sequence: SequenceId,
        time: Tick,
        proxy_mode: ProxyMode,
        quality: PreviewQuality,
        scope_tap: ScopeTapPoint,
    },
    AuditionSource {
        asset: AssetId,
        start: Tick,
        end: Tick,
    },
    StopSourceAudition,
    Play,
    Pause,
    /// Coalesced latest-wins per engine tick (02 §4 scrub rule).
    Seek(Tick),
    /// Live scrub target while the playhead is being dragged. Coalesced
    /// latest-wins like `Seek`, but decodes only a cheap keyframe preview; a
    /// trailing `Seek` (drag-release settle) supersedes it and lands the exact
    /// frame.
    ScrubSeek(Tick),
    /// Pause + snap ±n frames, evaluate exactly that tick (CAP-004).
    Step(i32),
    SetLoop(Option<(Tick, Tick)>),
    SetActiveSequence(SequenceId),
    SetProxyMode(ProxyMode),
    /// Retarget the single monitor (24 §3). Play forces Sequence (play-wins).
    SetPreviewTarget(PreviewTarget),
    /// Draft (default) vs Full interactive quality (24 §4).
    SetPreviewQuality(PreviewQuality),
    /// K-E2: choose which texture the scopes measure (03 §3.6 / 07 §5) — the
    /// program pre-`CaptionOverlay`, or one clip's post-`Grade` node. Pure view
    /// state, like [`EngineCmd::SetPreviewTarget`]: it changes nothing the
    /// document can observe, so it is not a `Command` and has no undo unit.
    SetScopeTap(ScopeTapPoint),
    /// D-12: run stabilization analysis for `clip` and warm the engine's
    /// analysis cache (22 §6.5).
    ///
    /// A command rather than a document `Command`: analysis is *generation, not
    /// history*, so it produces a cache entry and never an undo step. Running
    /// it on the engine thread keeps a long clip's integration off the UI.
    AnalyzeStabilization {
        clip: photonic_core::timeline::ClipId,
    },
    /// K-B5: enable/disable compare-effect mode (second clean compile without
    /// clip looks). View state only — not a document Command.
    SetCompareEffects(bool),
    /// Source-space seek while `PreviewTarget::Asset` (ignored for Sequence).
    SeekSource {
        asset: AssetId,
        time: Tick,
    },
    /// Asset relink / proxy swap (02 §5). P3 over-invalidates (whole node
    /// cache + decode sources) — the hash→asset index for targeted eviction
    /// is the proxy/relink story's seam.
    InvalidateRange(SequenceId, Tick, Tick),
    /// Start a headless export on a dedicated worker thread; progress is
    /// published on [`EngineStatus::export`]. A new `Export` (or
    /// [`EngineCmd::CancelExport`]) poisons any in-flight one.
    Export(Box<ExportJob>),
    /// Cancel the in-flight export (if any). The worker stops between frames.
    CancelExport,
    /// Probe an asset's media file (K-0.8 / 24): runs `ffprobe`, writes
    /// [`MediaProbe`] + content hash onto the asset via `set_asset_meta`, and
    /// invalidates any open decode source so the next present re-opens it.
    Probe(AssetId),
    /// Stop the engine thread. [`EngineSession`] sends this on drop/shutdown.
    Shutdown,
}

/// Immutable render input shared by publishers, engine and source caches.
/// Revision is the real document history revision; publication generation is
/// separate so a per-call format override can replace the view at that revision.
#[derive(Clone)]
pub struct RenderSnapshot {
    pub revision: u64,
    pub project: Option<Arc<TimelineProject>>,
    pub document: Option<Arc<Document>>,
}
impl RenderSnapshot {
    pub fn from_document(document: &Document, revision: u64) -> Self {
        let project = document
            .timeline
            .as_ref()
            .map(|project| Arc::new(project.clone()));
        let needs_vectors = project.as_ref().is_some_and(|project| {
            project
                .media
                .assets
                .values()
                .any(|asset| matches!(asset.source, AssetSource::EmbeddedVector { .. }))
        });
        Self {
            revision,
            project,
            document: needs_vectors.then(|| Arc::new(document.clone())),
        }
    }
}
struct PublishedSnapshot {
    generation: u64,
    value: RenderSnapshot,
}

/// What the GUI presents (02 §1): `Rgba16Float`, linear, premultiplied (D-09).
/// The present path is 03 §5's `present_engine_frame`.
pub struct EngineFrame {
    /// Lossy cached playback provenance. Native exports reject these frames.
    pub cached_preview: bool,
    pub inspection_request_id: Option<u64>,
    pub content_hash: crate::graph::ir::ContentHash,
    pub snapshot_generation: u64,
    /// Snapshot and processing settings that produced this particular frame.
    /// Consumers must not infer provenance from a newer status publication.
    pub doc_revision: u64,
    pub preview_quality: PreviewQuality,
    pub proxy_mode: ProxyMode,
    pub texture: Arc<wgpu::Texture>,
    /// Logical output dimensions; the backing texture may be rounded up by the
    /// pool and the GUI must crop to this size, especially for asset peeks.
    pub logical_size: (u32, u32),
    /// Exact frame-start tick this frame was evaluated at (sequence or source).
    pub time: Tick,
    pub sequence: SequenceId,
    /// When set, this frame is an asset source peek (24 §3), not program-out.
    pub preview_asset: Option<AssetId>,
    /// K-E2 scope tap for this frame: an **intermediate** texture of the same
    /// evaluation (03 §3.6 readback point), not a second render. `None` only for
    /// an asset peek or an empty program. Carries its logical size because the
    /// pooled texture is bucket-padded — scopes must measure the logical extent.
    pub scope_tap: Option<GpuFrame>,
    /// Which tap `scope_tap` actually is — the requested one, or
    /// [`ScopeTapPoint::Program`] after the 13 §10.2 fallback (the playhead is
    /// not over the requested clip). The UI labels from this, never from the
    /// request, so "Scoping: <clip>" cannot lie.
    pub scope_tap_point: ScopeTapPoint,
    /// K-B5: when compare-effects is on, the clean (no clip effects/grade)
    /// evaluation for the same tick. `None` when compare is off or the clean
    /// path failed.
    pub compare_clean: Option<Arc<wgpu::Texture>>,
}

/// Engine → GUI state (02 §1: playhead, dropped frames, cache stats, xruns).
#[derive(Clone, Debug)]
pub struct EngineStatus {
    pub source_audition: Option<crate::source_audition::SourceAuditionStatus>,
    pub source_audition_error: Option<String>,
    pub memory: EngineMemoryStatus,
    pub readiness: SourceReadinessStatus,
    pub command_admission: CommandAdmissionStatus,
    pub snapshot_generation: u64,
    pub playhead: Tick,
    pub playing: bool,
    /// Frames dropped by the cover-interval rule (late > 1 frame, 02 §4).
    pub dropped: u64,
    pub cache: CacheStats,
    /// Audio callback underrun frames (09 §5).
    pub audio_xruns: u64,
    /// Number of program frames published by the engine. The GUI may coalesce
    /// these to the newest texture; comparing this with presentation telemetry
    /// identifies a UI/vsync bottleneck separately from decode/compositing.
    pub frames_published: u64,
    /// Total graph-evaluation attempts by this engine session. Compare-effects
    /// mode performs a second clean evaluation, and both passes are counted.
    pub evaluations: u64,
    /// Duration of the most recent compile + graph evaluation in microseconds.
    /// This is deliberately cheap, per-frame telemetry for Linux performance
    /// reports; it is not a profiler substitute.
    pub last_evaluate_micros: u64,
    /// Primary program evaluations that produced no frame (for example an
    /// unavailable decode source), kept distinct from clock-driven dropped
    /// frames. A missing optional compare-effects texture does not mark the
    /// program evaluation as a miss.
    pub evaluation_misses: u64,
    /// The `CommandHistory` revision the current snapshot was taken at
    /// (02 §1's `doc_generation`).
    pub doc_revision: u64,
    pub active_sequence: Option<SequenceId>,
    /// Most recent command failure as a first-class diagnostic (36).
    /// GUI badge surface: `panels/video/diagnostics::diag_badge`.
    pub last_error: Option<photonic_core::diag::Diagnostic>,
    /// Current single-monitor target (24 §3).
    pub preview_target: PreviewTarget,
    /// Draft / Full interactive quality (24 §4).
    pub preview_quality: PreviewQuality,
    /// True while an exact-frame eval is outstanding after retarget/seek and
    /// no new frame was published this tick (GUI may keep last/poster).
    pub buffering: bool,
    /// Live progress of the in-flight (or most recent) export (02 §7). `None`
    /// until the session's first `EngineCmd::Export`. The GUI export dialog
    /// polls this wait-free.
    pub export: Option<ExportProgressSnapshot>,
    /// Master-bus output meter (G-4): linear peak/RMS per channel `[L, R]`,
    /// sampled from the mixer feeder's `StereoMeter` each status publish.
    /// `None` when no feeder is running (paused / no audio device).
    pub master_level: Option<MasterMeterSnapshot>,
    /// K-E1: latest master-bus spectrum in dBFS (downsampled to 64 bins).
    /// `None` when the mixer feeder is not running.
    pub spectrum_db: Option<Vec<f32>>,
    /// Total graph latency in samples (31 §3): max track-path latency + master
    /// chain. Published for A/V clock offset; 0 when no feeder is running.
    pub graph_latency_samples: u32,
    /// K-E2: the scope tap the engine is currently **asked** for. Echoed so a
    /// stateless UI can send [`EngineCmd::SetScopeTap`] only on a real change
    /// (compare `EngineFrame::scope_tap_point` to see what it actually got).
    pub scope_tap: ScopeTapPoint,
}

/// Managed per-session cache occupancy, not whole-process memory. Decode
/// working buffers, FFmpeg/audio internals, renderer scratch and handles held
/// outside these caches are separate allocations.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineMemoryStatus {
    pub decoded_ring_bytes: u64,
    pub decoded_ring_budget_bytes: u64,
    pub upload_bytes: u64,
    pub still_bytes: u64,
    pub vector_bytes: u64,
    pub graph_bytes: u64,
    pub gpu_cache_bytes: u64,
    pub gpu_cache_budget_bytes: u64,
    pub raster_jobs: usize,
    pub raster_job_byte_limit: u64,
    pub pressure: bool,
}
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceReadinessStatus {
    pub requested: usize,
    pub ready: usize,
    pub pending_source_builds: usize,
    pub pending_rasters: usize,
    pub capacity_limited: bool,
    pub failed_sources: usize,
}

/// Allocation-free preview/decode telemetry sampled by the UI or a benchmark.
///
/// `ring_hits` and `inline_seeks` are process-wide, monotonic decode counters;
/// they intentionally are not reset when a session opens or closes. The
/// remaining fields come from one published [`EngineStatus`] snapshot, so they
/// describe a coherent point in this session's engine state. The two sources
/// cannot be sampled as one cross-thread atomic transaction, so callers that
/// need interval measurements should take and subtract two snapshots.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct PreviewTelemetrySnapshot {
    /// Frames supplied from a decoded-frame ring since process start.
    pub ring_hits: u64,
    /// Decode-ring misses since process start. Misses now hold the last good
    /// frame rather than performing an engine-thread inline seek.
    pub decode_misses: u64,
    /// Compatibility alias for [`Self::decode_misses`]. This historic name is
    /// retained for existing benchmarks; it does not indicate an inline seek.
    pub inline_seeks: u64,
    /// Program frames published by this engine session.
    pub frames_published: u64,
    /// Total graph evaluations attempted by this engine session.
    pub evaluations: u64,
    /// Evaluations that did not produce an exact frame for this session.
    pub evaluation_misses: u64,
    /// Compile + graph-evaluation duration for the most recent attempt.
    pub last_evaluate_micros: u64,
}

impl PreviewTelemetrySnapshot {
    fn from_status(status: &EngineStatus, ring_hits: u64, inline_seeks: u64) -> Self {
        Self {
            ring_hits,
            decode_misses: inline_seeks,
            inline_seeks,
            frames_published: status.frames_published,
            evaluations: status.evaluations,
            evaluation_misses: status.evaluation_misses,
            last_evaluate_micros: status.last_evaluate_micros,
        }
    }
}

/// Wait-free master meter snapshot (G-4 / 09 §8). Linear amplitude, not dB —
/// the GUI converts for display (same unit as `StereoMeter::peak`/`rms`).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct MasterMeterSnapshot {
    pub peak: [f32; 2],
    pub rms: [f32; 2],
}

impl Default for EngineStatus {
    fn default() -> Self {
        EngineStatus {
            source_audition: None,
            source_audition_error: None,
            memory: EngineMemoryStatus::default(),
            readiness: SourceReadinessStatus::default(),
            command_admission: CommandAdmissionStatus::default(),
            snapshot_generation: 0,
            playhead: Tick::ZERO,
            playing: false,
            dropped: 0,
            cache: CacheStats::default(),
            audio_xruns: 0,
            frames_published: 0,
            evaluations: 0,
            last_evaluate_micros: 0,
            evaluation_misses: 0,
            doc_revision: 0,
            active_sequence: None,
            last_error: None,
            preview_target: PreviewTarget::default(),
            preview_quality: PreviewQuality::Draft,
            buffering: false,
            export: None,
            master_level: None,
            spectrum_db: None,
            graph_latency_samples: 0,
            scope_tap: ScopeTapPoint::Program,
        }
    }
}

/// Maximum pending commands and maximum commands handled before presenting.
pub const COMMAND_QUEUE_CAP: usize = 256;
const COMMANDS_PER_TICK: usize = 64;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandAdmissionStatus {
    pub pending: usize,
    pub coalesced: u64,
    /// Explicitly refused submissions; callers may retry semantic commands.
    pub rejected: u64,
}

fn coalesce_key(cmd: &EngineCmd) -> Option<u8> {
    match cmd {
        EngineCmd::Seek(_) | EngineCmd::ScrubSeek(_) => Some(0),
        EngineCmd::SetPreviewQuality(_) => Some(1),
        EngineCmd::SetProxyMode(_) => Some(2),
        EngineCmd::SetLoop(_) => Some(3),
        EngineCmd::SetScopeTap(_) => Some(4),
        EngineCmd::SetCompareEffects(_) => Some(5),
        _ => None,
    }
}

/// Latest view state wins only within a run of replaceable commands. Playback,
/// stepping, source retargets, probes and exports are ordering barriers.
pub fn coalesce_commands(batch: Vec<EngineCmd>) -> Vec<EngineCmd> {
    let mut out = VecDeque::with_capacity(batch.len());
    for cmd in batch {
        remove_superseded(&mut out, &cmd);
        out.push_back(cmd);
    }
    out.into_iter().collect()
}

fn remove_superseded(queue: &mut VecDeque<EngineCmd>, cmd: &EngineCmd) -> bool {
    let Some(key) = coalesce_key(cmd) else {
        return false;
    };
    // Position updates are the one stream that may cross other replaceable
    // view settings: a scrub storm should still collapse to its latest tick
    // when a quality/proxy toggle was queued in the same burst. Settings keep
    // their own ordering relative to one another, so a later quality request
    // does not erase an earlier quality request across a proxy/position change.
    let same_run = |old: &&EngineCmd| {
        if key == 0 {
            coalesce_key(old).is_some()
        } else {
            coalesce_key(old) == Some(key)
        }
    };
    let previous = queue
        .iter()
        .enumerate()
        .rev()
        .take_while(|(_, old)| same_run(old))
        .find(|(_, old)| coalesce_key(old) == Some(key))
        .map(|(i, _)| i);
    previous.is_some_and(|index| queue.remove(index).is_some())
}

struct CommandMailbox {
    queue: Mutex<VecDeque<EngineCmd>>,
    wake_tx: Sender<()>,
    wake_rx: Receiver<()>,
    closed: AtomicBool,
    requested_quality: AtomicU8,
    quality_dirty: AtomicBool,
    requested_proxy: AtomicU8,
    proxy_dirty: AtomicBool,
    coalesced: AtomicU64,
    rejected: AtomicU64,
}

impl CommandMailbox {
    fn new() -> Self {
        let (wake_tx, wake_rx) = crossbeam_channel::bounded(1);
        Self {
            queue: Mutex::new(VecDeque::with_capacity(COMMAND_QUEUE_CAP)),
            wake_tx,
            wake_rx,
            closed: AtomicBool::new(false),
            requested_quality: AtomicU8::new(0),
            quality_dirty: AtomicBool::new(false),
            requested_proxy: AtomicU8::new(0),
            proxy_dirty: AtomicBool::new(false),
            coalesced: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
        }
    }

    fn send(&self, cmd: EngineCmd) -> bool {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        // A sticky view setting has a dedicated latest-wins slot. Temporary
        // render guards can always restore it, even when semantic work is full.
        if let EngineCmd::SetPreviewQuality(quality) = &cmd {
            self.requested_quality.store(
                u8::from(*quality == PreviewQuality::Full),
                Ordering::Release,
            );
            if self.quality_dirty.swap(true, Ordering::AcqRel) {
                saturating_increment(&self.coalesced);
            }
            drop(queue);
            let _ = self.wake_tx.try_send(());
            return true;
        }
        if let EngineCmd::SetProxyMode(mode) = &cmd {
            let value = match mode {
                ProxyMode::Auto => 0,
                ProxyMode::ForceProxy => 1,
                ProxyMode::ForceOriginal => 2,
            };
            self.requested_proxy.store(value, Ordering::Release);
            if self.proxy_dirty.swap(true, Ordering::AcqRel) {
                saturating_increment(&self.coalesced);
            }
            drop(queue);
            let _ = self.wake_tx.try_send(());
            return true;
        }
        if matches!(cmd, EngineCmd::Shutdown) {
            self.closed.store(true, Ordering::Release);
            queue.clear(); // shutdown explicitly cancels outstanding work
        } else {
            if remove_superseded(&mut queue, &cmd) {
                saturating_increment(&self.coalesced);
            }
            if queue.len() >= COMMAND_QUEUE_CAP {
                saturating_increment(&self.rejected);
                return false;
            }
        }
        queue.push_back(cmd);
        drop(queue);
        let _ = self.wake_tx.try_send(());
        true
    }

    fn drain_into(&self, batch: &mut Vec<EngineCmd>) {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        let count = queue.len().min(COMMANDS_PER_TICK);
        if self.quality_dirty.swap(false, Ordering::AcqRel) {
            let quality = if self.requested_quality.load(Ordering::Acquire) == 1 {
                PreviewQuality::Full
            } else {
                PreviewQuality::Draft
            };
            batch.push(EngineCmd::SetPreviewQuality(quality));
        }
        if self.proxy_dirty.swap(false, Ordering::AcqRel) {
            let mode = match self.requested_proxy.load(Ordering::Acquire) {
                1 => ProxyMode::ForceProxy,
                2 => ProxyMode::ForceOriginal,
                _ => ProxyMode::Auto,
            };
            batch.push(EngineCmd::SetProxyMode(mode));
        }
        batch.extend(queue.drain(..count));
        if !queue.is_empty() {
            let _ = self.wake_tx.try_send(());
        }
    }

    fn status(&self) -> CommandAdmissionStatus {
        CommandAdmissionStatus {
            pending: self.queue.lock().unwrap_or_else(|e| e.into_inner()).len(),
            coalesced: self.coalesced.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
        }
    }
}

/// Matrix/range selection from a probe (02 §3 "BT.601/709 per probe"): trust
/// the container's tags when present, else the SD/HD resolution heuristic.
pub fn colorimetry_for_probe(details: &ProbeDetails) -> Colorimetry {
    let v = details.probe.video.as_ref();
    let color = v.map(|v| &v.color);
    let matrix = match color.and_then(|c| c.matrix.as_deref()) {
        Some("bt709") => Matrix::Bt709,
        Some("smpte170m") | Some("bt470bg") | Some("smpte240m") => Matrix::Bt601,
        _ => match v {
            Some(v) if v.height >= 720 => Matrix::Bt709,
            Some(_) => Matrix::Bt601,
            None => Matrix::Bt709,
        },
    };
    let range = match color.and_then(|c| c.full_range) {
        Some(true) => Range::Full,
        _ => Range::Limited,
    };
    Colorimetry { matrix, range }
}

// ── Facade ───────────────────────────────────────────────────────────────────

/// The engine facade (02 §1). Owns the shared GPU handle; each open document
/// gets its own [`EngineSession`] (engine thread + audio host + caches).
pub struct VideoEngine {
    gpu: GpuContext,
}

impl VideoEngine {
    /// Share the renderer's wgpu device/queue (02 §1). [`GpuContext`] is the
    /// crate's shared handle type — `GpuContext::new(device, queue)` wraps
    /// whatever the host renderer already owns.
    pub fn new(gpu: GpuContext) -> Self {
        VideoEngine { gpu }
    }

    /// Request an own headless adapter (CLI/MCP/tests). `None` when no GPU
    /// adapter is available (the adapter-skip convention).
    pub fn headless() -> Option<Self> {
        GpuContext::request_blocking().map(VideoEngine::new)
    }

    pub fn gpu(&self) -> &GpuContext {
        &self.gpu
    }

    /// Open a per-document session: spawns the engine thread, which snapshots
    /// `doc.timeline` whenever `history`'s revision moves (never blocking on
    /// either lock).
    pub fn open_session(
        &self,
        doc: Arc<Mutex<Document>>,
        history: Arc<Mutex<CommandHistory>>,
    ) -> EngineSession {
        let (_legacy_tx, rx) = crossbeam_channel::bounded(1);
        let mailbox = Arc::new(CommandMailbox::new());
        let thread_mailbox = Arc::clone(&mailbox);
        let frame: Arc<ArcSwapOption<EngineFrame>> = Arc::new(ArcSwapOption::from(None));
        let status: Arc<ArcSwap<EngineStatus>> =
            Arc::new(ArcSwap::from_pointee(EngineStatus::default()));
        let preview_status = Arc::new(ArcSwap::from_pointee(
            crate::preview::PreviewStatusSnapshot::default(),
        ));
        let thread_preview_status = Arc::clone(&preview_status);
        let snapshot_input = Arc::new(ArcSwapOption::from(None));
        let thread_input = Arc::clone(&snapshot_input);
        let gpu = self.gpu.clone();
        let frame_out = Arc::clone(&frame);
        let status_out = Arc::clone(&status);
        let join = std::thread::Builder::new()
            .name("photonic-video-engine".into())
            .spawn(move || {
                let mut thread = EngineThread::new(gpu, doc, history, rx, frame_out, status_out);
                thread.preview_status_out = thread_preview_status;
                thread.snapshot_input = thread_input;
                thread.mailbox = Some(thread_mailbox);
                thread.run();
            })
            .expect("spawn photonic-video engine thread");
        EngineSession {
            preview_status,
            snapshot_input,
            next_snapshot_generation: std::sync::atomic::AtomicU64::new(1),
            mailbox,
            frame,
            status,
            join: Some(join),
        }
    }
}

/// Per-open-document runtime handle (02 §1). Cheap wait-free reads on the GUI
/// side; commands are fire-and-forget.
pub struct EngineSession {
    preview_status: Arc<ArcSwap<crate::preview::PreviewStatusSnapshot>>,
    snapshot_input: Arc<ArcSwapOption<PublishedSnapshot>>,
    next_snapshot_generation: std::sync::atomic::AtomicU64,
    mailbox: Arc<CommandMailbox>,
    frame: Arc<ArcSwapOption<EngineFrame>>,
    status: Arc<ArcSwap<EngineStatus>>,
    join: Option<JoinHandle<()>>,
}

impl EngineSession {
    /// Wait-free publication of actual preview jobs/cache state.
    pub fn preview_status(&self) -> crate::preview::PreviewStatusSnapshot {
        self.preview_status.load_full().as_ref().clone()
    }

    /// Publish an already-cloned snapshot. Latest publication wins; unchanged
    /// callers retain and reuse their immutable input instead of deep comparing.
    pub fn publish_snapshot(&self, snapshot: RenderSnapshot) -> u64 {
        let generation = self
            .next_snapshot_generation
            .fetch_add(1, Ordering::Relaxed);
        self.snapshot_input.store(Some(Arc::new(PublishedSnapshot {
            generation,
            value: snapshot,
        })));
        let _ = self.mailbox.wake_tx.try_send(());
        generation
    }

    /// Send a command to the engine thread. Returns `false` if the engine has
    /// already shut down or its bounded queue is full. No admitted semantic
    /// command is silently discarded; a refused command may be retried.
    pub fn send(&self, cmd: EngineCmd) -> bool {
        self.mailbox.send(cmd)
    }

    /// Latest successfully admitted sticky quality, before the engine applies
    /// queued settings. Use this when temporarily overriding render quality.
    pub fn requested_preview_quality(&self) -> PreviewQuality {
        if self.mailbox.requested_quality.load(Ordering::Acquire) == 1 {
            PreviewQuality::Full
        } else {
            PreviewQuality::Draft
        }
    }

    pub fn requested_proxy_mode(&self) -> ProxyMode {
        match self.mailbox.requested_proxy.load(Ordering::Acquire) {
            1 => ProxyMode::ForceProxy,
            2 => ProxyMode::ForceOriginal,
            _ => ProxyMode::Auto,
        }
    }

    /// The most recently published frame (wait-free; `None` before the first
    /// evaluation).
    pub fn latest_frame(&self) -> Option<Arc<EngineFrame>> {
        self.frame.load_full()
    }

    /// The most recently published status (wait-free).
    pub fn status(&self) -> Arc<EngineStatus> {
        self.status.load_full()
    }

    /// Sample preview/decode counters without blocking the engine or resetting
    /// process-wide instrumentation. This clones the published `Arc` (an
    /// atomic refcount operation, not a heap allocation), avoiding ArcSwap's
    /// thread-local guard setup on a UI thread's first sample.
    pub fn preview_telemetry(&self) -> PreviewTelemetrySnapshot {
        let status = self.status.load_full();
        PreviewTelemetrySnapshot::from_status(
            &status,
            RING_HITS.load(Ordering::Relaxed),
            INLINE_SEEKS.load(Ordering::Relaxed),
        )
    }

    /// Stop the engine thread and join it. (Dropping the session does the
    /// same; this form surfaces the join point explicitly.)
    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        let _ = self.mailbox.send(EngineCmd::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for EngineSession {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

// ── LUT provider (K-0.5) ─────────────────────────────────────────────────────

/// One cached `.cube` LUT: the source path it was parsed from (so a relinked
/// asset re-parses) and the parsed table (`None` = a cached miss — offline or
/// unparsable).
struct LutEntry {
    path: PathBuf,
    table: Option<Arc<photonic_render::Lut3d>>,
}

/// Memoised `.cube` LUT cache (K-0.5): every referenced `Lut3d` asset is parsed
/// exactly ONCE, in [`warm`](LutCache::warm) (engine thread, on snapshot change),
/// so the compiler's [`LutProvider`] read is a lock-free `HashMap` hit and no
/// `.cube` parsing ever happens on the per-frame compile hot path. A read/parse
/// failure caches a negative entry (→ the grade op resolves inert / identity,
/// never a black frame — 07 §1) and records one diagnostic.
#[derive(Default)]
struct LutCache {
    entries: HashMap<AssetId, LutEntry>,
    /// One diagnostic per still-unresolved LUT, surfaced onto the compiled frame.
    failures: Vec<CompileDiagnostic>,
}

impl LutCache {
    /// (Re)parse every file-backed `Lut3d` asset in `project` whose path is not
    /// already cached, and rebuild the failure list. Off the per-frame path.
    fn warm(&mut self, project: &TimelineProject) {
        self.failures.clear();
        for (id, asset) in &project.media.assets {
            if asset.kind != AssetKind::Lut3d {
                continue;
            }
            let AssetSource::File { path, .. } = &asset.source else {
                continue;
            };
            // Parse only when absent or the source path changed (relink) — reads
            // never happen per frame.
            let stale = self
                .entries
                .get(id)
                .map(|e| &e.path != path)
                .unwrap_or(true);
            if stale {
                let table = std::fs::read_to_string(path)
                    .ok()
                    .and_then(|src| photonic_render::parse_cube(&src).ok())
                    .map(Arc::new);
                self.entries.insert(
                    *id,
                    LutEntry {
                        path: path.clone(),
                        table,
                    },
                );
            }
            if self
                .entries
                .get(id)
                .and_then(|e| e.table.as_ref())
                .is_none()
            {
                self.failures.push(CompileDiagnostic {
                    message: format!(
                        "LUT asset {id} could not be loaded from {}; the grade op \
                         renders inert (identity)",
                        path.display()
                    ),
                    graph: None,
                    node: None,
                    code: None,
                    severity: DiagSeverity::Warning,
                    clip: None,
                });
            }
        }
    }
}

impl LutProvider for LutCache {
    fn lut(&self, asset: AssetId) -> Option<Arc<photonic_render::Lut3d>> {
        self.entries.get(&asset).and_then(|e| e.table.clone())
    }
}

// ── Engine thread ────────────────────────────────────────────────────────────

enum PendingPreviewAction {
    Render(SequenceId, Option<(Tick, Tick)>),
    Clear(SequenceId, Option<(Tick, Tick)>),
}

struct EngineThread {
    preview_loading: Option<
        JoinHandle<(
            u64,
            Result<crate::preview::PreviewRuntime, crate::preview::PreviewError>,
        )>,
    >,
    preview_directory_epoch: u64,
    preview_error: Option<String>,
    preview_actions: VecDeque<PendingPreviewAction>,
    preview: Option<crate::preview::PreviewRuntime>,
    preview_directory: Option<PathBuf>,
    preview_profile: crate::preview::PreviewProfile,
    preview_status_out: Arc<ArcSwap<crate::preview::PreviewStatusSnapshot>>,
    preview_status_at: Instant,
    snapshot_input: Arc<ArcSwapOption<PublishedSnapshot>>,
    snapshot_generation: u64,
    doc: Arc<Mutex<Document>>,
    history: Arc<Mutex<CommandHistory>>,
    rx: Receiver<EngineCmd>,
    mailbox: Option<Arc<CommandMailbox>>,
    frame_out: Arc<ArcSwapOption<EngineFrame>>,
    status_out: Arc<ArcSwap<EngineStatus>>,

    evaluator: Evaluator,
    media: MediaSources,
    controller: PlaybackController,
    inspection_request_id: Option<u64>,
    source_audition: Option<crate::source_audition::SourceAudition>,
    source_audition_error: Option<String>,

    snapshot: Option<Arc<TimelineProject>>,
    /// Memoised `.cube` LUT tables (K-0.5), warmed on snapshot change and read
    /// lock-free by the compiler's `Grade` `Lut3d` resolution.
    lut_cache: LutCache,
    /// Solved deflicker gains per clip (see `graph::deflicker`), produced by an
    /// on-demand measurement job and read by the compiler.
    deflicker_gains: crate::graph::deflicker::GainTable,
    /// Fingerprint of the inputs each entry in `deflicker_gains` was measured
    /// from, so an edit that changes them re-measures and one that does not
    /// costs nothing.
    deflicker_fingerprints: std::collections::HashMap<ClipId, u64>,
    /// D-12 warm per-clip stabilization analyses (22 §6.4), read lock-free by
    /// the compiler exactly as `lut_cache` is.
    stabilization: crate::graph::stabilize::StabilizationCache,
    last_revision: Option<u64>,
    active_sequence_override: Option<SequenceId>,
    proxy_mode: ProxyMode,
    /// Hysteresis state behind [`ProxyMode::Auto`]. Owned by the engine thread,
    /// so observing a present adds neither a lock nor an allocation.
    adaptive_proxy: AdaptiveProxyPolicy,
    preview_target: PreviewTarget,
    preview_quality: PreviewQuality,
    /// Set when present requested a frame but eval produced none (24 §5).
    buffering: bool,
    last_error: Option<photonic_core::diag::Diagnostic>,
    /// True while the GUI is actively dragging the playhead (between a
    /// `ScrubSeek` and the settling `Seek`). Selects the cheap keyframe-preview
    /// decode path and lets the compositor hold the last frame between previews.
    scrubbing: bool,
    /// K-E2: which texture the scopes measure. View state, never a document
    /// mutation; defaults to the program tap so scopes stop reading the
    /// post-`CaptionOverlay` presented frame even before a clip is chosen.
    scope_tap: ScopeTapPoint,
    /// K-B5: when true, each present also evaluates a clean compile (no clip
    /// looks) into [`EngineFrame::compare_clean`].
    compare_effects: bool,

    audio: AudioEngine,
    feeder: Option<AudioFeeder>,
    xruns: Option<Arc<XrunCounters>>,
    tools: Option<FfmpegTools>,
    /// Playback telemetry: distinguish producer/evaluator pressure from
    /// clock-side drops reported by `PlaybackController`.
    frames_published: u64,
    /// Every call into the graph evaluator, including optional compare-effects
    /// clean passes. Kept separately from published frames because a single
    /// present can intentionally evaluate twice.
    evaluations: u64,
    last_evaluate_micros: u64,
    evaluation_misses: u64,
    /// Reused command drain buffer (avoids per-tick `Vec` alloc on the engine
    /// thread — Linux + Windows hot path).
    cmd_batch: Vec<EngineCmd>,
    /// Skip redundant status Arc allocations when nothing visible changed.
    last_status_sig: u64,
    /// Live export progress written by the background export worker, read into
    /// [`EngineStatus::export`] each publish (02 §7). Wait-free both sides.
    export_progress: Arc<ArcSwapOption<ExportProgressSnapshot>>,
    /// Cancel flag of the in-flight export worker (if any). A new export or an
    /// [`EngineCmd::CancelExport`] poisons it; shutdown poisons it too.
    export_cancel: Option<Arc<AtomicBool>>,
    /// Monotonic per-session export counter, stamped onto each snapshot.
    export_job_counter: u64,
    /// Live master-bus meter handle published by the mixer feeder (G-4).
    master_meter: Arc<ArcSwapOption<crate::audio::mixer::StereoMeter>>,
    /// K-E1: latest master-bus spectrum (dB) from the mixer feeder.
    spectrum_db: Arc<ArcSwapOption<Vec<f32>>>,
    /// Live graph latency samples published by the mixer feeder (31 §3).
    graph_latency: Arc<std::sync::atomic::AtomicU32>,
}

impl EngineThread {
    fn new(
        gpu: GpuContext,
        doc: Arc<Mutex<Document>>,
        history: Arc<Mutex<CommandHistory>>,
        rx: Receiver<EngineCmd>,
        frame_out: Arc<ArcSwapOption<EngineFrame>>,
        status_out: Arc<ArcSwap<EngineStatus>>,
    ) -> Self {
        let tools = locate().ok();
        EngineThread {
            preview_loading: None,
            preview_directory_epoch: 0,
            preview_error: None,
            preview_actions: VecDeque::new(),
            preview: None,
            preview_directory: None,
            preview_profile: crate::preview::PreviewProfile::default(),
            preview_status_out: Arc::new(ArcSwap::from_pointee(
                crate::preview::PreviewStatusSnapshot::default(),
            )),
            preview_status_at: Instant::now(),
            snapshot_input: Arc::new(ArcSwapOption::from(None)),
            snapshot_generation: 0,
            doc,
            history,
            rx,
            mailbox: None,
            frame_out,
            status_out,
            evaluator: Evaluator::with_budget(
                gpu,
                SESSION_GPU_CACHE_BYTES
                    - UPLOAD_CACHE_BYTES
                    - STILL_CACHE_BYTES
                    - VECTOR_CACHE_BYTES,
            ),
            media: MediaSources::new(tools.clone()),
            controller: PlaybackController::new(FrameRate::FPS_30),
            inspection_request_id: None,
            source_audition: None,
            source_audition_error: None,
            snapshot: None,
            lut_cache: LutCache::default(),
            deflicker_gains: crate::graph::deflicker::GainTable::new(),
            deflicker_fingerprints: std::collections::HashMap::new(),
            stabilization: crate::graph::stabilize::StabilizationCache::default(),
            last_revision: None,
            active_sequence_override: None,
            proxy_mode: ProxyMode::Auto,
            adaptive_proxy: AdaptiveProxyPolicy::new(),
            preview_target: PreviewTarget::default(),
            preview_quality: PreviewQuality::Draft,
            buffering: false,
            last_error: None,
            scrubbing: false,
            scope_tap: ScopeTapPoint::Program,
            compare_effects: false,
            audio: AudioEngine::new(),
            feeder: None,
            xruns: None,
            tools,
            frames_published: 0,
            evaluations: 0,
            last_evaluate_micros: 0,
            evaluation_misses: 0,
            cmd_batch: Vec::with_capacity(COMMANDS_PER_TICK + 2),
            last_status_sig: 0,
            export_progress: Arc::new(ArcSwapOption::from(None)),
            export_cancel: None,
            export_job_counter: 0,
            master_meter: Arc::new(ArcSwapOption::from(None)),
            spectrum_db: Arc::new(ArcSwapOption::from(None)),
            graph_latency: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    fn run(mut self) {
        loop {
            // 1. Wait briefly for commands, then drain the burst (a scrub
            //    produces many Seeks per engine tick — coalesced latest-wins).
            self.cmd_batch.clear();
            let poll_interval = if self.controller.is_playing()
                || self
                    .source_audition
                    .as_ref()
                    .is_some_and(|a| a.status().playing)
                || self.buffering
            {
                PLAYING_POLL_INTERVAL
            } else {
                IDLE_POLL_INTERVAL
            };
            if let Some(mailbox) = &self.mailbox {
                let _ = mailbox.wake_rx.recv_timeout(poll_interval);
                mailbox.drain_into(&mut self.cmd_batch);
            } else {
                match self.rx.recv_timeout(poll_interval) {
                    Ok(cmd) => {
                        self.cmd_batch.push(cmd);
                        while self.cmd_batch.len() < COMMANDS_PER_TICK {
                            match self.rx.try_recv() {
                                Ok(cmd) => self.cmd_batch.push(cmd),
                                Err(_) => break,
                            }
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            // A host can publish a snapshot and enqueue Play while GPU pass
            // initialization is still running. Consume that snapshot before
            // commands whose meaning depends on a loaded sequence.
            self.poll_snapshot();
            let mut shutdown = false;
            let mut batch = std::mem::take(&mut self.cmd_batch);
            for cmd in batch.drain(..) {
                if !self.handle(cmd) {
                    shutdown = true;
                    break;
                }
            }
            self.cmd_batch = batch;
            if shutdown {
                break;
            }

            // 2. doc_generation poll (02 §1): revision + changes_since via
            //    try_lock; contended ⇒ reuse the last snapshot.
            self.poll_snapshot();

            // Publish snapshot/revision changes before entering the potentially
            // expensive present path.  Consumers use this as the readiness
            // barrier for candidate previews and must not wait for a decode or
            // GPU evaluation that may be stalled by an unavailable source.
            self.publish_status();

            // 3. Present per the cover-interval rule.
            if let Some(audition) = self.source_audition.as_mut() {
                let state = audition.poll();
                if state.playing
                    || self.preview_target
                        != (PreviewTarget::Asset {
                            asset: state.asset,
                            source_time: state.playhead,
                        })
                {
                    self.preview_target = PreviewTarget::Asset {
                        asset: state.asset,
                        source_time: state.playhead,
                    };
                    self.controller.request_present();
                }
            }
            self.present();
            self.poll_preview();

            // 4. Publish status (wait-free for the reader).
            self.publish_status();
        }
        // Poison any in-flight export so its worker stops between frames rather
        // than outliving the engine thread (the worker owns its own session).
        if let Some(cancel) = self.export_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.stop_playing();
        self.preview = None;
        if let Some(loading) = self.preview_loading.take() {
            let _ = loading.join();
        }
        if let Some(mailbox) = &self.mailbox {
            mailbox.closed.store(true, Ordering::Release);
        }
    }

    /// Returns `false` on `Shutdown`.
    fn handle(&mut self, cmd: EngineCmd) -> bool {
        if matches!(
            &cmd,
            EngineCmd::Play
                | EngineCmd::Pause
                | EngineCmd::Seek(_)
                | EngineCmd::ScrubSeek(_)
                | EngineCmd::Step(_)
                | EngineCmd::SetActiveSequence(_)
                | EngineCmd::SetPreviewTarget(_)
                | EngineCmd::SeekSource { .. }
                | EngineCmd::InspectFrame { .. }
                | EngineCmd::Shutdown
        ) {
            if let Some(mut audition) = self.source_audition.take() {
                audition.stop();
            }
        }
        if matches!(
            &cmd,
            EngineCmd::Play
                | EngineCmd::Seek(_)
                | EngineCmd::ScrubSeek(_)
                | EngineCmd::Step(_)
                | EngineCmd::SetActiveSequence(_)
                | EngineCmd::SetPreviewTarget(_)
                | EngineCmd::SeekSource { .. }
                | EngineCmd::SetPreviewQuality(_)
                | EngineCmd::SetProxyMode(_)
                | EngineCmd::SetScopeTap(_)
                | EngineCmd::AuditionSource { .. }
        ) {
            self.inspection_request_id = None;
        }
        match cmd {
            EngineCmd::SetPreviewProfile(profile) => {
                self.preview_profile = profile;
                if let Some(preview) = self.preview.as_mut() {
                    preview.set_profile(profile);
                }
                self.publish_preview_status();
            }
            EngineCmd::SetPreviewCacheDir { path } => {
                if self.preview_directory.as_ref() != Some(&path) {
                    self.preview = None;
                    self.preview_directory_epoch = self.preview_directory_epoch.wrapping_add(1);
                    self.preview_directory = Some(path);
                    self.publish_preview_status();
                }
            }
            EngineCmd::RenderPreview { sequence, range } => {
                self.poll_snapshot();
                if let Err(error) = self.start_preview(sequence, range) {
                    self.preview_error = Some(error.to_string());
                    self.publish_preview_status();
                }
            }
            EngineCmd::CancelPreview { sequence } => {
                self.preview_actions.retain(|action| !matches!(action, PendingPreviewAction::Render(id,_) if *id == sequence));
                if let Some(preview) = self.preview.as_mut() {
                    preview.cancel(sequence);
                }
                self.publish_preview_status();
            }
            EngineCmd::ClearPreview { sequence, range } => {
                if let Some(preview) = self.preview.as_mut() {
                    preview.clear(sequence, range);
                } else if let Err(error) =
                    self.queue_preview_action(PendingPreviewAction::Clear(sequence, range))
                {
                    self.preview_error = Some(error.to_string());
                }
                self.publish_preview_status();
            }
            EngineCmd::InspectFrame {
                request_id,
                sequence,
                time,
                proxy_mode,
                quality,
                scope_tap,
            } => {
                self.stop_playing();
                self.scrubbing = false;
                self.active_sequence_override = Some(sequence);
                self.preview_target = PreviewTarget::Sequence { sequence };
                self.proxy_mode = proxy_mode;
                self.adaptive_proxy.reset();
                self.preview_quality = quality;
                self.scope_tap = scope_tap;
                self.controller.seek(time);
                self.inspection_request_id = Some(request_id);
            }
            EngineCmd::AuditionSource { asset, start, end } => {
                self.stop_playing();
                self.scrubbing = false;
                self.source_audition.take();
                self.source_audition_error = None;
                let result = self
                    .snapshot
                    .as_ref()
                    .and_then(|p| p.media.assets.get(&asset))
                    .ok_or_else(|| "source asset unavailable".to_owned())
                    .and_then(|asset| {
                        crate::source_audition::SourceAudition::start(
                            asset,
                            (start, end),
                            self.tools.clone(),
                        )
                        .map_err(|error| error.to_string())
                    });
                match result {
                    Ok(audition) => {
                        self.preview_target = PreviewTarget::Asset {
                            asset,
                            source_time: start,
                        };
                        self.source_audition = Some(audition);
                        self.controller.request_present();
                    }
                    Err(error) => self.source_audition_error = Some(error),
                }
            }
            EngineCmd::StopSourceAudition => {
                if let Some(mut audition) = self.source_audition.take() {
                    audition.stop();
                }
            }

            EngineCmd::Play => {
                self.scrubbing = false;
                // Play always wins: retarget to active sequence (24 §3.2).
                if let Some(project) = self.snapshot.as_ref() {
                    if let Some(seq) = self.effective_sequence(project) {
                        self.preview_target = PreviewTarget::Sequence { sequence: seq };
                    }
                }
                self.start_playing()
            }
            EngineCmd::Pause => self.stop_playing(),
            EngineCmd::Seek(t) => {
                // Settle: the drag (if any) is over — exact frame wanted.
                self.scrubbing = false;
                if self.controller.is_playing() && self.audio.is_started() {
                    // The rtrb ring is a fixed SPSC pair, so an audio-mastered
                    // seek restarts the device stream + feeder at the target
                    // (brief glitch — acceptable scrub behavior; gapless
                    // seek-while-playing is the decode-pool story's seam).
                    self.stop_playing();
                    self.controller.seek(t);
                    self.start_playing();
                } else {
                    self.controller.seek(t);
                }
            }
            EngineCmd::ScrubSeek(t) => {
                // Active drag: pause (a scrub is not playback) and seek. The
                // media source decodes only a keyframe preview (scrubbing flag),
                // so dragging stays responsive on long GOPs.
                self.stop_playing();
                self.scrubbing = true;
                self.controller.seek(t);
            }
            EngineCmd::Step(n) => {
                self.stop_playing(); // Step always pauses (02 §4)
                self.controller.step(n);
            }
            EngineCmd::SetLoop(range) => self.controller.set_loop(range),
            EngineCmd::SetActiveSequence(id) => {
                self.active_sequence_override = Some(id);
                // Keep sequence target in sync when user switches sequences.
                if matches!(self.preview_target, PreviewTarget::Sequence { .. }) {
                    self.preview_target = PreviewTarget::Sequence { sequence: id };
                }
                self.controller.request_present();
            }
            EngineCmd::SetProxyMode(mode) => {
                self.proxy_mode = mode;
                // A manual mode change starts any later Auto run with fresh
                // evidence; pressure accumulated under an explicit override
                // must not silently alter the user's next Auto preview.
                self.adaptive_proxy.reset();
                // Different quality flag ⇒ different content hashes; the old
                // entries age out hash-naturally (02 §5).
                self.controller.request_present();
            }
            EngineCmd::SetPreviewTarget(target) => {
                // While playing, ignore asset peeks (play-wins, 24 §3.2).
                if self.controller.is_playing() && matches!(target, PreviewTarget::Asset { .. }) {
                    // no-op
                } else {
                    self.preview_target = target;
                    self.controller.request_present();
                }
            }
            EngineCmd::SetPreviewQuality(q) => {
                self.preview_quality = q;
                self.controller.request_present();
            }
            EngineCmd::SetScopeTap(point) => {
                // Re-present so the next published frame carries the new tap.
                // The re-present is a cache-hit replay of the same graph (the tap
                // changes no content hash), not a re-render.
                self.scope_tap = point;
                self.controller.request_present();
            }
            EngineCmd::AnalyzeStabilization { clip } => {
                self.run_stabilization_analysis(clip);
                self.controller.request_present();
            }
            EngineCmd::SetCompareEffects(on) => {
                self.compare_effects = on;
                self.controller.request_present();
            }
            EngineCmd::SeekSource { asset, time } => {
                match &mut self.preview_target {
                    PreviewTarget::Asset {
                        asset: a,
                        source_time,
                    } if *a == asset => {
                        *source_time = time;
                        self.controller.request_present();
                    }
                    _ => {
                        // Arm asset peek if not already on this asset.
                        if !self.controller.is_playing() {
                            self.preview_target = PreviewTarget::Asset {
                                asset,
                                source_time: time,
                            };
                            self.controller.request_present();
                        }
                    }
                }
            }
            EngineCmd::InvalidateRange(seq, from, to) => {
                // The evaluator node cache is keyed by opaque content hash with
                // no hash→asset index, so evaluator eviction stays whole-cache
                // (always correct — over-invalidation, never stale).
                self.evaluator.invalidate_matching(|_| true);
                // Media decode sources / stills DO carry an asset key, so they
                // evict targeted: only assets whose clips overlap [from, to] in
                // `seq` lose their sidecar; every other primed ring survives.
                match self.snapshot.as_ref().and_then(|p| p.sequences.get(&seq)) {
                    Some(sequence) => {
                        let mut assets: HashSet<AssetId> = HashSet::new();
                        for track in sequence
                            .video_tracks
                            .iter()
                            .chain(sequence.audio_tracks.iter())
                        {
                            for clip in &track.clips {
                                // Half-open overlap with the invalidated range.
                                if clip.start < to && from < clip.end() {
                                    if let Some(asset) = clip.source.asset() {
                                        assets.insert(asset);
                                    }
                                }
                            }
                        }
                        self.media.invalidate_assets(&assets);
                    }
                    // No snapshot/sequence yet: fall back to the safe whole flush.
                    None => self.media.invalidate_all(),
                }
                self.controller.request_present();
            }
            EngineCmd::Export(job) => self.start_export(*job),
            EngineCmd::CancelExport => {
                if let Some(cancel) = &self.export_cancel {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
            EngineCmd::Probe(asset) => self.probe_asset(asset),
            EngineCmd::Shutdown => return false,
        }
        true
    }

    fn fail(&mut self, msg: String) {
        self.fail_diag(photonic_core::diag::DiagCode::ExportEncoderFailed, msg);
    }

    fn fail_diag(&mut self, code: photonic_core::diag::DiagCode, msg: String) {
        tracing::warn!(target: "photonic_video::session", "{msg}");
        self.last_error = Some(photonic_core::diag::Diagnostic::new(
            code,
            photonic_core::diag::Subject::Engine,
            msg,
        ));
    }

    /// K-0.8: run `ffprobe` on `asset`'s file, write probe + content hash into
    /// the document via `set_asset_meta`, and drop any open decode source so the
    /// next present re-opens against the fresh meta.
    fn probe_asset(&mut self, asset: AssetId) {
        use photonic_core::diag::DiagCode;
        let Some(tools) = self.tools.clone() else {
            self.fail_diag(
                DiagCode::MediaProbeFailed,
                format!("EngineCmd::Probe({asset}): ffmpeg/ffprobe tools are not available"),
            );
            return;
        };
        let Some(project) = self.snapshot.as_ref() else {
            self.fail_diag(
                DiagCode::MediaProbeFailed,
                format!("EngineCmd::Probe({asset}): no timeline snapshot is loaded yet"),
            );
            return;
        };
        let Some(media_asset) = project.media.assets.get(&asset) else {
            self.fail_diag(
                DiagCode::MediaNotFound,
                format!("EngineCmd::Probe({asset}): asset not in media pool"),
            );
            return;
        };
        let path = match &media_asset.source {
            AssetSource::File { path, .. } => path.clone(),
            other => {
                self.fail_diag(
                    DiagCode::MediaProbeFailed,
                    format!("EngineCmd::Probe({asset}): source is not a file ({other:?})"),
                );
                return;
            }
        };
        let details = match probe_details(&tools, &path) {
            Ok(d) => d,
            Err(e) => {
                self.fail_diag(
                    DiagCode::MediaProbeFailed,
                    format!("EngineCmd::Probe({asset}): {e}"),
                );
                return;
            }
        };
        let hash = content_hash(&path).ok();
        // Apply meta on the live document so undo/history and the next snapshot
        // pick it up (24 L1/L2 ladder). Drop locks before calling `fail`.
        let apply_err = {
            let mut doc = self.doc.lock().expect("engine doc lock");
            let mut history = self.history.lock().expect("engine history lock");
            match doc.timeline.as_ref() {
                None => Some("document has no timeline".to_string()),
                Some(timeline) => {
                    match photonic_core::timeline::ops::set_asset_meta(
                        timeline,
                        asset,
                        Some(details.probe.clone()),
                        hash,
                    ) {
                        Ok(cmd) => {
                            history.execute_discrete(
                                photonic_core::history::Command::Timeline(cmd),
                                &mut doc,
                            );
                            None
                        }
                        Err(e) => Some(format!("set_asset_meta: {e}")),
                    }
                }
            }
        };
        if let Some(msg) = apply_err {
            self.fail_diag(
                photonic_core::diag::DiagCode::MediaProbeFailed,
                format!("EngineCmd::Probe({asset}): {msg}"),
            );
            return;
        }
        // Force decode reopen against the updated meta.
        let mut ids = HashSet::new();
        ids.insert(asset);
        self.media.invalidate_assets(&ids);
        self.last_error = None;
        // K-G6: surface interlaced detection as a visible notice (no longer a
        // silent wrong-output path). Deinterlace itself is a separate node;
        // scan is persisted on VideoStreamInfo for triage / GUI.
        if let Some(msg) = crate::media::probe::interlaced_consequence(details.scan) {
            tracing::warn!(
                target: "photonic_video::session",
                %asset,
                scan = ?details.scan,
                "{msg}"
            );
        }
        tracing::info!(
            target: "photonic_video::session",
            %asset,
            scan = ?details.scan,
            "EngineCmd::Probe: media meta updated"
        );
    }

    /// Spawn a dedicated export worker (02 §7). The worker runs
    /// [`crate::export::job::run_export_job`] over the current frozen snapshot
    /// on its own thread + own headless session, publishing progress into
    /// `export_progress` (which `publish_status` mirrors onto
    /// [`EngineStatus::export`]). A prior in-flight export is poisoned first.
    fn start_export(&mut self, job: ExportJob) {
        // Poison any in-flight export so two never race the same output.
        if let Some(prev) = self.export_cancel.take() {
            prev.store(true, Ordering::Relaxed);
        }
        let Some(_) = self.snapshot.as_ref() else {
            self.fail(format!(
                "export requested for sequence {} but no timeline snapshot is loaded yet",
                job.sequence
            ));
            return;
        };
        let Some(tools) = self.tools.clone() else {
            self.fail("export requested but ffmpeg/ffprobe were not located".into());
            return;
        };
        let gpu = self.evaluator.gpu().clone();
        let snapshot = self.immutable_render_snapshot();
        self.export_job_counter += 1;
        let job_num = self.export_job_counter;

        let cancel = Arc::new(AtomicBool::new(false));
        self.export_cancel = Some(Arc::clone(&cancel));
        let progress = Arc::clone(&self.export_progress);
        // Seed a 0/0 running snapshot so the GUI shows the export immediately.
        progress.store(Some(Arc::new(ExportProgressSnapshot {
            job: job_num,
            frame: 0,
            total: 0,
            fps: 0.0,
            eta: Duration::ZERO,
            done: false,
            error: None,
        })));

        std::thread::Builder::new()
            .name("photonic-video-export".into())
            .spawn(move || {
                let prog = Arc::clone(&progress);
                let on_event = |event: crate::export::render_loop::ExportEvent| {
                    if let crate::export::render_loop::ExportEvent::Progress(p) = event {
                        prog.store(Some(Arc::new(ExportProgressSnapshot {
                            job: job_num,
                            frame: p.frame,
                            total: p.total,
                            fps: p.fps,
                            eta: p.eta,
                            done: false,
                            error: None,
                        })));
                    }
                };
                let result = crate::export::job::run_export_snapshot(
                    gpu, &snapshot, &job, &tools, &cancel, on_event,
                );
                // Final snapshot: carry the last frame/total, flip done, and
                // attach any error.
                let (frame, total) = progress
                    .load_full()
                    .map(|s| (s.frame, s.total))
                    .unwrap_or((0, 0));
                let error = match &result {
                    Ok(()) => None,
                    Err(e) => Some(e.to_string()),
                };
                progress.store(Some(Arc::new(ExportProgressSnapshot {
                    job: job_num,
                    frame,
                    total,
                    fps: 0.0,
                    eta: Duration::ZERO,
                    done: true,
                    error,
                })));
            })
            .expect("spawn photonic-video export worker");
    }

    /// Re-snapshot the timeline when the history revision moved. `try_lock`
    /// only — contention just means "reuse the last snapshot this tick".
    fn poll_snapshot(&mut self) {
        if let Some(published) = self.snapshot_input.load_full() {
            if published.generation == self.snapshot_generation {
                return;
            }
            self.snapshot_generation = published.generation;
            self.last_revision = Some(published.value.revision);
            self.snapshot = published.value.project.clone();
            if let Some(project) = &self.snapshot {
                self.media.set_project(Arc::clone(project));
                self.lut_cache.warm(project);
            } else {
                self.media.invalidate_all();
                self.media.project = None;
            }
            self.media
                .set_shared_document(published.value.document.clone(), published.value.revision);
            self.refresh_preview_snapshot();
            self.controller.request_present();
            return;
        }
        let summary = match self.history.try_lock() {
            Ok(h) => h.changes_since(self.last_revision.unwrap_or(0)),
            Err(_) => return,
        };
        if self.last_revision == Some(summary.revision) {
            return;
        }
        let Ok(doc) = self.doc.try_lock() else {
            return; // contended: keep playing off the previous snapshot
        };
        let snap = doc.timeline.as_ref().map(|p| Arc::new(p.clone()));
        drop(doc);
        // `summary.touched`/`overflowed` (vector NodeIds) is the targeted
        // vector-raster invalidation hook: when the session-level RasterVector
        // cache lands (02 §3 vector frames), touched nodes evict matching
        // `VectorStateKey` entries here. Clip/timeline edits already
        // invalidate hash-naturally via recompiled content hashes (02 §5).
        self.last_revision = Some(summary.revision);
        // Single Arc handoff to media + snapshot (was clone + move = 2 bumps).
        if let Some(ref p) = snap {
            self.media.set_project(Arc::clone(p));
            // Warm the LUT cache off the per-frame path (K-0.5): parse any newly
            // referenced `.cube` assets now so compile's provider read is a hit.
            self.lut_cache.warm(p);
        }
        self.snapshot = snap;
        self.refresh_preview_snapshot();
        self.controller.request_present();
    }

    fn immutable_render_snapshot(&self) -> RenderSnapshot {
        let document = self
            .snapshot_input
            .load_full()
            .and_then(|published| published.value.document.clone())
            .or_else(|| {
                self.doc
                    .try_lock()
                    .ok()
                    .map(|document| Arc::new(document.clone()))
            });
        RenderSnapshot {
            revision: self.last_revision.unwrap_or(0),
            project: self.snapshot.clone(),
            document,
        }
    }

    fn refresh_preview_snapshot(&mut self) {
        if self.preview.is_some() {
            let snapshot = Arc::new(self.immutable_render_snapshot());
            if let Some(preview) = self.preview.as_mut() {
                preview.set_snapshot(snapshot, self.snapshot_generation);
            }
        }
        self.publish_preview_status();
    }

    fn publish_preview_status(&mut self) {
        let mut status = self
            .preview
            .as_ref()
            .map(crate::preview::PreviewRuntime::status)
            .unwrap_or_default();
        status.doc_revision = self.last_revision.unwrap_or(0);
        status.snapshot_generation = self.snapshot_generation;
        status.profile = self.preview_profile;
        status.error = self.preview_error.clone().or(status.error);
        status.planning_ranges = status
            .planning_ranges
            .saturating_add(self.preview_actions.len());
        self.preview_status_out.store(Arc::new(status));
        self.preview_status_at = Instant::now();
    }

    fn start_preview(
        &mut self,
        sequence: SequenceId,
        range: Option<(Tick, Tick)>,
    ) -> Result<(), crate::preview::PreviewError> {
        self.preview_error = None;
        let seq = self
            .snapshot
            .as_ref()
            .and_then(|project| project.sequences.get(&sequence))
            .ok_or_else(|| {
                crate::preview::PreviewError::Invalid("preview sequence not found".into())
            })?;
        if range.is_none() && seq.preview_zones.is_empty() {
            return Err(crate::preview::PreviewError::Invalid(
                "no preview zones are marked".into(),
            ));
        }
        if let Some(preview) = self.preview.as_mut() {
            preview.set_profile(self.preview_profile);
            preview.set_playing(self.controller.is_playing());
            preview.request(sequence, range)?;
            self.publish_preview_status();
            return Ok(());
        }
        self.queue_preview_action(PendingPreviewAction::Render(sequence, range))
    }

    fn queue_preview_action(
        &mut self,
        action: PendingPreviewAction,
    ) -> Result<(), crate::preview::PreviewError> {
        if self.preview_actions.len() >= 128 {
            return Err(crate::preview::PreviewError::QueueFull);
        }
        if self.preview_loading.is_none() {
            let sequence = match &action {
                PendingPreviewAction::Render(id, _) | PendingPreviewAction::Clear(id, _) => *id,
            };
            let tools = self.tools.clone().ok_or_else(|| {
                crate::preview::PreviewError::Invalid("FFmpeg is unavailable".into())
            })?;
            let root = self.preview_directory.clone().unwrap_or_else(|| {
                std::env::temp_dir()
                    .join("photonic-preview-cache")
                    .join(sequence.to_string())
            });
            let gpu = self.evaluator.gpu().clone();
            let snapshot = Arc::new(self.immutable_render_snapshot());
            let generation = self.snapshot_generation;
            let epoch = self.preview_directory_epoch;
            // Reopening verifies bounded media/manifests on a loader thread;
            // a project with a populated disk cache cannot stall playback.
            self.preview_loading = Some(
                std::thread::Builder::new()
                    .name("photonic-preview-open".into())
                    .spawn(move || {
                        (
                            epoch,
                            crate::preview::PreviewRuntime::new(
                                gpu, tools, root, snapshot, generation,
                            ),
                        )
                    })?,
            );
        }
        self.preview_actions.push_back(action);
        self.publish_preview_status();
        Ok(())
    }

    fn poll_preview(&mut self) {
        if self
            .preview_loading
            .as_ref()
            .is_some_and(|loading| loading.is_finished())
        {
            let result = self.preview_loading.take().expect("finished loader").join();
            match result {
                Ok((epoch, _)) if epoch != self.preview_directory_epoch => {
                    let mut actions = std::mem::take(&mut self.preview_actions);
                    if let Some(action) = actions.pop_front() {
                        if let Err(error) = self.queue_preview_action(action) {
                            self.preview_error = Some(error.to_string());
                        }
                        self.preview_actions.extend(actions);
                    }
                    self.publish_preview_status();
                }
                Ok((_, Ok(mut preview))) => {
                    preview.set_snapshot(
                        Arc::new(self.immutable_render_snapshot()),
                        self.snapshot_generation,
                    );
                    preview.set_profile(self.preview_profile);
                    let mut error = None;
                    for action in self.preview_actions.drain(..) {
                        match action {
                            PendingPreviewAction::Render(sequence, range) => {
                                if let Err(failure) = preview.request(sequence, range) {
                                    error = Some(failure.to_string());
                                }
                            }
                            PendingPreviewAction::Clear(sequence, range) => {
                                preview.clear(sequence, range)
                            }
                        }
                    }
                    self.preview = Some(preview);
                    self.preview_error = error;
                    self.publish_preview_status();
                }
                result => {
                    self.preview_actions.clear();
                    self.preview_error = Some(match result {
                        Ok((_, Err(error))) => error.to_string(),
                        _ => "preview cache loader panicked".into(),
                    });
                    self.publish_preview_status();
                }
            }
        }
        let Some(preview) = self.preview.as_mut() else {
            return;
        };
        let playing = self.controller.is_playing()
            || self
                .source_audition
                .as_ref()
                .is_some_and(|source| source.status().playing);
        preview.set_playing(playing);
        if let Some(project) = self.snapshot.as_ref() {
            preview.poll(|context, tick| {
                Some(
                    compile_full(
                        project,
                        context.sequence,
                        context.format_index,
                        tick,
                        Quality {
                            proxy: context.use_proxy,
                        },
                        None,
                        Some(&self.lut_cache),
                        false,
                        Some(&self.deflicker_gains),
                        Some(&self.stabilization),
                    )
                    .graph,
                )
            });
        }
        if self.preview_status_at.elapsed() >= Duration::from_millis(100) {
            self.publish_preview_status();
        }
    }

    /// Cap on frames sampled per clip. A 4-minute 60 fps clip is ~14 000
    /// frames; measuring every one would mean 14 000 GPU renders and readbacks.
    /// Auto-exposure hunting lives around 0.5 Hz, so a few hundred samples
    /// spanning the clip resolve it comfortably, and [`ClipGains`] interpolates
    /// between them.
    ///
    /// [`ClipGains`]: crate::graph::deflicker::ClipGains
    const DEFLICKER_MAX_SAMPLES: usize = 240;

    /// Consecutive frames rendered for the rolling-band burst.
    const DEFLICKER_BURST_FRAMES: usize = 12;

    /// Resolution the measurement renders at. The statistic is a frame mean, so
    /// it converges long before full resolution — and this keeps a whole-clip
    /// analysis to a few hundred small renders instead of a few hundred 4K ones.
    const DEFLICKER_SAMPLE_W: u32 = 320;
    /// See [`Self::DEFLICKER_SAMPLE_W`].
    const DEFLICKER_SAMPLE_H: u32 = 180;

    /// Fingerprint the inputs a clip's measurement depends on. Changing the
    /// source, the trimmed range or any deflicker param re-measures; moving the
    /// clip on the timeline, renaming it or editing an unrelated effect does not.
    fn deflicker_fingerprint(clip: &Clip, fx: &photonic_core::timeline::ClipEffect) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        match &clip.source {
            ClipSource::Asset { asset } | ClipSource::Vector { asset } => asset.hash(&mut h),
            other => std::mem::discriminant(other).hash(&mut h),
        }
        clip.source_in.0.hash(&mut h);
        clip.duration.0.hash(&mut h);
        for (path, value) in &fx.params.base.entries {
            path.as_str().hash(&mut h);
            format!("{value:?}").hash(&mut h);
        }
        h.finish()
    }

    /// True when some clip carries an enabled deflicker whose measurement is
    /// missing or stale. Borrow-only and allocation-free, so it is cheap enough
    /// to run every present.
    fn deflicker_pending(&self, project: &TimelineProject, seq_id: SequenceId) -> bool {
        use photonic_core::timeline::EffectKind;
        let Some(seq) = project.sequences.get(&seq_id) else {
            return false;
        };
        seq.video_tracks.iter().any(|track| {
            track.clips.iter().any(|clip| {
                matches!(clip.source, ClipSource::Asset { .. })
                    && clip.effects.iter().any(|f| {
                        f.kind == EffectKind::Deflicker
                            && f.enabled
                            && !f.inert
                            && self.deflicker_fingerprints.get(&clip.id)
                                != Some(&Self::deflicker_fingerprint(clip, f))
                    })
            })
        })
    }

    /// Measure pending clips, taking the snapshot only when there is work.
    fn measure_deflicker_if_pending(&mut self) {
        let Some(project) = self.snapshot.as_ref() else {
            return;
        };
        let Some(seq_id) = self.effective_sequence(project) else {
            return;
        };
        if !self.deflicker_pending(project, seq_id) {
            return;
        }
        let project = Arc::clone(project);
        self.ensure_deflicker_measured(project.as_ref(), seq_id, 0);
    }

    /// Run the deflicker analysis for any clip that needs it (32 §2's two-pass
    /// rule: a job, not a node).
    ///
    /// Measurement renders the clip's **source asset** in isolation rather than
    /// the composite, so an overlapping clip on another track cannot pollute the
    /// curve. The deflicker effect itself is absent from that graph, so there is
    /// no feedback: the job always measures uncorrected content.
    fn ensure_deflicker_measured(
        &mut self,
        project: &TimelineProject,
        seq_id: SequenceId,
        _format_index: usize,
    ) {
        use photonic_core::timeline::EffectKind;

        let Some(seq) = project.sequences.get(&seq_id) else {
            return;
        };
        // Collect the work under the immutable borrow, run it after.
        struct Job {
            clip: ClipId,
            asset: AssetId,
            source_in: Tick,
            duration: Tick,
            fingerprint: u64,
            params: crate::graph::deflicker::DeflickerParams,
            /// Ticks per sequence frame — the sample grid. Derived from the
            /// sequence rate, not assumed, or `params.window` (documented in
            /// seconds) would be scaled by `fps / assumed_fps`.
            frame_ticks: i64,
            /// Sequence frame rate, needed to snap the band advance to mains.
            fps: f32,
            speed: photonic_core::timeline::SpeedMap,
        }
        let mut todo: Vec<Job> = Vec::new();
        for track in &seq.video_tracks {
            for clip in &track.clips {
                let Some(fx) = clip
                    .effects
                    .iter()
                    .find(|f| f.kind == EffectKind::Deflicker && f.enabled && !f.inert)
                else {
                    continue;
                };
                let ClipSource::Asset { asset } = clip.source else {
                    continue;
                };
                let fp = Self::deflicker_fingerprint(clip, fx);
                if self.deflicker_fingerprints.get(&clip.id) == Some(&fp) {
                    continue;
                }
                let fps = seq.frame_rate.num as f32 / seq.frame_rate.den.max(1) as f32;
                let f = |path: &str, dflt: f32| -> f32 {
                    match fx.params.base.get(path) {
                        Some(photonic_core::timeline::PropValue::Float(v)) => *v as f32,
                        _ => dflt,
                    }
                };
                let params = crate::graph::deflicker::params_for(
                    f("params.amount", 0.85),
                    f("params.window", 4.0),
                    f("params.max_change", 0.25),
                    f("params.chroma_amount", 0.0),
                    fps,
                );
                let frame_ticks = (photonic_core::timeline::TICKS_PER_SECOND
                    * seq.frame_rate.den.max(1) as i64)
                    / seq.frame_rate.num.max(1) as i64;
                todo.push(Job {
                    clip: clip.id,
                    asset,
                    source_in: clip.source_in,
                    duration: clip.duration,
                    fingerprint: fp,
                    params,
                    frame_ticks: frame_ticks.max(1),
                    fps,
                    speed: clip.speed.clone(),
                });
            }
        }

        // One clip per call: a project with several unmeasured clips makes
        // progress each present instead of freezing the render thread for the
        // sum of them. The remainder is picked up on the next frame.
        for job in todo.into_iter().take(1) {
            let Job {
                clip: clip_id,
                asset,
                source_in,
                duration,
                fingerprint: fp,
                mut params,
                frame_ticks: frame,
                fps: fps_for_band,
                speed,
            } = job;
            let total = (duration.0 / frame.max(1)).max(1) as usize;
            let stride = total.div_ceil(Self::DEFLICKER_MAX_SAMPLES).max(1);
            let step_ticks = frame * stride as i64;
            // The solver's window is in *samples*, so a strided measurement must
            // shrink it by the same factor or the baseline spans far longer than
            // the user asked for.
            params.window_frames = (params.window_frames / stride).max(1) | 1;
            params.smooth_frames = (params.smooth_frames / stride).max(1) | 1;

            let mut images = Vec::new();
            let mut t = 0i64;
            while t < duration.0 {
                // Map clip-relative `t` through the speed map: `ClipGains` is
                // indexed by clip-relative dt at compile time, so a retimed clip
                // must be *measured* at the source frames it actually shows or
                // the curve is misaligned with the rendered frames.
                let src = Tick(source_in.0 + speed.source_delta(Tick(t)).0);
                let compiled = compile_asset_peek(
                    project,
                    asset,
                    src,
                    Quality::PREVIEW,
                    Self::DEFLICKER_SAMPLE_W,
                    Self::DEFLICKER_SAMPLE_H,
                );
                if let Some(tex) = self.evaluator.evaluate(
                    &compiled.graph,
                    (Self::DEFLICKER_SAMPLE_W, Self::DEFLICKER_SAMPLE_H),
                    &mut self.media,
                ) {
                    let px = crate::graph::eval::read_texture_rgba16f(
                        self.evaluator.gpu(),
                        &tex,
                        Self::DEFLICKER_SAMPLE_W,
                        Self::DEFLICKER_SAMPLE_H,
                    );
                    images.push(crate::graph::ops::Image {
                        width: Self::DEFLICKER_SAMPLE_W,
                        height: Self::DEFLICKER_SAMPLE_H,
                        pixels: px,
                    });
                }
                t += step_ticks;
            }
            if images.is_empty() {
                // Record the fingerprint anyway. Without this the clip stays
                // "pending" and the whole 240-sample loop re-runs on every
                // present, forever, for a clip that cannot be measured.
                tracing::warn!(clip = ?clip_id, "deflicker: no frames decoded; leaving clip uncorrected");
                self.deflicker_fingerprints.insert(clip_id, fp);
                continue;
            }
            let gains = crate::graph::deflicker::measure_clip(&images, Tick(0), step_ticks, params);

            // Rolling bands need CONSECUTIVE frames — the strided pass above
            // aliases the per-frame phase advance into nonsense — so they get
            // their own short burst from the middle of the clip. One burst is
            // enough: the advance is set by the beat between the mains frequency
            // and the frame rate, and does not drift over a shot.
            let burst_start = (duration.0 / 2).max(0);
            let mut burst = Vec::new();
            for k in 0..Self::DEFLICKER_BURST_FRAMES {
                let t = burst_start + frame * k as i64;
                if t >= duration.0 {
                    break;
                }
                let src = Tick(source_in.0 + speed.source_delta(Tick(t)).0);
                let compiled = compile_asset_peek(
                    project,
                    asset,
                    src,
                    Quality::PREVIEW,
                    Self::DEFLICKER_SAMPLE_W,
                    Self::DEFLICKER_SAMPLE_H,
                );
                if let Some(tex) = self.evaluator.evaluate(
                    &compiled.graph,
                    (Self::DEFLICKER_SAMPLE_W, Self::DEFLICKER_SAMPLE_H),
                    &mut self.media,
                ) {
                    let px = crate::graph::eval::read_texture_rgba16f(
                        self.evaluator.gpu(),
                        &tex,
                        Self::DEFLICKER_SAMPLE_W,
                        Self::DEFLICKER_SAMPLE_H,
                    );
                    burst.push(crate::graph::rolling_bands::row_profile(
                        &crate::graph::ops::Image {
                            width: Self::DEFLICKER_SAMPLE_W,
                            height: Self::DEFLICKER_SAMPLE_H,
                            pixels: px,
                        },
                    ));
                }
            }
            // The burst starts mid-clip, so its frame index is the anchor; and the
            // advance is snapped to the mains value for this rate, which is what
            // makes extrapolating to the clip's ends safe.
            let anchor_frame = burst_start / frame.max(1);
            let band =
                crate::graph::rolling_bands::track_from_burst(&burst, fps_for_band, anchor_frame);
            if let Some(b) = band {
                tracing::info!(clip = ?clip_id, cycles = b.cycles, amplitude = b.amplitude,
                    dphase = b.dphase, "deflicker: rolling band detected");
            }
            let gains = gains.with_band(band, frame);
            tracing::info!(
                clip = ?clip_id,
                samples = images.len(),
                stride,
                "deflicker: measured clip"
            );
            self.deflicker_gains.insert(clip_id, gains);
            self.deflicker_fingerprints.insert(clip_id, fp);
        }
    }

    /// D-12: analyze `clip` and warm the stabilization cache (22 §6.4).
    ///
    /// Runs on the engine thread, off the UI. Geometry comes from the clip's
    /// own sequence format and frame rate, so the corrections are indexed at
    /// the rate the timeline will ask for them.
    ///
    /// A failure records a diagnostic and leaves the cache untouched, which
    /// means the clip keeps rendering from its unstabilized source rather than
    /// going black — the same posture an unresolvable LUT takes.
    fn run_stabilization_analysis(&mut self, clip_id: photonic_core::timeline::ClipId) {
        use crate::graph::stabilize::{analyze_clip, geometry_for_clip};

        let Some(project) = self.snapshot.clone() else {
            return;
        };
        // Locate the clip and the format it will be rendered at.
        let found = project.sequences.values().find_map(|seq| {
            seq.tracks()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .map(|c| (seq, c))
        });
        let Some((seq, clip)) = found else {
            self.stabilization
                .failures
                .push(format!("stabilization: clip {clip_id:?} not found"));
            return;
        };
        let Some(spec) = clip.stabilization.clone() else {
            self.stabilization.invalidate(clip_id);
            return;
        };
        let format = seq.format();
        // Exact rational rate, not a rounded float: at 23.976 the drift
        // between the two accumulates to a whole frame over a long clip, which
        // would misindex the corrections near the end.
        let rate = seq.frame_rate;
        let fps = rate.num as f64 / rate.den.max(1) as f64;
        let geom = geometry_for_clip(format.width as f64, format.height as f64, fps, clip);
        let analysis_key = stabilization_analysis_key(clip, &spec, &geom);

        // Sidecar paths are stored as the user gave them; project-relative
        // resolution is the caller's job, and the session is the caller.
        match analyze_clip(&spec, geom, |p| p.to_path_buf()) {
            Ok(analysis) => {
                self.stabilization
                    .insert(clip_id, analysis_key.clone(), analysis);
                self.mark_stabilization_analyzed(clip_id, &spec, analysis_key);
            }
            Err(e) => {
                self.stabilization
                    .failures
                    .push(format!("stabilization analysis failed: {e}"));
                self.stabilization.invalidate(clip_id);
            }
        }
    }

    /// Publish the generation-only analysis key to the shared document and the
    /// engine snapshot. It is skipped if the user changed the recipe while the
    /// analysis was running, so a late result can never mark a newer recipe as
    /// analyzed.
    fn mark_stabilization_analyzed(
        &mut self,
        clip_id: photonic_core::timeline::ClipId,
        analyzed_spec: &photonic_core::timeline::StabilizationSpec,
        key: String,
    ) {
        let mut doc = self.doc.lock().expect("engine document lock poisoned");
        let Some(project) = doc.timeline.as_mut() else {
            return;
        };
        let Some(clip) = project
            .sequences
            .values_mut()
            .flat_map(|seq| seq.tracks_for_mut(photonic_core::timeline::TrackKind::Video))
            .flat_map(|track| track.clips.iter_mut())
            .find(|clip| clip.id == clip_id)
        else {
            return;
        };
        let Some(spec) = clip.stabilization.as_mut() else {
            return;
        };
        if spec.binding != analyzed_spec.binding
            || spec.smoothness != analyzed_spec.smoothness
            || spec.horizon_lock != analyzed_spec.horizon_lock
            || spec.crop_mode != analyzed_spec.crop_mode
            || spec.max_zoom != analyzed_spec.max_zoom
        {
            return;
        }
        spec.analysis_key = Some(key);
        self.snapshot = Some(Arc::new(project.clone()));
    }

    fn effective_sequence(&self, project: &TimelineProject) -> Option<SequenceId> {
        self.active_sequence_override
            .filter(|id| project.sequences.contains_key(id))
            .or(project.active_sequence)
            .or_else(|| project.sequence_order.first().copied())
            .or_else(|| project.sequences.keys().next().copied())
    }

    fn present(&mut self) {
        // Deflicker is measured before the snapshot borrow below, because the
        // job needs `&mut self`. The pending check is a borrow-only scan, so the
        // steady state costs no Arc bump — the reason that borrow exists.
        self.measure_deflicker_if_pending();
        // Borrow the snapshot Arc — no atomic bump every tick (was `.clone()`).
        let Some(project) = self.snapshot.as_ref() else {
            return;
        };
        let Some(seq_id) = self.effective_sequence(project) else {
            return;
        };
        let Some(seq) = project.sequences.get(&seq_id) else {
            return;
        };
        self.controller.set_rate(seq.frame_rate);

        match self.controller.tick() {
            PresentDecision::Hold => {}
            PresentDecision::LoopWrap(start) => {
                let was_playing = self.controller.is_playing();
                self.stop_playing();
                self.controller.seek(start);
                if was_playing {
                    self.start_playing();
                }
            }
            PresentDecision::Present(t) => {
                let evaluate_started = Instant::now();
                let quality = self.interactive_quality();
                let format_index = seq.active_format.min(seq.formats.len().saturating_sub(1));
                self.media.set_playing(
                    self.controller.is_playing()
                        || self
                            .source_audition
                            .as_ref()
                            .is_some_and(|a| a.status().playing),
                );
                self.media.set_scrubbing(self.scrubbing);

                // Normalize nil sequence target once we know the active seq.
                if let PreviewTarget::Sequence { sequence } = &self.preview_target {
                    if *sequence == SequenceId::nil() || *sequence != seq_id {
                        self.preview_target = PreviewTarget::Sequence { sequence: seq_id };
                    }
                }

                let (compiled, canvas, frame_time, preview_asset) = match &self.preview_target {
                    PreviewTarget::Asset { asset, source_time }
                        if !self.controller.is_playing() =>
                    {
                        let (fw, fh) = seq
                            .formats
                            .get(format_index)
                            .map(|f| (f.width, f.height))
                            .unwrap_or((1920, 1080));
                        let canvas = self.preview_canvas(fw, fh);
                        let compiled = compile_asset_peek(
                            project.as_ref(),
                            *asset,
                            *source_time,
                            quality,
                            canvas.0,
                            canvas.1,
                        );
                        (compiled, canvas, *source_time, Some(*asset))
                    }
                    _ => {
                        // Thread the pre-warmed LUT cache so `Grade` `Lut3d` ops
                        // resolve to real tables (K-0.5); parse-failure diagnostics
                        // ride along on the compiled frame.
                        let mut compiled = compile_full(
                            project.as_ref(),
                            seq_id,
                            format_index,
                            t,
                            quality,
                            None,
                            Some(&self.lut_cache),
                            false,
                            Some(&self.deflicker_gains),
                            Some(&self.stabilization),
                        );
                        compiled
                            .diagnostics
                            .extend(self.lut_cache.failures.iter().cloned());
                        let (fw, fh) = seq
                            .formats
                            .get(format_index)
                            .map(|f| (f.width, f.height))
                            .unwrap_or((1, 1));
                        let canvas = self.preview_canvas(fw, fh);
                        (compiled, canvas, t, None)
                    }
                };

                // Gate the document snapshot on actual vector use: only when the
                // compiled frame references a `RasterVector` op do we clone the
                // document for the offscreen vector rasterizer, so a project with
                // no embedded-vector clips never pays the clone.
                if compiled
                    .graph
                    .nodes
                    .iter()
                    .any(|n| matches!(n.op, IrOp::RasterVector { .. }))
                {
                    if self.snapshot_generation == 0 {
                        if let Ok(doc) = self.doc.try_lock() {
                            self.media
                                .set_document(&doc, self.last_revision.unwrap_or(0));
                        }
                    }
                }
                // K-E2: resolve the requested scope tap against THIS frame's
                // graph. `resolve_tap` applies the 13 §10.2 fallback (a clip the
                // playhead is not over → the program tap), and reports which
                // point survived so the frame can be labelled honestly. An asset
                // peek carries no taps at all — it is not a sequence render.
                let (tap_point, tap_node) = match compiled.resolve_tap(self.scope_tap) {
                    Some((point, node)) => (point, Some(node)),
                    None => (ScopeTapPoint::Program, None),
                };
                self.media.begin_frame();
                let cached = if self.controller.is_playing()
                    && self.preview_quality == PreviewQuality::Full
                    && self.proxy_mode == ProxyMode::ForceOriginal
                    && self.inspection_request_id.is_none()
                    && preview_asset.is_none()
                    && !self.compare_effects
                    && !self.scrubbing
                    && tap_point == ScopeTapPoint::Program
                    && !compiled
                        .graph
                        .nodes
                        .iter()
                        .any(|node| matches!(node.op, IrOp::CaptionOverlay { .. }))
                {
                    self.preview.as_mut().and_then(|preview| {
                        preview.frame(seq_id, format_index, frame_time, &compiled.graph)
                    })
                } else {
                    None
                };
                let cached_preview = cached.is_some();
                let (evaluated_frame, tap_tex) = if let Some(frame) = cached {
                    let tap = Some(frame.clone());
                    (Some(frame), tap)
                } else {
                    self.evaluations = self.evaluations.saturating_add(1);
                    self.evaluator.evaluate_with_tap_frame(
                        &compiled.graph,
                        canvas,
                        &mut self.media,
                        tap_node,
                    )
                };
                let logical_size = evaluated_frame
                    .as_ref()
                    .map(|frame| (frame.width, frame.height));
                let frame_tex = evaluated_frame.map(|frame| frame.texture);
                // K-B5: second compile with clip looks skipped. Upstream source
                // nodes share content hashes with the full compile so the node
                // cache pays only the look-stack delta.
                let compare_clean = if self.compare_effects && preview_asset.is_none() {
                    let clean = compile_with_providers(
                        project.as_ref(),
                        seq_id,
                        format_index,
                        t,
                        quality,
                        None,
                        Some(&self.lut_cache),
                        Some(&self.stabilization),
                        true,
                    );
                    self.evaluations = self.evaluations.saturating_add(1);
                    self.evaluator
                        .evaluate(&clean.graph, canvas, &mut self.media)
                } else {
                    None
                };
                let evaluation_missed = frame_tex.is_none();
                if let Some(texture) = frame_tex {
                    self.frame_out.store(Some(Arc::new(EngineFrame {
                        cached_preview,
                        inspection_request_id: self.inspection_request_id,
                        content_hash: compiled
                            .graph
                            .output
                            .map(|id| compiled.graph.nodes[id.0 as usize].content_hash)
                            .expect("published frame has graph output"),
                        snapshot_generation: self.snapshot_generation,
                        doc_revision: self.last_revision.unwrap_or(0),
                        preview_quality: self.preview_quality,
                        proxy_mode: self.proxy_mode,
                        texture,
                        logical_size: logical_size.expect("frame texture has logical size"),
                        time: frame_time,
                        sequence: seq_id,
                        preview_asset,
                        scope_tap: tap_tex,
                        scope_tap_point: tap_point,
                        compare_clean,
                    })));
                    self.frames_published = self.frames_published.saturating_add(1);
                    self.buffering = false;
                } else {
                    self.evaluation_misses = self.evaluation_misses.saturating_add(1);
                    self.buffering = true;
                    self.controller.request_present();
                }
                let evaluate_elapsed = evaluate_started.elapsed();
                self.last_evaluate_micros =
                    u64::try_from(evaluate_elapsed.as_micros()).unwrap_or(u64::MAX);
                let proxy_changed = observe_adaptive_proxy(
                    &mut self.adaptive_proxy,
                    self.preview_quality,
                    self.proxy_mode,
                    seq.frame_rate,
                    evaluate_elapsed,
                    evaluation_missed,
                );
                if proxy_changed {
                    // The quality bit participates in graph source identity.
                    // Request a present so the new source selection takes
                    // effect without waiting for the next playback clock edge.
                    self.controller.request_present();
                }
                // Ring prefetch now runs off the engine thread: each evaluated
                // `DecodeVideo` node steers its source's background decode
                // worker (see `MediaSources::video_texture`), so decode overlaps
                // compositing instead of pumping inline here.
                //
                // Cut-ahead: warm the *next* clip's decode worker before the
                // playhead reaches it, so crossing a clip boundary hits a primed
                // ring instead of cold-starting (transparent flicker). Only while
                // playing — a paused/scrubbing playhead has no imminent cut.
                if self.controller.is_playing() {
                    // E-1: cut-ahead lead is max(fixed cut-ahead, graph source-range
                    // window) so temporal nodes (deinterlace …) expand decode warm.
                    let cut = Tick(seq.frame_rate.ticks_per_frame().0 * CUT_AHEAD_LEAD_FRAMES);
                    let graph_range =
                        crate::graph::source_range::graph_source_range(&compiled.graph, t);
                    let lead =
                        crate::playback::prefetch::combined_prefetch_lead(cut, graph_range, t);
                    self.media.prefetch_upcoming(seq, t, lead, quality);
                }
            }
        }
    }

    /// Proxy flag from session `ProxyMode` + Draft/Full (24 §4).
    fn interactive_quality(&self) -> Quality {
        let proxy = match (self.preview_quality, self.proxy_mode) {
            (PreviewQuality::Full, _) => false,
            (_, ProxyMode::ForceOriginal) => false,
            (_, ProxyMode::ForceProxy) => true,
            (_, ProxyMode::Auto) => self.adaptive_proxy.choice() == PreviewMediaChoice::Proxy,
        };
        Quality { proxy }
    }

    /// Draft caps long edge at [`DRAFT_MAX_LONG_EDGE`]; Full uses format size.
    fn preview_canvas(&self, w: u32, h: u32) -> (u32, u32) {
        match self.preview_quality {
            PreviewQuality::Draft => fit_long_edge(w, h, DRAFT_MAX_LONG_EDGE),
            PreviewQuality::Full => (w.max(1), h.max(1)),
        }
    }

    fn start_playing(&mut self) {
        if let Some(preview) = &self.preview {
            preview.set_playing(true);
        }
        if self.controller.is_playing() {
            return;
        }
        let start = self.controller.playhead();
        let (producer, consumer, xruns) = audio_ring();
        match self.audio.start(consumer) {
            Ok(sample_rate) => {
                self.xruns = Some(xruns);
                // Feed the ring from the snapshot's audio tracks; without a
                // snapshot/sequence the ring stays empty and the callback
                // emits silence (the master clock still advances).
                if let Some(project) = self.snapshot.clone() {
                    if let Some(seq) = self.effective_sequence(&project) {
                        self.feeder = Some(spawn_audio_feeder(
                            project,
                            seq,
                            start,
                            sample_rate,
                            producer,
                            self.tools.clone(),
                            Arc::clone(&self.master_meter),
                            Arc::clone(&self.spectrum_db),
                            Arc::clone(&self.graph_latency),
                        ));
                    }
                }
                self.controller.play_audio(self.audio.clock());
            }
            Err(err) => {
                // No device (headless/CI) ⇒ soft clock (02 §4 paused/scrub
                // clock doubles as the device-less playback clock).
                tracing::debug!(
                    target: "photonic_video::session",
                    "audio device unavailable ({err}); playing on soft clock"
                );
                self.controller.play_soft();
            }
        }
    }

    fn stop_playing(&mut self) {
        self.controller.pause();
        self.feeder = None; // Drop stops + joins the feeder thread
        self.audio.stop();
        // Clear live meter / latency so the GUI settles to silence (G-4).
        self.master_meter.store(None);
        self.spectrum_db.store(None);
        self.graph_latency.store(0, Ordering::Relaxed);
    }

    fn publish_status(&mut self) {
        let memory = self
            .media
            .memory_status(self.evaluator.cache_stats().resident_bytes);
        let readiness = self.media.readiness_status();
        let source_audition = self.source_audition.as_ref().map(|a| a.status());
        let playhead = self.controller.playhead();
        let playing = self.controller.is_playing();
        let dropped = self.controller.dropped();
        let audio_xruns = self
            .xruns
            .as_ref()
            .map(|x| x.underrun_frames())
            .unwrap_or(0);
        let doc_revision = self.last_revision.unwrap_or(0);
        let active_sequence = self
            .snapshot
            .as_ref()
            .and_then(|p| self.effective_sequence(p));
        let export = self.export_progress.load_full();
        let master_level = self.master_meter.load_full().map(|m| MasterMeterSnapshot {
            peak: m.peak(),
            rms: m.rms(),
        });
        let spectrum_db = self.spectrum_db.load_full().map(|v| (*v).clone());
        let graph_latency_samples = self.graph_latency.load(Ordering::Relaxed);
        // Cheap signature: skip Arc allocation when the GUI-visible fields are
        // unchanged (idle paused loops were allocating status every 100 ms).
        let sig = {
            let mut h = playhead.0 as u64;
            h = h.wrapping_mul(0x9E37_79B9).wrapping_add(playing as u64);
            h = h.wrapping_mul(0x9E37_79B9).wrapping_add(dropped);
            h = h.wrapping_mul(0x9E37_79B9).wrapping_add(audio_xruns);
            h = h
                .wrapping_mul(0x9E37_79B9)
                .wrapping_add(self.frames_published);
            h = h.wrapping_mul(0x9E37_79B9).wrapping_add(self.evaluations);
            h = h
                .wrapping_mul(0x9E37_79B9)
                .wrapping_add(self.evaluation_misses);
            h = h.wrapping_mul(0x9E37_79B9).wrapping_add(doc_revision);
            h = h
                .wrapping_mul(0x9E37_79B9)
                .wrapping_add(self.buffering as u64);
            h = h
                .wrapping_mul(0x9E37_79B9)
                .wrapping_add(match self.preview_quality {
                    PreviewQuality::Draft => 0,
                    PreviewQuality::Full => 1,
                });
            // Fold export progress so a background worker's advance still
            // republishes status even when every interactive field is idle.
            if let Some(e) = export.as_ref() {
                h = h.wrapping_mul(0x9E37_79B9).wrapping_add(e.job);
                h = h.wrapping_mul(0x9E37_79B9).wrapping_add(e.frame);
                h = h.wrapping_mul(0x9E37_79B9).wrapping_add(e.total);
                h = h.wrapping_mul(0x9E37_79B9).wrapping_add(e.done as u64);
                h = h
                    .wrapping_mul(0x9E37_79B9)
                    .wrapping_add(e.error.is_some() as u64);
            }
            // Fold meter peak so the GUI re-samples while audio is live (G-4).
            if let Some(m) = master_level {
                h = h
                    .wrapping_mul(0x9E37_79B9)
                    .wrapping_add(m.peak[0].to_bits() as u64);
                h = h
                    .wrapping_mul(0x9E37_79B9)
                    .wrapping_add(m.peak[1].to_bits() as u64);
            }
            h = h
                .wrapping_mul(0x9E37_79B9)
                .wrapping_add(graph_latency_samples as u64);
            // K-E2: fold the requested tap so a tap change republishes status
            // even on a paused, otherwise-idle playhead — the GUI compares this
            // echo against its own choice to decide whether to resend.
            h = h
                .wrapping_mul(0x9E37_79B9)
                .wrapping_add(match self.scope_tap {
                    ScopeTapPoint::Program => 0,
                    ScopeTapPoint::Clip(id) => 1 ^ (id.0.as_u128() as u64),
                });
            h
        };
        let command_admission = self
            .mailbox
            .as_ref()
            .map(|m| m.status())
            .unwrap_or_default();
        let sig = sig
            .wrapping_mul(0x9E37_79B9)
            .wrapping_add(self.snapshot_generation)
            .wrapping_add(command_admission.pending as u64)
            .wrapping_add(command_admission.coalesced)
            .wrapping_add(command_admission.rejected)
            .wrapping_add(memory.decoded_ring_bytes)
            .wrapping_add(memory.gpu_cache_bytes)
            .wrapping_add(readiness.pending_source_builds as u64)
            .wrapping_add(readiness.pending_rasters as u64)
            .wrapping_add(source_audition.as_ref().map_or(0, |a| a.playhead.0 as u64))
            .wrapping_add(self.source_audition_error.is_some() as u64);
        if sig == self.last_status_sig && self.last_error.is_none() {
            // Still refresh playhead while playing (sig includes playhead).
            // When identical, skip the Arc::new + store.
            return;
        }
        self.last_status_sig = sig;
        self.status_out.store(Arc::new(EngineStatus {
            memory,
            readiness,
            source_audition,
            source_audition_error: self.source_audition_error.clone(),
            command_admission,
            snapshot_generation: self.snapshot_generation,
            playhead,
            playing,
            dropped,
            cache: self.evaluator.cache_stats(),
            audio_xruns,
            frames_published: self.frames_published,
            evaluations: self.evaluations,
            last_evaluate_micros: self.last_evaluate_micros,
            evaluation_misses: self.evaluation_misses,
            doc_revision,
            active_sequence,
            last_error: self.last_error.clone(),
            preview_target: self.preview_target.clone(),
            preview_quality: self.preview_quality,
            buffering: self.buffering,
            export: export.as_ref().map(|e| (**e).clone()),
            master_level,
            spectrum_db,
            graph_latency_samples,
            scope_tap: self.scope_tap,
        }));
    }
}

/// Feed one completed present into Auto proxy hysteresis. Evaluation time is
/// deliberately the end-to-end engine work rather than a sum of decode and
/// evaluation timings: the background decoder overlaps compositing, so adding
/// their durations would double-count work and select proxies too aggressively.
/// Decode-input resolution independently verifies that a selected proxy is
/// Ready and on disk, otherwise it falls back to the original.
fn observe_adaptive_proxy(
    policy: &mut AdaptiveProxyPolicy,
    preview_quality: PreviewQuality,
    proxy_mode: ProxyMode,
    frame_rate: FrameRate,
    evaluate_time: Duration,
    evaluation_missed: bool,
) -> bool {
    if preview_quality != PreviewQuality::Draft || proxy_mode != ProxyMode::Auto {
        policy.reset();
        return false;
    }

    let before = policy.choice();
    let frame_budget = ticks_to_duration(frame_rate.ticks_per_frame());
    // `resolve_decode_input` below is the final per-asset readiness check.
    // Supplying `true` here lets one session-level policy prepare the next
    // source while retaining that no-proxy fallback at the media boundary.
    let after = policy.observe(
        PreviewPressure {
            evaluate_time,
            frame_budget,
            missed_frames: u32::from(evaluation_missed),
            ..PreviewPressure::default()
        },
        true,
    );
    after != before
}

/// Convert the timeline's integer tick domain into a wall-clock duration.
///
/// Timeline ticks are deliberately much finer than microseconds
/// ([`TICKS_PER_SECOND`] is 705,600,000), so treating ticks as microseconds
/// would inflate a 30fps preview budget from about 33ms to 23.52 seconds and
/// permanently suppress Auto proxy pressure detection.
fn ticks_to_duration(ticks: Tick) -> Duration {
    let ticks = u64::try_from(ticks.0.max(1)).unwrap_or(u64::MAX);
    let ticks_per_second = TICKS_PER_SECOND as u64;
    let seconds = ticks / ticks_per_second;
    // The remainder is strictly below 705.6M, so multiplying by 1e9 cannot
    // overflow u64. Truncation is less than one nanosecond and intentionally
    // conservative for the frame-budget comparison.
    let nanos = (ticks % ticks_per_second) * 1_000_000_000 / ticks_per_second;
    Duration::new(seconds, nanos as u32)
}

// ── Diagnostic counters (decode ring-hit vs inline-seek) ─────────────────────
// Temporary throughput instrumentation: lets a headless bench distinguish
// decoded-frame-ring hits from ring misses/held frames. `INLINE_SEEKS` keeps
// its historic name for public benchmark compatibility; the engine no longer
// performs inline seeks on a miss.
pub static RING_HITS: AtomicU64 = AtomicU64::new(0);
pub static INLINE_SEEKS: AtomicU64 = AtomicU64::new(0);

/// `(ring_hits, decode_misses)` since process start.
pub fn decode_stats() -> (u64, u64) {
    (
        RING_HITS.load(Ordering::Relaxed),
        INLINE_SEEKS.load(Ordering::Relaxed),
    )
}

/// Increment a diagnostic counter without allowing a long-running process to
/// wrap a monotonic total back to zero.
fn saturating_increment(counter: &AtomicU64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(1))
    });
}

fn stabilization_analysis_key(
    clip: &Clip,
    spec: &photonic_core::timeline::StabilizationSpec,
    geom: &crate::graph::stabilize::ClipGeometry,
) -> String {
    use std::hash::{Hash, Hasher};

    let mut h = std::collections::hash_map::DefaultHasher::new();
    clip.source_in.0.hash(&mut h);
    clip.duration.0.hash(&mut h);
    format!("{clip:?}").hash(&mut h);
    format!("{spec:?}").hash(&mut h);
    geom.source_start_s.to_bits().hash(&mut h);
    geom.source_end_s.to_bits().hash(&mut h);
    geom.fps.to_bits().hash(&mut h);
    format!("stabilization:{:016x}", h.finish())
}

// ── Media sources (GpuFrameSource over decode rings) ─────────────────────────

/// Cache identity for a video input. The boolean records the *resolved* input,
/// not a graph's proxy request: a requested proxy that is not ready on disk is
/// the same original decode/upload as an explicit original request.
type VideoSourceKey = (AssetId, bool);

struct ResolvedVideoInput {
    key: VideoSourceKey,
    path: PathBuf,
}

struct VideoSourceEntry {
    /// The source's decode ring. The engine only ever reads it (a ring hit
    /// never touches the decoder). The `DecodeSource` itself is owned by the
    /// background [`DecodeWorker`], which is the sole seeker/pumper — the engine
    /// never decodes inline, so a slow GOP decode cannot stall the compositor.
    ring: SharedRing,
    /// Background decode thread; dropped (stopped + joined) with the entry,
    /// which drops the last `DecodeSource` handle and kills the sidecar.
    worker: DecodeWorker,
    colorimetry: Colorimetry,
    rate: FrameRate,
    /// Monotonic use-stamp for LRU eviction (bumped on every steer/read).
    last_used: u64,
}

/// Resolves `DecodeVideo` ops to working textures over per-asset ffmpeg
/// sidecar decode rings (02 §3). Stills (`DecodeStill` via
/// `RasterImage::from_encoded`, downscaled to the requested logical size and
/// cached by `(asset, size)` — 26 K-C8, see [`crate::media::stills`]) are wired
/// here too. Vector frames (`RasterVector` via `HeadlessRenderer`, cached by
/// `VectorStateKey`) prepare pixels on bounded workers during playback/scrub.
struct MediaSources {
    tools: Option<FfmpegTools>,
    project: Option<Arc<TimelineProject>>,
    /// `None` value = open failed; don't re-probe every frame (an
    /// `InvalidateRange` clears the entry and allows a retry).
    sources: HashMap<VideoSourceKey, Option<VideoSourceEntry>>,
    /// Sources whose expensive open (ffprobe + keyframe/pts index build, each a
    /// subprocess) is running on a background thread, so the engine present loop
    /// never stalls on a cold source. `drain_pending` promotes the completed
    /// build into `sources`; until then the source reads as absent and the
    /// compositor holds the last frame / shows transparent for a few presents.
    pending: HashMap<VideoSourceKey, std::sync::mpsc::Receiver<Option<VideoSourceEntry>>>,
    source_builds: Arc<AtomicU64>,
    upload_aliases: HashMap<UploadKey, UploadKey>,
    capacity_limited: bool,
    sources_requested: usize,
    sources_ready: usize,
    /// Uploaded working textures keyed by decoded pts — scrub back/forward
    /// over the same frames skips the GPU upload. LRU eviction preserves hot
    /// nearby frames when a timeline exceeds the bounded texture budget.
    uploads: UploadCache<GpuFrame>,
    /// Decoded still images uploaded to the working format, cached by
    /// `(asset, logical size)` (26 K-C8). A still's pixels never change until
    /// relink/`InvalidateRange`, but the size it is wanted at does — Draft vs
    /// Full preview, and a different sequence format.
    stills: StillCache<GpuFrame>,
    converter: Option<YuvConverter>,
    /// Whether the engine is playing — set each present before evaluate. Selects
    /// readiness policy: nonblocking while playing, exact wait while paused.
    playing: bool,
    /// Whether the playhead is being dragged — set each present. Enables the
    /// cheap keyframe-preview decode path + hold-last-frame between previews.
    scrubbing: bool,
    /// Monotonic counter for `VideoSourceEntry::last_used` (LRU eviction).
    use_counter: u64,
    /// Cloned document snapshot for rasterizing embedded-vector clips. Only
    /// populated (in `present`) when the compiled frame actually references a
    /// vector — non-vector projects never pay the clone.
    document: Option<Arc<Document>>,
    /// The history revision `document` was snapshotted at (avoid re-cloning).
    doc_revision: Option<u64>,
    /// Rasterized vector frames, cached by `VectorStateKey` (a vector only
    /// re-renders when its doc-state key changes).
    vectors: UploadCache<GpuFrame, VectorStateKey>,
    /// Lazily-created offscreen renderer for vector rasterization (its own wgpu
    /// device; frames come back as CPU bytes and re-upload onto the shared
    /// `GpuContext`, so there is no cross-device texture handoff).
    headless: Arc<Mutex<Option<HeadlessRenderer>>>,
    raster_jobs: HashMap<RasterKey, std::sync::mpsc::Receiver<Option<PreparedRaster>>>,
    raster_work: Arc<AtomicU64>,
    raster_epoch: Arc<AtomicU64>,
    raster_failed: HashSet<RasterKey>,
}

/// Upload-cache entry cap: ~a ring's worth per couple of assets.
const UPLOAD_CACHE_CAP: usize = 32;

type UploadKey = (AssetId, Tick, bool);

struct UploadCacheEntry<T> {
    value: T,
    last_used: u64,
    bytes: u64,
}

/// Small bounded LRU for decoded frames already converted into working-format
/// GPU textures. A wholesale clear creates a noticeable re-upload burst at the
/// cap; evicting exactly one cold entry keeps scrubbing and A/B playback warm.
struct UploadCache<T, K = UploadKey> {
    entries: HashMap<K, UploadCacheEntry<T>>,
    cap: usize,
    budget_bytes: u64,
}

impl<T, K: Copy + Eq + std::hash::Hash> UploadCache<T, K> {
    fn new(cap: usize) -> Self {
        Self {
            entries: HashMap::new(),
            cap: cap.max(1),
            budget_bytes: u64::MAX,
        }
    }

    fn with_byte_budget(mut self, bytes: u64) -> Self {
        self.budget_bytes = bytes;
        self
    }

    fn resident_bytes(&self) -> u64 {
        self.entries.values().map(|entry| entry.bytes).sum()
    }

    fn get(&mut self, key: &K, stamp: u64) -> Option<&T> {
        let entry = self.entries.get_mut(key)?;
        entry.last_used = stamp;
        Some(&entry.value)
    }

    #[cfg(test)]
    fn insert(&mut self, key: K, value: T, stamp: u64) {
        self.insert_sized(key, value, stamp, 0);
    }

    fn insert_sized(&mut self, key: K, value: T, stamp: u64, bytes: u64) {
        self.entries.remove(&key);
        if bytes > self.budget_bytes {
            return;
        }
        while self.entries.len() >= self.cap
            || self.resident_bytes().saturating_add(bytes) > self.budget_bytes
        {
            let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            self.entries.remove(&victim);
        }
        self.entries.insert(
            key,
            UploadCacheEntry {
                value,
                last_used: stamp,
                bytes,
            },
        );
    }

    fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    fn contains_key(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }
}

impl<T> UploadCache<T> {
    fn remove_assets(&mut self, assets: &HashSet<AssetId>) {
        self.entries
            .retain(|(asset, _, _), _| !assets.contains(asset));
    }
}

/// Per-session cache budgets. Other sessions, externally retained textures,
/// audio/FFmpeg internals and temporary renderer allocations are separate.
pub const SESSION_GPU_CACHE_BYTES: u64 = 1536 * 1024 * 1024;
const UPLOAD_CACHE_BYTES: u64 = 256 * 1024 * 1024;
const STILL_CACHE_BYTES: u64 = 128 * 1024 * 1024;
const VECTOR_CACHE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SOURCE_BUILDS: usize = 2;

const MAX_RASTER_JOBS: usize = 2;
/// Maximum combined native RGBA and packed output bytes of one raster job.
const RASTER_JOB_BYTES: u64 = 128 * 1024 * 1024;
const RASTER_ENCODED_BYTES: u64 = 64 * 1024 * 1024;
#[derive(Clone, Copy, Eq, PartialEq, Hash)]
enum RasterKey {
    Still(AssetId, u32, u32),
    Vector(VectorStateKey),
}
enum RasterTask {
    Still(PathBuf, (u32, u32)),
    Vector(Arc<Document>, u32, u32),
}
struct PreparedRaster {
    native: (u32, u32),
    width: u32,
    height: u32,
    texels: Vec<u8>,
}

fn prepare_raster(
    task: RasterTask,
    headless: &Mutex<Option<HeadlessRenderer>>,
) -> Option<PreparedRaster> {
    let (image, requested) = match task {
        RasterTask::Still(path, requested) => {
            if std::fs::metadata(&path).ok()?.len() > RASTER_ENCODED_BYTES {
                return None;
            }
            let mut reader = image::ImageReader::open(&path)
                .ok()?
                .with_guessed_format()
                .ok()?;
            let native = image::ImageReader::open(&path)
                .ok()?
                .with_guessed_format()
                .ok()?
                .into_dimensions()
                .ok()?;
            let target = still_target_size(native, requested);
            let needed = u64::from(native.0) * u64::from(native.1) * 4
                + u64::from(target.0) * u64::from(target.1) * 8;
            if needed > RASTER_JOB_BYTES {
                return None;
            }
            let mut limits = image::Limits::default();
            limits.max_alloc = Some(RASTER_JOB_BYTES);
            reader.limits(limits);
            let rgba = reader.decode().ok()?.into_rgba8();
            let image =
                photonic_core::RasterImage::from_rgba(rgba.width(), rgba.height(), rgba.into_raw())
                    .ok()?;
            (image, target)
        }
        RasterTask::Vector(document, w, h) => {
            if w == 0 || h == 0 || u64::from(w) * u64::from(h) * 12 > RASTER_JOB_BYTES {
                return None;
            }
            let mut renderer = headless.lock().unwrap_or_else(|e| e.into_inner());
            let renderer =
                renderer.get_or_insert_with(|| pollster::block_on(HeadlessRenderer::new()));
            let options = ExportOptions {
                background: ExportBackground::Transparent,
                ..Default::default()
            };
            let (bytes, rw, rh) = renderer.render_rgba_with_opts(&document, w, h, &options);
            (
                photonic_core::RasterImage::from_rgba(rw, rh, bytes).ok()?,
                (w, h),
            )
        }
    };
    Some(prepare_pixels(&image, requested.0, requested.1))
}

fn prepare_pixels(image: &photonic_core::RasterImage, width: u32, height: u32) -> PreparedRaster {
    let width = width.clamp(1, image.width.max(1));
    let height = height.clamp(1, image.height.max(1));
    let mut texels = Vec::with_capacity(width as usize * height as usize * 8);
    resample_linear_premult(image, width, height, |pixel| {
        for channel in pixel {
            texels.extend_from_slice(&f32_to_f16_bits(channel).to_le_bytes());
        }
    });
    PreparedRaster {
        native: (image.width, image.height),
        width,
        height,
        texels,
    }
}

struct WorkPermit(Arc<AtomicU64>);
impl WorkPermit {
    fn acquire(active: &Arc<AtomicU64>, limit: u64) -> Option<Self> {
        active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < limit).then_some(count + 1)
            })
            .ok()?;
        Some(Self(Arc::clone(active)))
    }
}
impl Drop for WorkPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

fn gpu_frame_bytes(frame: &GpuFrame) -> u64 {
    u64::from(frame.texture.width()) * u64::from(frame.texture.height()) * 8
}

/// Still-image texture cache cap, counted in `(asset, size)` entries (26 K-C8);
/// wholesale clear on overflow (each entry is cheap to redecode from disk).
const STILL_CACHE_CAP: usize = 16;

/// Rasterized-vector texture cache cap; wholesale clear on overflow.
const VECTOR_CACHE_CAP: usize = 16;

// How far ahead (in frames) to warm the next clip's decoder before a cut.
// ~0.8s at 30fps — enough lead for a keyframe-seek bootstrap to finish.
// Cut-ahead lead lives in `playback::prefetch::CUT_AHEAD_LEAD_FRAMES`.

// Cap on live decode sources (each = a worker thread + ffmpeg sidecar). With
// cut-ahead, a long timeline would otherwise accumulate them; evict the
// least-recently-used beyond this, never one on screen this frame.
// MAX_LIVE_SOURCES re-exported from playback::prefetch.

/// Assets whose decoded bytes could differ between two project snapshots.
///
/// Exactly three things change what an asset decodes to: its source (a relink),
/// its content hash (the bytes behind an unchanged path changed), and its proxy
/// (attach, detach, or a re-transcode, since the engine decodes the proxy when
/// one is ready). Everything else on `MediaAsset` — name, bin, rating, tags,
/// probe — is metadata, and evicting a warm decode source because someone
/// starred a clip would be a real performance regression.
///
/// An asset that is new in `new` is not reported: nothing is cached under an id
/// that has never been requested. An asset that disappeared is not reported
/// either — its entries are unreachable rather than wrong, and removal is the
/// cache budget's business, not correctness'.
fn changed_media_identities(old: &TimelineProject, new: &TimelineProject) -> HashSet<AssetId> {
    let mut changed = HashSet::new();
    for (id, new_asset) in &new.media.assets {
        let Some(old_asset) = old.media.assets.get(id) else {
            continue;
        };
        if old_asset.source != new_asset.source
            || old_asset.content_hash != new_asset.content_hash
            || old_asset.proxy != new_asset.proxy
        {
            changed.insert(*id);
        }
    }
    changed
}

impl MediaSources {
    fn new(tools: Option<FfmpegTools>) -> Self {
        MediaSources {
            tools,
            project: None,
            sources: HashMap::new(),
            pending: HashMap::new(),
            source_builds: Arc::new(AtomicU64::new(0)),
            upload_aliases: HashMap::new(),
            capacity_limited: false,
            sources_requested: 0,
            sources_ready: 0,
            document: None,
            doc_revision: None,
            vectors: UploadCache::new(VECTOR_CACHE_CAP).with_byte_budget(VECTOR_CACHE_BYTES),
            headless: Arc::new(Mutex::new(None)),
            raster_jobs: HashMap::new(),
            raster_work: Arc::new(AtomicU64::new(0)),
            raster_epoch: Arc::new(AtomicU64::new(0)),
            raster_failed: HashSet::new(),
            uploads: UploadCache::new(UPLOAD_CACHE_CAP).with_byte_budget(UPLOAD_CACHE_BYTES),
            stills: StillCache::new(STILL_CACHE_CAP).with_byte_budget(STILL_CACHE_BYTES),
            converter: None,
            playing: false,
            scrubbing: false,
            use_counter: 0,
        }
    }

    /// Adopt a new project snapshot, evicting any cached media whose identity
    /// changed.
    ///
    /// This eviction is load-bearing, not tidiness. Decode sources, uploads and
    /// the still cache are all keyed on `AssetId` — K-C8 added the requested
    /// SIZE to the still key, not the asset's identity — so a relink or a proxy
    /// swap leaves every one of them pointing at the previous file's bytes.
    /// Swapping the snapshot alone would keep serving the old picture for the
    /// rest of the session, which is invisible until export.
    ///
    /// `EngineCmd::InvalidateRange` is documented (02 §5) as the relink/proxy
    /// seam, but it has no senders anywhere in the workspace, and
    /// `invalidate_assets` is reached only from `EngineCmd::Probe`. Diffing here
    /// means correctness does not depend on a caller remembering to announce the
    /// change — the same reason [`Self::set_document`] clears the vector cache
    /// on its own rather than waiting to be told.
    fn set_project(&mut self, project: Arc<TimelineProject>) {
        if let Some(old) = self.project.as_ref() {
            if Arc::ptr_eq(old, &project) {
                return;
            }
            let changed = changed_media_identities(old, &project);
            self.invalidate_assets(&changed);
        }
        self.project = Some(project);
    }

    fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
    }

    fn set_scrubbing(&mut self, scrubbing: bool) {
        self.scrubbing = scrubbing;
    }

    /// Provide the document snapshot for vector rasterization at `revision`.
    /// Re-clones only when the revision moved; clears the rasterized-vector cache
    /// on a document change so stale vector frames don't linger.
    fn set_shared_document(&mut self, document: Option<Arc<Document>>, revision: u64) {
        let same_document = match (&self.document, &document) {
            (Some(old), Some(new)) => Arc::ptr_eq(old, new),
            (None, None) => true,
            _ => false,
        };
        if self.doc_revision != Some(revision) || !same_document {
            self.vectors.clear();
            self.raster_jobs.clear();
            self.raster_failed.clear();
            self.raster_epoch.fetch_add(1, Ordering::AcqRel);
        }
        self.document = document;
        self.doc_revision = Some(revision);
    }

    fn set_document(&mut self, doc: &Document, revision: u64) {
        if self.doc_revision == Some(revision) && self.document.is_some() {
            return;
        }
        self.document = Some(Arc::new(doc.clone()));
        self.doc_revision = Some(revision);
        self.vectors.clear();
        self.raster_jobs.clear();
        self.raster_failed.clear();
        self.raster_epoch.fetch_add(1, Ordering::AcqRel);
    }

    fn next_use_stamp(&mut self) -> u64 {
        self.use_counter += 1;
        self.use_counter
    }

    /// Whether `asset` is a file-backed video (worth building a decode source).
    fn is_video_asset(&self, asset: AssetId) -> bool {
        self.project
            .as_ref()
            .and_then(|p| p.media.assets.get(&asset))
            .is_some_and(|a| {
                a.kind == AssetKind::Video && matches!(a.source, AssetSource::File { .. })
            })
    }

    /// Cut-ahead: warm the next clip's decode worker if it starts within `lead`
    /// of `t` ([`cut_ahead_targets`]). Bounded to **one source request per call**
    /// so we never open dual always-on decoders for every upcoming cut. Playing
    /// requests are queued through `ensure_source`; the expensive probe and index
    /// build must not run on the present thread.
    fn prefetch_upcoming(&mut self, seq: &Sequence, t: Tick, lead: Tick, quality: Quality) {
        self.drain_pending();
        let mut requested_this_call = false;
        let targets = cut_ahead_targets(seq, t, lead);
        for target in targets {
            let asset = target.asset;
            let src_time = target.source_time;
            if !self.is_video_asset(asset) {
                continue;
            }
            let Some(input) = self.resolve_video_input(asset, quality.proxy) else {
                continue;
            };
            let key = input.key;
            if !self.sources.contains_key(&key) && !self.pending.contains_key(&key) {
                if requested_this_call {
                    continue; // amortize builds across presents
                }
                self.ensure_source(input);
                requested_this_call = true;
            }
            let stamp = self.next_use_stamp();
            if let Some(Some(entry)) = self.sources.get_mut(&key) {
                entry.last_used = stamp;
                // Only steer while the ring hasn't yet reached the target — once
                // the bootstrap seek has primed it, leave it alone so it doesn't
                // fight the real playback steer at the cut.
                if entry.ring.frame_covering(src_time).is_none() {
                    entry.worker.steer(src_time);
                }
            }
        }
        self.evict_stale();
    }

    /// Drop the least-recently-used sources beyond `MAX_LIVE_SOURCES`. Never
    /// evicts a source touched this present (highest `last_used`), so an
    /// on-screen worker is never killed. Dropping stops+joins its worker.
    fn evict_stale(&mut self) {
        let entries: Vec<_> = self
            .sources
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|e| (*k, e.last_used)))
            .collect();
        if entries.len() <= MAX_LIVE_SOURCES {
            return;
        }
        // Protect anything touched on the latest use stamp (this present).
        let protected_min = entries.iter().map(|(_, u)| *u).max().unwrap_or(0);
        let victims = lru_eviction_victims(&entries, MAX_LIVE_SOURCES, protected_min);
        for k in victims {
            self.sources.remove(&k);
        }
        // If still over (all entries protected), fall back to pure LRU without protect.
        if self.sources.values().filter(|v| v.is_some()).count() > MAX_LIVE_SOURCES {
            let entries: Vec<_> = self
                .sources
                .iter()
                .filter_map(|(k, v)| v.as_ref().map(|e| (*k, e.last_used)))
                .collect();
            for k in lru_eviction_victims(&entries, MAX_LIVE_SOURCES, u64::MAX) {
                self.sources.remove(&k);
            }
        }
    }

    fn invalidate_all(&mut self) {
        self.sources.clear();
        self.pending.clear();
        self.uploads.clear();
        self.upload_aliases.clear();
        self.stills.clear();
        self.vectors.clear();
        self.raster_jobs.clear();
        self.raster_failed.clear();
        self.raster_epoch.fetch_add(1, Ordering::AcqRel);
        self.document = None;
        self.doc_revision = None;
    }

    /// Targeted eviction: drop only the decode sources, uploads, and stills
    /// belonging to `assets` (relink/proxy-swap of specific media). Unrelated
    /// sidecars keep their primed rings, so a relink of one asset never cold-
    /// restarts every other source on the timeline.
    fn invalidate_assets(&mut self, assets: &HashSet<AssetId>) {
        if assets.is_empty() {
            return;
        }
        self.sources.retain(|(asset, _), _| !assets.contains(asset));
        self.pending.retain(|(asset, _), _| !assets.contains(asset));
        self.uploads.remove_assets(assets);
        self.upload_aliases
            .retain(|(asset, _, _), _| !assets.contains(asset));
        self.stills.remove_assets(assets);
        self.raster_jobs.clear();
        self.raster_failed.clear();
        self.raster_epoch.fetch_add(1, Ordering::AcqRel);
    }

    /// Promote any background source builds that have finished into `sources`.
    /// Called each present before the source map is read.
    fn drain_pending(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        use std::sync::mpsc::TryRecvError;
        let mut done: Vec<((AssetId, bool), Option<VideoSourceEntry>)> = Vec::new();
        self.pending.retain(|key, rx| match rx.try_recv() {
            Ok(entry) => {
                done.push((*key, entry));
                false
            }
            // Build thread panicked/dropped without sending: record as failed.
            Err(TryRecvError::Disconnected) => {
                done.push((*key, None));
                false
            }
            Err(TryRecvError::Empty) => true,
        });
        for (key, entry) in done {
            self.sources.insert(key, entry);
        }
    }

    /// Kick off (or no-op) the open of `(asset, proxy)`. The cheap path
    /// resolution runs here on the engine thread; the expensive probe + keyframe/
    /// pts index build (ffprobe/ffmpeg subprocesses) runs on a background thread
    /// so the present loop never stalls. The result lands in `sources` on a later
    /// `drain_pending`.
    fn ensure_source(&mut self, input: ResolvedVideoInput) {
        let key = input.key;
        if self.sources.contains_key(&key) || self.pending.contains_key(&key) {
            return;
        }
        // Paused (e.g. a scrub/seek/step): build synchronously so the exact frame
        // shows immediately with no transparent flash — there are no playback
        // frames to drop, so the one-time cold-open cost is harmless and the
        // seek-shows-exact-frame contract holds. Only *playing* opens go
        // off-thread (below), where a present-loop stall would drop frames; and
        // cut-ahead pre-builds the next clip off-thread before it is on screen.
        if !self.playing && !self.scrubbing {
            self.make_source_room();
            let entry = self.build_source(input.path);
            self.sources.insert(key, entry);
            return;
        }
        let Some(permit) = WorkPermit::acquire(&self.source_builds, MAX_SOURCE_BUILDS as u64)
        else {
            self.capacity_limited = true;
            return;
        };
        self.make_source_room();
        let Some(tools) = self.tools.clone() else {
            self.sources.insert(key, None);
            return;
        };
        let path = input.path;
        let (tx, rx) = std::sync::mpsc::channel();
        let path_for_thread = path;
        let spawned = std::thread::Builder::new()
            .name("photonic-source-build".into())
            .spawn(move || {
                let _permit = permit;
                let _ = tx.send(build_source_entry(tools, path_for_thread));
            });
        match spawned {
            Ok(_) => {
                self.pending.insert(key, rx);
            }
            Err(_) => {
                self.sources.insert(key, None);
            }
        }
    }

    fn prepare_raster(&mut self, key: RasterKey, task: RasterTask) -> Option<PreparedRaster> {
        if self.raster_failed.contains(&key) {
            return None;
        }
        if let Some(rx) = self.raster_jobs.get(&key) {
            match rx.try_recv() {
                Ok(result) => {
                    self.raster_jobs.remove(&key);
                    if result.is_none() && self.raster_failed.len() < 128 {
                        self.raster_failed.insert(key);
                    }
                    return result;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.raster_jobs.remove(&key);
                    return None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return None,
            }
        }
        if !self.playing && !self.scrubbing {
            let result = prepare_raster(task, &self.headless);
            if result.is_none() && self.raster_failed.len() < 128 {
                self.raster_failed.insert(key);
            }
            return result;
        }
        if self.raster_jobs.len() >= MAX_RASTER_JOBS {
            self.capacity_limited = true;
            return None;
        }
        let Some(permit) = WorkPermit::acquire(&self.raster_work, MAX_RASTER_JOBS as u64) else {
            self.capacity_limited = true;
            return None;
        };
        let headless = Arc::clone(&self.headless);
        let epoch = Arc::clone(&self.raster_epoch);
        let requested_epoch = epoch.load(Ordering::Acquire);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        if std::thread::Builder::new()
            .name("photonic-raster-prepare".into())
            .spawn(move || {
                let _permit = permit;
                if epoch.load(Ordering::Acquire) != requested_epoch {
                    return;
                }
                let result = prepare_raster(task, &headless);
                if epoch.load(Ordering::Acquire) == requested_epoch {
                    let _ = tx.send(result);
                }
            })
            .is_ok()
        {
            self.raster_jobs.insert(key, rx);
        }
        None
    }

    fn make_source_room(&mut self) {
        let cap = MAX_LIVE_SOURCES.saturating_sub(self.pending.len()).max(1);
        while self.sources.len() >= cap {
            let Some(victim) = self
                .sources
                .iter()
                .min_by_key(|(_, entry)| entry.as_ref().map_or(0, |entry| entry.last_used))
                .map(|(key, _)| *key)
            else {
                break;
            };
            self.sources.remove(&victim);
        }
    }

    fn readiness_status(&self) -> SourceReadinessStatus {
        SourceReadinessStatus {
            requested: self.sources_requested,
            ready: self.sources_ready,
            pending_source_builds: self.source_builds.load(Ordering::Acquire) as usize,
            pending_rasters: self.raster_work.load(Ordering::Acquire) as usize,
            capacity_limited: self.capacity_limited,
            failed_sources: self
                .sources
                .values()
                .filter(|source| source.is_none())
                .count()
                + self.raster_failed.len(),
        }
    }
    fn memory_status(&self, graph_bytes: u64) -> EngineMemoryStatus {
        let decoded_ring_bytes = self
            .sources
            .values()
            .filter_map(|entry| entry.as_ref())
            .map(|entry| entry.ring.resident_bytes())
            .sum();
        let upload_bytes = self.uploads.resident_bytes();
        let still_bytes = self.stills.resident_bytes();
        let vector_bytes = self.vectors.resident_bytes();
        let gpu_cache_bytes = graph_bytes + upload_bytes + still_bytes + vector_bytes;
        let decoded_ring_budget_bytes =
            MAX_LIVE_SOURCES as u64 * crate::decode::ring::DEFAULT_RING_BYTES;
        EngineMemoryStatus {
            decoded_ring_bytes,
            decoded_ring_budget_bytes,
            upload_bytes,
            still_bytes,
            vector_bytes,
            graph_bytes,
            gpu_cache_bytes,
            gpu_cache_budget_bytes: SESSION_GPU_CACHE_BYTES,
            raster_jobs: self.raster_work.load(Ordering::Acquire) as usize,
            raster_job_byte_limit: RASTER_JOB_BYTES,
            pressure: self.capacity_limited
                || decoded_ring_bytes > decoded_ring_budget_bytes
                || gpu_cache_bytes > SESSION_GPU_CACHE_BYTES,
        }
    }

    fn begin_frame(&mut self) {
        self.sources_requested = 0;
        self.sources_ready = 0;
        self.capacity_limited = false;
        self.next_use_stamp();
        self.drain_pending();
    }

    /// Resolve an asset's requested input once, then use its actual selection
    /// for every cache and worker key. A missing on-disk proxy deliberately
    /// canonicalizes to `(asset, false)`, avoiding a second original decoder and
    /// upload-cache entry when Auto requests proxies globally.
    fn resolve_video_input(
        &self,
        asset: AssetId,
        requested_proxy: bool,
    ) -> Option<ResolvedVideoInput> {
        let project = self.project.as_ref()?;
        let media_asset = project.media.assets.get(&asset)?;
        let original = match &media_asset.source {
            AssetSource::File { path, .. } => path,
            // Embedded vectors go through RasterVector, never DecodeVideo.
            _ => return None,
        };
        let path = crate::media::proxy::resolve_decode_input(
            original,
            media_asset.proxy.as_ref(),
            requested_proxy,
        );
        let resolved_proxy = requested_proxy && path != *original;
        Some(ResolvedVideoInput {
            key: (asset, resolved_proxy),
            path,
        })
    }

    /// Synchronous open (paused path + spawn-failure fallback) for a previously
    /// resolved input. The playing path uses the same path off-thread via
    /// `ensure_source`.
    fn build_source(&self, path: PathBuf) -> Option<VideoSourceEntry> {
        let tools = self.tools.clone()?;
        build_source_entry(tools, path)
    }
}

/// The expensive part of opening a decode source — ffprobe + keyframe/pts index
/// build (subprocess-heavy) + worker spawn. Takes owned, `Send` inputs so it can
/// run on a background thread off the engine present loop (see
/// `MediaSources::ensure_source`). GPU-free: the first frame's upload happens
/// later in `video_texture` with the shared `GpuContext`.
fn build_source_entry(tools: FfmpegTools, path: std::path::PathBuf) -> Option<VideoSourceEntry> {
    let details = probe_details(&tools, &path).ok()?;
    let video = details.probe.video.clone()?;
    let keyframes = KeyframeIndex::build(&tools, &path).ok()?;
    let pts_kind = if details.is_vfr {
        PtsKind::Vfr(Arc::new(PtsIndex::build(&tools, &path).ok()?))
    } else {
        PtsKind::Cfr(video.frame_rate)
    };
    let params = SourceParams {
        input: path,
        width: video.width,
        height: video.height,
        pix_fmt: PixFmt::for_alpha(details.has_alpha),
        pts_kind,
        keyframes,
    };
    let ring = SharedRing::preview();
    let decode = Arc::new(Mutex::new(DecodeSource::new(tools, params, ring.clone())));
    let worker = DecodeWorker::spawn(decode, video.frame_rate);
    Some(VideoSourceEntry {
        ring,
        worker,
        colorimetry: colorimetry_for_probe(&details),
        rate: video.frame_rate,
        last_used: 0,
    })
}

impl GpuFrameSource for MediaSources {
    fn cache_namespace(&self) -> u64 {
        let document = if self.document.is_some() {
            self.raster_epoch
                .load(Ordering::Acquire)
                .wrapping_add(1)
                .wrapping_mul(0x9e3779b97f4a7c15)
        } else {
            0
        };
        if self.scrubbing {
            document ^ self.use_counter.wrapping_add(1).max(1)
        } else {
            document
        }
    }
    fn video_texture(
        &mut self,
        gpu: &GpuContext,
        asset: AssetId,
        src_time: Tick,
        proxy: bool,
    ) -> Option<GpuFrame> {
        self.sources_requested += 1;
        self.drain_pending();
        let input = self.resolve_video_input(asset, proxy)?;
        let requested_key = (asset, src_time, input.key.1);
        if !self.scrubbing {
            if let Some(key) = self.upload_aliases.get(&requested_key) {
                if let Some(frame) = self.uploads.get(key, self.use_counter) {
                    self.sources_ready += 1;
                    return Some(frame.clone());
                }
            }
        }
        let source_key = input.key;
        self.ensure_source(input);
        let playing = self.playing;
        let scrubbing = self.scrubbing;
        let stamp = self.next_use_stamp();
        // A source still building in the background reads as absent here → the
        // caller composites transparent / holds the last frame for a few presents
        // until `drain_pending` promotes it. No engine-thread stall.
        let entry = self.sources.get_mut(&source_key)?.as_mut()?;
        entry.last_used = stamp;
        let colorimetry = entry.colorimetry;

        // Steer the background decode worker off the engine thread (decode ∥
        // composite). Scrub mode decodes only a cheap keyframe preview; normal
        // mode keeps the ring pumped ahead. The engine never seeks inline, so a
        // slow GOP decode can never stall the compositor one frame at a time.
        if scrubbing {
            entry.worker.steer_scrub(src_time);
        } else {
            entry.worker.steer(src_time);
        }

        // Ring-hit acceptance. Normally require the covering frame within one
        // frame tick (a discontinuous seek must not serve an old frame; Step
        // must never re-serve the previous one). While scrubbing, accept any
        // resident frame at/before src_time — the keyframe preview may be up to
        // a GOP stale, which is the intended cheap-scrub tradeoff.
        let tolerance = entry.rate.ticks_per_frame().0;
        let within =
            |f: &Arc<crate::decode::DecodedFrame>| scrubbing || src_time.0 - f.pts.0 < tolerance;

        let ready = if playing || scrubbing {
            entry.ring.try_frame_covering(src_time).filter(within)
        } else {
            entry
                .ring
                .frame_covering(src_time)
                .filter(within)
                .or_else(|| {
                    entry
                        .ring
                        .wait_for_frame(src_time, Duration::from_millis(500))
                        .filter(within)
                })
        };
        let Some(frame) = ready else {
            saturating_increment(&INLINE_SEEKS);
            return None;
        };
        saturating_increment(&RING_HITS);

        // Upload (cached by decoded pts) and record as the new last-good frame.
        let key = (asset, frame.pts, source_key.1);
        let texture = if let Some(cached) = self.uploads.get(&key, stamp) {
            cached.clone()
        } else {
            let converter = self
                .converter
                .get_or_insert_with(|| YuvConverter::new(gpu.device()));
            let (width, height) = frame.planes.dims();
            let bucket = crate::graph::ir::TextureDesc { width, height }.bucket();
            let converted = converter.convert_to_size(
                gpu.device(),
                gpu.queue(),
                &frame.planes.as_yuv_planes(),
                colorimetry,
                bucket,
            );
            // Convert directly into the evaluator's physical pool bucket. The
            // `GpuFrame` keeps source dimensions logical, so the padded margin
            // never participates in sampling.
            let texture = GpuFrame::new(Arc::new(converted), width, height);
            self.uploads
                .insert_sized(key, texture.clone(), stamp, gpu_frame_bytes(&texture));
            texture
        };
        if !scrubbing {
            if self.upload_aliases.len() >= 256 {
                self.upload_aliases.clear();
            }
            self.upload_aliases.insert(requested_key, key);
        }
        self.sources_ready += 1;
        Some(texture)
    }

    fn still_texture(
        &mut self,
        gpu: &GpuContext,
        asset: AssetId,
        req_w: u32,
        req_h: u32,
    ) -> Option<GpuFrame> {
        // DecodeStill: decode the encoded image asset, resample it to the size
        // the evaluator asked for, upload it into the working format, and cache
        // it on `(asset, that size)` (02 §3, 26 K-C8). `req_w`/`req_h` are the
        // LOGICAL canvas size — already preview-scaled, never a pool bucket —
        // so a Draft canvas uploads a Draft-sized still instead of the full
        // 6000 px original. An `InvalidateRange` touching the asset drops every
        // size of it and forces a redecode on relink.
        self.sources_requested += 1;
        let requested = (req_w.max(1), req_h.max(1));
        if let Some(frame) = self.stills.get(asset, requested) {
            self.sources_ready += 1;
            return Some(frame.clone());
        }
        let project = self.project.as_ref()?;
        let media_asset = project.media.assets.get(&asset)?;
        // Only file-backed stills; embedded vectors go through RasterVector.
        let AssetSource::File { path, .. } = &media_asset.source else {
            return None;
        };
        let key = RasterKey::Still(asset, requested.0, requested.1);
        let task = RasterTask::Still(path.clone(), requested);
        let prepared = self.prepare_raster(key, task)?;
        let frame = upload_prepared(gpu, &prepared);
        self.stills
            .insert(asset, prepared.native, requested, frame.clone());
        self.sources_ready += 1;
        Some(frame)
    }

    fn vector_texture(
        &mut self,
        gpu: &GpuContext,
        vref: VectorRef,
        key: VectorStateKey,
        w: u32,
        h: u32,
    ) -> Option<GpuFrame> {
        self.sources_requested += 1;
        // Cache hit: a vector only re-renders when its doc-state key changes.
        let stamp = self.next_use_stamp();
        if let Some(frame) = self.vectors.get(&key, stamp) {
            self.sources_ready += 1;
            return Some(frame.clone());
        }
        // Whole-document embedded vectors are rendered fully. Sub-references
        // (a specific artboard / node) need scene-subset extraction — a
        // follow-up; they evaluate transparent until then.
        if !matches!(vref, VectorRef::WholeDocument) {
            return None;
        }
        let document = self.document.clone()?;
        let prepared =
            self.prepare_raster(RasterKey::Vector(key), RasterTask::Vector(document, w, h))?;
        let frame = upload_prepared(gpu, &prepared);
        self.sources_ready += 1;
        self.vectors
            .insert_sized(key, frame.clone(), stamp, gpu_frame_bytes(&frame));
        Some(frame)
    }
}

/// Pad a source upload to the texture pool's 64px size bucket (03 §3.4).
///
/// Source uploads share the pool's physical bucket convention, while
/// [`GpuFrame`] carries the native logical width and height separately. The
/// evaluator normalizes that logical region to the canvas with explicit texel
/// loads, so padding never participates in sampling.
///
/// The padded texture keeps `COPY_SRC`, like the unpadded upload it copies from,
/// so what was *actually* uploaded stays readable — otherwise a source upload
/// small enough to need padding (a Draft-scale still, K-C8) becomes the one case
/// no diagnostic or test can inspect.
fn pad_to_pool_bucket(gpu: &GpuContext, src: wgpu::Texture) -> wgpu::Texture {
    let (w, h) = (src.width(), src.height());
    let bucket = crate::graph::ir::TextureDesc {
        width: w,
        height: h,
    }
    .bucket();
    if bucket == (w, h) {
        return src;
    }
    let padded = gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("video_upload_bucket_padded"),
        size: wgpu::Extent3d {
            width: bucket.0,
            height: bucket.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let mut enc = gpu
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("video_upload_pad"),
        });
    enc.copy_texture_to_texture(
        src.as_image_copy(),
        padded.as_image_copy(),
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue().submit([enc.finish()]);
    padded
}

fn upload_prepared(gpu: &GpuContext, prepared: &PreparedRaster) -> GpuFrame {
    let (w, h) = (prepared.width, prepared.height);
    let texture = gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("still_upload"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        // COPY_SRC so `pad_to_pool_bucket` can blit into a larger bucket.
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    gpu.queue().write_texture(
        texture.as_image_copy(),
        &prepared.texels,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(w * 8), // 4 channels × 2 bytes (f16)
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    GpuFrame::new(Arc::new(pad_to_pool_bucket(gpu, texture)), w, h)
}

/// IEEE-754 binary32 → binary16 bit pattern (round-toward-zero). Matches the
/// in-tree `photonic_render` f16 packers; sufficient for the [0, 1]-ish working
/// range (subnormals flush to signed zero).
fn f32_to_f16_bits(v: f32) -> u16 {
    let b = v.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let e = ((b >> 23) & 0xff) as i32 - 112; // 127 - 15
    let m = b & 0x7fffff;
    if e <= 0 {
        sign
    } else if e >= 0x1f {
        sign | 0x7c00
    } else {
        sign | ((e as u16) << 10) | ((m >> 13) as u16)
    }
}

// ── Audio feeder (mixer worker, 02 §1 / 09 §5) ───────────────────────────────

/// Handle to the mixer worker thread; dropping stops + joins it.
pub(crate) struct AudioFeeder {
    stop: Arc<AtomicBool>,
    prefill: Arc<AtomicU8>,
    join: Option<JoinHandle<()>>,
}

impl AudioFeeder {
    /// 0 preparing, 1 first bounded block ready, 2 source decode failed.
    pub(crate) fn prefill_state(&self) -> u8 {
        self.prefill.load(Ordering::Acquire)
    }
    #[cfg(test)]
    pub(crate) fn has_finished(&self) -> bool {
        self.join.as_ref().is_none_or(|join| join.is_finished())
    }
}

impl Drop for AudioFeeder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Spawn the mixer worker: renders [`BLOCK_FRAMES`]-frame blocks from the
/// snapshot's audio tracks (voices resolved per block per 09 §4's seam) into
/// the lock-free ring the cpal callback drains.
pub(crate) fn spawn_audio_feeder(
    project: Arc<TimelineProject>,
    sequence: SequenceId,
    start: Tick,
    sample_rate: u32,
    producer: RingProducer,
    tools: Option<FfmpegTools>,
    master_meter: Arc<ArcSwapOption<crate::audio::mixer::StereoMeter>>,
    spectrum_db: Arc<ArcSwapOption<Vec<f32>>>,
    graph_latency: Arc<std::sync::atomic::AtomicU32>,
) -> AudioFeeder {
    spawn_audio_feeder_inner(
        project,
        sequence,
        start,
        sample_rate,
        producer,
        tools,
        master_meter,
        spectrum_db,
        graph_latency,
        None,
    )
}

pub(crate) fn spawn_source_audio_feeder(
    project: Arc<TimelineProject>,
    sequence: SequenceId,
    start: Tick,
    sample_rate: u32,
    producer: RingProducer,
    tools: Option<FfmpegTools>,
    master_meter: Arc<ArcSwapOption<crate::audio::mixer::StereoMeter>>,
    spectrum_db: Arc<ArcSwapOption<Vec<f32>>>,
    graph_latency: Arc<std::sync::atomic::AtomicU32>,
    end: Tick,
) -> AudioFeeder {
    spawn_audio_feeder_inner(
        project,
        sequence,
        start,
        sample_rate,
        producer,
        tools,
        master_meter,
        spectrum_db,
        graph_latency,
        Some(end),
    )
}

fn spawn_audio_feeder_inner(
    project: Arc<TimelineProject>,
    sequence: SequenceId,
    start: Tick,
    sample_rate: u32,
    producer: RingProducer,
    tools: Option<FfmpegTools>,
    master_meter: Arc<ArcSwapOption<crate::audio::mixer::StereoMeter>>,
    spectrum_db: Arc<ArcSwapOption<Vec<f32>>>,
    graph_latency: Arc<std::sync::atomic::AtomicU32>,
    output_end: Option<Tick>,
) -> AudioFeeder {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);
    let prefill = Arc::new(AtomicU8::new(0));
    let prefill_worker = Arc::clone(&prefill);
    let join = std::thread::Builder::new()
        .name("photonic-video-mixer".into())
        .spawn(move || {
            feeder_main(
                project,
                sequence,
                start,
                sample_rate,
                producer,
                tools,
                stop_flag,
                master_meter,
                spectrum_db,
                graph_latency,
                output_end,
                prefill_worker,
            )
        })
        .expect("spawn photonic-video mixer thread");
    AudioFeeder {
        stop,
        prefill,
        join: Some(join),
    }
}

fn feeder_main(
    project: Arc<TimelineProject>,
    sequence: SequenceId,
    start: Tick,
    sample_rate: u32,
    mut producer: RingProducer,
    tools: Option<FfmpegTools>,
    stop: Arc<AtomicBool>,
    master_meter: Arc<ArcSwapOption<crate::audio::mixer::StereoMeter>>,
    spectrum_db: Arc<ArcSwapOption<Vec<f32>>>,
    graph_latency: Arc<std::sync::atomic::AtomicU32>,
    output_end: Option<Tick>,
    prefill: Arc<AtomicU8>,
) {
    let sample_rate = sample_rate.max(1);
    let block_ticks =
        Tick(((BLOCK_FRAMES as i128 * TICKS_PER_SECOND as i128) / sample_rate as i128) as i64);
    let mut out = vec![0f32; BLOCK_FRAMES * CHANNELS];
    let mut t = start;

    let Some(seq) = project.sequences.get(&sequence) else {
        // No sequence: keep the ring fed with silence so the callback (and
        // master clock) run smoothly.
        while !stop.load(Ordering::Relaxed) {
            if output_end.is_some_and(|end| t >= end) {
                break;
            }
            if producer.is_full() {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            producer.push_block(&out);
        }
        return;
    };

    let mut mixer = Mixer::new(sample_rate);
    if output_end.is_some() {
        mixer.set_declick(crate::audio::mixer::DeclickConfig {
            enabled: false,
            ..Default::default()
        });
    }
    // G-4: publish the live output meter so EngineStatus can sample it.
    master_meter.store(Some(mixer.output_meter()));
    let default_clip_audio = ClipAudio::new();
    // Persistent per-clip PCM sidecars: opened when a clip becomes audible
    // (seeked to its mapped source position), read sequentially block after
    // block, dropped when the clip stops sounding.
    let mut pcm: HashMap<ClipId, Box<dyn PcmSource>> = HashMap::new();

    while !stop.load(Ordering::Relaxed) {
        if output_end.is_some_and(|end| t >= end) {
            break;
        }
        if producer.is_full() {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }

        // Which clips sound at t? Audio tracks only in P3 — video-clip
        // embedded audio arrives with the linked-clips story.
        let active: Vec<(&photonic_core::timeline::Track, &Clip)> = seq
            .audio_tracks
            .iter()
            .filter(|track| track.enabled && track.audio.is_some())
            .flat_map(|track| {
                track
                    .clips
                    .iter()
                    .filter(|clip| {
                        clip.enabled
                            && clip.start <= t
                            && t < clip.end()
                            && matches!(clip.source, ClipSource::Asset { .. })
                    })
                    .map(move |clip| (track, clip))
            })
            .collect();

        // Open sidecars for newly-audible clips; drop finished ones.
        if let Some(tools) = tools.as_ref() {
            for (_, clip) in &active {
                if pcm.contains_key(&clip.id) {
                    continue;
                }
                let ClipSource::Asset { asset } = clip.source else {
                    continue;
                };
                let Some(AssetSource::File { path, .. }) =
                    project.media.assets.get(&asset).map(|a| &a.source)
                else {
                    continue;
                };
                let (offset, stream) = clip
                    .audio
                    .as_ref()
                    .map(|a| (a.offset, a.stream))
                    .unwrap_or((Tick::ZERO, None));
                let identity = matches!(clip.speed, SpeedMap::Constant(r) if r == Ratio::ONE);
                if identity {
                    let src_pos = crate::playback::pcm::source_seek_with_offset(
                        clip.source_in,
                        t,
                        clip.start,
                        offset,
                    );
                    if let Ok(source) =
                        FfmpegPcmSource::spawn_stream(tools, path, src_pos, sample_rate, stream)
                    {
                        pcm.insert(clip.id, Box::new(source));
                    }
                } else {
                    let (lo, hi) = clip.speed.source_delta_range(clip.duration);
                    let source_start =
                        Tick((clip.source_in.0 - offset.0 + lo.floor() as i64).max(0));
                    let source_end = (clip.source_in.0 - offset.0) as f64 + hi.ceil();
                    let frames = ((source_end - source_start.0 as f64).max(0.0)
                        * sample_rate as f64
                        / TICKS_PER_SECOND as f64)
                        .ceil() as usize
                        + 2;
                    if let Ok(mut decoder) = FfmpegPcmSource::spawn_stream(
                        tools,
                        path,
                        source_start,
                        sample_rate,
                        stream,
                    ) {
                        let window = read_pcm_window(&mut decoder, frames);
                        pcm.insert(
                            clip.id,
                            Box::new(TimeWarpPcmSource::new(
                                window,
                                sample_rate,
                                source_start,
                                clip.source_in,
                                offset,
                                clip.speed.clone(),
                                t - clip.start,
                            )),
                        );
                    }
                }
            }
        }
        if output_end.is_some() && active.iter().any(|(_, clip)| !pcm.contains_key(&clip.id)) {
            prefill.store(2, Ordering::Release);
            return;
        }
        let active_ids: HashSet<ClipId> = active.iter().map(|(_, c)| c.id).collect();
        pcm.retain(|id, _| active_ids.contains(id));

        // Build this block's voice list (09 §4: the playback side resolves
        // "what's audible" once per block; the mixer owns only signal flow).
        let mut refs: HashMap<ClipId, &mut Box<dyn PcmSource>> =
            pcm.iter_mut().map(|(id, src)| (*id, src)).collect();
        let mut voices: Vec<TrackVoice<'_>> = Vec::new();
        for track in seq
            .audio_tracks
            .iter()
            .filter(|track| track.enabled && track.audio.is_some())
        {
            let track_audio = track.audio.as_ref().expect("filtered to Some");
            let mut clips: Vec<ClipVoice<'_>> = Vec::new();
            for (_, clip) in active.iter().filter(|(tr, _)| tr.id == track.id) {
                if let Some(source) = refs.remove(&clip.id) {
                    clips.push(ClipVoice {
                        audio: clip.audio.as_ref().unwrap_or(&default_clip_audio),
                        elapsed: t - clip.start,
                        remaining: clip.end() - t,
                        source: source.as_mut(),
                    });
                }
            }
            voices.push(TrackVoice {
                id: track.id,
                audio: track_audio,
                clips,
            });
        }

        out.fill(0.0);
        mixer.render_block(t, &mut voices, &seq.audio_master, &mut out);
        if let Some(end) = output_end {
            crate::source_audition::bound_source_output(&mut out, t, end, sample_rate);
        }
        // 31 §3: publish total graph latency for A/V clock offset.
        graph_latency.store(mixer.last_graph_latency_samples(), Ordering::Relaxed);
        // K-E1: publish dB spectrum (downsampled) for the scopes panel.
        if let Ok(mags) = mixer.last_spectrum().lock() {
            if !mags.is_empty() {
                let db = crate::audio::spectrum::to_db(&mags, 1.0);
                let n = 64usize.min(db.len());
                let step = (db.len() / n).max(1);
                let mut bins = Vec::with_capacity(n);
                for i in 0..n {
                    let start = i * step;
                    let end = ((i + 1) * step).min(db.len());
                    let slice = &db[start..end];
                    let peak = slice.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                    bins.push(peak);
                }
                spectrum_db.store(Some(Arc::new(bins)));
            }
        }
        if producer.push_block(&out) {
            prefill.store(1, Ordering::Release);
        }
        t = t + block_ticks;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_cache_evicts_only_the_coldest_frame() {
        let mut cache = UploadCache::new(2);
        let a = AssetId::new();
        let b = AssetId::new();
        let c = AssetId::new();
        let ka = (a, Tick(0), false);
        let kb = (b, Tick(0), false);
        let kc = (c, Tick(0), false);

        cache.insert(ka, "a", 1);
        cache.insert(kb, "b", 2);
        assert_eq!(cache.get(&ka, 3), Some(&"a"), "touches promote a hit");
        cache.insert(kc, "c", 4);

        assert_eq!(cache.len(), 2);
        assert!(cache.contains_key(&ka), "recently touched frame survives");
        assert!(!cache.contains_key(&kb), "coldest frame alone is evicted");
        assert!(cache.contains_key(&kc), "new frame is cached");
    }

    #[test]
    fn engine_status_starts_with_zeroed_performance_telemetry() {
        let status = EngineStatus::default();
        assert_eq!(status.frames_published, 0);
        assert_eq!(status.evaluations, 0);
        assert_eq!(status.last_evaluate_micros, 0);
        assert_eq!(status.evaluation_misses, 0);
    }

    #[test]
    fn preview_telemetry_reports_explicit_evaluation_attempts() {
        let status = EngineStatus {
            frames_published: 17,
            // 17 successful program passes, 3 primary misses, plus one clean
            // compare-effects pass: this cannot be derived from publications.
            evaluations: 21,
            evaluation_misses: 3,
            last_evaluate_micros: 2_400,
            ..EngineStatus::default()
        };

        let telemetry = PreviewTelemetrySnapshot::from_status(&status, 41, 7);

        assert_eq!(telemetry.ring_hits, 41);
        assert_eq!(telemetry.inline_seeks, 7);
        assert_eq!(telemetry.frames_published, 17);
        assert_eq!(telemetry.evaluation_misses, 3);
        assert_eq!(telemetry.evaluations, 21);
        assert_eq!(telemetry.last_evaluate_micros, 2_400);
    }

    #[test]
    fn preview_telemetry_preserves_a_saturated_evaluation_counter() {
        let status = EngineStatus {
            evaluations: u64::MAX,
            ..EngineStatus::default()
        };

        let telemetry = PreviewTelemetrySnapshot::from_status(&status, 0, 0);

        assert_eq!(telemetry.evaluations, u64::MAX);
    }

    #[test]
    fn preview_telemetry_public_accessor_uses_the_published_status() {
        let status = Arc::new(ArcSwap::from_pointee(EngineStatus {
            frames_published: 5,
            evaluations: 8,
            evaluation_misses: 2,
            last_evaluate_micros: 777,
            ..EngineStatus::default()
        }));
        let session = EngineSession {
            preview_status: Arc::new(ArcSwap::from_pointee(
                crate::preview::PreviewStatusSnapshot::default(),
            )),
            snapshot_input: Arc::new(ArcSwapOption::from(None)),
            next_snapshot_generation: AtomicU64::new(1),
            mailbox: Arc::new(CommandMailbox::new()),
            frame: Arc::new(ArcSwapOption::from(None)),
            status,
            join: None,
        };

        let telemetry = session.preview_telemetry();
        assert_eq!(telemetry.frames_published, 5);
        assert_eq!(telemetry.evaluations, 8);
        assert_eq!(telemetry.evaluation_misses, 2);
        assert_eq!(telemetry.last_evaluate_micros, 777);
        assert_eq!(telemetry.decode_misses, telemetry.inline_seeks);
    }

    #[test]
    fn auto_proxy_uses_timeline_ticks_for_a_30fps_frame_budget() {
        let mut policy = AdaptiveProxyPolicy::new();
        let rate = FrameRate::FPS_30;
        let expected_budget = Duration::from_micros(33_333);
        assert!(
            ticks_to_duration(rate.ticks_per_frame()) >= expected_budget,
            "30fps ticks must convert to roughly 33.3ms, not tens of seconds"
        );

        // 34ms is just over a 30fps frame, so exactly three consecutive
        // samples must switch Auto to the proxy hysteresis state.
        for _ in 0..2 {
            assert!(!observe_adaptive_proxy(
                &mut policy,
                PreviewQuality::Draft,
                ProxyMode::Auto,
                rate,
                Duration::from_millis(34),
                false,
            ));
            assert_eq!(policy.choice(), PreviewMediaChoice::Original);
        }
        assert!(observe_adaptive_proxy(
            &mut policy,
            PreviewQuality::Draft,
            ProxyMode::Auto,
            rate,
            Duration::from_millis(34),
            false,
        ));
        assert_eq!(policy.choice(), PreviewMediaChoice::Proxy);
    }

    #[test]
    fn resolved_source_keys_share_original_for_unready_proxies() {
        use photonic_core::timeline::{MediaAsset, ProxyRef};

        let dir = std::env::temp_dir().join(format!(
            "photonic-source-key-{}-{}",
            std::process::id(),
            AssetId::new()
        ));
        std::fs::create_dir_all(&dir).expect("create source-key fixture dir");
        let ready_original = dir.join("ready-original.mp4");
        let ready_proxy = dir.join("ready.proxy.mp4");
        let unready_original = dir.join("unready-original.mp4");
        std::fs::write(&ready_original, []).expect("write ready original");
        std::fs::write(&ready_proxy, []).expect("write ready proxy");
        std::fs::write(&unready_original, []).expect("write unready original");

        let mut project = TimelineProject::new();
        let mut ready = MediaAsset::new(
            AssetKind::Video,
            AssetSource::File {
                path: ready_original.clone(),
                rel_path: None,
            },
        );
        ready.proxy = Some(ProxyRef::ready_generated(ready_proxy.clone()));
        let ready_id = ready.id;
        project.media.assets.insert(ready_id, ready);

        let mut unready = MediaAsset::new(
            AssetKind::Video,
            AssetSource::File {
                path: unready_original.clone(),
                rel_path: None,
            },
        );
        // Status may say Ready while the file is gone; selection must still
        // canonicalize to the original input.
        unready.proxy = Some(ProxyRef::ready_generated(dir.join("missing.proxy.mp4")));
        let unready_id = unready.id;
        project.media.assets.insert(unready_id, unready);

        let mut media = MediaSources::new(None);
        media.set_project(Arc::new(project));

        let ready_proxy_input = media
            .resolve_video_input(ready_id, true)
            .expect("ready video input");
        assert_eq!(ready_proxy_input.key, (ready_id, true));
        assert_eq!(ready_proxy_input.path, ready_proxy);

        let unready_proxy_input = media
            .resolve_video_input(unready_id, true)
            .expect("unready proxy fallback");
        let unready_original_input = media
            .resolve_video_input(unready_id, false)
            .expect("unready original input");
        assert_eq!(unready_proxy_input.key, (unready_id, false));
        assert_eq!(unready_proxy_input.key, unready_original_input.key);
        assert_eq!(unready_proxy_input.path, unready_original);
        assert_eq!(unready_proxy_input.path, unready_original_input.path);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn playing_cut_ahead_queues_source_open_off_present_thread() {
        use photonic_core::timeline::{
            AssetKind, AssetSource, Clip, ClipSource, MediaAsset, Sequence, Track, TrackKind,
        };

        let mut project = TimelineProject::new();
        let asset = MediaAsset::new(
            AssetKind::Video,
            AssetSource::File {
                path: std::path::PathBuf::from("/missing/cut-ahead.mp4"),
                rel_path: None,
            },
        );
        let asset_id = asset.id;
        project.media.assets.insert(asset_id, asset);

        let mut seq = Sequence::new("seq", FrameRate::FPS_30, 320, 180);
        let mut track = Track::new(TrackKind::Video, "V1");
        track.clips.push(Clip::new(
            ClipSource::Asset { asset: asset_id },
            Tick(1_000_000),
            Tick(1_000_000),
        ));
        seq.video_tracks.push(track);

        let mut media = MediaSources::new(Some(FfmpegTools {
            ffmpeg: std::path::PathBuf::from("/missing/ffmpeg"),
            ffprobe: std::path::PathBuf::from("/missing/ffprobe"),
        }));
        media.playing = true;
        media.set_project(Arc::new(project));
        media.prefetch_upcoming(&seq, Tick::ZERO, Tick(1_000_000), Quality::FULL);

        let key = (asset_id, false);
        assert!(
            media.pending.contains_key(&key),
            "playing cut-ahead must queue the expensive source build"
        );
        assert!(
            !media.sources.contains_key(&key),
            "the present thread must not synchronously insert a failed build"
        );
    }

    #[test]
    fn diagnostic_counter_increment_saturates_without_wrapping() {
        let counter = AtomicU64::new(u64::MAX - 1);
        saturating_increment(&counter);
        saturating_increment(&counter);
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn seek_coalescing_preserves_transport_barriers() {
        let batch = vec![
            EngineCmd::Seek(Tick(1)),
            EngineCmd::Pause,
            EngineCmd::Seek(Tick(2)),
            EngineCmd::Play,
            EngineCmd::Seek(Tick(3)),
            EngineCmd::SetProxyMode(ProxyMode::ForceProxy),
        ];
        let out = coalesce_commands(batch);
        assert_eq!(out.len(), 6);
        assert!(matches!(out[0], EngineCmd::Seek(Tick(1))));
        assert!(matches!(out[1], EngineCmd::Pause));
        assert!(matches!(out[2], EngineCmd::Seek(Tick(2))));
        assert!(matches!(out[3], EngineCmd::Play));
        assert!(matches!(out[4], EngineCmd::Seek(Tick(3))));
    }

    #[test]
    fn seek_coalescing_passes_through_seekless_batches() {
        let out = coalesce_commands(vec![EngineCmd::Play, EngineCmd::Pause]);
        assert_eq!(out.len(), 2);
        let out = coalesce_commands(vec![EngineCmd::Seek(Tick(9))]);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], EngineCmd::Seek(Tick(9))));
    }

    #[test]
    fn f16_pack_matches_known_bit_patterns() {
        assert_eq!(f32_to_f16_bits(0.0), 0x0000);
        assert_eq!(f32_to_f16_bits(1.0), 0x3c00);
        assert_eq!(f32_to_f16_bits(0.5), 0x3800);
        assert_eq!(f32_to_f16_bits(2.0), 0x4000);
        // Sign preserved.
        assert_eq!(f32_to_f16_bits(-1.0), 0xbc00);
    }

    /// The EOTF the still-upload path decodes with. It is `graph::ops`' shared
    /// one (K-C8 removed the byte-identical private copy that used to live
    /// here), so CPU kernels, GPU uniforms and still uploads cannot drift.
    #[test]
    fn srgb_to_linear_endpoints() {
        use crate::graph::ops::srgb_to_linear;
        assert!((srgb_to_linear(0.0) - 0.0).abs() < 1e-6);
        assert!((srgb_to_linear(1.0) - 1.0).abs() < 1e-4);
        // Monotone and inside the unit interval mid-scale.
        let mid = srgb_to_linear(0.5);
        assert!(
            mid > 0.0 && mid < 0.5,
            "sRGB 0.5 decodes to ~0.214, got {mid}"
        );
    }

    #[test]
    fn still_upload_premultiplies_transparent_black_to_zero() {
        // No GPU dependency: verify the CPU premultiply/pack path directly.
        // A fully transparent pixel packs to all-zero f16 (premultiplied).
        use crate::graph::ops::srgb_to_linear;
        let (r, g, b, a) = (255u8, 255u8, 255u8, 0u8);
        let af = a as f32 / 255.0;
        let rl = srgb_to_linear(r as f32 / 255.0) * af;
        let gl = srgb_to_linear(g as f32 / 255.0) * af;
        let bl = srgb_to_linear(b as f32 / 255.0) * af;
        assert_eq!(f32_to_f16_bits(rl), 0);
        assert_eq!(f32_to_f16_bits(gl), 0);
        assert_eq!(f32_to_f16_bits(bl), 0);
        assert_eq!(f32_to_f16_bits(af), 0);
    }

    /// A 64×64 four-quadrant PNG on disk, plus a project whose media pool holds
    /// it as an `Image` asset. Quadrants survive any correct area downscale by
    /// an even factor with their colours exactly intact, so the *content* of a
    /// resampled still is checkable, not just its dimensions.
    fn quadrant_still_project(dir: &std::path::Path) -> (Arc<TimelineProject>, AssetId) {
        let size = 64u32;
        let mut img = photonic_core::RasterImage::new(size, size);
        for y in 0..size {
            for x in 0..size {
                let rgba = match (x < size / 2, y < size / 2) {
                    (true, true) => [255, 0, 0, 255],
                    (false, true) => [0, 255, 0, 255],
                    (true, false) => [0, 0, 255, 255],
                    (false, false) => [255, 255, 255, 255],
                };
                img.set_pixel(x, y, rgba);
            }
        }
        let path = dir.join("quadrants.png");
        std::fs::write(&path, img.to_png()).expect("write still fixture");

        let mut project = TimelineProject::new();
        let asset = photonic_core::timeline::MediaAsset::new(
            photonic_core::timeline::AssetKind::Image,
            AssetSource::File {
                path,
                rel_path: None,
            },
        );
        let id = asset.id;
        project.media.assets.insert(id, asset);
        (Arc::new(project), id)
    }

    /// A relink must evict the asset's cached media; unrelated metadata edits
    /// must not.
    ///
    /// Before this, `set_project` only swapped the snapshot Arc, so after a
    /// relink the engine kept serving the OLD file's decoded still for the rest
    /// of the session — invisible until export. `EngineCmd::InvalidateRange` is
    /// documented as the seam for exactly this and has no senders anywhere.
    #[test]
    fn a_relink_evicts_cached_media_but_a_metadata_edit_does_not() {
        let Some(gpu) = crate::graph::eval::GpuContext::request_blocking() else {
            eprintln!("no GPU adapter — skipping relink invalidation test");
            return;
        };
        let dir = std::env::temp_dir().join(format!("photonic-relink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (project, id) = quadrant_still_project(&dir);

        let mut media = MediaSources::new(None);
        media.set_project(project.clone());
        media.still_texture(&gpu, id, 64, 64).expect("still");
        assert_eq!(media.stills.len(), 1, "fixture must warm the cache first");

        // A metadata-only edit: same file, same bytes, same proxy.
        let mut rated = (*project).clone();
        rated.media.assets.get_mut(&id).unwrap().rating = Some(5);
        media.set_project(Arc::new(rated.clone()));
        assert_eq!(
            media.stills.len(),
            1,
            "a rating edit must not evict a warm decode — over-invalidating here \
             would be a real performance regression"
        );

        // A relink: the path now points at a different file.
        let other = dir.join("relinked.png");
        std::fs::write(&other, photonic_core::RasterImage::new(8, 8).to_png())
            .expect("write relink target");
        let mut relinked = rated;
        relinked.media.assets.get_mut(&id).unwrap().source = AssetSource::File {
            path: other,
            rel_path: None,
        };
        media.set_project(Arc::new(relinked));
        assert_eq!(
            media.stills.len(),
            0,
            "a relink must evict the still decoded from the previous file"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The identity diff reports a relink, a byte change and a proxy swap, and
    /// stays silent for metadata. Pure, so it runs without a GPU.
    #[test]
    fn changed_media_identities_tracks_bytes_not_metadata() {
        use photonic_core::timeline::{AssetKind, MediaAsset, ProxyRef};

        let mut base = TimelineProject::new();
        let asset = MediaAsset::new(
            AssetKind::Video,
            AssetSource::File {
                path: std::path::PathBuf::from("/a.mp4"),
                rel_path: None,
            },
        );
        let id = asset.id;
        base.media.assets.insert(id, asset);

        let edit = |f: &dyn Fn(&mut MediaAsset)| {
            let mut p = base.clone();
            f(p.media.assets.get_mut(&id).unwrap());
            p
        };

        // Unchanged: nothing to evict.
        assert!(changed_media_identities(&base, &base.clone()).is_empty());

        for (label, mutate) in [
            (
                "relink",
                &(|a: &mut MediaAsset| {
                    a.source = AssetSource::File {
                        path: std::path::PathBuf::from("/b.mp4"),
                        rel_path: None,
                    }
                }) as &dyn Fn(&mut MediaAsset),
            ),
            (
                "byte change",
                &(|a: &mut MediaAsset| a.content_hash = Some("deadbeef".into())),
            ),
            (
                "proxy attach",
                &(|a: &mut MediaAsset| a.proxy = Some(ProxyRef::ready_generated("/a.proxy.mp4"))),
            ),
        ] {
            let changed = changed_media_identities(&base, &edit(mutate));
            assert!(changed.contains(&id), "{label} must invalidate");
        }

        for (label, mutate) in [
            (
                "rating",
                &(|a: &mut MediaAsset| a.rating = Some(3)) as &dyn Fn(&mut MediaAsset),
            ),
            (
                "tags",
                &(|a: &mut MediaAsset| a.tags = vec!["b-roll".into()]),
            ),
        ] {
            let changed = changed_media_identities(&base, &edit(mutate));
            assert!(changed.is_empty(), "{label} must NOT invalidate");
        }

        // A brand-new asset has nothing cached under its id.
        let mut added = base.clone();
        let fresh = MediaAsset::new(
            AssetKind::Video,
            AssetSource::File {
                path: std::path::PathBuf::from("/c.mp4"),
                rel_path: None,
            },
        );
        added.media.assets.insert(fresh.id, fresh);
        assert!(changed_media_identities(&base, &added).is_empty());
    }

    /// Assert a still frame's four quadrants are the fixture's colours, read
    /// back over its **logical** `w`×`h` region (the physical texture is pool-
    /// bucket padded and is generally bigger).
    fn assert_quadrants(gpu: &GpuContext, frame: &GpuFrame) {
        let (w, h) = (frame.width, frame.height);
        let px = crate::graph::eval::read_texture_rgba16f(gpu, &frame.texture, w, h);
        let at = |x: u32, y: u32| px[(y * w + x) as usize];
        let near = |got: [f32; 4], want: [f32; 4]| {
            got.iter()
                .zip(want.iter())
                .all(|(g, w)| (g - w).abs() < 1e-3)
        };
        for (x, y, want, name) in [
            (w / 4, h / 4, [1.0, 0.0, 0.0, 1.0], "top-left red"),
            (3 * w / 4, h / 4, [0.0, 1.0, 0.0, 1.0], "top-right green"),
            (w / 4, 3 * h / 4, [0.0, 0.0, 1.0, 1.0], "bottom-left blue"),
            (3 * w / 4, 3 * h / 4, [1.0; 4], "bottom-right white"),
        ] {
            let got = at(x, y);
            assert!(
                near(got, want),
                "{w}x{h} still: {name} quadrant at ({x},{y}) is {got:?}, want {want:?}"
            );
        }
    }

    /// K-C8. The same still requested at two logical sizes must come back at
    /// **both** sizes, each correct — the asset-only key served whichever was
    /// decoded first for the other, so this fails against it twice over: the
    /// 16×16 request came back 64×64, and reading its logical 16×16 region got
    /// the red quadrant everywhere instead of all four.
    #[test]
    fn still_cache_keys_on_the_requested_logical_size() {
        let Some(gpu) = GpuContext::request_blocking() else {
            eprintln!("skip still cache size test: no GPU adapter");
            return;
        };
        let dir = std::env::temp_dir().join(format!("photonic-kc8-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (project, id) = quadrant_still_project(&dir);

        let mut media = MediaSources::new(None);
        media.set_project(project);

        // Full canvas, then a Draft-sized one.
        let full = media.still_texture(&gpu, id, 64, 64).expect("full still");
        let draft = media.still_texture(&gpu, id, 16, 16).expect("draft still");

        assert_eq!((full.width, full.height), (64, 64));
        assert_eq!(
            (draft.width, draft.height),
            (16, 16),
            "the 16x16 request must not be served the 64x64 upload"
        );
        assert_quadrants(&gpu, &full);
        assert_quadrants(&gpu, &draft);
        assert_eq!(media.stills.len(), 2, "one entry per requested size");

        // The key is the LOGICAL size, not the physical one: a 16×16 still is
        // padded up into a 64×64 pool bucket, and keying on that bucket would
        // have collapsed these two requests back into one entry.
        assert_eq!(
            crate::graph::ir::TextureDesc {
                width: 16,
                height: 16
            }
            .bucket(),
            (64, 64)
        );
        assert!(
            draft.texture.width() >= draft.width,
            "physical texture is bucket-padded past the logical picture"
        );

        // Not over-specified either: a canvas at or above native canonicalizes
        // onto the full-resolution entry instead of allocating a duplicate.
        let oversized = media
            .still_texture(&gpu, id, 4096, 4096)
            .expect("oversized request");
        assert!(
            Arc::ptr_eq(&oversized.texture, &full.texture),
            "a canvas larger than the image must reuse the native-size entry"
        );
        assert_eq!(media.stills.len(), 2, "no duplicate full-resolution entry");

        // Relink eviction drops every size of the asset, not just one.
        media.invalidate_assets(&HashSet::from([id]));
        assert_eq!(media.stills.len(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
#[path = "session_t005_tests.rs"]
mod t005_tests;
