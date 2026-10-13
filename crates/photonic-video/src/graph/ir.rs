//! Frame-graph IR — the normative type contract from 02 §2, pinned in code.
//!
//! Properties (02 §2, restated as the contract this module guarantees):
//! - A `FrameGraph` is a pure function of (document snapshot, sequence, format,
//!   tick, quality flags): same inputs ⇒ identical graph ⇒ identical pixels.
//! - All keyframe evaluation happens at compile time; the IR carries resolved
//!   params and the evaluator is time-ignorant.
//! - Every node has a content hash — `hash(op, resolved params, input hashes)`
//!   — which is the cache key for the node-result texture cache (02 §5) and
//!   the texture pool underneath it (03 §3.4).
//!
//! P1 consumers: the renderer's texture pool + Tier-B vector conversion pass
//! (03 §2.5/§3.4) key their allocations by [`ContentHash`] and [`TextureDesc`].
//! The compiler (`graph::compile`) and evaluator (`graph::eval`) land in P3.

use glam::Mat3;
use photonic_core::layer::BlendMode;
use photonic_render::caption_decoration::{CaptionDecoration, CaptionPaint};
use photonic_render::native_transfer::AcescctDirection;

use crate::contract::{
    AssetId, CaptionBatch, EffectKind, MatteModel, ResolvedGradeOp, ResolvedParams,
    ResolvedTextBlock, Tick, VectorRef, VectorStateKey,
};

/// Scene-linear AP1 blend functions with defined, unclamped extended-channel math.
/// Normal uses direct premultiplied source-over without an alpha division or
/// artistic cutoff, retaining valid extended RGB at very small coverage.
/// Polynomial/min/max/absolute-value functions extend the familiar separable
/// equations to signed scene values; this is not display-referred CSS matching.
/// Overlay/HardLight use a 0.5 scene-channel pivot; PinLight uses the same
/// piecewise min/max extension. These pivots are creative AP1 operations, not
/// perceptual middle gray. SoftLight extends its cubic low-backdrop branch to
/// negative AP1 values and uses sqrt only above 0.25; this can amplify negative
/// channels and is not a perceptual/HDR-safe contrast operation. Like other
/// extended blends, results remain subject to RGBA16F storage limits.
/// LinearDodge, LinearBurn, Subtract and LinearLight use dedicated unclamped
/// arithmetic IR; DarkerColor/LighterColor use AP1 Y to select a whole RGB.
/// Legacy equations/weights are unchanged. The nine bounded artistic modes use a dedicated
/// overlap-only unit-range contract; they do not preserve HDR overlap values.
pub fn native_blend_supported(mode: BlendMode) -> bool {
    native_arithmetic_blend(mode)
        || native_bounded_blend(mode)
        || native_luminance_blend(mode)
        || matches!(
            mode,
            BlendMode::Normal
                | BlendMode::Overlay
                | BlendMode::HardLight
                | BlendMode::PinLight
                | BlendMode::SoftLight
                | BlendMode::Multiply
                | BlendMode::Screen
                | BlendMode::Darken
                | BlendMode::Lighten
                | BlendMode::Difference
                | BlendMode::Exclusion
        )
}

/// Native-only unclamped arithmetic equations. These have their own IR/cache
/// identity: Legacy uses the original bounded raster equations for these names.
pub fn native_arithmetic_blend(mode: BlendMode) -> bool {
    matches!(
        mode,
        BlendMode::LinearDodge
            | BlendMode::LinearBurn
            | BlendMode::Subtract
            | BlendMode::LinearLight
    )
}

/// Bounded artistic overlap functions on unit-range straight AP1 operands.
/// Source-over retains original signed/extended color outside the overlap.
pub fn native_bounded_blend(mode: BlendMode) -> bool {
    matches!(
        mode,
        BlendMode::Hue
            | BlendMode::Saturation
            | BlendMode::Color
            | BlendMode::Luminosity
            | BlendMode::ColorDodge
            | BlendMode::ColorBurn
            | BlendMode::Divide
            | BlendMode::VividLight
            | BlendMode::HardMix
    )
}

/// Bounded AP1 Hue/Saturation/Color/Luminosity use scene-linear AP1 Y.
pub fn native_nonseparable_blend(mode: BlendMode) -> bool {
    matches!(
        mode,
        BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity
    )
}

/// Whole-color selection by scene-linear AP1 luminance, not Legacy RGB luma.
pub fn native_luminance_blend(mode: BlendMode) -> bool {
    matches!(mode, BlendMode::DarkerColor | BlendMode::LighterColor)
}

/// Index of a node within one compiled [`FrameGraph`] arena. Not stable across
/// compiles — cross-frame identity is [`ContentHash`], never this index.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct IrNodeId(pub u32);

/// Output-port index on a multi-output node (e.g. `ChannelSplit`); 0 for all
/// single-output ops.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct OutPort(pub u8);

/// Content hash of (op, resolved params, input hashes) — the cache identity of
/// a node's result (02 §5). 128-bit (xxh3-128 class) so collisions are a
/// non-concern at cache scale.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ContentHash(pub u128);

/// Working-texture descriptor for the pooled allocator (03 §3.4): always
/// `Rgba16Float` and premultiplied; pool buckets round dimensions up to the
/// next 64px multiple. The color interpretation is [`WorkingColorDomain`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct TextureDesc {
    pub width: u32,
    pub height: u32,
}

/// Interpretation of source/working values entering a frame graph. Explicit
/// transfer nodes can change the domain at individual graph edges.
/// A texture's format alone cannot distinguish legacy Rec.709 from ACEScg.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum WorkingColorDomain {
    #[default]
    LegacyLinearRec709,
    SceneLinearAcescg,
}

/// Interpretation of a graph's published pixels. Presentation and export
/// must use this rather than infer a transfer function from texture format.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum FrameColorEncoding {
    LegacyLinearRec709,
    SceneLinearAcescg,
    Acescct,
    /// Unassociated key weight, stored as opaque grayscale.
    MatteWeight,
    /// Signed scene channel/luminance value in opaque grayscale storage; not a bounded key.
    ScalarChannel,
    SrgbDisplay,
    Bt709Video,
}

impl TextureDesc {
    /// Pool-bucket dimensions: round each dimension up to the next multiple of
    /// 64 so near-identical sequence formats share buckets (03 §3.4).
    pub fn bucket(&self) -> (u32, u32) {
        let up = |v: u32| v.div_ceil(64) * 64;
        (up(self.width), up(self.height))
    }
}

/// Geometry sampling mode for `Transform2D`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Sampling {
    Bilinear,
    Nearest,
}

/// Fit policy for `Resize`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum FitMode {
    /// Scale to fit inside, letterbox/pillarbox as needed.
    Fit,
    /// Scale to cover, cropping overflow.
    Fill,
    /// Non-uniform scale to exact dimensions.
    Stretch,
}

/// Premultiplied, linear-light Rec.709 RGBA (D-09).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LinearColor {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

/// Single channel selector for `ChannelSplit`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Channel {
    R,
    G,
    B,
    A,
}

/// Field order for interlaced sources (K-G6 / 32 §6).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum FieldOrder {
    #[default]
    TopFirst,
    BottomFirst,
}

/// Deinterlace algorithm (K-G6). Spatial methods need only the current frame;
/// source-range still declares `[out−1, out+1]` so temporal methods can land
/// without another contract change (E-1).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum DeinterlaceMethod {
    /// Keep one field, double lines (fast, soft vertical).
    OneField,
    /// Average even/odd lines (cheap comb reduction).
    #[default]
    LinearBlend,
    /// Spatial edge-adaptive interpolate (YADIF-style spatial half).
    YadifSpatial,
}

/// Per-node threading capability (E-4 / 32 §3).
///
/// Declared so the scheduler can parallelise safely as CPU-side work multiplies.
/// **Default for any undeclared kind is [`Threading::Serial`]** — fail safe, not
/// fast.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Threading {
    /// Pure function of inputs — freely parallelisable.
    Any,
    /// Holds per-instance state; one instance must not run concurrently with itself.
    PerInstance,
    /// Must run in frame order (temporal state, sequential DSP, …).
    Serial,
}

/// Threading capability declared by `op`. Undeclared / stateful / temporal ops
/// return [`Threading::Serial`].
pub fn threading_for_op(op: &IrOp) -> Threading {
    match op {
        // Pure pixel transforms and generators.
        IrOp::SolidColor { .. }
        | IrOp::Transform2D { .. }
        | IrOp::Transform2DTransparent { .. }
        // Pure per-pixel resample from fully-resolved params; no state, no
        // neighbouring frames.
        | IrOp::StabilizeWarp { .. }
        | IrOp::Effect { .. }
        | IrOp::Grade { .. }
        | IrOp::NativeMotionBlur { .. }
        | IrOp::NativeOutline { .. }
        | IrOp::NativeLinearHsl { .. }
        | IrOp::NativeLensBlur { .. }
        | IrOp::NativeSurfaceBlur { .. }
        | IrOp::NativeRankFilter { .. }
        | IrOp::NativeDeflicker{..}
        | IrOp::NativeUnpremultiply
        | IrOp::NativeAlphaView { .. }
        | IrOp::NativeTransferLevels { .. }
        | IrOp::NativeTransferCurves { .. }
        | IrOp::NativeColorBalance { .. }
        | IrOp::NativePhotoFilter { .. }
        | IrOp::NativeTransferMatrix { .. }
        | IrOp::NativeTransferArtistic { .. }
        | IrOp::NativeDropShadow { .. }
        | IrOp::NativePerspective { .. }
        | IrOp::NativeStylize { .. }
        | IrOp::NativeDetail { .. }
        | IrOp::NativeGeometricWarp { .. }
        | IrOp::NativeBoxBlur { .. }
        | IrOp::NativeMosaic { .. }
        | IrOp::NativeGaussianBlur { .. }
        | IrOp::NativeUnsharp { .. }
        | IrOp::NativeChannelExtract { .. }
        | IrOp::NativeImageToMask
        | IrOp::NativeMaskToImage
        | IrOp::NativeChannelCombine
        | IrOp::NativeInvert
        | IrOp::NativeVignette { .. }
        | IrOp::NativeGrain { .. }
        | IrOp::NativeDenoise { .. }
        | IrOp::NativeMaskShape { .. }
        | IrOp::NativePolygonMask { .. }
        | IrOp::NativeGlow { .. }
        | IrOp::NativeChromaKey { .. }
        | IrOp::NativeLumaKey { .. }
        | IrOp::NativeCanvas { .. }
        | IrOp::NativeDissolve { .. }
        | IrOp::NativeLumaWipe { .. }
        | IrOp::NativeExposure { .. }
        | IrOp::NativeLinearOffset { .. }
        | IrOp::NativePrinterLights { .. }
        | IrOp::NativeHighlightRolloff { .. }
        | IrOp::NativeSaturationVibrance { .. }
        | IrOp::NativeLogContrast { .. }
        | IrOp::NativeLogCdl { .. }
        | IrOp::WindowMatte { .. }
        | IrOp::BezierMatte { .. }
        | IrOp::QualifierMatte { .. }
        | IrOp::GradeKeyMix { .. }
        | IrOp::GradeMatteRefine { .. }
        | IrOp::GradeMatteApply
        | IrOp::NativeMatteCdl { .. }
        | IrOp::NativeRgbMatrix { .. }
        | IrOp::NativeTonalExposure { .. }
        | IrOp::GradeMatteConstant { .. }
        | IrOp::GradeLayerMix { .. }
        | IrOp::NativeLogQualifier { .. }
        | IrOp::NativeLogCurves { .. }
        | IrOp::NativeLut3d { .. }
        | IrOp::NativeMaskMix { .. }
        | IrOp::NativeAcescct { .. }
        | IrOp::NativeSdrOutput
        | IrOp::NativeSdrVideoOutput
        | IrOp::NativeLuminanceMerge { .. }
        | IrOp::NativeBoundedMerge { .. }
        | IrOp::NativeArithmeticMerge { .. }
        | IrOp::Merge { .. }
        | IrOp::WipeMix { .. }
        | IrOp::PushMix { .. }
        | IrOp::LumaWipeMix { .. }
        | IrOp::Crop { .. }
        | IrOp::Resize { .. }
        | IrOp::ChannelSplit { .. }
        | IrOp::ChannelCombine
        | IrOp::Output { .. } => Threading::Any,

        // Decode / rasterize hold per-source state (rings, caches).
        IrOp::DecodeVideo { .. }
        | IrOp::NativeDecodeVideo { .. }
        | IrOp::DecodeStill { .. }
        | IrOp::NativeDecodeStill { .. }
        | IrOp::NativeMediaVideo { .. }
        | IrOp::NativeMediaStill { .. }
        | IrOp::RasterVector { .. }
        | IrOp::NativeRasterVector { .. }
        | IrOp::TextGen { .. }
        | IrOp::NativeTextGen { .. }
        | IrOp::NativeDecoratedTextGen { .. }
        | IrOp::CaptionOverlay { .. }
        | IrOp::NativeCaptionOutput { .. } => Threading::PerInstance,

        // Matte inference is a sequential CPU worker op.
        IrOp::MatteExtract { .. } | IrOp::NativeMatteExtract => Threading::Serial,

        // Temporal / field-order aware — must not run out of order.
        IrOp::Deinterlace { .. } => Threading::Serial,
    }
}

/// Sweep axis + orientation for a directional `WipeMix`/`PushMix` transition
/// (08 §2.0b), lowered from `photonic_core::timeline::WipeDirection`. The name
/// states the direction the incoming clip is revealed / pushed toward: e.g.
/// `LeftToRight` reveals the incoming from the left edge as the mix advances.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum WipeDirection {
    LeftToRight,
    RightToLeft,
    TopToBottom,
    BottomToTop,
}

/// Fully-resolved parameters for one frame's stabilization warp (22 §6.4).
///
/// Everything here is already reduced to numbers for the current tick, so the
/// evaluator never touches the motion series, the recipe, or the analysis
/// cache — which is what lets the warp be [`Threading::Any`] and lets the CPU
/// reference and the WGSL twin consume byte-identical inputs.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct StabilizeWarp {
    /// Row-major 3×3: a ray in the virtual (stabilized) camera to the same ray
    /// in the real camera that captured this frame.
    pub rotation: [f32; 9],
    /// Zoom about the frame centre, `>= 1.0`, hiding the edges the rotation
    /// swings out of frame.
    pub zoom: f32,
    /// Source intrinsics **normalized by frame size**: `[fx/w, fy/h, cx/w,
    /// cy/h]`.
    ///
    /// Normalized rather than in pixels so the op is resolution-independent.
    /// The evaluator multiplies by whatever the decoded texture actually is,
    /// which means a proxy preview and a full-resolution export describe the
    /// *same* geometry — with pixel intrinsics, a half-size proxy silently got
    /// a lens twice as long as the one it was calibrated against.
    pub intrinsics: [f32; 4],
    /// Kannala-Brandt radial coefficients `k1..k4`; all zero for a pinhole.
    pub k: [f32; 4],
    /// True for the fisheye projection, false for rectilinear.
    pub fisheye: bool,
    /// Leave uncovered pixels transparent instead of clamping to the source
    /// edge — [`StabilizationCropMode::TransparentEdges`].
    ///
    /// [`StabilizationCropMode::TransparentEdges`]: photonic_core::timeline::StabilizationCropMode::TransparentEdges
    pub transparent_edges: bool,
}

