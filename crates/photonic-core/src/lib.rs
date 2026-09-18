// These expectations document intentional representation and hot-loop choices.
// Unlike `allow`, an expectation makes the strict gate fail if the corresponding
// code is removed or refactored, so this baseline cannot silently go stale.
#![expect(
    clippy::large_enum_variant,
    clippy::manual_checked_ops,
    clippy::manual_strip,
    clippy::neg_cmp_op_on_partial_ord,
    clippy::needless_range_loop,
    clippy::nonminimal_bool,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::wrong_self_convention
)]

pub mod annotation;
pub mod audit;
pub mod color;
pub mod color_cmyk;
pub mod css_vectors;
pub mod diag;
pub mod diagnostics;
pub mod document;
pub mod effects;
pub mod export;
pub mod history;
pub mod import;
pub mod layer;
pub mod migration;
pub mod node;
pub mod ops;
pub mod path;
pub mod path_policy;
pub mod photon_file;
pub mod raster;
pub mod selection;
pub mod style;
pub mod text_metrics;
pub mod timeline;
pub mod tokens;
pub mod transform;
pub mod units;

// Re-export the most commonly used types at the crate root
pub use annotation::{Annotation, AnnotationId};
pub use audit::{
    audit_timestamp, AuditEntry, AuditLog, MAX_AUDIT_ARGUMENT_BYTES, MAX_AUDIT_EXPORT_BYTES,
};
pub use color::Color;
pub use diag::{DiagCode, Diagnostic, Remedy, Severity, Subject};
pub use diagnostics::{crash_dir, CrashReport};
pub use document::{
    sample_fill_at, ActionSet, Artboard, ArtboardId, CharacterStyle, ColorSwatch,
    DimensionAnnotation, Document, DocumentId, DocumentVariable, EventTrigger, ExportProfile,
    GradientSwatch, GrammarRule, GraphicStyle, Guide, GuideOrientation, Page, ParagraphStyle,
    SpotColor, Symbol, WidthProfile, Workspace,
};
pub use history::{CheckpointInfo, Command, CommandHistory, HistorySnapshot};
pub use import::{
    import_svg, ImportError, ImportLimit, MAX_SVG_CSS_BYTES, MAX_SVG_DEPTH, MAX_SVG_ELEMENTS,
    MAX_SVG_INPUT_BYTES, MAX_SVG_PATH_POINTS, MAX_SVG_TEXT_BYTES,
};
pub use layer::{BlendMode, Layer, LayerId};
pub use node::{
    AssetExportSpec, DropShadow, Feather, FontStyle, GaussianGlow, GlowEffect, NodeId, ObjectBlur,
    PrimitiveKind, RasterNode, SceneNode, SceneNodeKind,
};
pub use path::PathData;
pub use path_policy::{DenyReason, PathAccess, PathPolicy, PathPolicyError, PathVerdict};
pub use photon_file::{
    load_photon, save_photon, write_atomic_file, write_atomic_file_with_mode,
    PHOTON_FILE_EXTENSION, PHOTON_FORMAT_VERSION,
};
pub use raster::{adjust::AdjustmentSpec, image::RasterImage, mask::Mask};
pub use selection::Selection;
pub use style::{
    interpolate_stops, interpolate_stops_with, ArrowheadStyle, Fill, FillKind, FluidGradient,
    FluidGradientPoint, Gradient, GradientInterpolation, GradientKind, GradientStop, GradientUnits,
    MeshGradient, Stroke,
};
pub use transform::Transform;
pub use units::{from_px, to_px, DocumentUnit};
