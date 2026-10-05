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
use photonic_render::native_transfer::AcescctDirection;

use crate::contract::{
    AssetId, CaptionBatch, EffectKind, MatteModel, ResolvedGradeOp, ResolvedParams,
    ResolvedTextBlock, Tick, VectorRef, VectorStateKey,
};

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
        | IrOp::NativeExposure { .. }
        | IrOp::NativeLinearOffset { .. }
        | IrOp::NativePrinterLights { .. }
        | IrOp::NativeHighlightRolloff { .. }
        | IrOp::NativeSaturationVibrance { .. }
        | IrOp::NativeLogContrast { .. }
        | IrOp::NativeLogCdl { .. }
        | IrOp::QualifierMatte { .. }
        | IrOp::GradeKeyMix { .. }
        | IrOp::GradeMatteRefine { .. }
        | IrOp::GradeMatteApply
        | IrOp::GradeMatteConstant { .. }
        | IrOp::GradeLayerMix { .. }
        | IrOp::NativeLogQualifier { .. }
        | IrOp::NativeLogCurves { .. }
        | IrOp::NativeLut3d { .. }
        | IrOp::NativeMaskMix { .. }
        | IrOp::NativeAcescct { .. }
        | IrOp::NativeSdrOutput
        | IrOp::NativeSdrVideoOutput
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
        | IrOp::RasterVector { .. }
        | IrOp::TextGen { .. }
        | IrOp::CaptionOverlay { .. } => Threading::PerInstance,

        // Matte inference is a sequential CPU worker op.
        IrOp::MatteExtract { .. } => Threading::Serial,

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
    /// Decoded still image (cached per asset).
    DecodeStill {
        asset: AssetId,
    },
    /// Rasterized vector content (Tier A CPU-readback or Tier B GPU-direct,
    /// 03 §2.5), cached by `doc_state`.
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

    fn color_domains(&self) -> Result<Vec<FrameColorEncoding>, &'static str> {
        use FrameColorEncoding as Domain;
        let root = match self.working_color_domain {
            WorkingColorDomain::LegacyLinearRec709 => Domain::LegacyLinearRec709,
            WorkingColorDomain::SceneLinearAcescg => Domain::SceneLinearAcescg,
        };
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
                let expected = if matches!(node.op, IrOp::GradeMatteApply) && index == 1 {
                    Domain::MatteWeight
                } else {
                    input
                };
                if other? != expected {
                    return Err("graph operation mixes different color domains");
                }
            }
            if input == Domain::MatteWeight
                && !matches!(
                    node.op,
                    IrOp::GradeKeyMix { .. } | IrOp::GradeMatteRefine { .. }
                )
            {
                return Err("matte weights cannot enter an image operation");
            }
            if matches!(input, Domain::SrgbDisplay | Domain::Bt709Video) {
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
                        | IrOp::QualifierMatte { .. }
                        | IrOp::NativeLogQualifier { .. }
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
                        )
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
                IrOp::Merge { mode, .. }
                    if input == Domain::SceneLinearAcescg && mode != BlendMode::Normal =>
                {
                    return Err("non-Normal blend mode is not qualified in scene-linear ACEScg");
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
        match self.output.and_then(|id| self.nodes.get(id.0 as usize)) {
            Some(IrNode {
                op: IrOp::Output { w, h },
                ..
            }) if *w > 0 && *h > 0 => {
                glam::Vec2::new(canvas.0 as f32 / *w as f32, canvas.1 as f32 / *h as f32)
            }
            _ => glam::Vec2::ONE,
        }
    }
}

impl IrOp {
    /// Affine coordinates are authored in output pixels, including the pivot.
    /// Conjugation preserves scale/rotation while adapting translation to Draft.
    pub(crate) fn at_canvas_scale(&self, scale: glam::Vec2) -> std::borrow::Cow<'_, Self> {
        match self {
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
                    cue.font_size *= scale.y;
                }
                std::borrow::Cow::Owned(Self::TextGen { block })
            }
            Self::CaptionOverlay { cue_batch } if scale != glam::Vec2::ONE => {
                let mut cue_batch = cue_batch.clone();
                for cue in &mut cue_batch.cues {
                    cue.font_size *= scale.y;
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
    fn managed_graph_qualifies_only_normal_blend() {
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
        assert!(graph(BlendMode::Normal)
            .validate_working_color_domain()
            .is_ok());
        assert_eq!(
            graph(BlendMode::Multiply).validate_working_color_domain(),
            Err("non-Normal blend mode is not qualified in scene-linear ACEScg")
        );
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