impl StabilizeWarp {
    /// The do-nothing warp for a frame of `width`×`height`.
    pub fn identity(width: f32, height: f32) -> Self {
        StabilizeWarp {
            rotation: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            zoom: 1.0,
            // A 90°-ish pinhole; irrelevant while the rotation is identity,
            // since unprojection and reprojection cancel exactly.
            intrinsics: [0.5, 0.5 * width / height.max(1.0), 0.5, 0.5],
            k: [0.0; 4],
            fisheye: false,
            transparent_edges: false,
        }
    }

    /// True when this warp would leave the image untouched, so the compiler can
    /// skip emitting the pass entirely.
    pub fn is_identity(&self) -> bool {
        const I: [f32; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        self.zoom == 1.0
            && self
                .rotation
                .iter()
                .zip(I.iter())
                .all(|(a, b)| (a - b).abs() < 1e-7)
    }

    /// False when a parameter would make the warp undefined — non-finite
    /// values, a non-positive focal length, or a zoom that shrinks.
    ///
    /// The counterpart of [`transform_matrix_is_valid`] for this op: an invalid
    /// warp renders transparent rather than sampling undefined coordinates.
    ///
    /// [`transform_matrix_is_valid`]: super::ops::transform_matrix_is_valid
    pub fn is_valid(&self) -> bool {
        self.rotation.iter().all(|v| v.is_finite())
            && self.intrinsics.iter().all(|v| v.is_finite())
            && self.k.iter().all(|v| v.is_finite())
            && self.zoom.is_finite()
            && self.zoom >= 1.0
            && self.intrinsics[0] > 0.0
            && self.intrinsics[1] > 0.0
    }
}

/// Exact straight-linear-Rec709 neighborhood rank semantics.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum NativeRankKind {
    Median = 0,
    DustScratches = 1,
}

/// Native artistic effects in signed encoded sRGB; only Posterize/Threshold are bounded.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum NativeTransferArtisticKind {
    Posterize = 0,
    Threshold = 1,
    Desaturate = 2,
    Invert = 3,
}

/// Fixed-canvas native AP1 stylization.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum NativeStylizeKind {
    Emboss = 0,
    FindEdges = 1,
    ChromaticAberration = 2,
}

/// Native straight-color detail combination after coverage-normalized Gaussian blur.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum NativeDetailKind {
    SmartSharpen = 0,
    HighPass = 1,
    Clarity = 2,
}

/// Supported fixed-canvas native inverse-sampling geometry.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum NativeWarpKind {
    Pinch = 0,
    Spherize = 1,
    Ripple = 2,
}

/// One frame-graph operation. Each op is one wgpu render/compute pass (or a
/// CPU worker-thread op where noted), with a CPU `eval_cpu` reference
/// implementation for export determinism and golden tests (02 §2).
#[derive(Clone, Debug, PartialEq)]
pub enum IrOp {
    // ── sources ────────────────────────────────────────────────────────────
    /// Decoded video frame at `src_time` (source-clock ticks, post trim/speed
    /// mapping). YUV→linear conversion + premultiply happen in this op's pass
    /// (03 §3.2/§3.3).
    DecodeVideo {
        asset: AssetId,
        src_time: Tick,
        proxy: bool,
    },
    /// Native broadcast source with explicit range, matrix and chroma siting.
    /// Decoding must retain the source's bit depth into scene-linear ACEScg.
    NativeDecodeVideo {
        asset: AssetId,
        src_time: Tick,
        input: photonic_core::timeline::color::NativeInputColorInterpretation,
    },
    /// Explicit display-referred sRGB still converted to linear AP1.
    NativeDecodeStill {
        asset: AssetId,
    },
    /// Independent graph media. Native extent is retained only when an explicit
    /// canvas-sized Resize consumes a differently sized source.
    NativeMediaVideo {
        asset: AssetId,
        src_time: Tick,
        input: photonic_core::timeline::color::NativeInputColorInterpretation,
        w: u32,
        h: u32,
        native_extent: bool,
    },
    NativeMediaStill {
        asset: AssetId,
        w: u32,
        h: u32,
        native_extent: bool,
    },
    /// Decoded still image (cached per asset).
    DecodeStill {
        asset: AssetId,
    },
    /// Rasterized vector content (Tier A CPU-readback or Tier B GPU-direct,
    /// 03 §2.5), cached by `doc_state`.
    /// Native vector source: explicitly premultiplied linear Rec.709 raster,
    /// converted once to AP1 by the evaluator. Separate from Legacy uploads.
    NativeRasterVector {
        vref: VectorRef,
        doc_state: VectorStateKey,
        w: u32,
        h: u32,
    },
    RasterVector {
        vref: VectorRef,
        doc_state: VectorStateKey,
        w: u32,
        h: u32,
    },
    SolidColor {
        color: LinearColor,
    },

    // ── ops (unary unless stated) ──────────────────────────────────────────
    Transform2D {
        mat: Mat3,
        sampling: Sampling,
    },
    /// Native-managed transform with transparent pixels outside source bounds.
    /// Legacy transforms retain their original edge-clamp behavior.
    Transform2DTransparent {
        mat: Mat3,
        sampling: Sampling,
    },
    /// D-12 gyro stabilization warp (22 §6.4): undistort each output pixel to a
    /// calibrated ray, rotate it into the real camera's frame, reproject, and
    /// resample.
    ///
    /// Sits **beneath** [`IrOp::Transform2D`] in the clip chain, matching the
    /// render order 22 §6.4 fixes: lens undistort, orientation warp, resample
    /// source, *then* the ordinary clip transform, effects and grade. It
    /// corrects what the camera did; the clip transform is what the editor
    /// chose, and the two must not be conflated.
    ///
    /// Params are fully resolved for the current tick by the compiler — the
    /// evaluator stays time-ignorant.
    StabilizeWarp {
        warp: StabilizeWarp,
        sampling: Sampling,
    },
    /// Effect pass; arity 0..N comes from the `EffectKind` registry entry
    /// (0-input = generator, e.g. `MaskShapeGen` — 08 §3).
    Effect {
        kind: EffectKind,
        params: ResolvedParams,
    },
    /// Serial corrector stack (07). Params are the keyframe-resolved form of
    /// the authoring `GradeOp`.
    Grade {
        ops: Vec<ResolvedGradeOp>,
    },
    /// Endpoint-complete softened analytic map in premultiplied AP1.
    NativeLumaWipe {
        kind: super::luma_wipe::LumaWipeKind,
        softness: f32,
        invert: bool,
        t: f32,
    },
    /// Convex interpolation of two premultiplied AP1 images, including alpha.
    /// Inputs are incoming, outgoing; t=0 is outgoing and t=1 is incoming.
    NativeDissolve {
        t: f32,
    },
    /// Authored picture dimensions at the native output boundary. Identity on
    /// pixels; supplies the basis for Draft transforms and spatial effects.
    NativeCanvas {
        w: u32,
        h: u32,
    },
    /// Explicit bounded straight-AP1 inversion: (1-clamp(rgb/alpha,0,1))*alpha.
    NativeInvert,
    /// Image to unassociated AP1 component or alpha signal.
    NativeChannelExtract {
        channel: Channel,
    },
    /// Image to AP1 scene luminance, equivalent to XYZ Y, without clipping.
    NativeImageToMask,
    /// Scalar signal to opaque AP1 grayscale image at a typed port boundary.
    NativeMaskToImage,
    /// Pinned offline subject inference on a bounded asynchronous worker.
    NativeMatteExtract,
    /// Four unassociated signals to premultiplied AP1; clamp only alpha.
    NativeChannelCombine,
    /// Smooth radial AP1 gain; signed extended RGB and source alpha retained.
    NativeVignette {
        amount: f32,
        feather: f32,
    },
    /// Deterministic signed scene-linear noise at authored pixel centers.
    NativeGrain {
        amount: f32,
        monochrome: bool,
        seed: u32,
        pixel_scale: [f32; 2],
    },
    /// Bounded alpha-aware bilateral smoothing in straight AP1 range coordinates.
    NativeDenoise {
        strength: f32,
        spatial_scale: f32,
    },
    /// Bounded display-authored captions applied after the managed output transform.
    NativeCaptionOutput {
        cue_batch: CaptionBatch,
        video: bool,
        paints: Vec<CaptionPaint>,
    },
    NativeDecoratedTextGen {
        block: ResolvedTextBlock,
        decoration: CaptionDecoration,
    },
    /// Glyphon linear-sRGB title raster explicitly converted into AP1.
    NativeTextGen {
        block: ResolvedTextBlock,
    },
    /// Zero-input AP1 white shape generator with normalized geometry.
    NativeMaskShape {
        rect: bool,
        center: [f32; 2],
        size: [f32; 2],
        rotation: f32,
        feather: f32,
    },
    /// Static normalized polygon; even-odd fill and inward local-space feather.
    NativePolygonMask {
        vertices: Vec<[f32; 2]>,
        center: [f32; 2],
        size: [f32; 2],
        rotation: f32,
        feather: f32,
    },
    /// AP1 bright extraction, native Gaussian convolution and tinted screen-add.
    NativeGlow {
        sigma: f32,
        threshold: f32,
        intensity: f32,
        tint: [f32; 3],
    },
    /// AP1 RGB-distance chroma key with dominant-channel spill suppression.
    NativeChromaKey {
        key: [f32; 3],
        tolerance: f32,
        softness: f32,
        spill: f32,
    },
    /// AP1 luminance key; multiplies premultiplied RGBA by a smooth keep factor.
    NativeLumaKey {
        threshold: f32,
        softness: f32,
        invert: bool,
    },
    /// Coverage-preserving straight-color unsharp mask in scene-linear AP1.
    NativeUnsharp {
        amount: f32,
        sigma: f32,
    },
    /// Radius-independent coverage outline using authored-pixel jump flooding.
    NativeOutline {
        thickness: f32,
        color: [f32; 3],
        opacity: f32,
        pixel_scale: [f32; 2],
    },
    /// Catalog LinearStraight HSL with a continuous signed/HDR envelope.
    NativeLinearHsl {
        vibrance: bool,
        controls: [f32; 3],
    },
    /// Discrete circular aperture average of premultiplied AP1 RGBA.
    NativeLensBlur {
        radius: f32,
        pixel_scale: [f32; 2],
    },
    /// Exact authored-tap bilateral Surface Blur with an explicit work guard.
    NativeSurfaceBlur {
        radius: u32,
        threshold: f32,
        pixel_scale: [f32; 2],
    },
    /// Exact independent straight-linear709 rank with authored pixel footprint.
    NativeRankFilter {
        kind: NativeRankKind,
        radius: u32,
        threshold: u8,
        pixel_scale: [f32; 2],
    },
    /// Diagnostic numeric RGB/alpha with retained coverage, matching the float utility.
    /// This does not change the associated working-storage convention.
    NativeUnpremultiply,
    /// Analysis-resolved bounded perceptual correction with linear HDR tails.
    NativeDeflicker {
        gain: [f32; 3],
        band: [f32; 3],
    },
    /// Diagnostic alpha/associated/straight RGB view in working AP1.
    NativeAlphaView {
        mode: u8,
    },
    /// Catalog levels with bounded gamma and linear signed/headroom tails.
    NativeTransferLevels {
        black_white: [f32; 2],
        gamma: f32,
        output: [f32; 2],
    },
    /// Catalog five-knot transfer-space Hermite curve with extended endpoints.
    NativeTransferCurves {
        knots: [[f32; 2]; 5],
        contrast: f32,
    },
    /// Extended transfer-space tonal shifts with original source coverage.
    NativeColorBalance {
        shifts: [[f32; 3]; 3],
        preserve: bool,
    },
    /// Extended signed encoded-sRGB tint with optional encoded-luma preservation.
    NativePhotoFilter {
        color: [f32; 3],
        density: f32,
        preserve: bool,
    },
    /// Extended signed encoded-sRGB matrix with AP1 adaptation and original alpha.
    NativeTransferMatrix {
        matrix: [[f32; 3]; 3],
    },
    /// AP1 -> encoded sRGB artistic function -> AP1, original coverage.
    /// Posterize/Threshold are bounded; Desaturate preserves extended range.
    NativeTransferArtistic {
        kind: NativeTransferArtisticKind,
        parameter: f32,
    },
    /// Shifted, equal-weight blurred alpha under the original AP1 image.
    NativeDropShadow {
        offset: [f32; 2],
        radius: u32,
        color: [f32; 3],
        opacity: f32,
        pixel_scale: [f32; 2],
    },
    /// Fixed-canvas projective inverse in normalized pixel-center coordinates.
    NativePerspective {
        inverse: [f32; 9],
    },
    /// Coverage-preserving AP1 stylization in authored pixel coordinates.
    NativeStylize {
        kind: NativeStylizeKind,
        amount: f32,
        pixel_scale: [f32; 2],
    },
    /// Source-coverage-preserving AP1 detail combination; sigma is authored pixels,
    /// except Clarity derives sigma from authored canvas dimensions.
    NativeDetail {
        kind: NativeDetailKind,
        amount: f32,
        sigma: f32,
        threshold: f32,
        pixel_scale: [f32; 2],
    },
    /// Fixed-canvas authored-coordinate inverse warp in premultiplied AP1.
    NativeGeometricWarp {
        kind: NativeWarpKind,
        amount: f32,
        wavelength: f32,
        pixel_scale: [f32; 2],
    },
    /// Equal-weight separable convolution of premultiplied AP1 RGBA.
    NativeBoxBlur {
        radius: [u32; 2],
    },
    /// Origin-anchored average cells of premultiplied AP1 RGBA.
    NativeMosaic {
        block: [u32; 2],
    },
    /// Bounded directional line convolution in premultiplied AP1. The displacement
    /// is in authored pixels; logical edges clamp, padding stays transparent.
    NativeMotionBlur {
        delta: [f32; 2],
    },
    /// Gaussian convolution of premultiplied scene-linear AP1 RGBA; no clipping or transfer.
    NativeGaussianBlur {
        sigma: f32,
    },
    /// Scene-linear ACEScg exposure, distinct from the legacy grade stack.
    NativeExposure {
        stops: f32,
    },
    /// Unclamped scene-linear RGB offset, applied to straight color and then
    /// premultiplied by the original alpha.
    NativeLinearOffset {
        rgb: [f32; 3],
    },
    /// Twelve points are one stop on each ACEScg channel.
    NativePrinterLights {
        points: [f32; 3],
    },
    /// Scene-linear highlight compression with a shared RGB scale.
    NativeHighlightRolloff {
        knee: f32,
        strength: f32,
    },
    /// Blend corrected/original images through a sequence-coordinate window.
    NativeMaskMix {
        mask: photonic_render::grade::ResolvedMask,
    },
    /// Creative LUT in explicitly declared ACEScg/ACEScct coordinates.
    NativeLut3d {
        lut: photonic_render::grade::ResolvedLut3d,
    },
    /// Master/RGB curves in ACEScct with endpoint tangent extrapolation.
    NativeLogCurves {
        curves: Box<photonic_render::grade::ResolvedCurves>,
    },
    /// HSL key in bounded ACEScct/AP1 coordinates and no-clamp log CDL.
    NativeLogQualifier {
        qualifier: Box<photonic_render::grade::ResolvedHslQualifier>,
    },
    /// Primitive unassociated geometry key; one image input supplies dimensions.
    BezierMatte {
        window: photonic_core::timeline::BezierWindow,
    },
    WindowMatte {
        mask: photonic_render::grade::ResolvedMask,
    },
    /// Key-only output; input is ACEScct for native grading, Legacy linear otherwise.
    QualifierMatte {
        qualifier: Box<photonic_render::grade::ResolvedHslQualifier>,
        mask: Option<photonic_render::grade::ResolvedMask>,
        native: bool,
    },
    GradeKeyMix {
        mode: photonic_core::timeline::GradeKeyMixMode,
    },
    /// Inputs: corrected image, original image, unassociated matte.
    GradeMatteRefine {
        refinement: photonic_core::timeline::GradeMatteRefinement,
    },
    GradeMatteApply,
    /// Scene AP1 image + unassociated key; fused logarithmic CDL application.
    NativeMatteCdl {
        cdl: photonic_render::grade::ResolvedCdl,
    },
    NativeTonalExposure {
        tonal: photonic_render::tonal_exposure::ResolvedTonalExposure,
    },
    NativeRgbMatrix {
        rows: [[f32; 3]; 3],
    },
    GradeMatteConstant {
        weight: f32,
    },
    /// Interpolate grade branches while preserving the bottom image's coverage.
    GradeLayerMix {
        opacity: f32,
    },
    /// No-clamp CDL in ACEScct; negative SOP values pass through power.
    NativeLogCdl {
        cdl: photonic_render::grade::ResolvedCdl,
    },
    /// ACEScct pivot/code value and log2 contrast slope.
    NativeLogContrast {
        pivot: f32,
        amount: f32,
    },
    /// Scene-linear AP1 luminance/chroma adjustment, independent of Legacy Rec.709 semantics.
    NativeSaturationVibrance {
        saturation: f32,
        vibrance: f32,
    },
    NativeAcescct {
        direction: AcescctDirection,
    },
    /// ACES 2-style 100-nit SDR view transform. Converts scene-linear ACEScg
    /// into premultiplied sRGB display code values; not a creative grade.
    NativeSdrOutput,
    /// Same 100-nit SDR rendering, encoded as a BT.709 video signal for a
    /// dedicated managed export path. Never feed this to the GUI presenter.
    NativeSdrVideoOutput,
    /// Binary over-composite with full 26-blend-mode support via
    /// COMPOSITE_SHADER (03 §2.4). Inputs: a (top), b (bottom).
    Merge {
        mode: BlendMode,
        opacity: f32,
    },
    /// Native AP1-only arithmetic source-over. Straight RGB equations for
    /// LinearDodge/Burn/Subtract/LinearLight are B+S, B+S-1, B-S, B+2S-1.
    /// No unit-range clipping; ordinary source-over alpha and RGBA16F bounds.
    NativeArithmeticMerge {
        mode: BlendMode,
        opacity: f32,
    },
    /// Bounded artistic overlap: clamp straight AP1 operands to 0..1 for the
    /// blend function only; source-over uses original extended RGB and alpha.
    NativeBoundedMerge {
        mode: BlendMode,
        opacity: f32,
    },
    /// Select the whole straight RGB with lower/higher AP1 Y, then source-over.
    /// Exact luminance ties choose backdrop. Signed working values are retained.
    NativeLuminanceMerge {
        mode: BlendMode,
        opacity: f32,
    },
    /// Directional wipe transition (08 §2.0b): a per-pixel `smoothstep` edge
    /// sweeping across `direction` at normalised position `t`, with a `softness`
    /// half-width feather; a premultiplied lerp between the two layers. Inputs:
    /// [incoming, outgoing]. `t` is the compile-time eased mix factor, so distinct
    /// ticks yield distinct content hashes (like the cross-dissolve). At `t == 0`
    /// the output equals `outgoing`; at `t == 1` it equals `incoming`.
    WipeMix {
        direction: WipeDirection,
        softness: f32,
        t: f32,
    },
    /// Directional push transition (08 §2.0b): both layers translate along
    /// `direction` by `t`, the incoming sliding in as the outgoing slides out,
    /// with `ops::transform2d` edge-clamp/pixel-center sampling. Inputs:
    /// [incoming, outgoing]. `t == 0` is `outgoing`, `t == 1` is `incoming`.
    PushMix {
        direction: WipeDirection,
        t: f32,
    },
    /// Analytical luma-map wipe (26 K-B7): per-pixel switch time from a
    /// Photonic-authored map (`kind`), mixed with `soft_mix(t, m, softness)`.
    /// Inputs: [incoming, outgoing]. `t == 0` is `outgoing`, `t == 1` is
    /// `incoming`. Invert flips the map so white switches first.
    LumaWipeMix {
        kind: crate::graph::luma_wipe::LumaWipeKind,
        softness: f32,
        invert: bool,
        t: f32,
    },
    CaptionOverlay {
        cue_batch: CaptionBatch,
    },
    /// Crop margins in normalized source coordinates. The output keeps the
    /// current canvas size; pixels inside the margins are transparent.
    Crop {
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    },
    Resize {
        w: u32,
        h: u32,
        fit: FitMode,
    },
    /// U²-Net matte inference via photonic-matte — CPU worker-thread op, NOT a
    /// GPU pass; cached aggressively (08 §3 "slow node").
    MatteExtract {
        model: MatteModel,
    },
    /// Styled-text raster for graph `Text` nodes (08 §3); glyphon pipeline.
    TextGen {
        block: ResolvedTextBlock,
    },
    /// Image → single-channel Mask.
    ChannelSplit {
        channel: Channel,
    },
    /// 3-4 Mask inputs → Image.
    ChannelCombine,
    /// Deinterlace (K-G6): convert interlaced fields to progressive. Unary
    /// input (current frame). Declares source-range `[out−1, out+1]` so
    /// temporal algorithms can wire neighbouring fields later (E-1). Not an
    /// effect — lives on the IR contract because it needs temporal access.
    Deinterlace {
        method: DeinterlaceMethod,
        field_order: FieldOrder,
    },
    Output {
        w: u32,
        h: u32,
    },
}

