//! Export render loop + encoder sidecar + presets (02-engine.md §7,
//! 05-import-export.md §3, 03-render-color-pipeline.md §3.5).
//!
//! - [`presets`] — `ExportPreset` schema, the §3.5 built-in catalog, §3.4
//!   alpha-allow-list validation, app-level preset persistence (§3.6).
//! - [`convert`] — working format (`Rgba16Float` readback) → encoder pix_fmt
//!   (§3.5 steps 2-6), the CPU-side inverse of decode's YUV→working pass.
//! - [`encoder`] — the ffmpeg encode sidecar: process/arg building, codec
//!   capability probing, video-stdin + second audio input (unix FIFO or
//!   Windows/non-unix temp f32le file).
//! - [`render_loop`] — the engine-independent `export_frames` shell (02 §7)
//!   that the P3 evaluator feeds.
//! - [`job`] — the single engine-backed export path (02 §7, 10 §6): resolves a
//!   `session::ExportJob` against a frozen project and drives a dedicated
//!   headless session through `render_loop::export_frames`. Both the GUI
//!   (`EngineCmd::Export`) and MCP `export_sequence` funnel through it.
//! - [`offline_audio`] — K-0.7 offline sequence mix + loudness (09 §7, 31 §6).
//! - [`job_queue`] — K-F1 shared multi-job render queue (GUI + MCP).
//! - [`extract_frame`] — K-E4 program-monitor still → PNG (full quality colour path).

pub mod convert;
pub mod encoder;
pub mod extract_frame;
pub mod job;
pub mod job_queue;
pub mod offline_audio;
pub mod presets;
pub mod render_loop;

pub use extract_frame::{default_extract_path, flatten_pixels, write_frame_png, ExtractFrameError};
pub use job_queue::{QueueJobId, QueueJobStatus, QueuedExport, RenderQueue};
