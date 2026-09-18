//! Frame-graph IR, compiler, evaluator, and result cache (02 §2).
//!
//! - [`ir`] — the normative IR type contract (02 §2, pinned in P1).
//! - [`compile`] — timeline snapshot → [`ir::FrameGraph`] (02 §2 compilation).
//! - [`ops`] — CPU pixel kernels shared by the reference evaluator.
//! - [`eval_cpu`] — the f32 CPU reference evaluator (golden/parity tests, 03 §6).
//! - [`cache`] — content-hash → GPU-texture result cache over the pool (02 §5).
//! - [`eval`] — the wgpu evaluator (02 §2 evaluation).
//! - [`raster_bridge`] — K-B16 CPU bridge from `photonic_core::raster` kernels.
//! - [`source_range`] — E-1 temporal source-range contract (32 §1).
//! - [`analysis`] — E-2 analysis-as-node foundation (32 §2 image, 31 §5 audio).
//! - [`deflicker`] — exposure-drift stabilization, measured once and cached (E-2).
//! - [`rolling_bands`] — rolling-shutter AC banding: per-scanline detect + correct.

pub mod ir;

pub mod analysis;
pub mod cache;
pub mod compile;
pub mod deflicker;
pub mod eval;
pub mod eval_cpu;
pub mod luma_wipe;
pub mod ops;
pub mod panorama;
pub mod panorama_gpu;
pub mod raster_bridge;
pub mod rolling_bands;
pub mod source_range;
/// D-12 gyro-metadata stabilization analysis: bias, integration, smoothing,
/// horizon lock, lens model and crop solving (22 §6.4).
pub mod stabilize;

pub use source_range::{
    exceeds_soft_cap, graph_source_range, source_range_for_op, FrameRange, SOURCE_RANGE_SOFT_CAP,
};

pub use compile::{
    compile, CompileCode, CompileDiagnostic, CompiledFrame, DiagSeverity, Quality, ScopeTapPoint,
    ViewNodeOverride,
};