/// One node in a compiled frame graph.
#[derive(Clone, Debug, PartialEq)]
pub struct IrNode {
    pub op: IrOp,
    /// Inputs in op-defined order (e.g. Merge: [a/top, b/bottom]), each
    /// addressing a producing node's output port.
    pub inputs: Vec<(IrNodeId, OutPort)>,
    /// Cache identity: hash(op, resolved params, input hashes). Computed at
    /// compile time (02 §2).
    pub content_hash: ContentHash,
}

/// A compiled, topologically-ordered frame graph for one (sequence, format,
/// tick, quality) tuple. Arena indices are compile-local; caching is by
/// [`ContentHash`].
#[derive(Clone, Debug, PartialEq, Default)]
pub struct FrameGraph {
    /// Color interpretation of premultiplied working textures. Existing graph
    /// construction defaults to Legacy SDR; managed graphs must opt in.
    pub working_color_domain: WorkingColorDomain,
    /// Topological order: every node's inputs precede it.
    pub nodes: Vec<IrNode>,
    /// The terminal `Output` node.
    pub output: Option<IrNodeId>,
}

impl FrameGraph {
    /// Ancestors demanded by presentation and its optional independent scope tap.
    pub fn demanded_nodes(&self, tap: Option<IrNodeId>) -> Vec<bool> {
        let mut needed = vec![false; self.nodes.len()];
        let mut pending: Vec<_> = self.output.into_iter().chain(tap).collect();
        while let Some(id) = pending.pop() {
            let index = id.0 as usize;
            if index >= needed.len() || needed[index] {
                continue;
            }
            needed[index] = true;
            pending.extend(self.nodes[index].inputs.iter().map(|(id, _)| *id));
        }
        needed
    }

    /// Distinct native decoder clocks. Equal asset/time requests share one lane,
    /// even when their downstream input interpretations differ.
    pub fn native_decode_requests(&self, tap: Option<IrNodeId>) -> Vec<(AssetId, Tick)> {
        let needed = self.demanded_nodes(tap);
        let mut requests: Vec<_> = self
            .nodes
            .iter()
            .zip(needed)
            .filter_map(|(node, needed)| {
                if !needed {
                    return None;
                }
                match node.op {
                    IrOp::NativeDecodeVideo {
                        asset, src_time, ..
                    }
                    | IrOp::NativeMediaVideo {
                        asset, src_time, ..
                    } => Some((asset, src_time)),
                    _ => None,
                }
            })
            .collect();
        requests.sort_by_key(|(asset, tick)| (asset.0.as_u128(), tick.0));
        requests.dedup();
        requests
    }

