// Render-pass signatures and indexed tessellation loops are deliberately kept
// close to the GPU command model. Expectations keep that decision visible to
// the strict gate instead of suppressing the lints permanently.
#![expect(
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    clippy::type_complexity
)]

pub mod canvas;
pub mod capability;
pub mod caption;
pub mod caption_decoration;
pub mod color;
pub mod compositor;
pub mod gpu_state;
pub mod grade;
pub mod grade_gpu;
pub mod grade_graph;
pub mod headless;
pub mod lut;
pub mod matte_inspection;
pub mod native_caption;
pub mod native_display;
pub mod native_transfer;
pub mod pipeline;
pub mod renderer;
pub mod scene_measurement;
pub mod scene_scopes;
pub mod scopes;
pub mod tessellator;
pub mod text_outline;
pub mod text_path;
pub mod video;

pub use canvas::CanvasView;
pub use capability::{
    check_capability_floor, CapabilityReport, CapabilityRequirement, MIN_TEXTURE_DIMENSION_2D,
};
pub use caption::{CaptionCompositor, CaptionCueRun, CaptionWordRun};
pub use color::{Colorimetry, Matrix, Range};
pub use gpu_state::{backoff_delay, GpuHealth, GpuState, MAX_RECOVERY_ATTEMPTS};
pub use grade::{
    apply_grade_cpu, resolve, ResolvedCdl, ResolvedCurves, ResolvedGradeOp, ResolvedGradePayload,
    ResolvedHslQualifier, ResolvedLut3d, ResolvedMask,
};
pub use grade_gpu::{apply_grade_op_gpu, apply_grade_stack_gpu, qualifier_matte_gpu};
pub use headless::{
    document_needs_cpu_compositor, ExportBackground, ExportOptions, HeadlessRenderer,
};
pub use lut::{parse_cube, CubeError, Lut3d};
pub use renderer::PhotonicRenderer;
pub use scopes::{
    scopes_from_pixels_cpu, scopes_from_texture_gpu, Histogram, Scopes, Vectorscope, Waveform,
};
pub use text_outline::{
    layout_text_flat, outline_document_text, resolve_document_font, ResolvedFace,
};

pub mod tonal_exposure;
pub mod white_balance;

pub mod window_tracking;

pub mod native_vector;
