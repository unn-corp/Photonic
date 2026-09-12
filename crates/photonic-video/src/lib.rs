//! photonic-video — the temporal engine for Photonic's video editor module.
//!
//! Spec: `docs/specs/video-editor/02-engine.md` (engine architecture) and
//! `01-data-model.md` (timeline data model this crate evaluates).
//!
//! P1 status: **signature stub only** (execution-plan spine item 0b,
//! `12-agent-execution-plan.md` §3). The `graph::ir` module pins the frame-graph
//! IR type contract that the P1 renderer work (03 §3.4 texture pool, Tier-B
//! conversion pass) compiles against. Evaluator, decode, playback, audio, and
//! export land in P3 per the phase plan.

// GPU passes keep their command-model parameters and indexed pixel loops. The
// data enums also remain inline because they cross public and worker boundaries.
// `expect` makes these narrowly documented choices fail the strict gate once
// they stop applying, instead of silently disabling the lints.
#![expect(
    clippy::large_enum_variant,
    clippy::manual_clamp,
    clippy::needless_range_loop,
    clippy::too_many_arguments
)]

pub mod graph;

/// Audio engine host + mixer (02 §1, 09).
pub mod audio;
/// Caption/TTS provider abstraction + subtitle interchange (06).
pub mod captions;
/// FFmpeg-sidecar decode: process mgmt, pipes, scheduling, rings (02 §3).
pub mod decode;
/// Export render loop + encoder sidecar + presets (02 §7, 05).
pub mod export;
/// Media probing, pool services, keyframe index (02 §1 module map).
pub mod media;
/// Playback controller, clock, prefetch (02 §4).
pub mod playback;
/// Content-addressed, bounded disk previews for timeline playback (33).
pub mod preview;
/// `VideoEngine` facade + per-document `EngineSession` (02 §1).
pub mod session;
pub mod source_audition;

/// Pooled `Rgba16Float` working-texture allocator (03 §3.4). The P1 renderer /
/// P3 evaluator request textures from here keyed by [`graph::ir::ContentHash`].
pub mod pool;

/// Timeline-contract types the IR references ahead of the P2 data-model landing.
///
/// `01-data-model.md` §1/§3 pins these shapes; P2 moves them into
/// `photonic_core::timeline` and this module becomes a re-export of that home
/// (one import-path swap, no semantic change). Kept here so the P1 stub is
/// self-contained and compile-checked without front-running P2's crate work.
pub mod contract;

/// Cross-crate test-support surface (29 §3 / CAP-019). Always compiled — NOT
/// `#[cfg(test)]` — because integration tests in other crates (the acceptance-
/// story harness) must be able to import [`testing::frame_compare`], the single
/// home of the 11 §1.2 frame-comparison metric.
pub mod testing;

// ── Facade re-exports (02 §1) — the names Wire-phase consumers import ────────
pub use graph::eval::GpuContext;
pub use media::thumbnails::{RgbaThumb, ThumbHandle, ThumbnailCache, WaveformCache};
pub use session::{
    coalesce_commands, colorimetry_for_probe, AssetReadiness, EngineCmd, EngineFrame,
    EngineSession, EngineStatus, ExportJob, MasterMeterSnapshot, PreviewQuality, PreviewTarget,
    PreviewTelemetrySnapshot, ProxyMode, RenderJobOptions, RenderSnapshot, VideoEngine,
};