    pub fn output_color_encoding(&self) -> Result<FrameColorEncoding, &'static str> {
        let domains = self.color_domains()?;
        self.output
            .and_then(|id| domains.get(id.0 as usize).copied())
            .ok_or("color-domain output is not topologically available")
    }

    /// Color interpretation at an intermediate graph tap. Validate the whole
    /// graph first so a tap cannot appear qualified inside an invalid graph.
    pub fn node_color_encoding(&self, id: IrNodeId) -> Result<FrameColorEncoding, &'static str> {
        self.color_domains()?
            .get(id.0 as usize)
            .copied()
            .ok_or("color-domain tap is not topologically available")
    }

    /// Validate the color domain at every graph edge. A legacy grade cannot
    /// consume ACEScg or ACEScct; scene exposure runs before log encoding;
    /// ACEScct decoding restores scene-linear ACEScg.
    pub fn validate_working_color_domain(&self) -> Result<(), &'static str> {
        self.color_domains().map(|_| ())
    }

    /// Logical source-pixel dimensions for explicitly native-resolution paths.
    /// Only qualified operations may retain these dimensions before
    /// an explicit Resize normalizes the graph. Count every source/intermediate
    /// allocation, including duplicate hashes, against the resident texel bound.
    pub(crate) fn native_processing_extents(
        &self,
    ) -> Result<Vec<Option<(u32, u32)>>, &'static str> {
        let mut extents: Vec<Option<(u32, u32)>> = Vec::with_capacity(self.nodes.len());
        let mut texels = 0_u64;
        for node in &self.nodes {
            let mut incoming = None;
            for (id, _) in &node.inputs {
                let extent = extents
                    .get(id.0 as usize)
                    .copied()
                    .ok_or("color-domain input is not topologically available")?;
                incoming = incoming.or(extent);
            }
            let extent = match node.op {
                IrOp::NativeMediaVideo { w, h, native_extent: true, .. }
                | IrOp::NativeMediaStill { w, h, native_extent: true, .. } => {
                    if w == 0 || h == 0 || w > 8192 || h > 8192 {
                        return Err("native-resolution MediaIn requires bounded source dimensions");
                    }
                    Some((w, h))
                }
                IrOp::NativeInvert | IrOp::NativeChromaKey { .. } | IrOp::NativeLumaKey { .. }
                | IrOp::NativeGaussianBlur { .. } | IrOp::NativeUnsharp { .. } | IrOp::NativeGlow { .. }
                    if incoming.is_some() && node.inputs.len() == 1 => incoming,
                IrOp::Merge { .. }
                    if incoming.is_some() && node.inputs.len() == 2
                        && node.inputs.iter().all(|(id, _)| extents[id.0 as usize] == incoming) => incoming,
                IrOp::Resize { .. } => None,
                _ if incoming.is_some() => return Err(
                    "native-resolution processing requires an explicit Resize before this operation"
                ),
                _ => None,
            };
            if let Some((w, h)) = extent {
                let (bw, bh) = TextureDesc {
                    width: w,
                    height: h,
                }
                .bucket();
                // Conservatively include all same-sized scratch textures,
                // even when identity parameters take a cheaper fast path.
                let allocations = match node.op {
                    IrOp::NativeGaussianBlur { .. } => 4,
                    IrOp::NativeUnsharp { .. } => 5,
                    IrOp::NativeGlow { .. } => 6,
                    _ => 1,
                };
                texels = texels.saturating_add(u64::from(bw) * u64::from(bh) * allocations);
                if texels > 16_777_216 {
                    return Err(
                        "native mixed-resolution graph exceeds its padded-extent-texel budget",
                    );
                }
            }
            extents.push(extent);
        }
        if self
            .output
            .and_then(|id| extents.get(id.0 as usize))
            .is_some_and(Option::is_some)
        {
            return Err("native-resolution processing requires an explicit Resize before output");
        }
        Ok(extents)
    }

    fn color_domains(&self) -> Result<Vec<FrameColorEncoding>, &'static str> {
        use FrameColorEncoding as Domain;
        let root = match self.working_color_domain {
            WorkingColorDomain::LegacyLinearRec709 => Domain::LegacyLinearRec709,
            WorkingColorDomain::SceneLinearAcescg => Domain::SceneLinearAcescg,
        };
        self.native_processing_extents()?;
        let mut domains = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            let mut inputs = node.inputs.iter().map(|(id, _)| {
                domains
                    .get(id.0 as usize)
                    .copied()
                    .ok_or("color-domain input is not topologically available")
            });
            let input = inputs.next().transpose()?.unwrap_or(root);
            for (index, other) in inputs.enumerate() {
                let expected = if (matches!(node.op, IrOp::GradeMatteApply) && index == 1)
                    || (matches!(node.op, IrOp::NativeMatteCdl { .. }) && index == 0)
                {
                    Domain::MatteWeight
                } else {
                    input
                };
                if other? != expected {
                    return Err("graph operation mixes different color domains");
                }
            }
            if input == Domain::ScalarChannel
                && !matches!(
                    node.op,
                    IrOp::NativeMaskToImage | IrOp::NativeChannelCombine
                )
            {
                return Err("scalar channel signals require an explicit image conversion");
            }
            if input == Domain::MatteWeight
                && !matches!(
                    node.op,
                    IrOp::GradeKeyMix { .. } | IrOp::GradeMatteRefine { .. }
                )
            {
                return Err("matte weights cannot enter an image operation");
            }
            if matches!(input, Domain::SrgbDisplay | Domain::Bt709Video)
                && !matches!(node.op, IrOp::NativeCaptionOutput { .. })
            {
                return Err("display-encoded pixels cannot enter another graph operation");
            }
            if input == Domain::Acescct
                && !matches!(
                    node.op,
                    IrOp::NativeAcescct {
                        direction: AcescctDirection::Decode
                    } | IrOp::Grade { .. }
                        | IrOp::NativeExposure { .. }
                        | IrOp::NativeLinearOffset { .. }
                        | IrOp::NativePrinterLights { .. }
                        | IrOp::NativeHighlightRolloff { .. }
                        | IrOp::NativeSaturationVibrance { .. }
                        | IrOp::NativeLogContrast { .. }
                        | IrOp::NativeLogCdl { .. }
                        | IrOp::WindowMatte { .. }
                        | IrOp::BezierMatte { .. }
                        | IrOp::QualifierMatte { .. }
                        | IrOp::NativeLogQualifier { .. }
                        | IrOp::GradeMatteApply
                        | IrOp::NativeLogCurves { .. }
                        | IrOp::NativeLut3d { .. }
                        | IrOp::NativeMaskMix { .. }
                        | IrOp::NativeSdrOutput
                        | IrOp::NativeSdrVideoOutput
                )
            {
                return Err("operation is not qualified in the ACEScct grading domain");
            }
            let output = match node.op {
                IrOp::BezierMatte { ref window } => {
                    if node.inputs.len() != 1
                        || !matches!(
                            input,
                            Domain::LegacyLinearRec709
                                | Domain::SceneLinearAcescg
                                | Domain::Acescct
                        )
                    {
                        return Err("Bezier matte requires one working image input");
                    }
                    window.validate()?;
                    Domain::MatteWeight
                }
                IrOp::WindowMatte { ref mask } => {
                    if node.inputs.len() != 1
                        || !matches!(
                            input,
                            Domain::LegacyLinearRec709
                                | Domain::SceneLinearAcescg
                                | Domain::Acescct
                        )
                    {
                        return Err("window matte requires one working image input");
                    }
                    photonic_render::grade_gpu::validate_native_mask(mask)?;
                    Domain::MatteWeight
                }
                IrOp::QualifierMatte {
                    native,
                    ref qualifier,
                    ref mask,
                } => {
                    if node.inputs.len() != 1
                        || input
                            != if native {
                                Domain::Acescct
                            } else {
                                Domain::LegacyLinearRec709
                            }
                    {
                        return Err("qualifier matte requires its declared image domain");
                    }
                    photonic_render::native_transfer::validate_native_qualifier(qualifier)?;
                    if let Some(mask) = mask {
                        photonic_render::grade_gpu::validate_native_mask(mask)?;
                    }
                    Domain::MatteWeight
                }
                IrOp::GradeMatteConstant { weight } => {
                    if !node.inputs.is_empty()
                        || !weight.is_finite()
                        || !(0.0..=1.0).contains(&weight)
                    {
                        return Err("constant matte requires a finite weight and no inputs");
                    }
                    Domain::MatteWeight
                }
                IrOp::GradeMatteRefine { refinement } => {
                    if node.inputs.len() != 1 || input != Domain::MatteWeight {
                        return Err("matte refinement requires one matte input");
                    }
                    refinement.validate()?;
                    Domain::MatteWeight
                }
                IrOp::GradeKeyMix { .. } => {
                    if node.inputs.len() != 2 || input != Domain::MatteWeight {
                        return Err("key mixer requires two mattes");
                    }
                    Domain::MatteWeight
                }
                IrOp::GradeMatteApply | IrOp::GradeLayerMix { .. } => {
                    let arity = if matches!(node.op, IrOp::GradeMatteApply) {
                        3
                    } else {
                        2
                    };
                    if node.inputs.len() != arity
                        || !matches!(
                            input,
                            Domain::LegacyLinearRec709 | Domain::SceneLinearAcescg
                        ) && !(matches!(node.op, IrOp::GradeMatteApply)
                            && input == Domain::Acescct)
                    {
                        return Err("grade image mixer requires image inputs and a typed matte when applicable");
                    }
                    if let IrOp::GradeLayerMix { opacity } = node.op {
                        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
                            return Err("grade opacity must be finite within 0..=1");
                        }
                    }
                    input
                }

                IrOp::DecodeVideo { .. } if root != Domain::LegacyLinearRec709 => {
                    return Err("legacy video source cannot enter a managed graph");
                }
                IrOp::DecodeStill { .. } | IrOp::RasterVector { .. } | IrOp::TextGen { .. }
                    if root == Domain::SceneLinearAcescg =>
                {
                    return Err("display-authored source requires a managed input transform");
                }
                IrOp::Effect { .. } if input == Domain::SceneLinearAcescg => {
                    return Err("legacy effect is not qualified in scene-linear ACEScg");
                }
                IrOp::CaptionOverlay { .. } if input == Domain::SceneLinearAcescg => {
                    return Err("display-authored captions require a managed output stage");
                }
                IrOp::NativeLuminanceMerge { mode, opacity } => {
                    if root != Domain::SceneLinearAcescg
                        || input != Domain::SceneLinearAcescg
                        || node.inputs.len() != 2
                        || !native_luminance_blend(mode)
                        || !opacity.is_finite()
                        || !(0.0..=1.0).contains(&opacity)
                    {
                        return Err("native luminance merge requires two AP1 inputs, a whole-color mode and finite opacity in 0..=1");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeBoundedMerge { mode, opacity } => {
                    if root != Domain::SceneLinearAcescg
                        || input != Domain::SceneLinearAcescg
                        || node.inputs.len() != 2
                        || !native_bounded_blend(mode)
                        || !opacity.is_finite()
                        || !(0.0..=1.0).contains(&opacity)
                    {
                        return Err("native bounded merge requires two AP1 inputs, a bounded artistic mode and finite opacity in 0..=1");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeArithmeticMerge { mode, opacity } => {
                    if root != Domain::SceneLinearAcescg
                        || input != Domain::SceneLinearAcescg
                        || node.inputs.len() != 2
                        || !native_arithmetic_blend(mode)
                        || !opacity.is_finite()
                        || !(0.0..=1.0).contains(&opacity)
                    {
                        return Err("native arithmetic merge requires two AP1 inputs, a qualified arithmetic mode and finite opacity in 0..=1");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::Merge { mode, opacity } if input == Domain::SceneLinearAcescg => {
                    if node.inputs.len() != 2
                        || !opacity.is_finite()
                        || !(0.0..=1.0).contains(&opacity)
                    {
                        return Err("native merge requires two scene-linear AP1 inputs and finite opacity in 0..=1");
                    }
                    if !native_blend_supported(mode)
                        || native_arithmetic_blend(mode)
                        || native_bounded_blend(mode)
                        || native_luminance_blend(mode)
                    {
                        return Err("blend mode is not qualified in scene-linear ACEScg");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeRasterVector { vref, w, h, .. } => {
                    if root != Domain::SceneLinearAcescg
                        || !node.inputs.is_empty()
                        || vref != VectorRef::WholeDocument
                        || w == 0
                        || h == 0
                        || w > 8192
                        || h > 8192
                        || u64::from(w) * u64::from(h) > 16_777_216
                    {
                        return Err("native vector requires a bounded whole-document source in a managed graph");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeMediaVideo {
                    ref input, w, h, ..
                } => {
                    if root != Domain::SceneLinearAcescg
                        || !node.inputs.is_empty()
                        || w == 0
                        || h == 0
                        || w > 16384
                        || h > 16384
                        || input
                            .validate_asset_kind(photonic_core::timeline::AssetKind::Video)
                            .is_err()
                    {
                        return Err("native MediaIn video requires explicit input and bounded source dimensions");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeMediaStill { w, h, .. } => {
                    if root != Domain::SceneLinearAcescg
                        || !node.inputs.is_empty()
                        || w == 0
                        || h == 0
                        || w > 16384
                        || h > 16384
                    {
                        return Err("native MediaIn still requires bounded source dimensions");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeDecodeStill { .. } => {
                    if root != Domain::SceneLinearAcescg || !node.inputs.is_empty() {
                        return Err("native still requires a managed graph and no inputs");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeDecodeVideo { ref input, .. } => {
                    if !node.inputs.is_empty() {
                        return Err("native video source cannot consume graph inputs");
                    }
                    if root != Domain::SceneLinearAcescg
                        || input
                            .validate_asset_kind(photonic_core::timeline::AssetKind::Video)
                            .is_err()
                    {
                        return Err("native video source requires a valid managed ACEScg input");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::Grade { .. } if input != Domain::LegacyLinearRec709 => {
                    return Err("legacy grade operation cannot run in scene-linear ACEScg");
                }
                IrOp::Transform2DTransparent { mat, .. } if input == Domain::SceneLinearAcescg => {
                    if node.inputs.len() != 1 || !mat.is_finite() || mat.determinant().abs() < 1e-12
                    {
                        return Err("native transform requires one AP1 input and a finite invertible matrix");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::Crop {
                    left,
                    top,
                    right,
                    bottom,
                } if input == Domain::SceneLinearAcescg => {
                    if node.inputs.len() != 1
                        || [left, top, right, bottom]
                            .iter()
                            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
                    {
                        return Err(
                            "native crop requires one AP1 input and finite margins in 0..=1",
                        );
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::WipeMix { t, softness, .. } if input == Domain::SceneLinearAcescg => {
                    if node.inputs.len() != 2
                        || !t.is_finite()
                        || !(0.0..=1.0).contains(&t)
                        || !softness.is_finite()
                        || !(0.0..=0.5).contains(&softness)
                    {
                        return Err(
                            "native wipe requires two AP1 images, mix 0..=1 and softness 0..=0.5",
                        );
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::LumaWipeMix { .. } if input == Domain::SceneLinearAcescg => {
                    return Err("native luma wipe requires its endpoint-complete progress mapping");
                }
                IrOp::NativeLumaWipe { t, softness, .. } => {
                    if input != Domain::SceneLinearAcescg
                        || node.inputs.len() != 2
                        || !t.is_finite()
                        || !(0.0..=1.0).contains(&t)
                        || !softness.is_finite()
                        || !(0.0..=0.5).contains(&softness)
                    {
                        return Err("native luma wipe requires two AP1 images, mix 0..=1 and softness 0..=0.5");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::PushMix { t, .. } if input == Domain::SceneLinearAcescg => {
                    if node.inputs.len() != 2 || !t.is_finite() || !(0.0..=1.0).contains(&t) {
                        return Err("native push requires two AP1 images and finite mix in 0..=1");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeDissolve { t } => {
                    if node.inputs.len() != 2
                        || input != Domain::SceneLinearAcescg
                        || !t.is_finite()
                        || !(0.0..=1.0).contains(&t)
                    {
                        return Err(
                            "native dissolve requires two AP1 images and finite mix in 0..=1",
                        );
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::Resize { w, h, .. } if input == Domain::SceneLinearAcescg => {
                    if node.inputs.len() != 1
                        || w == 0
                        || h == 0
                        || w > 8192
                        || h > 8192
                        || u64::from(w) * u64::from(h) > 16_777_216
                    {
                        return Err(
                            "native resize requires one AP1 image and bounded nonzero dimensions",
                        );
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeCanvas { w, h } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || w == 0
                        || h == 0
                    {
                        return Err("native canvas requires one AP1 image and nonzero dimensions");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeLumaKey {
                    threshold,
                    softness,
                    ..
                } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || !threshold.is_finite()
                        || !(0.0..=1.0).contains(&threshold)
                        || !softness.is_finite()
                        || !(0.0..=1.0).contains(&softness)
                    {
                        return Err("native luma key requires one AP1 image and threshold/softness in 0..=1");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeChromaKey {
                    key,
                    tolerance,
                    softness,
                    spill,
                } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || key
                            .iter()
                            .any(|v| !v.is_finite() || !(-16.0..=16.0).contains(v))
                        || [tolerance, softness, spill]
                            .iter()
                            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
                    {
                        return Err("native chroma key requires one AP1 image, bounded finite key and controls in 0..=1");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeGlow {
                    sigma,
                    threshold,
                    intensity,
                    tint,
                } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || !sigma.is_finite()
                        || !(0.0..=500.0).contains(&sigma)
                        || !threshold.is_finite()
                        || !(0.0..=1.0).contains(&threshold)
                        || !intensity.is_finite()
                        || !(0.0..=10.0).contains(&intensity)
                        || tint
                            .iter()
                            .any(|v| !v.is_finite() || !(-16.0..=16.0).contains(v))
                    {
                        return Err("native glow requires one AP1 image, sigma 0..500, threshold 0..1, intensity 0..10 and finite bounded tint");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeMaskShape {
                    center,
                    size,
                    rotation,
                    feather,
                    ..
                } => {
                    if !node.inputs.is_empty()
                        || center
                            .iter()
                            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
                        || size
                            .iter()
                            .any(|v| !v.is_finite() || !(0.0..=2.0).contains(v))
                        || !rotation.is_finite()
                        || !feather.is_finite()
                        || !(0.0..=1.0).contains(&feather)
                    {
                        return Err(
                            "native shape requires zero inputs and finite normalized geometry",
                        );
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativePolygonMask {
                    ref vertices,
                    center,
                    size,
                    rotation,
                    feather,
                } => {
                    if !node.inputs.is_empty()
                        || !super::ops::native_polygon_vertices_valid(vertices)
                        || center
                            .iter()
                            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
                        || size
                            .iter()
                            .any(|v| !v.is_finite() || !(0.0..=2.0).contains(v))
                        || !rotation.is_finite()
                        || !feather.is_finite()
                        || !(0.0..=1.0).contains(&feather)
                    {
                        return Err("native polygon requires zero inputs, 3..128 normalized finite vertices and bounded shape geometry");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeCaptionOutput {
                    ref cue_batch,
                    video,
                    ref paints,
                } => {
                    if root != Domain::SceneLinearAcescg
                        || node.inputs.len() != 1
                        || input
                            != if video {
                                Domain::Bt709Video
                            } else {
                                Domain::SrgbDisplay
                            }
                    {
                        return Err("native captions require exactly one matching display-encoded managed input");
                    }
                    let source = &self.nodes[node.inputs[0].0 .0 as usize];
                    if !matches!(
                        (&source.op, video),
                        (IrOp::NativeSdrOutput, false) | (IrOp::NativeSdrVideoOutput, true)
                    ) {
                        return Err("native caption output must immediately follow its matching managed SDR transform");
                    }
                    validate_native_caption_batch(cue_batch)?;
                    if !paints.is_empty() && paints.len() != cue_batch.cues.len() {
                        return Err("native caption paint count must match active cues");
                    }
                    for (paint, cue) in paints.iter().zip(&cue_batch.cues) {
                        paint.validate(cue.words.len())?;
                    }
                    input
                }
                IrOp::NativeDecoratedTextGen {
                    ref block,
                    ref decoration,
                } => {
                    if root != Domain::SceneLinearAcescg || !node.inputs.is_empty() {
                        return Err("decorated native title requires zero inputs in AP1");
                    }
                    validate_native_text_block(block)?;
                    decoration.validate()?;
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeTextGen { ref block } => {
                    if root != Domain::SceneLinearAcescg || !node.inputs.is_empty() {
                        return Err("native title requires zero inputs in an AP1 graph");
                    }
                    validate_native_text_block(block)?;
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeDenoise {
                    strength,
                    spatial_scale,
                } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || !strength.is_finite()
                        || !(0.0..=1.0).contains(&strength)
                        || !spatial_scale.is_finite()
                        || !(1.0 / 65536.0..=4.0).contains(&spatial_scale)
                    {
                        return Err("native denoise requires one AP1 image, strength 0..=1 and bounded positive spatial scale");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeGrain {
                    amount,
                    seed,
                    pixel_scale,
                    ..
                } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || !amount.is_finite()
                        || !(0.0..=1.0).contains(&amount)
                        || seed > 1_000_000_000
                        || pixel_scale
                            .iter()
                            .any(|v| !v.is_finite() || !(1.0 / 65536.0..=65536.0).contains(v))
                    {
                        return Err("native grain requires one AP1 image, amount 0..=1, seed 0..=1000000000 and positive pixel scales");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeVignette { amount, feather } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || !amount.is_finite()
                        || !(-1.0..=1.0).contains(&amount)
                        || !feather.is_finite()
                        || !(0.0..=1.0).contains(&feather)
                    {
                        return Err("native vignette requires one AP1 image, amount -1..=1 and feather 0..=1");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeMatteExtract
                | IrOp::NativeChannelExtract { .. }
                | IrOp::NativeImageToMask => {
                    if root != Domain::SceneLinearAcescg
                        || input != Domain::SceneLinearAcescg
                        || node.inputs.len() != 1
                    {
                        return Err("native channel extraction requires one AP1 image");
                    }
                    Domain::ScalarChannel
                }
                IrOp::NativeMaskToImage => {
                    if root != Domain::SceneLinearAcescg
                        || input != Domain::ScalarChannel
                        || node.inputs.len() != 1
                    {
                        return Err("native mask image requires one scalar signal");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeChannelCombine => {
                    if root != Domain::SceneLinearAcescg
                        || input != Domain::ScalarChannel
                        || node.inputs.len() != 4
                    {
                        return Err("native channel combine requires four scalar signals");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeInvert => {
                    if node.inputs.len() != 1 || input != Domain::SceneLinearAcescg {
                        return Err("native invert requires one scene-linear AP1 image");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeUnsharp { amount, sigma } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || !amount.is_finite()
                        || !(0.0..=10.0).contains(&amount)
                        || !sigma.is_finite()
                        || !(0.0..=100.0).contains(&sigma)
                    {
                        return Err("native unsharp requires one AP1 image, amount 0..=10 and sigma 0..=100");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeOutline {
                    thickness,
                    color,
                    opacity,
                    pixel_scale,
                } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || !thickness.is_finite()
                        || !(0.0..=64.0).contains(&thickness)
                        || color
                            .iter()
                            .any(|v| !v.is_finite() || !(0.0..=16.0).contains(v))
                        || !opacity.is_finite()
                        || !(0.0..=1.0).contains(&opacity)
                        || pixel_scale
                            .iter()
                            .any(|v| !v.is_finite() || !(0.000001..=1_000_000.0).contains(v))
                    {
                        return Err("native outline requires one AP1 image and finite bounded thickness, color, opacity and pixel scale");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeLinearHsl { vibrance, controls } => {
                    let bound = if vibrance { 1.0 } else { 180.0 };
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || controls.iter().any(|v| !v.is_finite())
                        || !(-bound..=bound).contains(&controls[0])
                        || controls[1..].iter().any(|v| !(-1.0..=1.0).contains(v))
                        || (vibrance && (controls[1] != 0.0 || controls[2] != 0.0))
                    {
                        return Err(
                            "native linear HSL requires one AP1 image and bounded catalog controls",
                        );
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeSurfaceBlur {
                    radius,
                    threshold,
                    pixel_scale,
                } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || radius > 64
                        || !threshold.is_finite()
                        || !(0.0..=1.0).contains(&threshold)
                        || pixel_scale
                            .iter()
                            .any(|v| !v.is_finite() || !(0.000001..=1_000_000.0).contains(v))
                    {
                        return Err("native Surface Blur requires one AP1 image, radius0..64, threshold0..1 and finite positive pixel scale");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeLensBlur {
                    radius,
                    pixel_scale,
                } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || !radius.is_finite()
                        || !(0.0..=500.0).contains(&radius)
                        || pixel_scale
                            .iter()
                            .any(|v| !v.is_finite() || !(0.000001..=1_000_000.0).contains(v))
                    {
                        return Err("native Lens Blur requires one AP1 image, radius0..500 and finite positive pixel scale");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeRankFilter {
                    kind,
                    radius,
                    threshold,
                    pixel_scale,
                } => {
                    let max = if kind == NativeRankKind::Median {
                        500
                    } else {
                        16
                    };
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || radius > max
                        || (kind == NativeRankKind::Median && threshold != 0)
                        || pixel_scale
                            .iter()
                            .any(|v| !v.is_finite() || !(0.000001..=1_000_000.0).contains(v))
                    {
                        return Err("native rank filter requires one AP1 image, valid kind-specific radius/threshold and finite positive pixel scale");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeDeflicker { gain, band } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || gain
                            .iter()
                            .any(|v| !v.is_finite() || !(0.1..=10.0).contains(v))
                        || band.iter().any(|v| !v.is_finite())
                        || !(0.0..=512.0).contains(&band[0])
                        || band[0].fract() != 0.0
                        || !(0.0..=0.9).contains(&band[1])
                        || band[2].abs() > 100_000.0
                    {
                        return Err("native Deflicker requires one AP1 input and finite bounded resolved gains/bands");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeUnpremultiply => {
                    if node.inputs.len() != 1 || input != Domain::SceneLinearAcescg {
                        return Err("native Unpremultiply requires one AP1 image");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeAlphaView { mode } => {
                    if node.inputs.len() != 1 || input != Domain::SceneLinearAcescg || mode > 2 {
                        return Err("native Alpha View requires one AP1 image and mode0..2");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeTransferLevels {
                    black_white,
                    gamma,
                    output,
                } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || !gamma.is_finite()
                        || !(0.01..=9.99).contains(&gamma)
                        || black_white
                            .iter()
                            .chain(output.iter())
                            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
                    {
                        return Err("native Levels requires one AP1 image, finite unit ranges and gamma .01..9.99");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeTransferCurves { knots, contrast } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || !contrast.is_finite()
                        || !(-1.0..=1.0).contains(&contrast)
                        || knots
                            .iter()
                            .flatten()
                            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
                    {
                        return Err("native catalog Curves requires one AP1 input, five finite unit knots and contrast -1..1");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeColorBalance { shifts, .. } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || shifts
                            .iter()
                            .flatten()
                            .any(|v| !v.is_finite() || !(-1.0..=1.0).contains(v))
                    {
                        return Err(
                            "native Color Balance requires one AP1 image and finite shifts -1..1",
                        );
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativePhotoFilter { color, density, .. } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || !density.is_finite()
                        || !(0.0..=1.0).contains(&density)
                        || color
                            .iter()
                            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
                    {
                        return Err("native Photo Filter requires one AP1 image and finite unit color/density");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeTransferMatrix { matrix } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || matrix
                            .iter()
                            .flatten()
                            .any(|v| !v.is_finite() || !(-2.0..=2.0).contains(v))
                    {
                        return Err("native transfer matrix requires one AP1 image and finite coefficients -2..2");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeTransferArtistic { kind, parameter } => {
                    let valid = match kind {
                        NativeTransferArtisticKind::Posterize => {
                            (2.0..=255.0).contains(&parameter) && parameter.fract() == 0.0
                        }
                        NativeTransferArtisticKind::Threshold => (0.0..=1.0).contains(&parameter),
                        NativeTransferArtisticKind::Desaturate
                        | NativeTransferArtisticKind::Invert => parameter == 0.0,
                    };
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || !parameter.is_finite()
                        || !valid
                    {
                        return Err("native transfer-space artistic filter requires one AP1 image and a bounded kind-specific parameter");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeDropShadow {
                    offset,
                    radius,
                    color,
                    opacity,
                    pixel_scale,
                } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || offset
                            .iter()
                            .any(|v| !v.is_finite() || !(-200.0..=200.0).contains(v))
                        || radius > 64
                        || color
                            .iter()
                            .any(|v| !v.is_finite() || !(0.0..=16.0).contains(v))
                        || !opacity.is_finite()
                        || !(0.0..=1.0).contains(&opacity)
                        || pixel_scale
                            .iter()
                            .any(|v| !v.is_finite() || !(0.000001..=1_000_000.0).contains(v))
                    {
                        return Err("native drop shadow requires one AP1 image and finite bounded offset, radius, color, opacity and pixel scale");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativePerspective { inverse } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || !super::ops::native_perspective_matrix_valid(&inverse)
                    {
                        return Err("native perspective requires one AP1 image and a finite nonsingular bounded inverse");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeStylize {
                    kind,
                    amount,
                    pixel_scale,
                } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || !amount.is_finite()
                        || !(-64.0..=64.0).contains(&amount)
                        || (kind != NativeStylizeKind::ChromaticAberration && amount != 0.0)
                        || pixel_scale
                            .iter()
                            .any(|v| !v.is_finite() || !(0.000001..=1_000_000.0).contains(v))
                    {
                        return Err("native stylize requires one AP1 image and finite bounded kind-specific parameters");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeDetail {
                    kind,
                    amount,
                    sigma,
                    threshold,
                    pixel_scale,
                } => {
                    let valid = match kind {
                        NativeDetailKind::SmartSharpen => {
                            (0.0..=10.0).contains(&amount)
                                && (0.0..=100.0).contains(&sigma)
                                && (0.0..=1.0).contains(&threshold)
                        }
                        NativeDetailKind::HighPass => {
                            amount == 1.0 && (0.0..=500.0).contains(&sigma) && threshold == 0.0
                        }
                        NativeDetailKind::Clarity => {
                            (-1.0..=1.0).contains(&amount) && sigma == 0.0 && threshold == 0.0
                        }
                    };
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || !valid
                        || !amount.is_finite()
                        || !sigma.is_finite()
                        || !threshold.is_finite()
                        || pixel_scale
                            .iter()
                            .any(|v| !v.is_finite() || !(0.000001..=1_000_000.0).contains(v))
                    {
                        return Err("native detail requires one AP1 image and finite bounded kind-specific parameters");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeGeometricWarp {
                    kind,
                    amount,
                    wavelength,
                    pixel_scale,
                } => {
                    let max = if kind == NativeWarpKind::Ripple {
                        64.0
                    } else {
                        1.0
                    };
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || !amount.is_finite()
                        || !(-max..=max).contains(&amount)
                        || !wavelength.is_finite()
                        || !(1.0..=512.0).contains(&wavelength)
                        || (kind != NativeWarpKind::Ripple && wavelength != 1.0)
                        || pixel_scale
                            .iter()
                            .any(|v| !v.is_finite() || !(0.000001..=1_000_000.0).contains(v))
                    {
                        return Err("native geometric warp requires one AP1 image and finite bounded kind-specific parameters");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeBoxBlur { radius } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || radius.iter().any(|v| *v > 500)
                    {
                        return Err(
                            "native box blur requires one AP1 image and radii in 0..=500 pixels",
                        );
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeMosaic { block } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || block.iter().any(|v| !(1..=256).contains(v))
                    {
                        return Err("native mosaic requires one AP1 image and cell dimensions in 1..=256 pixels");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeMotionBlur { delta } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || delta.iter().any(|v| !v.is_finite())
                        || f64::from(delta[0]).hypot(f64::from(delta[1])) > 500.0001
                    {
                        return Err("native directional motion blur requires one AP1 image and a finite displacement of at most 500 pixels");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeGaussianBlur { sigma } => {
                    if node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                        || !sigma.is_finite()
                        || !(0.0..=500.0).contains(&sigma)
                    {
                        return Err("native Gaussian blur requires one scene-linear AP1 image and finite sigma in 0..=500 pixels");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeExposure { stops } => {
                    if !stops.is_finite() || !(-32.0..=32.0).contains(&stops) {
                        return Err("native exposure must be finite and within -32..=32 stops");
                    }
                    if input != Domain::SceneLinearAcescg {
                        return Err("native exposure requires scene-linear ACEScg");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeLinearOffset { rgb } => {
                    if rgb
                        .iter()
                        .any(|value| !value.is_finite() || !(-16.0..=16.0).contains(value))
                    {
                        return Err("native linear offset must be finite and within -16..=16");
                    }
                    if input != Domain::SceneLinearAcescg {
                        return Err("native linear offset requires scene-linear ACEScg");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativePrinterLights { points } => {
                    if points
                        .iter()
                        .any(|value| !value.is_finite() || !(-120.0..=120.0).contains(value))
                    {
                        return Err(
                            "native printer lights must be finite and within -120..=120 points",
                        );
                    }
                    if input != Domain::SceneLinearAcescg {
                        return Err("native printer lights require scene-linear ACEScg");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeHighlightRolloff { knee, strength } => {
                    if !knee.is_finite()
                        || !(0.0..=64.0).contains(&knee)
                        || !strength.is_finite()
                        || !(0.0..=64.0).contains(&strength)
                    {
                        return Err(
                            "native highlight roll-off parameters must be finite and within 0..=64",
                        );
                    }
                    if input != Domain::SceneLinearAcescg {
                        return Err("native highlight roll-off requires scene-linear ACEScg");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeMaskMix { ref mask } => {
                    photonic_render::grade_gpu::validate_native_mask(mask)?;
                    if node.inputs.len() != 2
                        || !matches!(input, Domain::SceneLinearAcescg | Domain::Acescct)
                    {
                        return Err("native mask mix requires two images in the same native grading coordinates");
                    }
                    input
                }
                IrOp::NativeLut3d { ref lut } => {
                    photonic_render::grade_gpu::validate_native_lut(lut)?;
                    if !matches!(input, Domain::SceneLinearAcescg | Domain::Acescct) {
                        return Err("native LUT requires ACEScg or ACEScct input");
                    }
                    input
                }
                IrOp::NativeLogCurves { ref curves } => {
                    photonic_render::grade_gpu::validate_native_curves(curves)?;
                    if input != Domain::Acescct {
                        return Err("native curves require ACEScct");
                    }
                    Domain::Acescct
                }
                IrOp::NativeLogQualifier { ref qualifier } => {
                    photonic_render::native_transfer::validate_native_qualifier(qualifier)?;
                    if input != Domain::Acescct {
                        return Err("native qualifier requires ACEScct");
                    }
                    Domain::Acescct
                }
                IrOp::NativeTonalExposure { tonal } => {
                    tonal.validate()?;
                    if !tonal.native_ap1
                        || node.inputs.len() != 1
                        || input != Domain::SceneLinearAcescg
                    {
                        return Err("native tonal exposure requires one scene AP1 input");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeRgbMatrix { rows } => {
                    photonic_render::white_balance::validate_matrix(&rows)?;
                    if node.inputs.len() != 1 || input != Domain::SceneLinearAcescg {
                        return Err("native linear color matrix requires one scene AP1 input");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeMatteCdl { cdl } => {
                    photonic_render::native_transfer::validate_native_cdl(&cdl)?;
                    if node.inputs.len() != 2 || input != Domain::SceneLinearAcescg {
                        return Err("native matte CDL requires a scene image and typed matte");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeLogCdl { cdl } => {
                    photonic_render::native_transfer::validate_native_cdl(&cdl)?;
                    if input != Domain::Acescct {
                        return Err("native CDL requires ACEScct");
                    }
                    Domain::Acescct
                }
                IrOp::NativeLogContrast { pivot, amount } => {
                    if !pivot.is_finite()
                        || !(0.0..=1.0).contains(&pivot)
                        || !amount.is_finite()
                        || !(-4.0..=4.0).contains(&amount)
                    {
                        return Err("native contrast parameters are invalid");
                    }
                    if input != Domain::Acescct {
                        return Err("native contrast requires ACEScct");
                    }
                    Domain::Acescct
                }
                IrOp::NativeSaturationVibrance {
                    saturation,
                    vibrance,
                } => {
                    if !saturation.is_finite()
                        || !(0.0..=4.0).contains(&saturation)
                        || !vibrance.is_finite()
                        || !(-1.0..=1.0).contains(&vibrance)
                    {
                        return Err("native saturation/vibrance parameters are invalid");
                    }
                    if input != Domain::SceneLinearAcescg {
                        return Err("native saturation/vibrance requires scene-linear ACEScg");
                    }
                    Domain::SceneLinearAcescg
                }
                IrOp::NativeAcescct {
                    direction: AcescctDirection::Encode,
                } if input == Domain::SceneLinearAcescg => Domain::Acescct,
                IrOp::NativeAcescct {
                    direction: AcescctDirection::Decode,
                } if input == Domain::Acescct => Domain::SceneLinearAcescg,
                IrOp::NativeAcescct { .. } => {
                    return Err("ACEScct transfer has an invalid input color domain");
                }
                IrOp::NativeSdrOutput if input == Domain::SceneLinearAcescg => Domain::SrgbDisplay,
                IrOp::NativeSdrOutput => {
                    return Err("SDR output transform requires scene-linear ACEScg");
                }
                IrOp::NativeSdrVideoOutput if input == Domain::SceneLinearAcescg => {
                    Domain::Bt709Video
                }
                IrOp::NativeSdrVideoOutput => {
                    return Err("SDR video output transform requires scene-linear ACEScg");
                }
                _ => input,
            };
            domains.push(output);
        }
        if let Some(output) = self.output {
            match domains.get(output.0 as usize) {
                Some(Domain::ScalarChannel) => {
                    return Err("scalar channels require an image conversion before display");
                }
                Some(Domain::Acescct | Domain::MatteWeight) => {
                    return Err("ACEScct grading coordinates cannot be a displayed output");
                }
                Some(domain)
                    if self.nodes.iter().any(|node| {
                        matches!(node.op, IrOp::NativeSdrOutput | IrOp::NativeSdrVideoOutput)
                    }) && !matches!(domain, Domain::SrgbDisplay | Domain::Bt709Video) =>
                {
                    return Err("managed SDR output transform must be the graph output");
                }
                None => return Err("color-domain output is not topologically available"),
                _ => {}
            }
        }
        Ok(domains)
    }

    /// Keep decoded detail until a direct transform samples the portrait crop.
    /// Other consumers retain the canvas-sized source contract.
    pub(crate) fn native_video_sources(&self) -> Vec<bool> {
        let mut native: Vec<bool> = self
            .nodes
            .iter()
            .map(|n| {
                matches!(
                    n.op,
                    IrOp::DecodeVideo { .. } | IrOp::NativeDecodeVideo { .. }
                )
            })
            .collect();
        for node in &self.nodes {
            if !matches!(
                node.op,
                IrOp::Transform2D { .. } | IrOp::Transform2DTransparent { .. }
            ) {
                for (id, _) in &node.inputs {
                    native[id.0 as usize] = false;
                }
            }
        }
        if let Some(id) = self.output {
            native[id.0 as usize] = false;
        }
        native
    }

    /// Convert authored output pixels to the processing canvas used by Draft.
    pub(crate) fn canvas_scale(&self, canvas: (u32, u32)) -> glam::Vec2 {
        let mut terminal = self.output.and_then(|id| self.nodes.get(id.0 as usize));
        if let Some(node) = terminal {
            if matches!(node.op, IrOp::NativeCaptionOutput { .. }) {
                terminal = node
                    .inputs
                    .first()
                    .and_then(|(id, _)| self.nodes.get(id.0 as usize));
            }
        }
        let boundary = match terminal {
            Some(node) if matches!(node.op, IrOp::NativeSdrOutput | IrOp::NativeSdrVideoOutput) => {
                node.inputs
                    .first()
                    .and_then(|(id, _)| self.nodes.get(id.0 as usize))
            }
            _ => terminal,
        };
        match boundary {
            Some(IrNode {
                op: IrOp::Output { w, h } | IrOp::NativeCanvas { w, h },
                ..
            }) if *w > 0 && *h > 0 => {
                glam::Vec2::new(canvas.0 as f32 / *w as f32, canvas.1 as f32 / *h as f32)
            }
            _ => glam::Vec2::ONE,
        }
    }
}

pub(crate) fn validate_native_caption_batch(batch: &CaptionBatch) -> Result<(), &'static str> {
    if batch.cues.len() > 16
        || batch
            .cues
            .iter()
            .flat_map(|c| &c.words)
            .map(|w| w.text.len())
            .sum::<usize>()
            > 4096
        || batch.cues.iter().map(|c| c.words.len()).sum::<usize>() > 128
    {
        return Err("native captions exceed the bounded cue, word or text budget");
    }
    for cue in &batch.cues {
        // SlideUp may start up to 5% below an authored bottom-edge anchor.
        if !cue.anchor[1].is_finite() || !(0.0..=1.05).contains(&cue.anchor[1]) {
            return Err("native caption vertical anchor exceeds the bounded slide animation");
        }
        let mut checked = cue.clone();
        checked.anchor[1] = checked.anchor[1].min(1.0);
        validate_native_text_block(&ResolvedTextBlock { cue: Some(checked) })?;
    }
    Ok(())
}

pub(crate) fn validate_native_text_block(block: &ResolvedTextBlock) -> Result<(), &'static str> {
    let Some(cue) = &block.cue else {
        return Ok(());
    };
    if cue.words.len() > 128
        || cue.words.iter().map(|w| w.text.len()).sum::<usize>() > 4096
        || !cue.font_size.is_finite()
        || !(1.0..=512.0).contains(&cue.font_size)
        || !cue.line_height_mul.is_finite()
        || !(0.5..=4.0).contains(&cue.line_height_mul)
        || !cue.max_width.is_finite()
        || !(0.001..=1.0).contains(&cue.max_width)
        || cue
            .anchor
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
        || cue.words.iter().any(|w| {
            w.font_family.is_empty()
                || w.font_family.len() > 128
                || !(1..=1000).contains(&w.font_weight)
                || w.metrics.is_some_and(|[size, height]| {
                    !size.is_finite()
                        || !(1.0..=512.0).contains(&size)
                        || !height.is_finite()
                        || !(0.5..=2048.0).contains(&height)
                })
        })
    {
        return Err("native title has unsupported text length, font or layout bounds");
    }
    Ok(())
}

impl IrOp {
    /// Native resize dimensions are authored pixels, like spatial coordinates.
    /// Legacy explicit raster sizes retain their existing behavior.
    pub(crate) fn at_evaluation_scale(
        &self,
        scale: glam::Vec2,
        domain: WorkingColorDomain,
    ) -> std::borrow::Cow<'_, Self> {
        if domain == WorkingColorDomain::SceneLinearAcescg {
            if let Self::Resize { w, h, fit } = self {
                return std::borrow::Cow::Owned(Self::Resize {
                    w: ((*w as f32 * scale.x).round() as u32).max(1),
                    h: ((*h as f32 * scale.y).round() as u32).max(1),
                    fit: *fit,
                });
            }
        }
        self.at_canvas_scale(scale)
    }

    /// Affine coordinates are authored in output pixels, including the pivot.
    /// Conjugation preserves scale/rotation while adapting translation to Draft.
    pub(crate) fn at_canvas_scale(&self, scale: glam::Vec2) -> std::borrow::Cow<'_, Self> {
        match self {
            Self::NativeCaptionOutput {
                cue_batch,
                video,
                paints,
            } if scale != glam::Vec2::ONE => {
                let mut cue_batch = cue_batch.clone();
                for cue in &mut cue_batch.cues {
                    cue.scale_metrics(scale.y);
                }
                std::borrow::Cow::Owned(Self::NativeCaptionOutput {
                    cue_batch,
                    video: *video,
                    paints: paints.iter().map(|p| p.scaled(scale.y)).collect(),
                })
            }
            Self::NativeDecoratedTextGen { block, decoration } if scale != glam::Vec2::ONE => {
                let mut block = block.clone();
                if let Some(cue) = &mut block.cue {
                    cue.scale_metrics(scale.y);
                }
                std::borrow::Cow::Owned(Self::NativeDecoratedTextGen {
                    block,
                    decoration: decoration.scaled(scale.y),
                })
            }
            Self::NativeTextGen { block } if scale != glam::Vec2::ONE => {
                let mut block = block.clone();
                if let Some(cue) = &mut block.cue {
                    cue.scale_metrics(scale.y);
                }
                std::borrow::Cow::Owned(Self::NativeTextGen { block })
            }
            Self::NativeDenoise {
                strength,
                spatial_scale,
            } if scale != glam::Vec2::ONE => std::borrow::Cow::Owned(Self::NativeDenoise {
                strength: *strength,
                spatial_scale: (*spatial_scale * scale.min_element()).clamp(1.0 / 65536.0, 4.0),
            }),
            Self::NativeGrain {
                amount,
                monochrome,
                seed,
                pixel_scale,
            } if scale != glam::Vec2::ONE => std::borrow::Cow::Owned(Self::NativeGrain {
                amount: *amount,
                monochrome: *monochrome,
                seed: *seed,
                pixel_scale: [pixel_scale[0] * scale.x, pixel_scale[1] * scale.y],
            }),
            Self::NativeGlow {
                sigma,
                threshold,
                intensity,
                tint,
            } if scale != glam::Vec2::ONE => std::borrow::Cow::Owned(Self::NativeGlow {
                sigma: *sigma * scale.min_element(),
                threshold: *threshold,
                intensity: *intensity,
                tint: *tint,
            }),
            Self::NativeUnsharp { amount, sigma } if scale != glam::Vec2::ONE => {
                std::borrow::Cow::Owned(Self::NativeUnsharp {
                    amount: *amount,
                    sigma: *sigma * scale.min_element(),
                })
            }
            Self::NativeOutline {
                thickness,
                color,
                opacity,
                pixel_scale,
            } if scale != glam::Vec2::ONE => std::borrow::Cow::Owned(Self::NativeOutline {
                thickness: *thickness,
                color: *color,
                opacity: *opacity,
                pixel_scale: [pixel_scale[0] * scale.x, pixel_scale[1] * scale.y],
            }),
            Self::NativeSurfaceBlur {
                radius,
                threshold,
                pixel_scale,
            } if scale != glam::Vec2::ONE => std::borrow::Cow::Owned(Self::NativeSurfaceBlur {
                radius: *radius,
                threshold: *threshold,
                pixel_scale: [pixel_scale[0] * scale.x, pixel_scale[1] * scale.y],
            }),
            Self::NativeLensBlur {
                radius,
                pixel_scale,
            } if scale != glam::Vec2::ONE => std::borrow::Cow::Owned(Self::NativeLensBlur {
                radius: *radius,
                pixel_scale: [pixel_scale[0] * scale.x, pixel_scale[1] * scale.y],
            }),
            Self::NativeRankFilter {
                kind,
                radius,
                threshold,
                pixel_scale,
            } if scale != glam::Vec2::ONE => std::borrow::Cow::Owned(Self::NativeRankFilter {
                kind: *kind,
                radius: *radius,
                threshold: *threshold,
                pixel_scale: [pixel_scale[0] * scale.x, pixel_scale[1] * scale.y],
            }),
            Self::NativeDropShadow {
                offset,
                radius,
                color,
                opacity,
                pixel_scale,
            } if scale != glam::Vec2::ONE => std::borrow::Cow::Owned(Self::NativeDropShadow {
                offset: *offset,
                radius: *radius,
                color: *color,
                opacity: *opacity,
                pixel_scale: [pixel_scale[0] * scale.x, pixel_scale[1] * scale.y],
            }),
            Self::NativeStylize {
                kind,
                amount,
                pixel_scale,
            } if scale != glam::Vec2::ONE => std::borrow::Cow::Owned(Self::NativeStylize {
                kind: *kind,
                amount: *amount,
                pixel_scale: [pixel_scale[0] * scale.x, pixel_scale[1] * scale.y],
            }),
            Self::NativeDetail {
                kind,
                amount,
                sigma,
                threshold,
                pixel_scale,
            } if scale != glam::Vec2::ONE => std::borrow::Cow::Owned(Self::NativeDetail {
                kind: *kind,
                amount: *amount,
                sigma: *sigma,
                threshold: *threshold,
                pixel_scale: [pixel_scale[0] * scale.x, pixel_scale[1] * scale.y],
            }),
            Self::NativeGeometricWarp {
                kind,
                amount,
                wavelength,
                pixel_scale,
            } if scale != glam::Vec2::ONE => std::borrow::Cow::Owned(Self::NativeGeometricWarp {
                kind: *kind,
                amount: *amount,
                wavelength: *wavelength,
                pixel_scale: [pixel_scale[0] * scale.x, pixel_scale[1] * scale.y],
            }),
            Self::NativeBoxBlur { radius } if scale != glam::Vec2::ONE => {
                std::borrow::Cow::Owned(Self::NativeBoxBlur {
                    radius: [
                        (radius[0] as f32 * scale.x).round().max(0.0) as u32,
                        (radius[1] as f32 * scale.y).round().max(0.0) as u32,
                    ],
                })
            }
            Self::NativeMosaic { block } if scale != glam::Vec2::ONE => {
                std::borrow::Cow::Owned(Self::NativeMosaic {
                    block: [
                        (block[0] as f32 * scale.x).round().max(1.0) as u32,
                        (block[1] as f32 * scale.y).round().max(1.0) as u32,
                    ],
                })
            }
            Self::NativeMotionBlur { delta } if scale != glam::Vec2::ONE => {
                std::borrow::Cow::Owned(Self::NativeMotionBlur {
                    delta: [delta[0] * scale.x, delta[1] * scale.y],
                })
            }
            Self::NativeGaussianBlur { sigma } if scale != glam::Vec2::ONE => {
                // Draft keeps picture aspect ratio; use the smaller scale for
                // its one-pixel rounding difference and isotropic authored sigma.
                std::borrow::Cow::Owned(Self::NativeGaussianBlur {
                    sigma: *sigma * scale.min_element(),
                })
            }
            Self::Transform2D { mat, sampling } if scale != glam::Vec2::ONE => {
                let basis = Mat3::from_scale(scale);
                std::borrow::Cow::Owned(Self::Transform2D {
                    mat: basis * *mat * basis.inverse(),
                    sampling: *sampling,
                })
            }
            Self::Transform2DTransparent { mat, sampling } if scale != glam::Vec2::ONE => {
                let basis = Mat3::from_scale(scale);
                std::borrow::Cow::Owned(Self::Transform2DTransparent {
                    mat: basis * *mat * basis.inverse(),
                    sampling: *sampling,
                })
            }
            Self::TextGen { block } if scale != glam::Vec2::ONE => {
                let mut block = block.clone();
                if let Some(cue) = &mut block.cue {
                    cue.scale_metrics(scale.y);
                }
                std::borrow::Cow::Owned(Self::TextGen { block })
            }
            Self::CaptionOverlay { cue_batch } if scale != glam::Vec2::ONE => {
                let mut cue_batch = cue_batch.clone();
                for cue in &mut cue_batch.cues {
                    cue.scale_metrics(scale.y);
                }
                std::borrow::Cow::Owned(Self::CaptionOverlay { cue_batch })
            }
            _ => std::borrow::Cow::Borrowed(self),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_graph_rejects_display_authored_still_without_input_transform() {
        let graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![IrNode {
                op: IrOp::DecodeStill {
                    asset: AssetId::new(),
                },
                inputs: vec![],
                content_hash: ContentHash(1),
            }],
            output: Some(IrNodeId(0)),
        };
        assert_eq!(
            graph.validate_working_color_domain(),
            Err("display-authored source requires a managed input transform")
        );
    }

    #[test]
    fn managed_graph_rejects_legacy_effects_and_caption_overlay() {
        let source = IrNode {
            op: IrOp::SolidColor {
                color: LinearColor {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.0,
                },
            },
            inputs: vec![],
            content_hash: ContentHash(1),
        };
        for (op, expected) in [
            (
                IrOp::Effect {
                    kind: EffectKind::Blur,
                    params: ResolvedParams::default(),
                },
                "legacy effect is not qualified in scene-linear ACEScg",
            ),
            (
                IrOp::CaptionOverlay {
                    cue_batch: CaptionBatch::default(),
                },
                "display-authored captions require a managed output stage",
            ),
        ] {
            let graph = FrameGraph {
                working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                nodes: vec![
                    source.clone(),
                    IrNode {
                        op,
                        inputs: vec![(IrNodeId(0), OutPort::default())],
                        content_hash: ContentHash(2),
                    },
                ],
                output: Some(IrNodeId(1)),
            };
            assert_eq!(graph.validate_working_color_domain(), Err(expected));
        }
    }

    #[test]
    fn native_nonseparable_rejects_wrong_domains_modes_arity_and_opacity() {
        let source = |hash| IrNode {
            op: IrOp::SolidColor {
                color: LinearColor {
                    r: 0.5,
                    g: 0.25,
                    b: 0.0,
                    a: 0.5,
                },
            },
            inputs: vec![],
            content_hash: ContentHash(hash),
        };
        let mut graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![
                source(1),
                source(2),
                IrNode {
                    op: IrOp::NativeBoundedMerge {
                        mode: BlendMode::Hue,
                        opacity: 0.5,
                    },
                    inputs: vec![
                        (IrNodeId(0), OutPort::default()),
                        (IrNodeId(1), OutPort::default()),
                    ],
                    content_hash: ContentHash(3),
                },
            ],
            output: Some(IrNodeId(2)),
        };
        assert!(graph.validate_working_color_domain().is_ok());
        graph.working_color_domain = WorkingColorDomain::LegacyLinearRec709;
        assert!(graph.validate_working_color_domain().is_err());
        graph.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
        for (mode, opacity) in [
            (BlendMode::Normal, 0.5),
            (BlendMode::Hue, f32::NAN),
            (BlendMode::Hue, -0.1),
            (BlendMode::Hue, 1.1),
        ] {
            graph.nodes[2].op = IrOp::NativeBoundedMerge { mode, opacity };
            assert!(graph.validate_working_color_domain().is_err());
        }
        graph.nodes[2].op = IrOp::NativeBoundedMerge {
            mode: BlendMode::Luminosity,
            opacity: 1.0,
        };
        graph.nodes[2].inputs.pop();
        assert!(graph.validate_working_color_domain().is_err());
        graph.nodes[2]
            .inputs
            .push((IrNodeId(1), OutPort::default()));
        graph.nodes[2].op = IrOp::Merge {
            mode: BlendMode::Luminosity,
            opacity: 1.0,
        };
        assert!(
            graph.validate_working_color_domain().is_err(),
            "Legacy bounded merge must not silently implement Native arithmetic"
        );
    }

    #[test]
    fn native_bounded_rejects_wrong_domains_modes_arity_and_opacity() {
        let source = |hash| IrNode {
            op: IrOp::SolidColor {
                color: LinearColor {
                    r: 0.5,
                    g: 0.25,
                    b: 0.0,
                    a: 0.5,
                },
            },
            inputs: vec![],
            content_hash: ContentHash(hash),
        };
        let mut graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![
                source(1),
                source(2),
                IrNode {
                    op: IrOp::NativeBoundedMerge {
                        mode: BlendMode::ColorDodge,
                        opacity: 0.5,
                    },
                    inputs: vec![
                        (IrNodeId(0), OutPort::default()),
                        (IrNodeId(1), OutPort::default()),
                    ],
                    content_hash: ContentHash(3),
                },
            ],
            output: Some(IrNodeId(2)),
        };
        assert!(graph.validate_working_color_domain().is_ok());
        graph.working_color_domain = WorkingColorDomain::LegacyLinearRec709;
        assert!(graph.validate_working_color_domain().is_err());
        graph.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
        for (mode, opacity) in [
            (BlendMode::Normal, 0.5),
            (BlendMode::ColorDodge, f32::NAN),
            (BlendMode::ColorDodge, -0.1),
            (BlendMode::ColorDodge, 1.1),
        ] {
            graph.nodes[2].op = IrOp::NativeBoundedMerge { mode, opacity };
            assert!(graph.validate_working_color_domain().is_err());
        }
        graph.nodes[2].op = IrOp::NativeBoundedMerge {
            mode: BlendMode::Divide,
            opacity: 1.0,
        };
        graph.nodes[2].inputs.pop();
        assert!(graph.validate_working_color_domain().is_err());
        graph.nodes[2]
            .inputs
            .push((IrNodeId(1), OutPort::default()));
        graph.nodes[2].op = IrOp::Merge {
            mode: BlendMode::Divide,
            opacity: 1.0,
        };
        assert!(
            graph.validate_working_color_domain().is_err(),
            "Legacy bounded merge must not silently implement Native arithmetic"
        );
    }

    #[test]
    fn native_arithmetic_rejects_wrong_domains_modes_arity_and_opacity() {
        let source = |hash| IrNode {
            op: IrOp::SolidColor {
                color: LinearColor {
                    r: 0.5,
                    g: 0.25,
                    b: 0.0,
                    a: 0.5,
                },
            },
            inputs: vec![],
            content_hash: ContentHash(hash),
        };
        let mut graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![
                source(1),
                source(2),
                IrNode {
                    op: IrOp::NativeArithmeticMerge {
                        mode: BlendMode::LinearDodge,
                        opacity: 0.5,
                    },
                    inputs: vec![
                        (IrNodeId(0), OutPort::default()),
                        (IrNodeId(1), OutPort::default()),
                    ],
                    content_hash: ContentHash(3),
                },
            ],
            output: Some(IrNodeId(2)),
        };
        assert!(graph.validate_working_color_domain().is_ok());
        graph.working_color_domain = WorkingColorDomain::LegacyLinearRec709;
        assert!(graph.validate_working_color_domain().is_err());
        graph.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
        for (mode, opacity) in [
            (BlendMode::Normal, 0.5),
            (BlendMode::LinearDodge, f32::NAN),
            (BlendMode::LinearDodge, -0.1),
            (BlendMode::LinearDodge, 1.1),
        ] {
            graph.nodes[2].op = IrOp::NativeArithmeticMerge { mode, opacity };
            assert!(graph.validate_working_color_domain().is_err());
        }
        graph.nodes[2].op = IrOp::NativeArithmeticMerge {
            mode: BlendMode::Subtract,
            opacity: 1.0,
        };
        graph.nodes[2].inputs.pop();
        assert!(graph.validate_working_color_domain().is_err());
        graph.nodes[2]
            .inputs
            .push((IrNodeId(1), OutPort::default()));
        graph.nodes[2].op = IrOp::Merge {
            mode: BlendMode::Subtract,
            opacity: 1.0,
        };
        assert!(
            graph.validate_working_color_domain().is_err(),
            "Legacy bounded merge must not silently implement Native arithmetic"
        );
    }

    #[test]
    fn native_luminance_rejects_wrong_domains_modes_arity_and_opacity() {
        let source = |hash| IrNode {
            op: IrOp::SolidColor {
                color: LinearColor {
                    r: 0.5,
                    g: 0.25,
                    b: 0.0,
                    a: 0.5,
                },
            },
            inputs: vec![],
            content_hash: ContentHash(hash),
        };
        let mut graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![
                source(1),
                source(2),
                IrNode {
                    op: IrOp::NativeLuminanceMerge {
                        mode: BlendMode::DarkerColor,
                        opacity: 0.5,
                    },
                    inputs: vec![
                        (IrNodeId(0), OutPort::default()),
                        (IrNodeId(1), OutPort::default()),
                    ],
                    content_hash: ContentHash(3),
                },
            ],
            output: Some(IrNodeId(2)),
        };
        assert!(graph.validate_working_color_domain().is_ok());
        graph.working_color_domain = WorkingColorDomain::LegacyLinearRec709;
        assert!(graph.validate_working_color_domain().is_err());
        graph.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
        for (mode, opacity) in [
            (BlendMode::Normal, 0.5),
            (BlendMode::DarkerColor, f32::NAN),
            (BlendMode::DarkerColor, -0.1),
            (BlendMode::DarkerColor, 1.1),
        ] {
            graph.nodes[2].op = IrOp::NativeLuminanceMerge { mode, opacity };
            assert!(graph.validate_working_color_domain().is_err());
        }
        graph.nodes[2].op = IrOp::NativeLuminanceMerge {
            mode: BlendMode::LighterColor,
            opacity: 1.0,
        };
        graph.nodes[2].inputs.pop();
        assert!(graph.validate_working_color_domain().is_err());
        graph.nodes[2]
            .inputs
            .push((IrNodeId(1), OutPort::default()));
        graph.nodes[2].op = IrOp::Merge {
            mode: BlendMode::LighterColor,
            opacity: 1.0,
        };
        assert!(
            graph.validate_working_color_domain().is_err(),
            "Legacy luma merge must not silently implement Native AP1 whole-color selection"
        );
    }

    #[test]
    fn managed_graph_qualifies_only_extended_safe_blends() {
        let source = |hash| IrNode {
            op: IrOp::SolidColor {
                color: LinearColor {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.0,
                },
            },
            inputs: vec![],
            content_hash: ContentHash(hash),
        };
        let graph = |mode| FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![
                source(1),
                source(2),
                IrNode {
                    op: IrOp::Merge { mode, opacity: 1.0 },
                    inputs: vec![
                        (IrNodeId(0), OutPort::default()),
                        (IrNodeId(1), OutPort::default()),
                    ],
                    content_hash: ContentHash(3),
                },
            ],
            output: Some(IrNodeId(2)),
        };
        for mode in [
            BlendMode::Normal,
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::Difference,
            BlendMode::Exclusion,
            BlendMode::Overlay,
            BlendMode::HardLight,
            BlendMode::PinLight,
            BlendMode::SoftLight,
        ] {
            assert!(graph(mode).validate_working_color_domain().is_ok());
        }
        for mode in [
            BlendMode::Hue,
            BlendMode::ColorDodge,
            BlendMode::LinearDodge,
        ] {
            assert!(graph(mode).validate_working_color_domain().is_err());
        }
        let mut invalid = graph(BlendMode::Normal);
        for opacity in [f32::NAN, -0.1, 1.1] {
            invalid.nodes[2].op = IrOp::Merge {
                mode: BlendMode::Normal,
                opacity,
            };
            assert!(invalid.validate_working_color_domain().is_err());
        }
    }

    #[test]
    fn native_denoise_rejects_invalid_domain_arity_and_controls() {
        let op = |strength, spatial_scale| IrOp::NativeDenoise {
            strength,
            spatial_scale,
        };
        let mut graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![
                IrNode {
                    op: IrOp::SolidColor {
                        color: LinearColor {
                            r: -0.25,
                            g: 2.0,
                            b: 0.0,
                            a: 0.5,
                        },
                    },
                    inputs: vec![],
                    content_hash: ContentHash(1),
                },
                IrNode {
                    op: op(0.5, 1.0),
                    inputs: vec![(IrNodeId(0), OutPort::default())],
                    content_hash: ContentHash(2),
                },
            ],
            output: Some(IrNodeId(1)),
        };
        assert!(graph.validate_working_color_domain().is_ok());
        graph.working_color_domain = WorkingColorDomain::LegacyLinearRec709;
        assert!(graph.validate_working_color_domain().is_err());
        graph.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
        for invalid in [
            op(f32::NAN, 1.0),
            op(-0.01, 1.0),
            op(1.01, 1.0),
            op(0.5, 0.0),
            op(0.5, 4.1),
            op(0.5, f32::INFINITY),
        ] {
            graph.nodes[1].op = invalid;
            assert!(graph.validate_working_color_domain().is_err());
        }
        graph.nodes[1].op = op(0.5, 1.0);
        graph.nodes[1].inputs.clear();
        assert!(graph.validate_working_color_domain().is_err());
        graph.nodes[1].inputs = vec![(IrNodeId(0), OutPort::default()); 2];
        assert!(graph.validate_working_color_domain().is_err());
    }

    #[test]
    fn native_grain_rejects_invalid_domain_arity_and_controls() {
        let op = |amount, seed, pixel_scale| IrOp::NativeGrain {
            amount,
            monochrome: true,
            seed,
            pixel_scale,
        };
        let mut graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![
                IrNode {
                    op: IrOp::SolidColor {
                        color: LinearColor {
                            r: -0.25,
                            g: 2.0,
                            b: 0.0,
                            a: 0.5,
                        },
                    },
                    inputs: vec![],
                    content_hash: ContentHash(1),
                },
                IrNode {
                    op: op(0.1, 1, [1.0; 2]),
                    inputs: vec![(IrNodeId(0), OutPort::default())],
                    content_hash: ContentHash(2),
                },
            ],
            output: Some(IrNodeId(1)),
        };
        assert!(graph.validate_working_color_domain().is_ok());
        graph.working_color_domain = WorkingColorDomain::LegacyLinearRec709;
        assert!(graph.validate_working_color_domain().is_err());
        graph.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
        for invalid in [
            op(f32::NAN, 1, [1.0; 2]),
            op(-0.01, 1, [1.0; 2]),
            op(1.01, 1, [1.0; 2]),
            op(0.1, 1_000_000_001, [1.0; 2]),
            op(0.1, 1, [0.0, 1.0]),
            op(0.1, 1, [f32::INFINITY, 1.0]),
            op(0.1, 1, [1e-30, 1.0]),
        ] {
            graph.nodes[1].op = invalid;
            assert!(graph.validate_working_color_domain().is_err());
        }
        graph.nodes[1].op = op(0.1, 1, [1.0; 2]);
        graph.nodes[1].inputs.clear();
        assert!(graph.validate_working_color_domain().is_err());
        graph.nodes[1].inputs = vec![(IrNodeId(0), OutPort::default()); 2];
        assert!(graph.validate_working_color_domain().is_err());
    }

    #[test]
    fn native_vignette_rejects_invalid_domain_arity_and_controls() {
        let mut graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![
                IrNode {
                    op: IrOp::SolidColor {
                        color: LinearColor {
                            r: -0.25,
                            g: 2.0,
                            b: 0.0,
                            a: 0.5,
                        },
                    },
                    inputs: vec![],
                    content_hash: ContentHash(1),
                },
                IrNode {
                    op: IrOp::NativeVignette {
                        amount: -0.5,
                        feather: 0.5,
                    },
                    inputs: vec![(IrNodeId(0), OutPort::default())],
                    content_hash: ContentHash(2),
                },
            ],
            output: Some(IrNodeId(1)),
        };
        assert!(graph.validate_working_color_domain().is_ok());
        graph.working_color_domain = WorkingColorDomain::LegacyLinearRec709;
        assert!(graph.validate_working_color_domain().is_err());
        graph.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
        for (amount, feather) in [
            (f32::NAN, 0.5),
            (-1.01, 0.5),
            (1.01, 0.5),
            (0.0, f32::INFINITY),
            (0.0, -0.01),
            (0.0, 1.01),
        ] {
            graph.nodes[1].op = IrOp::NativeVignette { amount, feather };
            assert!(graph.validate_working_color_domain().is_err());
        }
        graph.nodes[1].op = IrOp::NativeVignette {
            amount: 0.0,
            feather: 0.0,
        };
        graph.nodes[1].inputs.clear();
        assert!(graph.validate_working_color_domain().is_err());
        graph.nodes[1].inputs = vec![(IrNodeId(0), OutPort::default()); 2];
        assert!(graph.validate_working_color_domain().is_err());
    }

    #[test]
    fn native_resize_rejects_invalid_allocations_and_domains() {
        let mut graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![
                IrNode {
                    op: IrOp::SolidColor {
                        color: LinearColor {
                            r: -0.25,
                            g: 2.0,
                            b: 0.0,
                            a: 0.5,
                        },
                    },
                    inputs: vec![],
                    content_hash: ContentHash(1),
                },
                IrNode {
                    op: IrOp::Resize {
                        w: 16,
                        h: 8,
                        fit: FitMode::Fit,
                    },
                    inputs: vec![(IrNodeId(0), OutPort::default())],
                    content_hash: ContentHash(2),
                },
            ],
            output: Some(IrNodeId(1)),
        };
        assert!(graph.validate_working_color_domain().is_ok());
        for (w, h) in [
            (0, 8),
            (8, 0),
            (8193, 1),
            (8192, 8192),
            (u32::MAX, u32::MAX),
        ] {
            graph.nodes[1].op = IrOp::Resize {
                w,
                h,
                fit: FitMode::Fit,
            };
            assert!(graph.validate_working_color_domain().is_err());
        }
        graph.nodes[1].op = IrOp::Resize {
            w: 16,
            h: 8,
            fit: FitMode::Fit,
        };
        graph.nodes[1].inputs.clear();
        assert!(graph.validate_working_color_domain().is_err());
        graph.nodes[1].inputs = vec![(IrNodeId(0), OutPort::default()); 2];
        assert!(graph.validate_working_color_domain().is_err());
        graph.nodes[1].inputs.truncate(1);
        graph.nodes[0].op = IrOp::GradeMatteConstant { weight: 0.5 };
        assert!(graph.validate_working_color_domain().is_err());
    }

    #[test]
    fn native_gaussian_rejects_wrong_domains_arity_and_nonfinite_radius() {
        let mut graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![
                IrNode {
                    op: IrOp::SolidColor {
                        color: LinearColor {
                            r: 0.1,
                            g: 0.2,
                            b: 0.3,
                            a: 1.0,
                        },
                    },
                    inputs: vec![],
                    content_hash: ContentHash(1),
                },
                IrNode {
                    op: IrOp::NativeGaussianBlur { sigma: 2.0 },
                    inputs: vec![(IrNodeId(0), OutPort::default())],
                    content_hash: ContentHash(2),
                },
            ],
            output: Some(IrNodeId(1)),
        };
        assert!(graph.validate_working_color_domain().is_ok());
        graph.working_color_domain = WorkingColorDomain::LegacyLinearRec709;
        assert!(graph.validate_working_color_domain().is_err());
        graph.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
        for sigma in [f32::NAN, f32::INFINITY, -1.0, 500.01] {
            graph.nodes[1].op = IrOp::NativeGaussianBlur { sigma };
            assert!(graph.validate_working_color_domain().is_err());
        }
        graph.nodes[1].op = IrOp::NativeGaussianBlur { sigma: 2.0 };
        graph.nodes[1].inputs.clear();
        assert!(graph.validate_working_color_domain().is_err());
        graph.nodes[1].inputs = vec![(IrNodeId(0), OutPort::default()); 2];
        assert!(graph.validate_working_color_domain().is_err());
        graph.nodes[1] = IrNode {
            op: IrOp::NativeAcescct {
                direction: AcescctDirection::Encode,
            },
            inputs: vec![(IrNodeId(0), OutPort::default())],
            content_hash: ContentHash(3),
        };
        graph.nodes.push(IrNode {
            op: IrOp::NativeGaussianBlur { sigma: 2.0 },
            inputs: vec![(IrNodeId(1), OutPort::default())],
            content_hash: ContentHash(4),
        });
        graph.output = Some(IrNodeId(2));
        assert!(graph.validate_working_color_domain().is_err());
    }

    #[test]
    fn acescg_graph_rejects_legacy_grade_stack() {
        let grade = IrNode {
            op: IrOp::Grade { ops: Vec::new() },
            inputs: Vec::new(),
            content_hash: ContentHash(1),
        };
        let legacy = FrameGraph {
            nodes: vec![grade.clone()],
            ..Default::default()
        };
        assert_eq!(
            legacy.working_color_domain,
            WorkingColorDomain::LegacyLinearRec709
        );
        assert!(legacy.validate_working_color_domain().is_ok());
        let managed = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![grade],
            ..Default::default()
        };
        assert_eq!(
            managed.validate_working_color_domain(),
            Err("legacy grade operation cannot run in scene-linear ACEScg")
        );
    }

    #[test]
    fn color_domain_validation_rejects_mixed_and_forward_edges() {
        let source = IrNode {
            op: IrOp::SolidColor {
                color: LinearColor {
                    r: 0.1,
                    g: 0.1,
                    b: 0.1,
                    a: 1.0,
                },
            },
            inputs: vec![],
            content_hash: ContentHash(1),
        };
        let encoded = IrNode {
            op: IrOp::NativeAcescct {
                direction: AcescctDirection::Encode,
            },
            inputs: vec![(IrNodeId(0), OutPort::default())],
            content_hash: ContentHash(2),
        };
        let merge = IrNode {
            op: IrOp::Merge {
                mode: BlendMode::Normal,
                opacity: 1.0,
            },
            inputs: vec![
                (IrNodeId(1), OutPort::default()),
                (IrNodeId(0), OutPort::default()),
            ],
            content_hash: ContentHash(3),
        };
        let mut graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![source, encoded, merge],
            output: Some(IrNodeId(2)),
        };
        assert_eq!(
            graph.validate_working_color_domain(),
            Err("graph operation mixes different color domains")
        );
        graph.nodes[2].inputs[0].0 = IrNodeId(3);
        assert_eq!(
            graph.validate_working_color_domain(),
            Err("color-domain input is not topologically available")
        );
    }

    #[test]
    fn node_color_encoding_tracks_scene_log_and_display_taps() {
        let ops = [
            IrOp::SolidColor {
                color: LinearColor {
                    r: 0.18,
                    g: 0.18,
                    b: 0.18,
                    a: 1.0,
                },
            },
            IrOp::NativeAcescct {
                direction: AcescctDirection::Encode,
            },
            IrOp::NativeAcescct {
                direction: AcescctDirection::Decode,
            },
            IrOp::NativeSdrOutput,
        ];
        let nodes = ops
            .into_iter()
            .enumerate()
            .map(|(i, op)| IrNode {
                op,
                inputs: if i == 0 {
                    vec![]
                } else {
                    vec![(IrNodeId((i - 1) as u32), OutPort::default())]
                },
                content_hash: ContentHash(i as u128),
            })
            .collect();
        let graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes,
            output: Some(IrNodeId(3)),
        };
        assert_eq!(
            graph.node_color_encoding(IrNodeId(0)),
            Ok(FrameColorEncoding::SceneLinearAcescg)
        );
        assert_eq!(
            graph.node_color_encoding(IrNodeId(1)),
            Ok(FrameColorEncoding::Acescct)
        );
        assert_eq!(
            graph.node_color_encoding(IrNodeId(2)),
            Ok(FrameColorEncoding::SceneLinearAcescg)
        );
        assert_eq!(
            graph.node_color_encoding(IrNodeId(3)),
            Ok(FrameColorEncoding::SrgbDisplay)
        );
        assert_eq!(
            graph.output_color_encoding(),
            Ok(FrameColorEncoding::SrgbDisplay)
        );
        assert_eq!(
            graph.node_color_encoding(IrNodeId(4)),
            Err("color-domain tap is not topologically available")
        );
    }

    #[test]
    fn native_video_source_requires_explicit_managed_input() {
        use photonic_core::timeline::color::{
            InputMatrix, InputSignalRange, NativeInputColorInterpretation, NativeInputStandard,
        };
        let valid = NativeInputColorInterpretation {
            hlg_peak_nits: None,
            reference_white_nits: None,
            version: 1,
            standard: NativeInputStandard::Bt709Scene,
            range: InputSignalRange::Limited,
            matrix: InputMatrix::Bt709,
            chroma_location: None,
        };
        let node = IrNode {
            op: IrOp::NativeDecodeVideo {
                asset: AssetId::new(),
                src_time: Tick(0),
                input: valid.clone(),
            },
            inputs: vec![],
            content_hash: ContentHash(1),
        };
        let mut graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![node],
            output: Some(IrNodeId(0)),
        };
        assert_eq!(
            graph.output_color_encoding(),
            Ok(FrameColorEncoding::SceneLinearAcescg)
        );
        graph.working_color_domain = WorkingColorDomain::LegacyLinearRec709;
        assert_eq!(
            graph.validate_working_color_domain(),
            Err("native video source requires a valid managed ACEScg input")
        );
        graph.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
        if let IrOp::NativeDecodeVideo { input, .. } = &mut graph.nodes[0].op {
            input.range = InputSignalRange::FromMetadata;
        }
        assert_eq!(
            graph.validate_working_color_domain(),
            Err("native video source requires a valid managed ACEScg input")
        );
        graph.nodes[0].op = IrOp::DecodeVideo {
            asset: AssetId::new(),
            src_time: Tick(0),
            proxy: false,
        };
        assert_eq!(
            graph.validate_working_color_domain(),
            Err("legacy video source cannot enter a managed graph")
        );
    }

    #[test]
    fn threading_defaults_serial_safe_for_matte_and_any_for_pure() {
        assert_eq!(
            threading_for_op(&IrOp::SolidColor {
                color: LinearColor {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            }),
            Threading::Any
        );
        assert_eq!(
            threading_for_op(&IrOp::MatteExtract {
                model: MatteModel::U2NetP,
            }),
            Threading::Serial
        );
        assert_eq!(
            threading_for_op(&IrOp::DecodeStill {
                asset: AssetId::new(),
            }),
            Threading::PerInstance
        );
        assert_eq!(
            threading_for_op(&IrOp::Deinterlace {
                method: DeinterlaceMethod::LinearBlend,
                field_order: FieldOrder::TopFirst,
            }),
            Threading::Serial
        );
    }

    #[test]
    fn texture_bucket_rounds_up_to_64() {
        assert_eq!(
            TextureDesc {
                width: 1920,
                height: 1080
            }
            .bucket(),
            (1920, 1088)
        );
        assert_eq!(
            TextureDesc {
                width: 1,
                height: 64
            }
            .bucket(),
            (64, 64)
        );
        assert_eq!(
            TextureDesc {
                width: 1921,
                height: 1081
            }
            .bucket(),
            (1984, 1088)
        );
    }

    #[test]
    fn frame_rate_ticks_are_integral_for_ntsc() {
        use crate::contract::{FrameRate, TICKS_PER_SECOND};
        let r2997 = FrameRate {
            num: 30000,
            den: 1001,
        };
        assert_eq!(r2997.ticks_per_frame().0, TICKS_PER_SECOND / 30000 * 1001);
        assert_eq!(r2997.ticks_per_frame().0 * 30000, TICKS_PER_SECOND * 1001);
    }
}
