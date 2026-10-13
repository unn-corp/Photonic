//! Frame-graph compiler (02 §2 "Compilation"): lowers a timeline snapshot at one
//! `(sequence, format, tick, quality)` tuple into a [`FrameGraph`] IR.
//!
//! The compiler is a **pure function** of its inputs — same inputs ⇒ identical
//! graph ⇒ (via the evaluator) identical pixels (02 §2's normative property).
//! Every keyframe is resolved here, at compile time, so the evaluator is
//! time-ignorant. Every node carries a [`ContentHash`] of
//! `hash(op discriminant, resolved params, input hashes)` — deterministic across
//! runs (no `Instant`, no random state), which is what makes the node-result
//! cache (02 §5) and golden tests (11) possible. Identical subgraphs collapse
//! to one node via that hash (the mechanism behind `TimeOffset` dedup, 02 §2
//! step 7 / 08 §3.4).
//!
//! The numbered steps below mirror 02 §2 exactly:
//! 1. per enabled video track, find the clip covering `t`;
//! 2. per clip build the chain source → **asset effects/grade** → Transform2D →
//!    clip effects → clip grade (the four effect scopes, 35 §2, all share the one
//!    "effects beneath grade" ordering rule in [`apply_stack`]). **Frame-rate
//!    conform (38 §3):** a source whose rate ≠ the sequence rate is conformed by
//!    nearest-covering-source-frame selection — `DecodeVideo { src_time }` picks
//!    the covering source frame with no blending and no rate conversion, so
//!    preview and export are identical (they differ only in `proxy`). A per-clip
//!    `FrameRateConformed` Info states this; it is not emergent behaviour.
//! 3. per-clip composition splices the clip's **source op only** (02 §2 step 3 /
//!    08 §4), the still-applied Transform2D/effects/grade chain riding on top;
//! 4. fold tracks with `Merge` — each track's own content passes through its
//!    **track effects/grade** and merges at the track's `blend`/`opacity` (35 §2);
//!    Adjustment clips re-root the stack below;
//! 5. the **master effects/grade** (35 §2.4(d)) run on the folded program, then
//!    `CaptionOverlay` from enabled caption tracks covering `t` (captions ride
//!    above the master grade, in final display colour);
//! 6. splice the project graph (08 §5) between the fold result and `Output`;
//! 7. `TimeOffset` expansion by re-lowering the upstream subgraph at `t−offset`
//!    (dedup-by-hash keeps it bounded; soft cap 4 distinct offsets);
//! 8. constant-fold / dead-branch-eliminate (disabled clips, opacity 0).
//!
//! **Nesting is one cache subtree (38 §2.5):** a nested sequence lowers through
//! the same content-hash dedup as everything else, so N nest clips referencing
//! the same sequence at the same effective `src_time` (same source_in and same
//! start-relative offset, no per-clip transform/effect differences) collapse to
//! ONE shared subtree — the inner sources evaluate once, not N times. This is a
//! strong argument for nesting over duplication and is otherwise invisible; it is
//! stated here and pinned by `ten_identical_nests_share_one_subtree`. A nest also
//! renders in the OUTER format, not the inner sequence's `active_format` (38 §2.3).

use std::collections::{HashMap, HashSet};

use glam::{Mat3, Vec2};
use photonic_core::color::Color;
use photonic_core::layer::BlendMode;
use photonic_core::timeline::{
    self, AnchorSpace, AnimProps, AssetKind, CaptionAnim, CaptionCue, CaptionStyle, CaptionTrack,
    CaptionWord, Clip, ClipEffect, ClipId, ClipSource, ClipTransform, EaseCurve, EffectKind,
    EffectParams, FrameRate, Grade, GradeGraph, GradeGraphNode, GradeOp, GradeOpId, GradeOpKind,
    GradeOpParams, GraphId, GraphNode, GraphNodeId, GraphNodeParams, GraphOp, InPort, KaraokeMode,
    LutInterp, NodeGraph, PropPath, PropSet, PropTargetKind, PropValue, Ratio, ScanType, Sequence,
    SequenceFormat, SequenceId, SpeedMap, TextClipContent, TimeSource, TimelineProject,
    TransitionKind, VfxOwner,
};
use photonic_render::caption::CaptionWordRun;

use crate::contract::{
    AssetId, CaptionBatch, CaptionCueRun, MatteModel, ResolvedParams, ResolvedTextBlock, Tick,
    VectorRef, VectorStateKey, TICKS_PER_SECOND,
};
use crate::graph::ir::{
    Channel, ContentHash, DeinterlaceMethod, FieldOrder, FitMode, FrameGraph, IrNode, IrNodeId,
    IrOp, LinearColor, NativeDetailKind, NativeStylizeKind, NativeWarpKind, OutPort, Sampling,
    TextureDesc, WipeDirection, WorkingColorDomain,
};

/// K-G6: if the asset's probe reports interlaced, return the default
/// deinterlace method + field order for auto-insertion after `DecodeVideo`.
fn deinterlace_for_asset(
    project: &TimelineProject,
    asset: AssetId,
) -> Option<(DeinterlaceMethod, FieldOrder)> {
    let a = project.media.assets.get(&asset)?;
    let v = a.probe.as_ref()?.video.as_ref()?;
    if !v.scan.is_interlaced() {
        return None;
    }
    let order = match v.scan {
        ScanType::InterlacedBottomFirst => FieldOrder::BottomFirst,
        _ => FieldOrder::TopFirst,
    };
    // Default algorithm: linear blend — cheap, always available, good enough
    // for preview; Yadif spatial is selectable later via clip policy.
    Some((DeinterlaceMethod::LinearBlend, order))
}

/// Preview vs full-resolution compile flags (02 §2's "quality flags"). `proxy`
/// selects proxy media where available (session state, `SetProxyMode`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Quality {
    /// Decode proxy media instead of originals (preview). Export forces `false`.
    /// `Default` (`false`) is full quality.
    pub proxy: bool,
}

impl Quality {
    /// Preview quality (proxy media allowed).
    pub const PREVIEW: Quality = Quality { proxy: true };
    /// Full quality (originals; the export/scopes path).
    pub const FULL: Quality = Quality { proxy: false };
}

/// Session-only viewer pin (08 §6.7): reroute the effective output to a specific
/// node of the graph currently being edited, without changing the DAG that gets
/// built (shared upstream nodes stay cache-compatible). Never carried by export.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ViewNodeOverride {
    pub graph: GraphId,
    pub node: GraphNodeId,
}

/// Supplies parsed 3D-LUT tables so `Grade` `Lut3d` ops resolve to a ready-to-
/// sample table (K-0.5 / 07 §3.8). Object-safe and threaded as `&dyn LutProvider`
/// (never a generic) so `compile`'s call tree is not monomorphised over the
/// provider type.
///
/// **Hot-path invariant:** [`lut`](LutProvider::lut) is called during compile,
/// which runs per frame, so it MUST be a lock-free read of a pre-warmed cache —
/// never parse a `.cube` file here. A `None` result (offline / unresolvable /
/// failed asset) keeps the LUT op inert (identity), never a black frame (07 §1).
pub struct NativeLutBinding {
    pub table: std::sync::Arc<photonic_render::Lut3d>,
    pub space: timeline::color::NativeLutSpace,
}

pub trait LutProvider {
    fn lut(&self, asset: AssetId) -> Option<std::sync::Arc<photonic_render::Lut3d>>;
    /// Return only a verified, pinned creative LUT with equal native input/output spaces.
    fn native_lut(&self, _asset: AssetId) -> Option<NativeLutBinding> {
        None
    }
}

struct NativeLutView<'a> {
    provider: &'a dyn LutProvider,
    project: &'a TimelineProject,
}
impl LutProvider for NativeLutView<'_> {
    fn lut(&self, asset: AssetId) -> Option<std::sync::Arc<photonic_render::Lut3d>> {
        self.native_lut(asset).map(|binding| binding.table)
    }
    fn native_lut(&self, asset: AssetId) -> Option<NativeLutBinding> {
        let media = self.project.media.assets.get(&asset)?;
        if media.kind != AssetKind::Lut3d || media.lut_full_hash.is_none() {
            return None;
        }
        let space = media.lut_color.as_ref()?.validate_for_native_grade().ok()?;
        let binding = self.provider.native_lut(asset)?;
        (binding.space == space).then_some(binding)
    }
}

/// Supplies the compile-resolved deflicker gain for a clip at a tick
/// (see [`crate::graph::deflicker`]).
///
/// Deflicker is measured over a *whole range* and applied per frame, so the gain
/// cannot be derived from the timeline alone the way every other effect param
/// can — it is the verdict of an analysis job. Threading it in here keeps that
/// asymmetry in one place: the evaluator stays pure, and a clip with no
/// measurement simply lowers with no gain (an exact pass-through).
pub trait DeflickerGains {
    /// Per-channel gain for `clip` at clip-relative `dt`, or `None` when this
    /// clip has not been measured.
    fn gain(&self, clip: ClipId, dt: Tick) -> Option<[f32; 3]>;

    /// Owner-aware native lookup. Existing clip-only providers remain valid;
    /// shared owners must provide an explicit keyed measurement.
    fn native_gain(
        &self,
        key: &super::deflicker::NativeDeflickerKey,
        at: Tick,
    ) -> Option<[f32; 3]> {
        match key.owner {
            VfxOwner::Clip(clip) => self.gain(clip, at),
            _ => None,
        }
    }
    fn native_band(
        &self,
        key: &super::deflicker::NativeDeflickerKey,
        at: Tick,
    ) -> Option<super::rolling_bands::BandModel> {
        match key.owner {
            VfxOwner::Clip(clip) => self.band(clip, at),
            _ => None,
        }
    }
    /// Private analysis callers may collect unresolved dependencies, but must
    /// prove the selected input has none before evaluating it.
    fn analysis_mode(&self) -> bool {
        false
    }

    /// Rolling-band fit for `clip` at `dt`, if the detector found one. Defaults
    /// to `None` so a store that only does exposure need not implement it.
    fn band(&self, _clip: ClipId, _dt: Tick) -> Option<crate::graph::rolling_bands::BandModel> {
        None
    }
}

/// Resolved D-12 stabilization analysis for a clip (22 §6.4), threaded into
/// compile as `&dyn StabilizationProvider`.
///
/// **Hot-path invariant**, identical to [`LutProvider`]'s: this is called during
/// compile, which runs per frame, so it MUST be a lock-free read of an
/// already-computed analysis — never parse a motion file or run the integrator
/// here. A `None` result (unanalyzed, analysis in flight, cache miss) leaves
/// the clip on its unstabilized source path rather than stalling the frame or
/// rendering black, matching how an unresolvable LUT stays inert.
pub trait StabilizationProvider {
    /// The analysis for `clip`, if one is warm.
    fn analysis(
        &self,
        clip: ClipId,
        key: &str,
        source_start_s: f64,
        source_end_s: f64,
    ) -> Option<std::sync::Arc<crate::graph::stabilize::StabilizationAnalysis>>;
}

/// A stable diagnostic code for the coded compile/load conditions 38 registers
/// (§1.2 / §2.2 / §2.4 / §3.5). Kept as a compiler-local enum until 36 §3's
/// `DiagCode` registry lands; the variant names are byte-identical to 36's
/// registry (`TransitionHandleClipped`, `NestedSequenceShortened`,
/// `FrameRateConformed`) so folding `CompileCode` into `DiagCode::Compile*` /
/// `Media::*` is a mechanical rename.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum CompileCode {
    /// A sequence requires a color pipeline that this build cannot execute.
    ColorPipelineUnavailable,
    /// An enabled grading corrector was bypassed because it cannot resolve.
    /// Preview remains available; final export must reject this frame.
    GradeUnresolved,
    /// 38 §1.2 — a transition was shortened (Info) or suppressed (Warning)
    /// because the outgoing clip's source handle is too short.
    TransitionHandleClipped,
    /// 38 §2.4 — a nest references past the inner sequence's content; the last
    /// rendered frame is held (Warning).
    NestedSequenceShortened,
    /// 38 §2.2 / §3.5 — a source (or nested sequence) rate differs from the
    /// sequence rate; frames are conformed by nearest-covering selection (Info).
    FrameRateConformed,
    /// Direction-unsafe spatial karaoke is replaced with binary word highlighting.
    CaptionSweepDirectionFallback,
}

/// Severity of a [`CompileDiagnostic`]. Mirrors 36 §3's `Severity` so the two
/// merge without a value remap when the shared registry lands.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum DiagSeverity {
    #[default]
    Info,
    Warning,
    Error,
}

/// A compile diagnostic (02 §2 step 3 / 08 §6.6). Carries the offending
/// `GraphNodeId` where one applies so the node editor can badge the exact node,
/// not just show a generic "composition failed" toast. `code`/`severity`/`clip`
/// are the typed channel 38 needs (defaulting to an uncoded `Info` with no clip,
/// so every pre-existing `plain`/`at` call is unchanged).
#[derive(Clone, Debug, PartialEq)]
pub struct CompileDiagnostic {
    /// Structured corrector/dependency context for grading failures.
    pub grade: Option<photonic_render::grade::GradeDiagnostic>,
    pub message: String,
    pub graph: Option<GraphId>,
    pub node: Option<GraphNodeId>,
    pub code: Option<CompileCode>,
    pub severity: DiagSeverity,
    pub clip: Option<ClipId>,
}

impl CompileDiagnostic {
    fn plain(message: impl Into<String>) -> Self {
        CompileDiagnostic {
            grade: None,
            message: message.into(),
            graph: None,
            node: None,
            code: None,
            severity: DiagSeverity::Info,
            clip: None,
        }
    }
    fn at(graph: GraphId, node: GraphNodeId, message: impl Into<String>) -> Self {
        CompileDiagnostic {
            grade: None,
            message: message.into(),
            graph: Some(graph),
            node: Some(node),
            code: None,
            severity: DiagSeverity::Info,
            clip: None,
        }
    }
    /// A coded, severity-tagged diagnostic optionally anchored to a clip (38's
    /// typed channel). The subject is the clip, not a graph node.
    fn coded(
        code: CompileCode,
        severity: DiagSeverity,
        clip: Option<ClipId>,
        message: impl Into<String>,
    ) -> Self {
        CompileDiagnostic {
            grade: None,
            message: message.into(),
            graph: None,
            node: None,
            code: Some(code),
            severity,
            clip,
        }
    }
}

/// The result of a compile: the graph plus any diagnostics (never black-frames
/// silently — a failed splice falls back to the default chain and records why).
///
/// `program_tap` / `clip_taps` are the K-E2 scope readback points (03 §3.6,
/// 07 §5). They are **indices into `graph`, not extra nodes**: every tap is a
/// node the program evaluation already renders, so reading one costs no extra
/// evaluation (see [`ScopeTapPoint`]).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CompiledFrame {
    pub native_deflicker_inputs: Vec<(super::deflicker::NativeDeflickerKey, IrNodeId)>,
    pub native_deflicker_pending: Vec<(super::deflicker::NativeDeflickerKey, IrNodeId)>,
    pub graph: FrameGraph,
    pub diagnostics: Vec<CompileDiagnostic>,
    /// The folded program after the master stack and **before** `CaptionOverlay`
    /// (03 §3.6). `None` for an empty sequence / an asset peek.
    pub program_tap: Option<IrNodeId>,
    /// Every clip lowered into this frame, at its post-`Grade` node — the clip's
    /// own texture before the track fold (07 §5). Clips whose span does not cover
    /// the compiled tick, or that fold away (disabled track, zero opacity), are
    /// absent: that absence IS the 13 §10.2 "playhead is not over the clip"
    /// fallback signal. A `Vec` and not a `HashMap` because a compiled frame
    /// holds a handful of clips and this is per-frame hot-path allocation.
    pub clip_taps: Vec<(ClipId, IrNodeId)>,
    /// Clip image immediately before its own grade, after source effects,
    /// clip effects, transform and group pre-grade. Used by qualifier matte
    /// inspection so the key sees the same input as the selected corrector.
    pub clip_pre_grade_taps: Vec<(ClipId, IrNodeId)>,
    /// Exact animated correctors resolved for ordinary clip grades that
    /// contain a qualifier. IDs are retained because unresolved/disabled ops
    /// make authoring indices differ from rendered indices.
    pub clip_grade_inspections: Vec<ClipGradeInspection>,
    /// Exact scene input and key for native clip qualifier inspection (unambiguous graph correctors included).
    pub native_qualifier_inspections: Vec<NativeQualifierInspection>,
    /// Exact scene inputs before native clip curves, including neutral curves.
    pub native_curve_inputs: Vec<(ClipId, GradeOpId, IrNodeId)>,
    pub native_graph_qualifier_inspections: Vec<(u32, NativeQualifierInspection)>,
    pub native_graph_curve_inputs: Vec<(ClipId, u32, GradeOpId, IrNodeId)>,
    /// Rendered, unassociated weights from reachable native clip matte nodes.
    pub native_graph_mattes: Vec<(ClipId, u32, IrNodeId)>,
    pub native_graph_images: Vec<(ClipId, u32, IrNodeId)>,
    pub native_stage_graph_outputs: Vec<GradeGraphStageOutput>,
    pub native_stage_grade_inputs: Vec<NativeStageGradeInput>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GradeGraphStageOutput {
    pub clip: ClipId,
    pub stage: timeline::GradeInspectionStage,
    pub node: u32,
    pub image: bool,
    pub output: IrNodeId,
}
impl GradeGraphStageOutput {
    pub fn find(
        outputs: &[Self],
        clip: ClipId,
        stage: timeline::GradeInspectionStage,
        node: u32,
        image: bool,
    ) -> Option<&Self> {
        let mut matches = outputs
            .iter()
            .filter(|entry| entry.matches(clip, stage, node, image));
        let found = matches.next()?;
        matches.next().is_none().then_some(found)
    }
    pub fn matches(
        &self,
        clip: ClipId,
        stage: timeline::GradeInspectionStage,
        node: u32,
        image: bool,
    ) -> bool {
        self.clip == clip && self.stage == stage && self.node == node && self.image == image
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClipGradeInspection {
    pub clip: ClipId,
    pub ops: Vec<(GradeOpId, photonic_render::grade::ResolvedGradeOp)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NativeQualifierInspection {
    pub spatial: Option<timeline::GradeQualifierSpatial>,
    pub clip: ClipId,
    pub op: GradeOpId,
    pub input: IrNodeId,
    pub qualifier: Box<photonic_render::grade::ResolvedHslQualifier>,
    pub mask: Option<photonic_render::grade::ResolvedMask>,
}

/// Exact correction input within a clip's own or linked look grade stage.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeStageGradeInput {
    pub clip: ClipId,
    pub stage: timeline::GradeInspectionStage,
    pub node: Option<u32>,
    pub op: GradeOpId,
    pub input: IrNodeId,
    pub corrector: bool,
    pub qualifier: Option<NativeQualifierInspection>,
}
impl NativeStageGradeInput {
    pub fn find(
        inputs: &[Self],
        clip: ClipId,
        stage: timeline::GradeInspectionStage,
        node: Option<u32>,
        op: GradeOpId,
    ) -> Option<&Self> {
        let mut matching = inputs.iter().filter(|input| {
            input.clip == clip
                && input.stage == stage
                && input.op == op
                && node.map_or(input.corrector, |node| input.node == Some(node))
        });
        let first = matching.next()?;
        matching.next().is_none().then_some(first)
    }
}

/// Which texture the scopes read (K-E2 / 03 §3.6, reconciled with 07 §5's
/// per-clip-with-fallback wording by 27 A-7).
///
/// Both variants name a node the frame graph *already contains*, so switching
/// the tap never adds a render pass — the tap is a lookup of an intermediate
/// result the program evaluation produced anyway.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum ScopeTapPoint {
    /// Sequence output, post-master-grade, **pre-`CaptionOverlay`** (03 §3.6).
    /// The fallback 07 §5 / 13 §10.2 mandate when no clip is selected — and
    /// deliberately not the *presented* frame, which is post-caption and so
    /// measures burnt-in caption pixels the colourist is not grading. The
    /// Native Managed program tap is after the display/output transform and
    /// before display-authored captions; it carries that explicit encoding.
    #[default]
    Program,
    /// Qualified Native Managed program immediately before its SDR output transform.
    NativeSceneProgram,
    /// The named clip's texture after its own `Grade`, before the track fold
    /// (07 §5). Falls back to [`ScopeTapPoint::Program`] when the clip is not in
    /// this frame.
    Clip(ClipId),
    /// The named clip immediately before its own grade. Intended for grade
    /// inspection; falls back to Program when the clip is not in this frame.
    ClipPreGrade(ClipId),
    /// Native qualifier's scene input after earlier correctors, before its key/CDL.
    NativeQualifierInput {
        clip: ClipId,
        op: GradeOpId,
    },
    /// Native curve scene input before this corrector.
    NativeCurveInput {
        clip: ClipId,
        op: GradeOpId,
    },
    NativeGraphQualifierInput {
        clip: ClipId,
        node: u32,
        op: GradeOpId,
    },
    GradeGraphImageOutput {
        clip: ClipId,
        node: u32,
    },
    GradeGraphMatteOutput {
        clip: ClipId,
        node: u32,
    },
    GradeStageInput {
        clip: ClipId,
        stage: timeline::GradeInspectionStage,
        node: Option<u32>,
        op: GradeOpId,
    },
    GradeStageGraphImageOutput {
        clip: ClipId,
        stage: timeline::GradeInspectionStage,
        node: u32,
    },
    GradeStageGraphMatteOutput {
        clip: ClipId,
        stage: timeline::GradeInspectionStage,
        node: u32,
    },
    NativeGraphCurveInput {
        clip: ClipId,
        node: u32,
        op: GradeOpId,
    },
}

impl CompiledFrame {
    /// Bind native vector qualification to the exact immutable drawing snapshot.
    /// Invalid sources become explicit diagnostic frames, never stale success.
    pub fn qualify_native_vectors(&mut self, document: Option<&photonic_core::Document>) {
        if !self
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeRasterVector { .. }))
        {
            return;
        }
        let result = document
            .ok_or("managed vector document snapshot is missing")
            .and_then(photonic_render::native_vector::validate_document);
        let Err(message) = result else {
            return;
        };
        self.diagnostics.push(CompileDiagnostic::coded(
            CompileCode::ColorPipelineUnavailable,
            DiagSeverity::Error,
            None,
            message,
        ));
        let mut hashes = Vec::with_capacity(self.graph.nodes.len());
        for n in &mut self.graph.nodes {
            if matches!(n.op, IrOp::NativeRasterVector { .. }) {
                n.op = IrOp::SolidColor {
                    color: LinearColor {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.0,
                    },
                };
                n.inputs.clear();
            }
            let inputs: Vec<u128> = n
                .inputs
                .iter()
                .map(|(id, _)| hashes[id.0 as usize])
                .collect();
            n.content_hash = content_hash(&n.op, &n.inputs, &inputs);
            hashes.push(n.content_hash.0);
        }
    }

    /// Resolve `point` to an IR node, or `None` when this frame has no such tap
    /// (empty program, or a clip the playhead is not over). Callers own the
    /// fallback policy; [`CompiledFrame::resolve_tap`] applies the 13 §10.2 one.
    pub fn tap(&self, point: ScopeTapPoint) -> Option<IrNodeId> {
        match point {
            ScopeTapPoint::Program => self.program_tap,
            ScopeTapPoint::NativeSceneProgram => {
                let output = self.graph.nodes.get(self.program_tap?.0 as usize)?;
                if !matches!(
                    output.op,
                    IrOp::NativeSdrOutput | IrOp::NativeSdrVideoOutput
                ) {
                    return None;
                }
                let source = output.inputs.first()?.0;
                (self.graph.node_color_encoding(source).ok()?
                    == crate::graph::ir::FrameColorEncoding::SceneLinearAcescg)
                    .then_some(source)
            }
            ScopeTapPoint::GradeGraphImageOutput { clip, node } => self
                .native_graph_images
                .iter()
                .find(|(c, id, _)| *c == clip && *id == node)
                .map(|(_, _, output)| *output),
            ScopeTapPoint::GradeGraphMatteOutput { clip, node } => self
                .native_graph_mattes
                .iter()
                .find(|(c, id, _)| *c == clip && *id == node)
                .map(|(_, _, output)| *output),
            ScopeTapPoint::GradeStageInput {
                clip,
                stage,
                node,
                op,
            } => {
                NativeStageGradeInput::find(&self.native_stage_grade_inputs, clip, stage, node, op)
                    .map(|input| input.input)
            }
            ScopeTapPoint::GradeStageGraphImageOutput { clip, stage, node } => {
                GradeGraphStageOutput::find(
                    &self.native_stage_graph_outputs,
                    clip,
                    stage,
                    node,
                    true,
                )
                .map(|entry| entry.output)
            }
            ScopeTapPoint::GradeStageGraphMatteOutput { clip, stage, node } => {
                GradeGraphStageOutput::find(
                    &self.native_stage_graph_outputs,
                    clip,
                    stage,
                    node,
                    false,
                )
                .map(|entry| entry.output)
            }
            ScopeTapPoint::Clip(id) => self
                .clip_taps
                .iter()
                .find(|(c, _)| *c == id)
                .map(|(_, n)| *n),
            ScopeTapPoint::NativeGraphQualifierInput { clip, node, op } => self
                .native_graph_qualifier_inspections
                .iter()
                .find(|(id, key)| *id == node && key.clip == clip && key.op == op)
                .map(|(_, key)| key.input),
            ScopeTapPoint::NativeGraphCurveInput { clip, node, op } => self
                .native_graph_curve_inputs
                .iter()
                .find(|(c, id, operator, _)| *c == clip && *id == node && *operator == op)
                .map(|(_, _, _, input)| *input),
            ScopeTapPoint::NativeCurveInput { clip, op } => self
                .native_curve_inputs
                .iter()
                .find(|(c, o, _)| *c == clip && *o == op)
                .map(|(_, _, input)| *input),
            ScopeTapPoint::NativeQualifierInput { clip, op } => self
                .native_qualifier_inspections
                .iter()
                .find(|inspection| inspection.clip == clip && inspection.op == op)
                .map(|inspection| inspection.input),
            ScopeTapPoint::ClipPreGrade(id) => self
                .clip_pre_grade_taps
                .iter()
                .find(|(c, _)| *c == id)
                .map(|(_, n)| *n),
        }
    }

    /// [`CompiledFrame::tap`] with the 13 §10.2 fallback applied: a clip tap that
    /// this frame does not carry degrades to the program tap rather than going
    /// blank. Returns the point actually used, so the UI can say which one it is
    /// ("Program" vs the clip's name) instead of silently lying.
    pub fn resolve_tap(&self, point: ScopeTapPoint) -> Option<(ScopeTapPoint, IrNodeId)> {
        if let Some(node) = self.tap(point) {
            return Some((point, node));
        }
        self.program_tap.map(|n| (ScopeTapPoint::Program, n))
    }
}

/// Soft cap on distinct `TimeOffset` values per composition (02 §2 step 7 /
/// 08 §3.4): beyond this a diagnostic warns but compilation still proceeds.
pub const TIME_OFFSET_SOFT_CAP: usize = 4;

/// Draft preview long-edge cap in pixels (24-preview-media-load §4).
pub const DRAFT_MAX_LONG_EDGE: u32 = 960;

/// Scale `(w, h)` so the long edge is ≤ `max_long_edge` (keeps aspect). No-op
/// when already smaller or either dim is zero.
pub fn fit_long_edge(w: u32, h: u32, max_long_edge: u32) -> (u32, u32) {
    if w == 0 || h == 0 || max_long_edge == 0 {
        return (w.max(1), h.max(1));
    }
    let long = w.max(h);
    if long <= max_long_edge {
        return (w, h);
    }
    let scale = max_long_edge as f64 / long as f64;
    let nw = ((w as f64) * scale).round().max(1.0) as u32;
    let nh = ((h as f64) * scale).round().max(1.0) as u32;
    (nw, nh)
}

/// Color encodings that the Legacy SDR BT.709/601 conversion cannot evaluate.
/// Unknown metadata retains legacy behavior; known HDR/log/wide-gamut tags fail closed.
fn unsupported_legacy_color<'a>(
    project: &'a TimelineProject,
    asset: AssetId,
) -> Option<(&'static str, &'a str)> {
    let color = &project
        .media
        .assets
        .get(&asset)?
        .probe
        .as_ref()?
        .video
        .as_ref()?
        .color;
    if let Some(transfer) = color.transfer.as_deref() {
        if matches!(
            transfer.to_ascii_lowercase().as_str(),
            "smpte2084" | "arib-std-b67" | "log100" | "log316" | "smpte428"
        ) {
            return Some(("transfer", transfer));
        }
    }
    if let Some(matrix) = color.matrix.as_deref() {
        if matches!(
            matrix.to_ascii_lowercase().as_str(),
            "bt2020nc"
                | "bt2020c"
                | "smpte2085"
                | "chroma-derived-nc"
                | "chroma-derived-c"
                | "ictcp"
        ) {
            return Some(("matrix", matrix));
        }
    }
    if let Some(primaries) = color.primaries.as_deref() {
        if matches!(
            primaries.to_ascii_lowercase().as_str(),
            "bt2020" | "smpte428" | "smpte431" | "smpte432"
        ) {
            return Some(("primaries", primaries));
        }
    }
    None
}

/// Single-asset source peek graph for the one-monitor `PreviewTarget::Asset`
/// path (24-preview-media-load §3). Decode/still → Output at `out_w`×`out_h`.
/// Isolated native source analysis tap. Unlike asset peek this has no display
/// transform; its output is explicitly scene-linear AP1 at a bounded size.
pub fn compile_native_asset_analysis(
    project: &TimelineProject,
    asset: AssetId,
    source_time: Tick,
    input: &timeline::color::NativeInputColorInterpretation,
    width: u32,
    height: u32,
) -> Result<CompiledFrame, String> {
    let media = project
        .media
        .assets
        .get(&asset)
        .ok_or("analysis source missing")?;
    input.validate_asset_kind(media.kind)?;
    if !matches!(media.kind, AssetKind::Video | AssetKind::Image)
        || width == 0
        || height == 0
        || width > 640
        || height > 360
    {
        return Err(
            "native analysis requires an image/video and bounded640x360 sample canvas".into(),
        );
    }
    if source_time < Tick::ZERO || deinterlace_for_asset(project, asset).is_some() {
        return Err("native analysis source time/interlace is unsupported".into());
    }
    let mut b = Builder::new();
    b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
    let src = b.push(
        if media.kind == AssetKind::Image {
            IrOp::NativeDecodeStill { asset }
        } else {
            IrOp::NativeDecodeVideo {
                asset,
                src_time: source_time,
                input: input.clone(),
            }
        },
        vec![],
    );
    let sample = b.push(
        IrOp::Resize {
            w: width,
            h: height,
            fit: super::ir::FitMode::Stretch,
        },
        vec![(src, OutPort::default())],
    );
    b.program_tap = Some(sample);
    Ok(b.finish(Some(sample)))
}

pub fn compile_asset_peek(
    project: &TimelineProject,
    asset: AssetId,
    source_time: Tick,
    quality: Quality,
    out_w: u32,
    out_h: u32,
) -> CompiledFrame {
    let mut b = Builder::new();
    let w = out_w.max(1);
    let h = out_h.max(1);
    if let Some(media) = project.media.assets.get(&asset) {
        if matches!(media.kind, AssetKind::Video | AssetKind::Image) {
            if let Some(input) = media.native_input_color.as_ref() {
                let error = input.validate_asset_kind(media.kind).err().or_else(|| {
                    deinterlace_for_asset(project, asset).map(|_| {
                        "native interlaced source peek needs a qualified pre-IDT deinterlace"
                            .to_owned()
                    })
                });
                if let Some(error) = error {
                    b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        None,
                        error,
                    ));
                    let blank = b.push(
                        IrOp::SolidColor {
                            color: LinearColor {
                                r: 0.0,
                                g: 0.0,
                                b: 0.0,
                                a: 0.0,
                            },
                        },
                        vec![],
                    );
                    let output = b.push(IrOp::Output { w, h }, vec![(blank, OutPort::default())]);
                    return b.finish(Some(output));
                }
                b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
                let source = b.push(
                    if media.kind == AssetKind::Image {
                        IrOp::NativeDecodeStill { asset }
                    } else {
                        IrOp::NativeDecodeVideo {
                            asset,
                            src_time: source_time,
                            input: input.clone(),
                        }
                    },
                    vec![],
                );
                b.program_tap = Some(source);
                let output = b.push(IrOp::NativeSdrOutput, vec![(source, OutPort::default())]);
                return b.finish(Some(output));
            }
        }
    }
    if let Some((field, value)) = unsupported_legacy_color(project, asset) {
        b.diag(CompileDiagnostic::coded(
            CompileCode::ColorPipelineUnavailable,
            DiagSeverity::Error,
            None,
            format!("Asset {asset} uses {value} {field}, which Legacy SDR cannot interpret"),
        ));
        let source = b.push(
            IrOp::SolidColor {
                color: LinearColor {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.0,
                },
            },
            vec![],
        );
        let output = b.push(IrOp::Output { w, h }, vec![(source, OutPort::default())]);
        return b.finish(Some(output));
    }
    let kind = project
        .media
        .assets
        .get(&asset)
        .map(|a| a.kind)
        .unwrap_or(AssetKind::Video);
    let src = match kind {
        AssetKind::Image => b.push(IrOp::DecodeStill { asset }, vec![]),
        AssetKind::Video | AssetKind::Audio | AssetKind::VectorDoc | AssetKind::Lut3d => b.push(
            IrOp::DecodeVideo {
                asset,
                src_time: source_time,
                proxy: quality.proxy,
            },
            vec![],
        ),
    };
    // K-E2: an asset peek has no clips and no fold, so its only readback point is
    // the decoded source itself. Recording it keeps the scopes panel usable while
    // the monitor is on a source peek (24 §3) instead of going "no signal".
    b.program_tap = Some(src);
    let output = b.push(IrOp::Output { w, h }, vec![(src, OutPort::default())]);
    b.finish(Some(output))
}

/// Conservative native-managed sequence preview. Explicitly interpreted video
/// tracks, Normal composites, and supported serial grade primaries are qualified.
/// Any authored stage this path cannot reproduce yields a diagnostic and blank frame.
pub fn compile_native_preview(
    project: &TimelineProject,
    sequence: SequenceId,
    format_index: usize,
    tick: Tick,
    quality: Quality,
) -> CompiledFrame {
    compile_native_sequence(
        project,
        sequence,
        format_index,
        tick,
        quality,
        false,
        false,
        None,
        None,
    )
}

/// Engine preview variant that verifies live source files before publishing a
/// frame. The pure graph compiler above deliberately does no filesystem I/O.
pub fn compile_native_preview_live(
    project: &TimelineProject,
    sequence: SequenceId,
    format_index: usize,
    tick: Tick,
    quality: Quality,
) -> CompiledFrame {
    compile_native_preview_live_with_source_errors(
        project,
        sequence,
        format_index,
        tick,
        quality,
        &HashMap::new(),
    )
}

/// Engine variant that also rejects a source whose fresh decoder probe differs
/// from the pixel format recorded when the project imported it.
pub fn compile_native_preview_live_with_source_errors(
    project: &TimelineProject,
    sequence: SequenceId,
    format_index: usize,
    tick: Tick,
    quality: Quality,
    source_errors: &HashMap<AssetId, String>,
) -> CompiledFrame {
    compile_native_sequence(
        project,
        sequence,
        format_index,
        tick,
        quality,
        false,
        true,
        Some(source_errors),
        None,
    )
}

/// Qualified BT.709 video-signal graph for offline delivery validation. The
/// export job remains gated until this path has frame and codec preflight.
pub fn compile_native_delivery(
    project: &TimelineProject,
    sequence: SequenceId,
    format_index: usize,
    tick: Tick,
    quality: Quality,
) -> CompiledFrame {
    compile_native_sequence(
        project,
        sequence,
        format_index,
        tick,
        quality,
        true,
        false,
        None,
        None,
    )
}

/// Pure delivery compilation with already verified native color dependencies.
pub fn compile_native_delivery_with_luts(
    project: &TimelineProject,
    sequence: SequenceId,
    format_index: usize,
    tick: Tick,
    quality: Quality,
    luts: &dyn LutProvider,
) -> CompiledFrame {
    compile_native_sequence(
        project,
        sequence,
        format_index,
        tick,
        quality,
        true,
        false,
        None,
        Some(luts),
    )
}

/// Delivery graph used by the live engine after its source workers have probed
/// the on-disk media. The pure delivery compiler above remains IO-free.
pub fn compile_native_delivery_live_with_source_errors(
    project: &TimelineProject,
    sequence: SequenceId,
    format_index: usize,
    tick: Tick,
    quality: Quality,
    source_errors: &HashMap<AssetId, String>,
) -> CompiledFrame {
    compile_native_sequence(
        project,
        sequence,
        format_index,
        tick,
        quality,
        true,
        true,
        Some(source_errors),
        None,
    )
}

/// Live native compiler with explicit output selection and pre-warmed color dependencies.
pub fn compile_native_live_with_luts(
    project: &TimelineProject,
    sequence: SequenceId,
    format_index: usize,
    tick: Tick,
    quality: Quality,
    delivery: bool,
    source_errors: &HashMap<AssetId, String>,
    luts: &dyn LutProvider,
) -> CompiledFrame {
    compile_native_sequence(
        project,
        sequence,
        format_index,
        tick,
        quality,
        delivery,
        true,
        Some(source_errors),
        Some(luts),
    )
}

/// Reasons an owner input cannot yet be measured. Pending dependencies are
/// distinct from invalid graphs so the scheduler never treats unity as data.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NativeDeflickerInputError {
    Pending(Vec<super::deflicker::NativeDeflickerKey>),
    Invalid(String),
}

/// Compile an isolated pre-effect AP1 input. Provisional downstream placeholders
/// remain private; all unresolved ancestors are rejected before evaluation.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compile_native_deflicker_input(
    project: &TimelineProject,
    sequence: SequenceId,
    format_index: usize,
    tick: Tick,
    target: super::deflicker::NativeDeflickerKey,
    luts: &dyn LutProvider,
    measured: &dyn DeflickerGains,
    source_errors: Option<&HashMap<AssetId, String>>,
    check_sources_online: bool,
) -> Result<CompiledFrame, NativeDeflickerInputError> {
    struct Probe<'a>(&'a dyn DeflickerGains);
    impl DeflickerGains for Probe<'_> {
        fn gain(&self, c: ClipId, t: Tick) -> Option<[f32; 3]> {
            self.0.gain(c, t)
        }
        fn native_gain(
            &self,
            k: &super::deflicker::NativeDeflickerKey,
            t: Tick,
        ) -> Option<[f32; 3]> {
            self.0.native_gain(k, t)
        }
        fn native_band(
            &self,
            k: &super::deflicker::NativeDeflickerKey,
            t: Tick,
        ) -> Option<super::rolling_bands::BandModel> {
            self.0.native_band(k, t)
        }
        fn analysis_mode(&self) -> bool {
            true
        }
    }
    let mut frame = compile_native_sequence_impl(
        project,
        sequence,
        format_index,
        tick,
        Quality::FULL,
        false,
        check_sources_online,
        source_errors,
        Some(luts),
        Some(&Probe(measured)),
        &[],
    );
    if let Some(error) = frame
        .diagnostics
        .iter()
        .find(|d| d.severity == DiagSeverity::Error)
    {
        return Err(NativeDeflickerInputError::Invalid(error.message.clone()));
    }
    let mut targets: Vec<_> = frame
        .native_deflicker_inputs
        .iter()
        .filter_map(|(k, node)| (*k == target).then_some(*node))
        .collect();
    targets.sort_by_key(|n| n.0);
    targets.dedup();
    if targets.len() != 1 {
        return Err(NativeDeflickerInputError::Invalid(
            "native analysis target is missing or has ambiguous retimed instances".into(),
        ));
    }
    let input = targets[0];
    let mut seen = HashSet::new();
    let mut todo = vec![input];
    while let Some(node) = todo.pop() {
        if seen.insert(node) {
            let item = frame.graph.nodes.get(node.0 as usize).ok_or_else(|| {
                NativeDeflickerInputError::Invalid("invalid analysis input edge".into())
            })?;
            todo.extend(item.inputs.iter().map(|(n, _)| *n));
        }
    }
    let mut pending = Vec::new();
    for (key, node) in &frame.native_deflicker_pending {
        if seen.contains(node) && !pending.contains(key) {
            pending.push(*key);
        }
    }
    if !pending.is_empty() {
        return Err(NativeDeflickerInputError::Pending(pending));
    }
    // Remove downstream nodes entirely: the domain validator correctly rejects
    // a display transform that is present but no longer the graph output.
    let mut remap: HashMap<IrNodeId, IrNodeId> = HashMap::new();
    let mut nodes: Vec<IrNode> = Vec::new();
    for (index, node) in std::mem::take(&mut frame.graph.nodes)
        .into_iter()
        .enumerate()
    {
        let old = IrNodeId(index as u32);
        if !seen.contains(&old) {
            continue;
        }
        let inputs: Vec<_> = node
            .inputs
            .iter()
            .map(|(id, port)| (remap[id], *port))
            .collect();
        let hashes: Vec<_> = inputs
            .iter()
            .map(|(id, _)| nodes[id.0 as usize].content_hash.0)
            .collect();
        let hash = content_hash(&node.op, &inputs, &hashes);
        let new = IrNodeId(nodes.len() as u32);
        nodes.push(IrNode {
            op: node.op,
            inputs,
            content_hash: hash,
        });
        remap.insert(old, new);
    }
    let input = remap[&input];
    frame = CompiledFrame {
        graph: FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes,
            output: Some(input),
        },
        diagnostics: frame.diagnostics,
        ..Default::default()
    };
    let format = project
        .sequences
        .get(&sequence)
        .and_then(|s| s.formats.get(format_index))
        .ok_or_else(|| NativeDeflickerInputError::Invalid("analysis canvas missing".into()))?;
    let mut output = input;
    for op in [
        IrOp::Resize {
            w: format.width,
            h: format.height,
            fit: super::ir::FitMode::Stretch,
        },
        IrOp::NativeCanvas {
            w: format.width,
            h: format.height,
        },
    ] {
        let inputs = vec![(output, OutPort::default())];
        let hash = content_hash(
            &op,
            &inputs,
            &[frame.graph.nodes[output.0 as usize].content_hash.0],
        );
        output = IrNodeId(frame.graph.nodes.len() as u32);
        frame.graph.nodes.push(IrNode {
            op,
            inputs,
            content_hash: hash,
        });
    }
    frame.graph.output = Some(output);
    frame.program_tap = Some(output);
    frame
        .graph
        .validate_working_color_domain()
        .map_err(|e| NativeDeflickerInputError::Invalid(e.into()))?;
    if frame.graph.output_color_encoding() != Ok(super::ir::FrameColorEncoding::SceneLinearAcescg) {
        return Err(NativeDeflickerInputError::Invalid(
            "analysis output is not scene-linear AP1".into(),
        ));
    }
    Ok(frame)
}

/// Validate export structure before its private engine completes analysis.
/// Unity measurements exist only inside this non-rendering validator. The graph
/// never escapes, and actual delivery still requires the engine's completed
/// source measurements; missing/failed analysis cannot publish a frame.
pub(crate) fn validate_native_delivery_before_analysis(
    project: &TimelineProject,
    sequence: SequenceId,
    format_index: usize,
    tick: Tick,
    luts: &dyn LutProvider,
) -> Result<(), String> {
    struct ShapeOnly;
    impl DeflickerGains for ShapeOnly {
        fn native_gain(
            &self,
            _: &super::deflicker::NativeDeflickerKey,
            _: Tick,
        ) -> Option<[f32; 3]> {
            Some([1.0; 3])
        }
        fn gain(&self, _: ClipId, _: Tick) -> Option<[f32; 3]> {
            Some([1.0; 3])
        }
    }
    let compiled = compile_native_sequence_impl(
        project,
        sequence,
        format_index,
        tick,
        Quality::FULL,
        true,
        false,
        None,
        Some(luts),
        Some(&ShapeOnly),
        &[],
    );
    if let Some(error) = compiled
        .diagnostics
        .iter()
        .find(|d| d.severity == DiagSeverity::Error)
    {
        return Err(format!(
            "native delivery first frame is unavailable: {}",
            error.message
        ));
    }
    if compiled.graph.output_color_encoding() != Ok(super::ir::FrameColorEncoding::Bt709Video) {
        return Err("native delivery first frame lacks BT.709 video-signal output".into());
    }
    Ok(())
}

/// Native live entry with already-computed analysis. Missing measurements stay
/// explicit compiler errors; they never masquerade as a completed correction.
#[allow(clippy::too_many_arguments)]
pub fn compile_native_live_with_analysis(
    project: &TimelineProject,
    sequence: SequenceId,
    format_index: usize,
    tick: Tick,
    quality: Quality,
    delivery: bool,
    source_errors: &HashMap<AssetId, String>,
    luts: &dyn LutProvider,
    deflicker: &dyn DeflickerGains,
) -> CompiledFrame {
    struct DeliveryMeasured<'a>(&'a dyn DeflickerGains);
    impl DeflickerGains for DeliveryMeasured<'_> {
        fn gain(&self, c: ClipId, t: Tick) -> Option<[f32; 3]> {
            self.0.gain(c, t)
        }
        fn native_gain(
            &self,
            k: &super::deflicker::NativeDeflickerKey,
            t: Tick,
        ) -> Option<[f32; 3]> {
            self.0.native_gain(k, t)
        }
        fn native_band(
            &self,
            k: &super::deflicker::NativeDeflickerKey,
            t: Tick,
        ) -> Option<super::rolling_bands::BandModel> {
            self.0.native_band(k, t)
        }
    }
    compile_native_sequence_impl(
        project,
        sequence,
        format_index,
        tick,
        quality,
        delivery,
        true,
        Some(source_errors),
        Some(luts),
        Some(&DeliveryMeasured(deflicker)),
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
fn compile_native_sequence(
    project: &TimelineProject,
    sequence: SequenceId,
    format_index: usize,
    tick: Tick,
    quality: Quality,
    delivery: bool,
    check_sources_online: bool,
    source_errors: Option<&HashMap<AssetId, String>>,
    luts: Option<&dyn LutProvider>,
) -> CompiledFrame {
    compile_native_sequence_impl(
        project,
        sequence,
        format_index,
        tick,
        quality,
        delivery,
        check_sources_online,
        source_errors,
        luts,
        None,
        &[],
    )
}

/// Source admission only. Each participant still passes the complete native
/// source/style/color/nest validation before a transition can render. Generators
/// have unbounded handles; nests use their content-end/speed handle accounting.
fn native_transition_source(source: &ClipSource) -> bool {
    matches!(
        source,
        ClipSource::Asset { .. }
            | ClipSource::SolidColor { .. }
            | ClipSource::Text { .. }
            | ClipSource::Vector { .. }
            | ClipSource::NestedSequence { .. }
    )
}

#[allow(clippy::too_many_arguments)]
fn compile_native_sequence_impl(
    project: &TimelineProject,
    sequence: SequenceId,
    format_index: usize,
    tick: Tick,
    quality: Quality,
    delivery: bool,
    check_sources_online: bool,
    source_errors: Option<&HashMap<AssetId, String>>,
    luts: Option<&dyn LutProvider>,
    deflicker: Option<&dyn DeflickerGains>,
    ancestry: &[SequenceId],
) -> CompiledFrame {
    compile_native_sequence_stages(
        project,
        sequence,
        format_index,
        tick,
        quality,
        delivery,
        check_sources_online,
        source_errors,
        luts,
        deflicker,
        ancestry,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn compile_native_sequence_stages(
    project: &TimelineProject,
    sequence: SequenceId,
    format_index: usize,
    tick: Tick,
    quality: Quality,
    delivery: bool,
    check_sources_online: bool,
    source_errors: Option<&HashMap<AssetId, String>>,
    luts: Option<&dyn LutProvider>,
    deflicker: Option<&dyn DeflickerGains>,
    ancestry: &[SequenceId],
    include_outer_stages: bool,
) -> CompiledFrame {
    let native_luts = luts.map(|provider| NativeLutView { provider, project });
    let mut b = Builder::new();
    b.deflicker = deflicker;
    b.luts = native_luts
        .as_ref()
        .map(|provider| provider as &dyn LutProvider);
    let Some(seq) = project.sequences.get(&sequence) else {
        b.diag(CompileDiagnostic::coded(
            CompileCode::ColorPipelineUnavailable,
            DiagSeverity::Error,
            None,
            "unknown native-managed sequence",
        ));
        return b.finish(None);
    };
    let Some(format) = seq.formats.get(format_index) else {
        b.diag(CompileDiagnostic::coded(
            CompileCode::ColorPipelineUnavailable,
            DiagSeverity::Error,
            None,
            "native-managed sequence has no selected format",
        ));
        return b.finish(None);
    };
    let blank = |mut b: Builder<'_>, message: &str, clip: Option<ClipId>| {
        b.diag(CompileDiagnostic::coded(
            CompileCode::ColorPipelineUnavailable,
            DiagSeverity::Error,
            clip,
            message,
        ));
        let source = b.transparent(format);
        let output = b.push(
            IrOp::Output {
                w: format.width,
                h: format.height,
            },
            vec![(source, OutPort::default())],
        );
        b.finish(Some(output))
    };
    if ancestry.contains(&sequence) || ancestry.len() >= 32 {
        return blank(
            b,
            "native nested sequence is cyclic or exceeds the 32-level depth limit",
            None,
        );
    }
    let mut sequence_path = ancestry.to_vec();
    sequence_path.push(sequence);
    let timeline::color::SequenceColorConfig::NativeManaged(config) = &seq.color else {
        return blank(b, "native preview requires a native-managed sequence", None);
    };
    if let Err(error) = config.validate() {
        return blank(b, &error, None);
    }
    if delivery && config.export != timeline::color::NativeOutputTransform::Bt709VideoSdr {
        return blank(
            b,
            "native delivery requires a BT.709 video-signal SDR output transform",
            None,
        );
    }
    let (captions, caption_paints) = if include_outer_stages {
        match resolve_native_caption_batch(seq, tick, format) {
            Ok(batch) => batch,
            Err(error) => return blank(b, &error, None),
        }
    } else {
        (CaptionBatch::default(), Vec::new())
    };
    for track in seq
        .caption_tracks
        .iter()
        .filter(|t| t.enabled && include_outer_stages)
    {
        for cue in track
            .cues
            .iter()
            .filter(|c| c.start <= tick && tick < c.end)
        {
            if let Some(note) = caption_sweep_direction_note(track, cue) {
                b.diag(CompileDiagnostic::coded(
                    CompileCode::CaptionSweepDirectionFallback,
                    DiagSeverity::Warning,
                    None,
                    note,
                ));
            }
        }
    }
    let active: Vec<_> = seq
        .video_tracks
        .iter()
        .filter(|track| track.enabled && track.kind.is_visual())
        .filter_map(|track| {
            covering_clip_index(&track.clips, tick).map(|index| (track, &track.clips[index], index))
        })
        .filter(|(_, clip, _)| clip.enabled)
        .collect();

    b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
    b.sequence_path = sequence_path.clone();
    let mut composite = None;
    for (track, clip, clip_index) in active {
        if matches!(clip.source, ClipSource::Adjustment) {
            if !track.effects.is_empty()
                || track.grade.is_some()
                || track.blend != BlendMode::Normal
                || track.opacity != 1.0
                || clip.group.is_some()
                || clip.composition.is_some()
                || clip.transition_in.is_some()
                || clip.transition_out.is_some()
                || clip.multicam.is_some()
                || clip.stabilization.is_some()
                || clip.transform != AnimProps::new(ClipTransform::default())
                || !clip.reframe.is_empty()
            {
                return blank(Builder::new(), "native adjustment preview supports only untransformed grading and qualified effects over the composite", Some(clip.id));
            }
            if let Some(below) = composite {
                let adjusted = apply_stack(
                    &mut b,
                    &clip.effects,
                    clip.grade.as_ref(),
                    below,
                    tick - clip.start,
                    Some(clip.id),
                    Some(VfxOwner::Clip(clip.id)),
                );
                if b.diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
                {
                    let diagnostics = std::mem::take(&mut b.diagnostics);
                    let mut failed = blank(
                        Builder::new(),
                        "native adjustment grade cannot render accurately",
                        Some(clip.id),
                    );
                    failed.diagnostics.extend(diagnostics);
                    return failed;
                }
                composite = Some(adjusted);
            }
            continue;
        }
        if !super::ir::native_blend_supported(track.blend)
            || !track.opacity.is_finite()
            || !(0.0..=1.0).contains(&track.opacity)
            || clip.multicam.is_some()
            || clip.stabilization.is_some()
        {
            return blank(
                Builder::new(),
                "native preview cannot reproduce an authored effect or composite stage",
                Some(clip.id),
            );
        }
        let valid_span = |transition: &timeline::Transition| {
            transition.duration.0 >= 0
                && transition.duration <= clip.duration
                && native_transition_source(&clip.source)
        };
        let supported_transition = |kind| {
            matches!(
                kind,
                TransitionKind::CrossDissolve
                    | TransitionKind::DipToBlack
                    | TransitionKind::DipToColor
                    | TransitionKind::Wipe
                    | TransitionKind::Push
                    | TransitionKind::LumaWipe
            )
        };
        if clip
            .transition_in
            .as_ref()
            .is_some_and(|tr| !valid_span(tr) || !supported_transition(tr.kind))
            || clip
                .transition_out
                .as_ref()
                .is_some_and(|tr| !valid_span(tr) || !supported_transition(tr.kind))
        {
            return blank(
                Builder::new(),
                "native transition kind, source or duration is not qualified",
                Some(clip.id),
            );
        }
        if clip
            .transition_in
            .as_ref()
            .is_some_and(|transition| transition.duration > Tick::ZERO)
            && (clip_index == 0 || track.clips[clip_index - 1].end() != clip.start)
        {
            return blank(
                Builder::new(),
                "native transition requires an adjacent outgoing clip at the cut",
                Some(clip.id),
            );
        }
        let (mut graded, mut clip_opacity) = match build_native_clip_chain(
            &mut b,
            project,
            seq,
            format_index,
            format,
            clip,
            tick,
            quality,
            check_sources_online,
            source_errors,
            luts,
            &sequence_path,
        ) {
            Ok(chain) => chain,
            Err(error) => {
                let diagnostics = std::mem::take(&mut b.diagnostics);
                let mut failed = blank(Builder::new(), &error, Some(clip.id));
                failed.diagnostics.extend(diagnostics);
                return failed;
            }
        };
        if clip.transition_in.as_ref().is_some_and(|tr| {
            tr.kind == TransitionKind::DipToColor
                && native_swatch_color(tr.params.color.unwrap_or_else(opaque_black)).is_none()
        }) {
            return blank(
                Builder::new(),
                "native dip color requires finite encoded sRGB and alpha in 0..=1",
                Some(clip.id),
            );
        }
        let incoming_grade_tap = graded;
        if let Some(transition) = active_transition(project, &track.clips, clip_index, tick) {
            let outgoing = &track.clips[transition.outgoing];
            if outgoing.enabled && !matches!(outgoing.source, ClipSource::Adjustment) {
                if !native_transition_source(&outgoing.source) {
                    return blank(
                        Builder::new(),
                        "native transition source is not qualified",
                        Some(outgoing.id),
                    );
                }
                let (outgoing_graded, outgoing_opacity) = match build_native_clip_chain(
                    &mut b,
                    project,
                    seq,
                    format_index,
                    format,
                    outgoing,
                    tick,
                    quality,
                    check_sources_online,
                    source_errors,
                    luts,
                    &sequence_path,
                ) {
                    Ok(chain) => chain,
                    Err(error) => {
                        let diagnostics = std::mem::take(&mut b.diagnostics);
                        let mut failed = blank(Builder::new(), &error, Some(outgoing.id));
                        failed.diagnostics.extend(diagnostics);
                        return failed;
                    }
                };
                b.clip_taps.push((outgoing.id, outgoing_graded));
                if let Some(handle) = transition.handle_clip {
                    b.diag_coded_once(CompileCode::TransitionHandleClipped, DiagSeverity::Info, Some(clip.id),
                        format!("native transition shortened from {} to {} ticks by the outgoing source handle",handle.requested.0,handle.available.0));
                }
                let incoming = bake_opacity(&mut b, graded, clip_opacity);
                let outgoing = bake_opacity(&mut b, outgoing_graded, outgoing_opacity);
                graded = match transition.kind {
                    TransitionKind::CrossDissolve => b.push(
                        IrOp::NativeDissolve { t: transition.t },
                        vec![
                            (incoming, OutPort::default()),
                            (outgoing, OutPort::default()),
                        ],
                    ),
                    TransitionKind::DipToBlack | TransitionKind::DipToColor => {
                        let color = if transition.kind == TransitionKind::DipToColor {
                            native_swatch_color(
                                transition.params.color.unwrap_or_else(opaque_black),
                            )
                            .expect("native dip color validated before lowering")
                        } else {
                            LinearColor {
                                r: 0.0,
                                g: 0.0,
                                b: 0.0,
                                a: 1.0,
                            }
                        };
                        let dip = b.push(IrOp::SolidColor { color }, vec![]);
                        if transition.t < 0.5 {
                            b.push(
                                IrOp::NativeDissolve {
                                    t: transition.t * 2.0,
                                },
                                vec![(dip, OutPort::default()), (outgoing, OutPort::default())],
                            )
                        } else {
                            b.push(
                                IrOp::NativeDissolve {
                                    t: (transition.t - 0.5) * 2.0,
                                },
                                vec![(incoming, OutPort::default()), (dip, OutPort::default())],
                            )
                        }
                    }
                    TransitionKind::LumaWipe => b.push(
                        IrOp::NativeLumaWipe {
                            kind: luma_wipe_kind(transition.params.luma_map),
                            softness: transition.params.softness,
                            invert: transition.params.invert,
                            t: transition.t,
                        },
                        vec![
                            (incoming, OutPort::default()),
                            (outgoing, OutPort::default()),
                        ],
                    ),
                    TransitionKind::Wipe | TransitionKind::Push => transition_mix(
                        &mut b,
                        transition.kind,
                        &transition.params,
                        outgoing,
                        incoming,
                        transition.t,
                    ),
                    _ => {
                        return blank(
                            Builder::new(),
                            "native transition is not qualified",
                            Some(clip.id),
                        )
                    }
                };
                clip_opacity = 1.0;
            }
        } else if let Some(id) = suppressed_transition_clip(project, &track.clips, clip_index, tick)
        {
            b.diag_coded_once(
                CompileCode::TransitionHandleClipped,
                DiagSeverity::Warning,
                Some(id),
                "native transition suppressed because the outgoing source has no remaining handle",
            );
        }
        if let Some(t) = active_fade_out(&track.clips, clip_index, tick) {
            graded = bake_opacity(&mut b, graded, 1.0 - t);
        }
        let track_graded = apply_stack(
            &mut b,
            &track.effects,
            track.grade.as_ref(),
            graded,
            tick,
            Some(clip.id),
            Some(VfxOwner::Track(track.id)),
        );
        if b.diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error)
        {
            let diagnostics = std::mem::take(&mut b.diagnostics);
            let mut failed = blank(
                Builder::new(),
                "native preview cannot render this clip accurately",
                Some(clip.id),
            );
            failed.diagnostics.extend(diagnostics);
            return failed;
        }
        b.clip_taps.push((clip.id, incoming_grade_tap));
        composite = Some(fold_over(
            &mut b,
            composite,
            track_graded,
            clip_opacity * track.opacity,
            track.blend,
        ));
    }
    let composite = composite.unwrap_or_else(|| b.transparent(format));
    let program = apply_stack(
        &mut b,
        &seq.master_effects,
        seq.master_grade.as_ref(),
        composite,
        tick,
        None,
        Some(VfxOwner::Master(seq.id)),
    );
    if b.diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error)
    {
        let diagnostics = std::mem::take(&mut b.diagnostics);
        let mut failed = blank(
            Builder::new(),
            "native preview cannot render this clip accurately",
            None,
        );
        failed.diagnostics.extend(diagnostics);
        return failed;
    }
    // Project processing is applied once, after the outer master grade and
    // before output encoding. Nested scene sources must not apply it again.
    let program = if ancestry.is_empty() && include_outer_stages {
        if let Some(graph_id) = project.project_graph {
            let Some(graph) = project.graphs.get(&graph_id) else {
                return blank(b, "native project graph is missing", None);
            };
            match lower_native_graph_with_sources(
                &mut b,
                graph,
                format,
                tick,
                None,
                Some(program),
                Some(NativeGraphSourceContext {
                    project,
                    clip_start: None,
                    clip: None,
                    sequence,
                    format_index,
                    quality,
                    check_online: check_sources_online,
                    source_errors,
                }),
            ) {
                Ok(output) => output,
                Err(error) => return blank(Builder::new(), &error, None),
            }
        } else {
            program
        }
    } else {
        program
    };
    if b.diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error)
    {
        let diagnostics = std::mem::take(&mut b.diagnostics);
        let mut failed = blank(
            Builder::new(),
            "native project graph cannot render accurately",
            None,
        );
        failed.diagnostics.extend(diagnostics);
        return failed;
    }
    let output_op = if delivery {
        IrOp::NativeSdrVideoOutput
    } else {
        IrOp::NativeSdrOutput
    };
    let program = b.push(
        IrOp::NativeCanvas {
            w: format.width,
            h: format.height,
        },
        vec![(program, OutPort::default())],
    );
    let output = b.push(output_op, vec![(program, OutPort::default())]);
    // Native program scopes measure the display/delivery signal. Clip taps
    // remain scene-linear and are gated until a scene-referred scale exists.
    b.program_tap = Some(output);
    let output = if captions.cues.is_empty() {
        output
    } else {
        b.push(
            IrOp::NativeCaptionOutput {
                cue_batch: captions,
                video: delivery,
                paints: caption_paints,
            },
            vec![(output, OutPort::default())],
        )
    };
    let compiled = b.finish(Some(output));
    if compiled.graph.native_decode_requests(None).len()
        > crate::playback::prefetch::MAX_LIVE_SOURCES
    {
        return blank(Builder::new(), "native preview exceeds the eight independent video-clock budget; reduce simultaneous sources or offsets", None);
    }
    if let Err(error) =
        super::native_matte::validate_capacity(&compiled.graph, (format.width, format.height), None)
    {
        return blank(Builder::new(), error, None);
    }
    if let Err(error) = compiled.graph.validate_working_color_domain() {
        return blank(Builder::new(), error, None);
    }
    let expected = if delivery {
        crate::graph::ir::FrameColorEncoding::Bt709Video
    } else {
        crate::graph::ir::FrameColorEncoding::SrgbDisplay
    };
    if compiled.graph.output_color_encoding() != Ok(expected) {
        return blank(
            Builder::new(),
            "native output did not produce its configured encoding",
            None,
        );
    }
    compiled
}

/// Build one native clip's scene-linear chain before track/master processing.
/// Transition partners can reuse this exact path at their borrowed source time.
#[allow(clippy::too_many_arguments)]
fn build_native_clip_chain(
    b: &mut Builder<'_>,
    project: &TimelineProject,
    seq: &Sequence,
    format_index: usize,
    format: &SequenceFormat,
    clip: &Clip,
    tick: Tick,
    quality: Quality,
    check_sources_online: bool,
    source_errors: Option<&HashMap<AssetId, String>>,
    luts: Option<&dyn LutProvider>,
    sequence_path: &[SequenceId],
) -> Result<(IrNodeId, f32), String> {
    let source_context = Some(NativeGraphSourceContext {
        project,
        clip_start: Some(clip.start),
        clip: Some(clip),
        sequence: seq.id,
        format_index,
        quality,
        check_online: check_sources_online,
        source_errors,
    });
    // Validate moving-source arithmetic before the ordinary source is built,
    // even if a later composition branch replaces that source entirely.
    if matches!(
        clip.source,
        ClipSource::Asset { .. } | ClipSource::Vector { .. } | ClipSource::NestedSequence { .. }
    ) {
        native_source_time_checked(clip, tick)?;
    }
    if clip.multicam.is_some() || clip.stabilization.is_some() {
        return Err("native clip chain does not yet qualify multicam or stabilization".into());
    }
    // Do not truncate malformed ancestry and silently omit an authored grade.
    let mut group_chain = Vec::new();
    let mut group = clip.group;
    let mut visited = HashSet::new();
    while let Some(id) = group {
        let Some(node) = seq.groups.get(&id) else {
            return Err(("native preview has a missing group reference").to_string());
        };
        if !visited.insert(id) {
            return Err(("native preview has a cyclic group reference").to_string());
        }
        if !matches!(
            node.kind,
            timeline::GroupKind::Normal | timeline::GroupKind::AvLink
        ) {
            return Err(("native preview has an unsupported group kind").to_string());
        }
        group_chain.push(id);
        group = node.parent;
    }
    let transform = clip
        .reframe
        .get(&format_index)
        .copied()
        .unwrap_or_else(|| eval_clip_transform(&clip.transform, tick - clip.start));
    let matrix = clip_transform_matrix(&transform, format);
    if !transform.opacity.is_finite()
        || !(0.0..=1.0).contains(&transform.opacity)
        || matrix
            .to_cols_array()
            .iter()
            .any(|value| !value.is_finite())
        || matrix.determinant().abs() < 1e-12
    {
        return Err(("native preview requires a finite, invertible clip transform and opacity within 0..=1").to_string());
    }
    let asset_graded = match clip.source {
        ClipSource::Asset { asset } => {
            if project
                .media
                .assets
                .get(&asset)
                .is_none_or(|media| !matches!(media.kind, AssetKind::Video | AssetKind::Image))
            {
                return Err(("native preview requires a video or sRGB still asset").to_string());
            }
            if check_sources_online {
                if let Some(error) = source_errors.and_then(|errors| errors.get(&asset)) {
                    return Err((error).to_string());
                }
                let Some(timeline::AssetSource::File { path, .. }) =
                    project.media.assets.get(&asset).map(|media| &media.source)
                else {
                    return Err(("native preview source is not a file-backed asset").to_string());
                };
                if !path.is_file() {
                    return Err((&format!(
                        "native preview source {asset} is offline: {}",
                        path.display()
                    ))
                        .to_string());
                }
                if let Some(diagnostic) = crate::color::inspect_input(project, seq, clip)
                    .and_then(|finding| finding.diagnostic)
                {
                    return Err((&diagnostic).to_string());
                }
            }
            let source = build_clip_source(
                b,
                project,
                seq,
                format_index,
                format,
                clip,
                tick,
                quality,
                &mut HashSet::new(),
            );
            let source = if let Some(graph_id) = clip.composition {
                let Some(graph) = project.graphs.get(&graph_id) else {
                    return Err(("native clip composition is missing").to_string());
                };
                match lower_native_graph_with_sources(
                    b, graph, format, tick, Some(source), None, source_context,
                ) {
                    Ok(output) => output,
                    Err(error) => return Err((&error).to_string()),
                }
            } else {
                source
            };
            let media = &project.media.assets[&asset];
            apply_stack(
                b,
                &media.effects,
                media.grade.as_ref(),
                source,
                tick - clip.start,
                Some(clip.id),
                Some(VfxOwner::Asset(asset)),
            )
        }
        ClipSource::Vector { asset } => {
            let media=project.media.assets.get(&asset).ok_or("native vector asset is missing")?;
            crate::color::validate_native_vector_asset(media,clip.native_input_color.as_ref())?;
            let src_time=clip.source_in+clip.speed.source_delta(tick-clip.start);
            let source=b.push(IrOp::NativeRasterVector{vref:VectorRef::WholeDocument,doc_state:vector_state_key(asset,format,src_time),w:format.width,h:format.height},vec![]);
            let source=if let Some(graph_id)=clip.composition {
                let graph=project.graphs.get(&graph_id).ok_or("native vector composition is missing")?;
                lower_native_graph_with_sources(
                    b, graph, format, tick, Some(source), None, source_context,
                )?
            } else {source};
            apply_stack(b,&media.effects,media.grade.as_ref(),source,tick-clip.start,Some(clip.id),Some(VfxOwner::Asset(asset)))
        }
        ClipSource::Text {ref content}=>{
            let style=&content.style;
            if style.highlight.is_some() || !matches!(style.animation, timeline::CaptionAnim::None | timeline::CaptionAnim::SlideUp) || native_swatch_color(style.fill).is_none()
            {return Err("native titles require finite sRGB fill; karaoke and word-timed title animation remain unqualified".into());}
            let decoration=native_text_decoration(style)?;
            // Validate authored metrics before the Legacy resolver's convenience clamps.
            if !style.font_size.is_finite() || !(1.0..=512.0).contains(&style.font_size) {
                return Err("native title font size must be finite in 1..=512".into());
            }
            let block=resolve_text_block(content);
            super::ir::validate_native_text_block(&block).map_err(str::to_string)?;
            let op=if decoration==photonic_render::caption_decoration::CaptionDecoration::default(){IrOp::NativeTextGen{block}}else{IrOp::NativeDecoratedTextGen{block,decoration}};
            let source=b.push(op,vec![]);
            // Titles have no word timestamps. SlideUp alone has an unambiguous
            // whole-block contract:200ms, +5% authored height ->0, opacity0->1.
            // Apply after paint so outline/background and glyphs stay together.
            let source=if style.animation == timeline::CaptionAnim::SlideUp {
                let elapsed = tick.0 as i128 - clip.start.0 as i128;
                let progress = (elapsed as f64 / ms_to_ticks(200) as f64).clamp(0.0, 1.0) as f32;
                if progress < 1.0 {
                    let shifted=b.push(IrOp::Transform2DTransparent {
                        mat: Mat3::from_translation(Vec2::new(0.0, (1.0-progress)*0.05*format.height as f32)),
                        sampling:Sampling::Bilinear,
                    },vec![(source,OutPort::default())]);
                    let blank=b.transparent(format);
                    b.push(IrOp::Merge { mode:BlendMode::Normal, opacity:progress },vec![(shifted,OutPort::default()),(blank,OutPort::default())])
                } else { source }
            } else { source };
            if let Some(graph_id)=clip.composition {
                let graph=project.graphs.get(&graph_id).ok_or("native title composition is missing")?;
                lower_native_graph_with_sources(
                    b, graph, format, tick, Some(source), None, source_context,
                )?
            } else {source}
        }
        ClipSource::SolidColor { color } => {
            let color = native_swatch_color(color)
                .ok_or("native solid requires finite encoded sRGB and alpha in 0..=1")?;
            let source = b.push(IrOp::SolidColor { color }, vec![]);
            if let Some(graph_id) = clip.composition {
                let graph = project
                    .graphs
                    .get(&graph_id)
                    .ok_or("native solid clip composition is missing")?;
                lower_native_graph_with_sources(
                    b, graph, format, tick, Some(source), None, source_context,
                )?
            } else {
                source
            }
        }
        ClipSource::NestedSequence {
            sequence: nested_id,
        } => {
            let Some(nested) = project.sequences.get(&nested_id) else {
                return Err(("native nested sequence is missing").to_string());
            };
            if nested.color != seq.color
                || !rates_equal(nested.frame_rate, seq.frame_rate)
                || nested.formats.get(format_index).is_none_or(|inner| {
                    inner.width != format.width || inner.height != format.height
                })
            {
                return Err(("native nest requires matching color configuration, frame rate and selected canvas").to_string());
            }
            let src_time = clip.source_in + clip.speed.source_delta(tick - clip.start);
            let end = nested.content_end();
            let fold_tick = if end > Tick::ZERO && src_time >= end {
                b.diag_coded_once(CompileCode::NestedSequenceShortened, DiagSeverity::Warning, Some(clip.id), "native nested sequence is shorter than the outer reference; holding its last frame");
                Tick((end.0 - nested.frame_rate.ticks_per_frame().0).max(0))
            } else {
                src_time
            };
            let inner = compile_native_sequence_impl(
                project,
                nested_id,
                format_index,
                fold_tick,
                quality,
                false,
                check_sources_online,
                source_errors,
                luts,
                b.deflicker,
                sequence_path,
            );
            if inner
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error)
            {
                b.diagnostics.extend(inner.diagnostics);
                return Err("native nested source cannot render accurately".into());
            }
            let Some(output) = inner.graph.output else {
                return Err(("native nested sequence has no output").to_string());
            };
            let output_node = &inner.graph.nodes[output.0 as usize];
            if matches!(output_node.op, IrOp::NativeCaptionOutput { .. }) {
                return Err("display-referred captions in nested native sequences are not yet qualified".into());
            }
            if !matches!(output_node.op, IrOp::NativeSdrOutput) || output_node.inputs.len() != 1 {
                return Err(("native nested sequence has an invalid output boundary").to_string());
            }
            let scene_output = output_node.inputs[0].0;
            let mut imported = Vec::with_capacity(output.0 as usize);
            for node in inner.graph.nodes.iter().take(output.0 as usize) {
                let inputs = node
                    .inputs
                    .iter()
                    .map(|(id, port)| (imported[id.0 as usize], *port))
                    .collect();
                imported.push(b.push(node.op.clone(), inputs));
            }
            for (key,node) in inner.native_deflicker_inputs {b.native_deflicker_inputs.push((key,imported[node.0 as usize]));}
            for (key,node) in inner.native_deflicker_pending {b.native_deflicker_pending.push((key,imported[node.0 as usize]));}
            for diagnostic in inner.diagnostics {
                b.diag(diagnostic);
            }
            let source = imported[scene_output.0 as usize];
            if let Some(graph_id) = clip.composition {
                let Some(graph) = project.graphs.get(&graph_id) else {
                    return Err(("native nested clip composition is missing").to_string());
                };
                match lower_native_graph_with_sources(
                    b, graph, format, tick, Some(source), None, source_context,
                ) {
                    Ok(output) => output,
                    Err(error) => return Err((&error).to_string()),
                }
            } else {
                source
            }
        }
        _ => {
            return Err(
                ("native preview requires a qualified asset, solid, plain title or nested sequence source")
                    .to_string(),
            )
        }
    };
    let transformed = if transform == ClipTransform::default() {
        asset_graded
    } else {
        b.push(
            IrOp::Transform2DTransparent {
                mat: matrix,
                sampling: Sampling::Bilinear,
            },
            vec![(asset_graded, OutPort::default())],
        )
    };
    let mut pre_graded = apply_stack(
        b,
        &clip.effects,
        None,
        transformed,
        tick - clip.start,
        Some(clip.id),
        Some(VfxOwner::Clip(clip.id)),
    );
    for id in group_chain.iter().rev() {
        if let Some(grade) = seq.groups[id].pre_grade.as_ref() {
            pre_graded = apply_member_group_grade(
                b,
                grade,
                pre_graded,
                tick - clip.start,
                clip.id,
                *id,
                true,
            );
        }
    }
    b.clip_pre_grade_taps.push((clip.id, pre_graded));
    let mut graded = clip.grade.as_ref().map_or(pre_graded, |grade| {
        apply_grade(
            b,
            grade,
            pre_graded,
            tick - clip.start,
            Some(VfxOwner::Clip(clip.id)),
        )
    });
    graded = apply_clip_look(b, project, clip, graded, tick - clip.start);
    for id in group_chain {
        if let Some(grade) = seq.groups[&id].post_grade.as_ref() {
            graded =
                apply_member_group_grade(b, grade, graded, tick - clip.start, clip.id, id, false);
        }
    }
    Ok((graded, transform.opacity as f32))
}

/// Compile the active sequence at `tick` in `format_index` to a frame graph.
///
/// `view_override` (08 §6.7) is session state — pass `None` for export/headless.
/// `Grade` `Lut3d` ops resolve inert (identity) with no LUT provider; use
/// [`compile_with_luts`] to thread one in.
pub fn compile(
    project: &TimelineProject,
    sequence: SequenceId,
    format_index: usize,
    tick: Tick,
    quality: Quality,
    view_override: Option<ViewNodeOverride>,
) -> CompiledFrame {
    compile_with_luts(
        project,
        sequence,
        format_index,
        tick,
        quality,
        view_override,
        None,
    )
}

/// [`compile`] with a [`LutProvider`] threaded in so `Grade` `Lut3d` ops resolve
/// to real tables (K-0.5). The provider read is a lock-free cache hit (no `.cube`
/// parsing on this per-frame path). `luts == None` behaves exactly like
/// [`compile`] (LUT ops bypassed with an export-blocking diagnostic).
#[allow(clippy::too_many_arguments)]
pub fn compile_with_luts(
    project: &TimelineProject,
    sequence: SequenceId,
    format_index: usize,
    tick: Tick,
    quality: Quality,
    view_override: Option<ViewNodeOverride>,
    luts: Option<&dyn LutProvider>,
) -> CompiledFrame {
    compile_with_luts_and_opts(
        project,
        sequence,
        format_index,
        tick,
        quality,
        view_override,
        luts,
        false,
    )
}

/// Like [`compile_with_luts`], with K-B5 `skip_clip_looks` for the clean side
/// of effect compare (clip effect stack + clip grade omitted; asset/track/master
/// stacks still apply).
#[allow(clippy::too_many_arguments)]
pub fn compile_with_luts_and_opts(
    project: &TimelineProject,
    sequence: SequenceId,
    format_index: usize,
    tick: Tick,
    quality: Quality,
    view_override: Option<ViewNodeOverride>,
    luts: Option<&dyn LutProvider>,
    skip_clip_looks: bool,
) -> CompiledFrame {
    compile_full(
        project,
        sequence,
        format_index,
        tick,
        quality,
        view_override,
        luts,
        skip_clip_looks,
        None,
        None,
    )
}

/// Like [`compile_with_luts_and_opts`], additionally threading compile-resolved
/// deflicker gains. This is the entry point a session uses once it has run the
/// deflicker analysis job; every other entry point delegates here with `None`,
/// which lowers each `Deflicker` effect as a pass-through.
/// [`compile_with_luts_and_opts`] plus a [`StabilizationProvider`], so D-12
/// clips resolve their per-frame warp (22 §6.4).
///
/// Separate entry point rather than another parameter on the existing ones:
/// every current caller wants `None` here, and threading an extra `Option`
/// through all of them would be churn for no gain.
#[allow(clippy::too_many_arguments)]
pub fn compile_with_providers(
    project: &TimelineProject,
    sequence: SequenceId,
    format_index: usize,
    tick: Tick,
    quality: Quality,
    view_override: Option<ViewNodeOverride>,
    luts: Option<&dyn LutProvider>,
    stabilization: Option<&dyn StabilizationProvider>,
    skip_clip_looks: bool,
) -> CompiledFrame {
    compile_full(
        project,
        sequence,
        format_index,
        tick,
        quality,
        view_override,
        luts,
        skip_clip_looks,
        None,
        stabilization,
    )
}

/// Like [`compile_with_providers`], additionally threading compile-resolved
/// deflicker gains for effects that depend on whole-range analysis.
#[allow(clippy::too_many_arguments)]
pub fn compile_full(
    project: &TimelineProject,
    sequence: SequenceId,
    format_index: usize,
    tick: Tick,
    quality: Quality,
    view_override: Option<ViewNodeOverride>,
    luts: Option<&dyn LutProvider>,
    skip_clip_looks: bool,
    deflicker: Option<&dyn DeflickerGains>,
    stabilization: Option<&dyn StabilizationProvider>,
) -> CompiledFrame {
    let mut b = Builder::new();
    b.luts = luts;
    b.stabilization = stabilization;
    b.skip_clip_looks = skip_clip_looks;
    b.deflicker = deflicker;

    let Some(seq) = project.sequences.get(&sequence) else {
        b.diag(CompileDiagnostic::plain(format!(
            "compile: unknown sequence {sequence}"
        )));
        return b.finish(None);
    };
    b.sequence_path.push(sequence);
    let format_index = format_index.min(seq.formats.len().saturating_sub(1));
    let Some(format) = seq.formats.get(format_index) else {
        b.diag(CompileDiagnostic::plain(format!(
            "compile: sequence {sequence} has no formats"
        )));
        return b.finish(None);
    };

    if matches!(
        seq.color,
        timeline::color::SequenceColorConfig::NativeManaged(_)
    ) {
        b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
    }

    if let Err(error) = crate::color::ensure_supported(seq) {
        b.diag(CompileDiagnostic::coded(
            CompileCode::ColorPipelineUnavailable,
            DiagSeverity::Error,
            None,
            error,
        ));
        let blank = b.transparent(format);
        let (w, h) = (format.width, format.height);
        let output = b.push(IrOp::Output { w, h }, vec![(blank, OutPort::default())]);
        return b.finish(Some(output));
    }

    let mut cycle = HashSet::new();
    cycle.insert(sequence);

    // Steps 1–4: fold the enabled video tracks bottom→top.
    let program = fold_sequence(
        &mut b,
        project,
        seq,
        format_index,
        format,
        tick,
        quality,
        &mut cycle,
    );

    // Master scope (35 §2.4(d)): the master effects/grade run on the folded
    // program BEFORE `CaptionOverlay` (captions are authored in final display
    // colour and must not be re-graded) and before the project graph (which stays
    // the final-look surface). Master keyframes are sequence-relative, so evaluate
    // the stack at `tick`, not any clip-relative offset.
    // TODO(30 §2.3): gate on the master stack's Applicability once a manifest type exists.
    let program = program.map(|p| {
        apply_stack(
            &mut b,
            &seq.master_effects,
            seq.master_grade.as_ref(),
            p,
            tick,
            None,
            Some(VfxOwner::Master(seq.id)),
        )
    });

    // K-E2 / 03 §3.6: the program scope tap is taken HERE — after the master
    // grade, before `CaptionOverlay`. Captions are authored in final display
    // colour (03 §3.6) and burning them into the measured signal is exactly the
    // defect 26 K-E2 names. The project-graph splice below is likewise excluded
    // because it lands after captions, so "before CaptionOverlay" and "after the
    // project graph" are not simultaneously satisfiable — the spec's stated
    // boundary (03 §3.6) wins.
    b.program_tap = program;

    // Step 5: caption overlay (enabled caption tracks with a cue covering t).
    let program = splice_captions(&mut b, seq, format, tick, program);

    // Step 6: project graph splice (08 §5) — between the fold result and Output.
    let program = splice_project_graph(&mut b, project, program, format, tick);

    // Terminal Output node (02 §2). Its input is the program, or transparent
    // black for an empty sequence.
    let out_input = program.unwrap_or_else(|| b.transparent(format));
    let output = b.push(
        IrOp::Output {
            w: format.width,
            h: format.height,
        },
        vec![(out_input, OutPort::default())],
    );

    // Step (08 §6.7): viewer pinning reroutes the effective output.
    let effective = resolve_view_override(&mut b, view_override, output);
    b.finish(Some(effective))
}

// ── Builder ─────────────────────────────────────────────────────────────────

/// Arena builder with content-hash dedup. Nodes are appended in dependency order
/// (every input is pushed before its consumer), so the finished `nodes` vector is
/// already topologically sorted (02 §2 "topo-sorted at build").
struct Builder<'a> {
    native_deflicker_inputs: Vec<(super::deflicker::NativeDeflickerKey, IrNodeId)>,
    native_deflicker_pending: Vec<(super::deflicker::NativeDeflickerKey, IrNodeId)>,
    working_color_domain: WorkingColorDomain,
    nodes: Vec<IrNode>,
    /// content hash → node id, so an identical (op, inputs) subgraph is emitted
    /// once (TimeOffset dedup, 02 §2 step 7).
    dedup: HashMap<u128, IrNodeId>,
    diagnostics: Vec<CompileDiagnostic>,
    /// Root-to-leaf sequence context while recursively folding nests.
    sequence_path: Vec<SequenceId>,
    /// Records every lowered `(graph, node)` → IR id so a `ViewNodeOverride`
    /// (08 §6.7) can reroute output to a pinned node.
    view_index: HashMap<(GraphId, GraphNodeId), IrNodeId>,
    /// Parsed-LUT provider (K-0.5), threaded so `Grade` `Lut3d` ops resolve to a
    /// real table. `None` = no provider (LUT ops resolve inert → identity).
    luts: Option<&'a dyn LutProvider>,
    /// Compile-resolved deflicker gains (see [`DeflickerGains`]). `None` = no
    /// measurement available, so every `Deflicker` effect lowers inert.
    deflicker: Option<&'a dyn DeflickerGains>,
    /// D-12 resolved stabilization analyses (22 §6.4). `None` = no provider, so
    /// every clip stays on its unstabilized path.
    stabilization: Option<&'a dyn StabilizationProvider>,
    /// K-E2 scope taps: each lowered clip's post-`Grade` node (07 §5).
    clip_taps: Vec<(ClipId, IrNodeId)>,
    clip_pre_grade_taps: Vec<(ClipId, IrNodeId)>,
    clip_grade_inspections: Vec<ClipGradeInspection>,
    native_qualifier_inspections: Vec<NativeQualifierInspection>,
    native_curve_inputs: Vec<(ClipId, GradeOpId, IrNodeId)>,
    native_graph_qualifier_inspections: Vec<(u32, NativeQualifierInspection)>,
    native_graph_curve_inputs: Vec<(ClipId, u32, GradeOpId, IrNodeId)>,
    native_graph_mattes: Vec<(ClipId, u32, IrNodeId)>,
    native_graph_images: Vec<(ClipId, u32, IrNodeId)>,
    native_stage_graph_outputs: Vec<GradeGraphStageOutput>,
    native_stage_grade_inputs: Vec<NativeStageGradeInput>,
    inspection_stage: timeline::GradeInspectionStage,
    inspection_member_clip: Option<ClipId>,
    clip_output_inspection: bool,
    /// K-E2: the folded program before `CaptionOverlay` (03 §3.6).
    program_tap: Option<IrNodeId>,
    /// K-B5 compare: when true, skip each clip's effect stack + grade so the
    /// clean side of a split compare shares every upstream source node by
    /// content hash with the graded compile.
    skip_clip_looks: bool,
}

impl<'a> Builder<'a> {
    fn new() -> Self {
        Builder {
            native_deflicker_inputs: Vec::new(),
            native_deflicker_pending: Vec::new(),
            working_color_domain: WorkingColorDomain::LegacyLinearRec709,
            nodes: Vec::new(),
            dedup: HashMap::new(),
            diagnostics: Vec::new(),
            sequence_path: Vec::new(),
            view_index: HashMap::new(),
            luts: None,
            deflicker: None,
            stabilization: None,
            clip_taps: Vec::new(),
            clip_pre_grade_taps: Vec::new(),
            clip_grade_inspections: Vec::new(),
            native_qualifier_inspections: Vec::new(),
            native_curve_inputs: Vec::new(),
            native_graph_qualifier_inspections: Vec::new(),
            native_graph_curve_inputs: Vec::new(),
            native_graph_mattes: Vec::new(),
            native_graph_images: Vec::new(),
            native_stage_graph_outputs: Vec::new(),
            native_stage_grade_inputs: Vec::new(),
            inspection_stage: timeline::GradeInspectionStage::Clip,
            inspection_member_clip: None,
            clip_output_inspection: true,
            program_tap: None,
            skip_clip_looks: false,
        }
    }

    fn diag(&mut self, d: CompileDiagnostic) {
        self.diagnostics.push(d);
    }

    /// The resolved D-12 warp for `clip` at clip-relative time `dt`, or `None`
    /// when the clip is unstabilized or its analysis is not warm (22 §6.4).
    ///
    /// Indexed by **source** time, not sequence time: the correction belongs to
    /// the recorded frame, so trimming a clip, moving it, or retiming it must
    /// keep each frame paired with the orientation the camera actually had when
    /// it was captured. Indexing by sequence position would desynchronise the
    /// stabilization the moment anyone slipped the clip.
    fn resolved_stabilize_warp(
        &self,
        clip: &Clip,
        dt: Tick,
    ) -> Option<crate::graph::ir::StabilizeWarp> {
        let spec = clip.stabilization.as_ref()?;
        // An unanalyzed or zero-strength recipe is not an error — it just means
        // there is nothing to apply yet.
        if spec.is_identity() {
            return None;
        }
        let key = spec.analysis_key.as_deref()?;
        let (source_start_s, source_end_s) = crate::graph::stabilize::source_time_range(clip);
        let analysis = self
            .stabilization?
            .analysis(clip.id, key, source_start_s, source_end_s)?;
        let src_time = clip.source_in + clip.speed.source_delta(dt);
        let frame = analysis.frame_index(src_time.as_seconds_f64());
        Some(analysis.warp_at(
            frame,
            matches!(
                spec.crop_mode,
                photonic_core::timeline::StabilizationCropMode::TransparentEdges
            ),
        ))
    }

    /// Whether a coded diagnostic with this `(code, clip)` subject was already
    /// emitted this compile — the once-per-(code, clip) dedupe 38 §2.2/§2.4/§3.5
    /// require. `compile()` is per-tick, so "once" here means once per compiled
    /// frame graph; session-level coalescing is 36 §4.1's job.
    fn has_coded(&self, code: CompileCode, clip: Option<ClipId>) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.code == Some(code) && d.clip == clip)
    }

    /// Push a coded diagnostic only if an identical `(code, clip)` was not
    /// already recorded (dedupe).
    fn diag_coded_once(
        &mut self,
        code: CompileCode,
        severity: DiagSeverity,
        clip: Option<ClipId>,
        message: impl Into<String>,
    ) {
        if !self.has_coded(code, clip) {
            self.diagnostics
                .push(CompileDiagnostic::coded(code, severity, clip, message));
        }
    }

    /// Append a node, deduplicating by content hash. Inputs must already exist.
    fn push(&mut self, op: IrOp, inputs: Vec<(IrNodeId, OutPort)>) -> IrNodeId {
        // Domain-specific arithmetic must never alias the Legacy bounded pass.
        let op = match op {
            IrOp::Merge { mode, opacity }
                if self.working_color_domain == WorkingColorDomain::SceneLinearAcescg
                    && super::ir::native_bounded_blend(mode) =>
            {
                IrOp::NativeBoundedMerge { mode, opacity }
            }
            IrOp::Merge { mode, opacity }
                if self.working_color_domain == WorkingColorDomain::SceneLinearAcescg
                    && super::ir::native_arithmetic_blend(mode) =>
            {
                IrOp::NativeArithmeticMerge { mode, opacity }
            }
            IrOp::Merge { mode, opacity }
                if self.working_color_domain == WorkingColorDomain::SceneLinearAcescg
                    && super::ir::native_luminance_blend(mode) =>
            {
                IrOp::NativeLuminanceMerge { mode, opacity }
            }
            op => op,
        };
        let input_hashes: Vec<u128> = inputs
            .iter()
            .map(|(id, _)| self.nodes[id.0 as usize].content_hash.0)
            .collect();
        let hash = content_hash(&op, &inputs, &input_hashes);
        if let Some(&existing) = self.dedup.get(&hash.0) {
            return existing;
        }
        let id = IrNodeId(self.nodes.len() as u32);
        self.nodes.push(IrNode {
            op,
            inputs,
            content_hash: hash,
        });
        self.dedup.insert(hash.0, id);
        id
    }

    /// A transparent-black premultiplied `SolidColor` sized to `format` — the
    /// universal "nothing here" input (missing-input default, 08 §3.3).
    fn transparent(&mut self, _format: &SequenceFormat) -> IrNodeId {
        self.push(
            IrOp::SolidColor {
                color: LinearColor {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.0,
                },
            },
            vec![],
        )
    }

    fn finish(mut self, output: Option<IrNodeId>) -> CompiledFrame {
        if self.native_qualifier_inspections.len() > 1 {
            let mut qualifier_counts = HashMap::new();
            for key in &self.native_qualifier_inspections {
                *qualifier_counts.entry((key.clip, key.op)).or_insert(0usize) += 1;
            }
            self.native_qualifier_inspections
                .retain(|key| qualifier_counts[&(key.clip, key.op)] == 1);
        }
        if self.native_curve_inputs.len() > 1 {
            let mut curve_counts = HashMap::new();
            for (clip, op, _) in &self.native_curve_inputs {
                *curve_counts.entry((*clip, *op)).or_insert(0usize) += 1;
            }
            self.native_curve_inputs
                .retain(|(clip, op, _)| curve_counts[&(*clip, *op)] == 1);
        }
        CompiledFrame {
            native_deflicker_inputs: self.native_deflicker_inputs,
            native_deflicker_pending: self.native_deflicker_pending,
            graph: FrameGraph {
                working_color_domain: self.working_color_domain,
                nodes: self.nodes,
                output,
            },
            diagnostics: self.diagnostics,
            program_tap: self.program_tap,
            clip_taps: self.clip_taps,
            clip_pre_grade_taps: self.clip_pre_grade_taps,
            clip_grade_inspections: self.clip_grade_inspections,
            native_qualifier_inspections: self.native_qualifier_inspections,
            native_curve_inputs: self.native_curve_inputs,
            native_graph_qualifier_inspections: self.native_graph_qualifier_inspections,
            native_graph_curve_inputs: self.native_graph_curve_inputs,
            native_graph_mattes: self.native_graph_mattes,
            native_graph_images: self.native_graph_images,
            native_stage_graph_outputs: self.native_stage_graph_outputs,
            native_stage_grade_inputs: self.native_stage_grade_inputs,
        }
    }
}

fn resolve_view_override(
    b: &mut Builder<'_>,
    view: Option<ViewNodeOverride>,
    real_output: IrNodeId,
) -> IrNodeId {
    let Some(view) = view else {
        return real_output;
    };
    match b.view_index.get(&(view.graph, view.node)) {
        Some(&id) => id,
        None => {
            b.diag(CompileDiagnostic::at(
                view.graph,
                view.node,
                "view override target is not on the active output path; showing real output",
            ));
            real_output
        }
    }
}

// ── Step 1–4: track fold ──────────────────────────────────────────────────────

/// Fold one sequence's enabled video tracks (bottom→top) into a single program
/// node, honouring Adjustment re-rooting. Returns `None` for an empty program.
#[allow(clippy::too_many_arguments)]
fn fold_sequence(
    b: &mut Builder<'_>,
    project: &TimelineProject,
    seq: &Sequence,
    format_index: usize,
    format: &SequenceFormat,
    tick: Tick,
    quality: Quality,
    cycle: &mut HashSet<SequenceId>,
) -> Option<IrNodeId> {
    if let Err(error) = crate::color::ensure_supported(seq) {
        b.diag(CompileDiagnostic::coded(
            CompileCode::ColorPipelineUnavailable,
            DiagSeverity::Error,
            None,
            error,
        ));
        return Some(b.transparent(format));
    }
    let mut acc: Option<IrNodeId> = None;

    for track in &seq.video_tracks {
        if !track.kind.is_visual() || !track.enabled {
            continue;
        }
        let clips = track.clips.as_slice();
        let Some(idx) = covering_clip_index(clips, tick) else {
            continue;
        };
        let clip = &clips[idx];
        if !clip.enabled {
            continue; // step 8: disabled clip is a dead branch.
        }

        // Adjustment clips (step 4 / 35 §2.4(b)): re-root the composite below
        // through the clip's OWN effect/grade stack rather than contributing a
        // source. The adjustment has no own content, so the track stack does NOT
        // apply here — the clip stack acts on the already-merged accumulator.
        if matches!(clip.source, ClipSource::Adjustment) {
            if let Some(below) = acc {
                let dt = tick - clip.start;
                acc = Some(apply_stack(
                    b,
                    &clip.effects,
                    clip.grade.as_ref(),
                    below,
                    dt,
                    Some(clip.id),
                    Some(VfxOwner::Clip(clip.id)),
                ));
            }
            continue;
        }

        // Transition partner (02 §2 step 1 / 08 §2.0b, 38 §1): at a cut the
        // incoming clip's `transition_in` borrows the OUTGOING clip past its own
        // out point, into its remaining source handle, and mixes. The overlap
        // duration is clamped to that handle (38 §1.1). A successful transition
        // contributes a single track image at opacity 1 (each partner's own
        // opacity is baked into its side of the mix).
        match active_transition(project, clips, idx, tick) {
            Some(tr) => {
                if let Some(node) = build_transition(
                    b,
                    project,
                    seq,
                    format_index,
                    format,
                    clips,
                    &tr,
                    tick,
                    quality,
                    cycle,
                ) {
                    // Track scope (35 §2.4(a)): the track effects/grade act on
                    // this track's OWN content (the transition image, opacity 1)
                    // before it merges — never on the accumulator. Sequence-
                    // relative keyframes.
                    // TODO(30 §2.3): gate on the track stack's Applicability once a manifest type exists.
                    let node = apply_stack(
                        b,
                        &track.effects,
                        track.grade.as_ref(),
                        node,
                        tick,
                        None,
                        Some(VfxOwner::Track(track.id)),
                    );
                    acc = Some(fold_over(b, acc, node, track.opacity, track.blend));
                    continue;
                }
                // Partner unavailable (disabled / opacity-0 / Adjustment): fall
                // through to the plain covering-clip render below.
            }
            None => {
                // 38 §1.2 case 3: an authored `transition_in` whose outgoing
                // handle is exhausted (clamped to zero) does not render — warn
                // and fall through to the plain covering-clip render.
                if let Some(cid) = suppressed_transition_clip(project, clips, idx, tick) {
                    b.diag_coded_once(
                        CompileCode::TransitionHandleClipped,
                        DiagSeverity::Warning,
                        Some(cid),
                        format!(
                            "transition on clip {} not rendered: the outgoing clip has \
                             no source handle past its out point (38 §1.2)",
                            clips[idx].name
                        ),
                    );
                }
            }
        }

        let Some((image, opacity)) = build_clip_chain(
            b,
            project,
            seq,
            format_index,
            format,
            clip,
            tick,
            quality,
            cycle,
        ) else {
            continue; // step 8: invisible / opacity-0 clip folded away.
        };
        // 38 §1.3: a `transition_out` at a gap / sequence end is a FADE-OUT (no
        // partner, no borrowed handle) — the clip's chain merged toward
        // transparent over the window. At a cut it is inert (the incoming clip's
        // `transition_in` owns the cut), so `active_fade_out` returns `None` there.
        let image = match active_fade_out(clips, idx, tick) {
            Some(t) => bake_opacity(b, image, (1.0 - t).clamp(0.0, 1.0)),
            None => image,
        };
        // Track scope (35 §2.4(a)): the track effects/grade act on this track's
        // OWN composited content before it merges into the accumulator — never on
        // the accumulator itself. Track keyframes are sequence-relative (`tick`).
        // TODO(30 §2.3): gate on the track stack's Applicability once a manifest type exists.
        let image = apply_stack(
            b,
            &track.effects,
            track.grade.as_ref(),
            image,
            tick,
            None,
            Some(VfxOwner::Track(track.id)),
        );
        acc = Some(fold_over(
            b,
            acc,
            image,
            opacity * track.opacity,
            track.blend,
        ));
    }
    acc
}

/// Index of the clip whose `[start, end)` covers `t` on this track (tracks are
/// sorted, non-overlapping — 01 §4).
fn covering_clip_index(clips: &[Clip], t: Tick) -> Option<usize> {
    clips.iter().position(|c| c.start <= t && t < c.end())
}

// ── Transitions (02 §2 step 1 / 08 §2.0b) ─────────────────────────────────────

/// A resolved clip transition covering `tick`: which two clips blend, the mix
/// factor (already eased; `0` = fully outgoing, `1` = fully incoming), and how.
struct ActiveTransition {
    outgoing: usize,
    incoming: usize,
    kind: TransitionKind,
    params: timeline::TransitionParams,
    /// Eased mix in `0..1`.
    t: f32,
    /// `Some` when the requested overlap was shortened to fit the outgoing
    /// clip's available source handle (38 §1.1) — drives the
    /// `TransitionHandleClipped` Info once the mix is confirmed to render.
    handle_clip: Option<HandleClip>,
}

/// Record of a transition whose requested duration exceeded the outgoing clip's
/// available source handle (38 §1.1). `available < requested`; `available == 0`
/// means the transition is suppressed entirely.
#[derive(Copy, Clone, Debug, PartialEq)]
struct HandleClip {
    requested: Tick,
    available: Tick,
}

/// Timeline-domain ticks of material the outgoing `clip` can borrow PAST its out
/// point for a transition (38 §1.1). `None` = unbounded / unknown (never clamp):
/// generators are infinite, an absent probe is unknown, a freeze-frame never
/// runs out.
fn available_handle_ticks(project: &TimelineProject, clip: &Clip) -> Option<Tick> {
    let source_duration: Tick = match &clip.source {
        // Generators / vectors are infinite; an unknown source is inert.
        ClipSource::SolidColor { .. }
        | ClipSource::Adjustment
        | ClipSource::Text { .. }
        | ClipSource::Vector { .. }
        | ClipSource::Unknown(_) => return None,
        ClipSource::NestedSequence { sequence } => project.sequences.get(sequence)?.content_end(),
        ClipSource::Asset { asset } => project.media.assets.get(asset)?.probe.as_ref()?.duration,
    };
    // Source ticks consumed at the out point, then the source ticks remaining.
    let out_src = clip.source_in + clip.speed.source_delta(clip.duration);
    let avail_src = (source_duration.0 - out_src.0).max(0);
    // Convert source-domain ticks back to the timeline domain via the speed
    // ratio (source ticks per timeline tick): `avail_timeline = avail_src / r`.
    // Mirrors `scale_ticks` (clip.rs) with multiply-before-divide in i128.
    let r = match &clip.speed {
        SpeedMap::Constant(r) => *r,
        // v1 approximation: past a clip's out point the ramp has ended, so the
        // LAST key's ratio is the one that holds — exact for the only case the
        // ramp can be in past the out point.
        SpeedMap::Keyframed { keys } => keys.last().map(|k| k.ratio).unwrap_or(Ratio::ONE),
    };
    if r.num == 0 {
        return None; // a frozen source never runs out.
    }
    let avail_timeline = (avail_src as i128 * r.den as i128) / r.num as i128;
    Some(Tick(avail_timeline.max(0) as i64))
}

/// Clamp a requested transition duration to the outgoing clip's available source
/// handle (38 §1.1). Returns `(requested, None)` when nothing constrains it
/// (`available_handle_ticks` is `None`, or the handle already covers the
/// request); `(available, Some(..))` when the handle is shorter (`available` may
/// be zero, meaning suppress).
fn clamp_transition(
    project: &TimelineProject,
    outgoing: &Clip,
    requested: Tick,
) -> (Tick, Option<HandleClip>) {
    match available_handle_ticks(project, outgoing) {
        Some(available) if available < requested => (
            available,
            Some(HandleClip {
                requested,
                available,
            }),
        ),
        _ => (requested, None),
    }
}

/// Detect a two-clip transition active at `tick` for the covering clip `idx`
/// (08 §2.0b, 38 §1). Only a `transition_in` at a cut mixes two clips: it
/// borrows the previous clip as the outgoing partner, over a window clamped to
/// that partner's source handle (38 §1.1). A `transition_out` is a fade-out, not
/// a two-clip mix (38 §1.3) — see [`active_fade_out`]. Returns `None` when no
/// transition is active or the handle clamps the overlap to zero (38 §1.2 case 3,
/// which falls through to the plain covering-clip render).
fn active_transition(
    project: &TimelineProject,
    clips: &[Clip],
    idx: usize,
    tick: Tick,
) -> Option<ActiveTransition> {
    let clip = &clips[idx];
    let tr = clip.transition_in.as_ref()?;
    if idx == 0 || tr.duration.0 <= 0 {
        return None;
    }
    let outgoing = &clips[idx - 1];
    let (duration, handle_clip) = clamp_transition(project, outgoing, tr.duration);
    if duration.0 <= 0 {
        return None; // 38 §1.2 case 3: handle exhausted → no transition.
    }
    let start = clip.start;
    let end = clip.start + duration;
    if start <= tick && tick < end {
        let raw = (tick - start).0 as f32 / duration.0 as f32;
        Some(ActiveTransition {
            outgoing: idx - 1,
            incoming: idx,
            kind: tr.kind,
            params: tr.params,
            t: ease(tr.params.curve, raw),
            handle_clip,
        })
    } else {
        None
    }
}

/// The `ClipId` of a covering clip whose authored `transition_in` window covers
/// `tick` but whose outgoing handle clamps the overlap to zero (38 §1.2 case 3).
/// Used only to emit the suppression Warning; `None` otherwise.
fn suppressed_transition_clip(
    project: &TimelineProject,
    clips: &[Clip],
    idx: usize,
    tick: Tick,
) -> Option<ClipId> {
    let clip = &clips[idx];
    let tr = clip.transition_in.as_ref()?;
    if idx == 0 || tr.duration.0 <= 0 {
        return None;
    }
    // Only relevant on frames the authored overlap would have covered.
    let start = clip.start;
    let end = clip.start + tr.duration;
    if !(start <= tick && tick < end) {
        return None;
    }
    let (duration, handle_clip) = clamp_transition(project, &clips[idx - 1], tr.duration);
    (handle_clip.is_some() && duration.0 == 0).then_some(clip.id)
}

/// Detect a `transition_out` FADE-OUT active at `tick` for clip `idx` (38 §1.3),
/// returning the eased progress `t∈0..1` (0 = fully visible, 1 = faded out). A
/// `transition_out` is legal only where no clip starts at this clip's end (a gap
/// or the sequence end); at a cut it is inert (the incoming clip's
/// `transition_in` owns the transition), so this returns `None` there.
fn active_fade_out(clips: &[Clip], idx: usize, tick: Tick) -> Option<f32> {
    let clip = &clips[idx];
    let tr = clip.transition_out.as_ref()?;
    if tr.duration.0 <= 0 {
        return None;
    }
    // A clip starting exactly at this clip's end means a cut — not a fade-out.
    let at_cut = clips
        .get(idx + 1)
        .is_some_and(|next| next.start == clip.end());
    if at_cut {
        return None;
    }
    let start = clip.end() - tr.duration;
    let end = clip.end();
    if start <= tick && tick < end {
        let raw = (tick - start).0 as f32 / tr.duration.0 as f32;
        Some(ease(tr.params.curve, raw))
    } else {
        None
    }
}

/// Build the transition mix node, or `None` when a partner can't contribute
/// (disabled / opacity-0 / Adjustment) so the caller falls back to the plain
/// covering-clip render. Each partner is evaluated at `tick` (the outgoing clip
/// past its own end, into its source handles — the standard NLE overlap model).
#[allow(clippy::too_many_arguments)]
fn build_transition(
    b: &mut Builder<'_>,
    project: &TimelineProject,
    seq: &Sequence,
    format_index: usize,
    format: &SequenceFormat,
    clips: &[Clip],
    tr: &ActiveTransition,
    tick: Tick,
    quality: Quality,
    cycle: &mut HashSet<SequenceId>,
) -> Option<IrNodeId> {
    let outgoing_clip = &clips[tr.outgoing];
    let incoming_clip = &clips[tr.incoming];
    if !outgoing_clip.enabled
        || !incoming_clip.enabled
        || matches!(outgoing_clip.source, ClipSource::Adjustment)
        || matches!(incoming_clip.source, ClipSource::Adjustment)
    {
        return None;
    }
    // 38 §1.1: a shortened overlap (the outgoing handle ran out) renders at the
    // clamped duration; record it once the mix is confirmed to render.
    if let Some(hc) = &tr.handle_clip {
        b.diag_coded_once(
            CompileCode::TransitionHandleClipped,
            DiagSeverity::Info,
            Some(incoming_clip.id),
            format!(
                "transition on clip {} shortened to {} ticks (outgoing source handle \
                 {} < requested {}, 38 §1.1)",
                incoming_clip.name, hc.available.0, hc.available.0, hc.requested.0
            ),
        );
    }
    let (out_img, out_op) = build_clip_chain(
        b,
        project,
        seq,
        format_index,
        format,
        outgoing_clip,
        tick,
        quality,
        cycle,
    )?;
    let (in_img, in_op) = build_clip_chain(
        b,
        project,
        seq,
        format_index,
        format,
        incoming_clip,
        tick,
        quality,
        cycle,
    )?;
    let outgoing = bake_opacity(b, out_img, out_op);
    let incoming = bake_opacity(b, in_img, in_op);
    Some(transition_mix(
        b, tr.kind, &tr.params, outgoing, incoming, tr.t,
    ))
}

/// Fade `node` toward transparent by `opacity` (premultiplied) when `opacity < 1`,
/// so a partner's own clip opacity is baked into its side of a transition before
/// the mix. A fully-opaque partner is returned unchanged.
fn bake_opacity(b: &mut Builder<'_>, node: IrNodeId, opacity: f32) -> IrNodeId {
    if opacity >= 1.0 {
        return node;
    }
    let transparent = b.push(
        IrOp::SolidColor {
            color: LinearColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
        },
        vec![],
    );
    b.push(
        IrOp::Merge {
            mode: BlendMode::Normal,
            opacity,
        },
        vec![
            (node, OutPort::default()),
            (transparent, OutPort::default()),
        ],
    )
}

/// Emit the time-parameterized mix for a transition at eased factor `t` (08 §2.0b).
/// Reuses `Merge` (premultiplied `over`) and `SolidColor` — the mix factor is a
/// compile-time constant, so distinct ticks produce distinct `Merge` opacities
/// (and thus distinct content hashes). Geometric `Wipe`/`Push` fall back to a
/// cross-dissolve in P3 (no directional wipe pass yet) with a diagnostic.
fn transition_mix(
    b: &mut Builder<'_>,
    kind: TransitionKind,
    params: &timeline::TransitionParams,
    outgoing: IrNodeId,
    incoming: IrNodeId,
    t: f32,
) -> IrNodeId {
    match kind {
        TransitionKind::CrossDissolve => merge_over(b, incoming, outgoing, t),
        TransitionKind::DipToBlack => dip_through(b, outgoing, incoming, t, opaque_black()),
        TransitionKind::DipToColor => dip_through(
            b,
            outgoing,
            incoming,
            t,
            params.color.unwrap_or_else(opaque_black),
        ),
        // Directional geometric transitions (08 §2.0b): dedicated binary IR ops
        // (inputs [incoming, outgoing]) rather than an overloaded `Merge`. `t` is
        // the compile-time eased factor, so distinct ticks give distinct content
        // hashes exactly as the cross-dissolve does. The CPU kernels in
        // `graph::ops` are the golden reference (02 §2), with WGSL twins in `eval`.
        TransitionKind::Wipe => b.push(
            IrOp::WipeMix {
                direction: wipe_direction(params.direction),
                softness: params.softness,
                t,
            },
            vec![
                (incoming, OutPort::default()),
                (outgoing, OutPort::default()),
            ],
        ),
        TransitionKind::Push => b.push(
            IrOp::PushMix {
                direction: wipe_direction(params.direction),
                t,
            },
            vec![
                (incoming, OutPort::default()),
                (outgoing, OutPort::default()),
            ],
        ),
        // Analytical luma-map wipe (26 K-B7): Photonic-authored maps, no asset.
        TransitionKind::LumaWipe => b.push(
            IrOp::LumaWipeMix {
                kind: luma_wipe_kind(params.luma_map),
                softness: params.softness,
                invert: params.invert,
                t,
            },
            vec![
                (incoming, OutPort::default()),
                (outgoing, OutPort::default()),
            ],
        ),
        // Forward-compat (39 §2.2): an unknown transition renders as a HARD CUT
        // — the incoming clip directly, no blend — never a guessed dissolve.
        // `TransitionKind` is `#[non_exhaustive]`, so this wildcard also catches
        // any future known kind a newer build adds (inert cut until this build
        // learns to render it), which is the correct conservative default.
        _ => {
            b.diag(CompileDiagnostic::plain(format!(
                "{kind:?} transition renders as a hard cut (this build does not \
                 understand it)"
            )));
            incoming
        }
    }
}

fn luma_wipe_kind(m: timeline::LumaWipeMap) -> crate::graph::luma_wipe::LumaWipeKind {
    use crate::graph::luma_wipe::LumaWipeKind;
    match m {
        timeline::LumaWipeMap::LinearH => LumaWipeKind::LinearH,
        timeline::LumaWipeMap::LinearV => LumaWipeKind::LinearV,
        timeline::LumaWipeMap::Radial => LumaWipeKind::Radial,
        timeline::LumaWipeMap::BarnDoorH => LumaWipeKind::BarnDoorH,
        timeline::LumaWipeMap::Clock => LumaWipeKind::Clock,
    }
}

/// Lower the authoring [`timeline::WipeDirection`] (the sweep axis + orientation
/// on `TransitionParams`) to the IR [`WipeDirection`] the Wipe/Push evaluators
/// consume. `Left`/`Up` reveal the incoming from that edge (`…ToRight`/`…ToTop`);
/// `Right`/`Down` are their mirrors.
fn wipe_direction(d: timeline::WipeDirection) -> WipeDirection {
    match d {
        timeline::WipeDirection::Left => WipeDirection::LeftToRight,
        timeline::WipeDirection::Right => WipeDirection::RightToLeft,
        timeline::WipeDirection::Up => WipeDirection::BottomToTop,
        timeline::WipeDirection::Down => WipeDirection::TopToBottom,
    }
}

/// `Merge` `top` over `bottom` at `opacity` (Normal blend), the fold primitive
/// shared by every transition kind.
fn merge_over(b: &mut Builder<'_>, top: IrNodeId, bottom: IrNodeId, opacity: f32) -> IrNodeId {
    b.push(
        IrOp::Merge {
            mode: BlendMode::Normal,
            opacity: opacity.clamp(0.0, 1.0),
        },
        vec![(top, OutPort::default()), (bottom, OutPort::default())],
    )
}

/// Dip-through-color mix (`DipToBlack`/`DipToColor`, 08 §2.0b): outgoing dips to
/// `color` over the first half (`t < 0.5`), then `color` reveals the incoming
/// over the second half.
fn dip_through(
    b: &mut Builder<'_>,
    outgoing: IrNodeId,
    incoming: IrNodeId,
    t: f32,
    color: Color,
) -> IrNodeId {
    let solid = b.push(
        IrOp::SolidColor {
            color: color_to_linear_premult(color),
        },
        vec![],
    );
    if t < 0.5 {
        // outgoing → color; the solid fades in over `[0, 0.5)`.
        merge_over(b, solid, outgoing, t * 2.0)
    } else {
        // color → incoming; the incoming fades in over `[0.5, 1]`.
        merge_over(b, incoming, solid, (t - 0.5) * 2.0)
    }
}

/// Document color swatches are encoded sRGB, not native AP1. Decode,
/// chromatically adapt, then premultiply once; malformed swatches fail closed.
fn native_swatch_color(c: Color) -> Option<LinearColor> {
    if [c.r, c.g, c.b, c.a]
        .iter()
        .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
    {
        return None;
    }
    let rgb = crate::color::native::encoded_srgb_to_linear_ap1_display([
        c.r as f64, c.g as f64, c.b as f64,
    ]);
    Some(LinearColor {
        r: rgb[0] as f32 * c.a,
        g: rgb[1] as f32 * c.a,
        b: rgb[2] as f32 * c.a,
        a: c.a,
    })
}

fn opaque_black() -> Color {
    Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    }
}

/// Ease `t∈0..1` under `curve` (08 §2.0b transition easing). `EaseInOut` is the
/// standard smooth-in/out quadratic; the endpoints are exact (0→0, 1→1).
fn ease(curve: EaseCurve, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match curve {
        EaseCurve::Linear => t,
        EaseCurve::EaseIn => t * t,
        EaseCurve::EaseOut => t * (2.0 - t),
        EaseCurve::EaseInOut => {
            if t < 0.5 {
                2.0 * t * t
            } else {
                1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
            }
        }
    }
}

/// Composite `top` over `acc` at `opacity` in blend `mode` (premultiplied) —
/// the track-fold merge (35 §2). `mode`/`opacity` are the track's `blend`/
/// (`clip_opacity × track.opacity`). A single bottom track needs no `Merge` only
/// when it is a plain fully-opaque `Normal` merge (any non-Normal blend or reduced
/// opacity must still emit the node so it reaches the accumulator's colour).
fn fold_over(
    b: &mut Builder<'_>,
    acc: Option<IrNodeId>,
    top: IrNodeId,
    opacity: f32,
    mode: BlendMode,
) -> IrNodeId {
    match acc {
        None if mode == BlendMode::Normal && opacity >= 1.0 => top,
        None => {
            let transparent = b.push(
                IrOp::SolidColor {
                    color: LinearColor {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.0,
                    },
                },
                vec![],
            );
            b.push(
                IrOp::Merge { mode, opacity },
                vec![(top, OutPort::default()), (transparent, OutPort::default())],
            )
        }
        Some(bottom) => b.push(
            IrOp::Merge { mode, opacity },
            vec![(top, OutPort::default()), (bottom, OutPort::default())],
        ),
    }
}

/// Step 2/3: build one clip's image node and its evaluated opacity, or `None`
/// when it folds away (disabled / opacity 0 — step 8). The chain is
/// source(or composition splice) → Transform2D → effects → grade.
#[allow(clippy::too_many_arguments)]
fn build_clip_chain(
    b: &mut Builder<'_>,
    project: &TimelineProject,
    seq: &Sequence,
    format_index: usize,
    format: &SequenceFormat,
    clip: &Clip,
    tick: Tick,
    quality: Quality,
    cycle: &mut HashSet<SequenceId>,
) -> Option<(IrNodeId, f32)> {
    let dt = tick - clip.start; // clip-relative time for keyframe eval (01 §6).

    // Per-format reframe override (CAP-012) is a static transform for this
    // format; otherwise evaluate the animated clip transform at dt.
    let xf = match clip.reframe.get(&format_index) {
        Some(over) => *over,
        None => eval_clip_transform(&clip.transform, dt),
    };
    let opacity = xf.opacity as f32;
    if opacity <= 0.0 {
        return None; // dead branch (step 8).
    }
    if seq.color.is_legacy() {
        if let ClipSource::Asset { asset } = clip.source {
            if let Some((field, value)) = unsupported_legacy_color(project, asset) {
                b.diag(CompileDiagnostic::coded(
                CompileCode::ColorPipelineUnavailable,
                DiagSeverity::Error,
                Some(clip.id),
                format!("Clip {} uses asset {asset} with {value} {field}, which Legacy SDR cannot interpret", clip.id),
            ));
                return Some((b.transparent(format), opacity));
            }
        }
    }

    // Step 3: composition substitutes the SOURCE op only; else the plain source.
    let source = match clip.composition {
        Some(graph_id) => lower_composition(
            b,
            project,
            seq,
            format_index,
            format,
            clip,
            graph_id,
            tick,
            quality,
            cycle,
        ),
        None => build_clip_source(
            b,
            project,
            seq,
            format_index,
            format,
            clip,
            tick,
            quality,
            cycle,
        ),
    };

    // Asset scope (35 §2.4(c)): the referenced material's own effects/grade — a
    // per-camera LUT / lens correction — apply in SOURCE space, before the clip's
    // `Transform2D`, and sit BENEATH the clip's own stack so a clip-level grade can
    // correct an asset-level one. Keyframes share the clip-relative `dt` domain.
    // Only `Asset`/`Vector` clips reference an asset; others have no asset stack.
    // TODO(30 §2.3): gate on the asset stack's Applicability once a manifest type exists.
    let source = match clip
        .source
        .asset()
        .and_then(|a| project.media.assets.get(&a))
    {
        Some(asset) => apply_stack(
            b,
            &asset.effects,
            asset.grade.as_ref(),
            source,
            dt,
            None,
            Some(VfxOwner::Asset(asset.id)),
        ),
        None => source,
    };

    // Remainder of step 2's chain, applied on top of the source/composition.
    let mut cur = source;

    // D-12 (22 §6.4): the stabilization warp goes here — in SOURCE space,
    // beneath the clip's own `Transform2D`. The order is not incidental: the
    // warp corrects what the *camera* did, so it must resolve against the
    // original framing, while `Transform2D` is what the *editor* chose and
    // applies to the already-corrected image. Swapping them would make the
    // correction depend on the edit, so re-framing a shot would break its
    // stabilization.
    //
    // Emitted only when a resolved, non-identity warp exists: an unanalyzed or
    // zero-strength recipe leaves the source path untouched rather than paying
    // for a pass that resamples to no effect (22 §6.5 — removing stabilization
    // restores the source path).
    if let Some(warp) = b.resolved_stabilize_warp(clip, dt) {
        if warp.is_identity() {
            // Nothing to do; skip the pass entirely.
        } else {
            cur = b.push(
                IrOp::StabilizeWarp {
                    warp,
                    sampling: Sampling::Bilinear,
                },
                vec![(cur, OutPort::default())],
            );
        }
    }

    cur = b.push(
        IrOp::Transform2D {
            mat: clip_transform_matrix(&xf, format),
            sampling: Sampling::Bilinear,
        },
        vec![(cur, OutPort::default())],
    );
    // Clip scope (35 §2.4): the clip's own effects/grade, clip-relative keyframes.
    // TODO(30 §2.3): gate on the clip stack's Applicability once a manifest type exists.
    // K-B5: compare-clean compile omits the look stack so the bypass side shares
    // every upstream (source/transform) node by content hash with the full compile.
    if !b.skip_clip_looks {
        cur = apply_stack(
            b,
            &clip.effects,
            None,
            cur,
            dt,
            Some(clip.id),
            Some(VfxOwner::Clip(clip.id)),
        );
        let group_chain = clip.group.map(|id| seq.group_chain(id)).unwrap_or_default();
        // Root→leaf before the shot correction; leaf→root after it. This is
        // per-member grading, not a grade on the composite of grouped clips.
        for id in group_chain.iter().rev() {
            if let Some(grade) = seq
                .groups
                .get(id)
                .and_then(|group| group.pre_grade.as_ref())
            {
                cur = apply_member_group_grade(b, grade, cur, dt, clip.id, *id, true);
            }
        }
        b.clip_pre_grade_taps.push((clip.id, cur));
        if let Some(grade) = clip.grade.as_ref() {
            if !grade.bypass
                && grade.graph.is_none()
                && grade
                    .ops
                    .iter()
                    .any(|op| op.enabled && op.kind == GradeOpKind::HslQualifier)
            {
                let (ops, _) =
                    photonic_render::grade::resolve_with_ids_and_diagnostics(grade, dt, |asset| {
                        b.luts.and_then(|provider| provider.lut(asset))
                    });
                b.clip_grade_inspections
                    .push(ClipGradeInspection { clip: clip.id, ops });
            }
            cur = apply_grade(b, grade, cur, dt, Some(VfxOwner::Clip(clip.id)));
        }
        cur = apply_clip_look(b, project, clip, cur, dt);
        for id in group_chain {
            if let Some(grade) = seq
                .groups
                .get(&id)
                .and_then(|group| group.post_grade.as_ref())
            {
                cur = apply_member_group_grade(b, grade, cur, dt, clip.id, id, false);
            }
        }
    }
    // K-E2 / 07 §5: this node — post-`Grade`, pre-fold — is the per-clip scope
    // tap. Recorded for every lowered clip (including clips inside a nest, which
    // reach here through `fold_sequence`'s recursion), so no second compile is
    // needed to answer `get_scopes(clip, at)`.
    b.clip_taps.push((clip.id, cur));
    Some((cur, opacity))
}

/// Append an enabled effect stack then a grade (if any) onto `input`, in the one
/// normative scope order (02 §2 steps 1–7 / §2.3, restated by 35 §2): every
/// enabled effect in author order, then the grade on top. Asset, track and
/// master scopes use both parts. The clip scope calls this with no grade, then
/// inserts group pre → clip grade → group post explicitly at its grade stage.
///
/// `dt` is the keyframe-evaluation domain for the scope being applied, and it is
/// NOT the same at every scope: the clip and asset stacks are **clip-relative**
/// (`dt = tick − clip.start`, 01 §6), while the track and master stacks are
/// **sequence-relative** (`dt = tick`). Passing the wrong domain mis-times every
/// keyframe on the stack with no error and no visible warning — get it right at
/// the call site.
///
/// `scope` names the clip this stack belongs to, when it belongs to one. Only
/// [`EffectKind::Deflicker`] consults it, to look up the gain its analysis job
/// measured; at track/master scope there is no clip to have measured, so a
/// deflicker there lowers inert rather than guessing.
fn apply_stack(
    b: &mut Builder<'_>,
    effects: &[ClipEffect],
    grade: Option<&Grade>,
    input: IrNodeId,
    dt: Tick,
    scope: Option<ClipId>,
    grade_owner: Option<VfxOwner>,
) -> IrNodeId {
    let mut cur = input;
    for (effect_index, fx) in effects.iter().enumerate() {
        if !fx.enabled || fx.inert {
            continue;
        }
        // K-B3: fold out-of-zone effects entirely (same as disabled) so the
        // NodeCache never pays for an inactive segment of the stack.
        if !fx.active_at(dt) {
            continue;
        }
        // Keyframe-resolve the effect's params at the scope's `dt` (K-0.2). The op
        // discriminant, kind, AND resolved params all participate in the content
        // hash (`hash_op`), so two clips differing only in e.g. Blur radius are
        // distinct cache identities — never a colliding NodeCache entry.
        // K-B16: a bridged raster id lowers as `Unknown(tag)` so eval_cpu can
        // dispatch the core raster kernel while the GPU still blit-passthroughs.
        let kind = if crate::graph::raster_bridge::is_bridged(fx.id.as_str()) {
            EffectKind::Unknown(photonic_core::timeline::UnknownTag::intern(fx.id.as_str()))
        } else {
            fx.kind
        };
        let mut params = resolve_effect_params(fx.kind, &fx.params.base, &fx.params, dt);
        if b.working_color_domain == WorkingColorDomain::SceneLinearAcescg {
            let sigma = params.get("params.radius").and_then(|value| match value {
                PropValue::Float(value) if value.is_finite() && (0.0..=500.0).contains(value) => {
                    Some(*value as f32)
                }
                _ => None,
            });
            // Native routing uses the declared kind; `kind` above is the
            // Legacy raster-bridge projection of the same catalog id.
            if fx.kind == EffectKind::Deflicker
                && (fx.id.as_str().is_empty() || fx.id.as_str() == "color.deflicker")
            {
                let value = |path: &str, lo: f64, hi: f64| match params.get(path) {
                    Some(PropValue::Float(v)) if v.is_finite() && (lo..=hi).contains(v) => {
                        Some(*v as f32)
                    }
                    _ => None,
                };
                let amount = value("params.amount", 0.0, 1.0);
                let chroma = value("params.chroma_amount", 0.0, 1.0);
                let valid = value("params.window", 0.2, 30.0).is_some()
                    && value("params.max_change", 0.0, 0.9).is_some();
                if !valid
                    || amount.is_none()
                    || chroma.is_none()
                    || !matches!(
                        grade_owner,
                        Some(
                            VfxOwner::Clip(_)
                                | VfxOwner::Asset(_)
                                | VfxOwner::Track(_)
                                | VfxOwner::Master(_)
                        )
                    )
                {
                    b.diag(CompileDiagnostic::coded(CompileCode::ColorPipelineUnavailable,DiagSeverity::Error,scope,"native Deflicker requires a supported owner and finite catalog ranges; analysis controls sample on the bounded exposure grid"));
                } else {
                    let Some(sequence) = b.sequence_path.last().copied() else {
                        b.diag(CompileDiagnostic::coded(
                            CompileCode::ColorPipelineUnavailable,
                            DiagSeverity::Error,
                            scope,
                            "native Deflicker requires a sequence context",
                        ));
                        continue;
                    };
                    let key = super::deflicker::NativeDeflickerKey {
                        sequence,
                        owner: grade_owner.unwrap(),
                        clip_context: scope,
                        effect_index: effect_index as u32,
                    };
                    b.native_deflicker_inputs.push((key, cur));
                    if amount == Some(0.0) {
                        continue;
                    }
                    if let Some(measured) = b.deflicker.and_then(|d| d.native_gain(&key, dt)) {
                        let logs = measured.map(f32::ln);
                        let shared = 0.2126 * logs[0] + 0.7152 * logs[1] + 0.0722 * logs[2];
                        let gain = logs.map(|v| {
                            ((shared + (v - shared) * chroma.unwrap()) * amount.unwrap()).exp()
                        });
                        let band = b.deflicker.and_then(|d| d.native_band(&key, dt)).map_or(
                            [0.0; 3],
                            |band| {
                                [
                                    band.cycles as f32,
                                    band.amplitude * amount.unwrap(),
                                    band.phase.rem_euclid(std::f32::consts::TAU),
                                ]
                            },
                        );
                        cur = b.push(
                            IrOp::NativeDeflicker { gain, band },
                            vec![(cur, OutPort::default())],
                        );
                    } else if b.deflicker.is_some_and(|d| d.analysis_mode()) {
                        cur = b.push(
                            IrOp::NativeDeflicker {
                                gain: [1.0; 3],
                                band: [0.0; 3],
                            },
                            vec![(cur, OutPort::default())],
                        );
                        b.native_deflicker_pending.push((key, cur));
                    } else {
                        b.diag(CompileDiagnostic::coded(CompileCode::ColorPipelineUnavailable,DiagSeverity::Error,scope,"native Deflicker analysis is unavailable or incomplete for this effect owner/context"));
                    }
                }
            } else if fx.id.as_str() == "noise.reduce"
                && matches!(fx.kind,EffectKind::Unknown(tag) if tag.as_str()=="noise.reduce")
            {
                match params.get("params.strength") {
                    Some(PropValue::Float(v)) if v.is_finite() && (0.0..=1.0).contains(v) => {
                        cur = b.push(
                            IrOp::NativeDenoise {
                                strength: *v as f32,
                                spatial_scale: 1.0,
                            },
                            vec![(cur, OutPort::default())],
                        )
                    }
                    _ => b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        "native denoise strength must be finite in 0..=1",
                    )),
                }
            } else if fx.id.as_str() == "stylize.grain"
                && matches!(fx.kind,EffectKind::Unknown(tag) if tag.as_str()=="stylize.grain")
            {
                let scalar = |path: &str, max: f64| match params.get(path) {
                    Some(PropValue::Float(v)) if v.is_finite() && (0.0..=max).contains(v) => {
                        Some(*v)
                    }
                    _ => None,
                };
                match (scalar("params.amount",1.0),scalar("params.monochrome",1.0),scalar("params.seed",1_000_000_000.0)) {
                    (Some(amount),Some(mono),Some(seed))=>cur=b.push(IrOp::NativeGrain {amount:amount as f32,monochrome:mono>0.5,seed:seed.round() as u32,pixel_scale:[1.0;2]},vec![(cur,OutPort::default())]),
                    _=>b.diag(CompileDiagnostic::coded(CompileCode::ColorPipelineUnavailable,DiagSeverity::Error,scope,"native grain requires finite amount/monochrome 0..=1 and seed 0..=1000000000")),
                }
            } else if fx.id.as_str() == "stylize.vignette"
                && matches!(fx.kind, EffectKind::Unknown(tag) if tag.as_str() == "stylize.vignette")
            {
                let scalar = |path: &str, lo: f64, hi: f64| match params.get(path) {
                    Some(PropValue::Float(v)) if v.is_finite() && (lo..=hi).contains(v) => {
                        Some(*v as f32)
                    }
                    _ => None,
                };
                match (
                    scalar("params.amount", -1.0, 1.0),
                    scalar("params.feather", 0.0, 1.0),
                ) {
                    (Some(amount), Some(feather)) => {
                        cur = b.push(
                            IrOp::NativeVignette { amount, feather },
                            vec![(cur, OutPort::default())],
                        )
                    }
                    _ => b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        "native vignette requires finite amount -1..=1 and feather 0..=1",
                    )),
                }
            } else if matches!(fx.id.as_str(), "color.posterize" | "color.threshold")
                && matches!(fx.kind,EffectKind::Unknown(tag) if tag.as_str()==fx.id.as_str())
            {
                use super::ir::NativeTransferArtisticKind;
                let (kind, path, lo, hi) = if fx.id.as_str() == "color.posterize" {
                    (
                        NativeTransferArtisticKind::Posterize,
                        "params.levels",
                        2.0,
                        255.0,
                    )
                } else {
                    (
                        NativeTransferArtisticKind::Threshold,
                        "params.level",
                        0.0,
                        1.0,
                    )
                };
                match params.get(path){Some(PropValue::Float(v))if v.is_finite()&&(lo..=hi).contains(v)=>{let parameter=if kind==NativeTransferArtisticKind::Posterize{v.round()as f32}else{*v as f32};cur=b.push(IrOp::NativeTransferArtistic{kind,parameter},vec![(cur,OutPort::default())]);},_=>b.diag(CompileDiagnostic::coded(CompileCode::ColorPipelineUnavailable,DiagSeverity::Error,scope,"native bounded transfer-space artistic parameter is outside its catalog range"))}
            } else if matches!(
                fx.id.as_str(),
                "color.channel_mixer" | "color.black_and_white"
            ) && matches!(fx.kind,EffectKind::Unknown(tag)if tag.as_str()==fx.id.as_str())
            {
                let bw = fx.id.as_str() == "color.black_and_white";
                let paths: &[&str] = if bw {
                    &["params.wr", "params.wg", "params.wb"]
                } else {
                    &[
                        "params.rr",
                        "params.rg",
                        "params.rb",
                        "params.gr",
                        "params.gg",
                        "params.gb",
                        "params.br",
                        "params.bg",
                        "params.bb",
                    ]
                };
                let range = if bw { 0.0..=1.0 } else { -2.0..=2.0 };
                let values: Option<Vec<f32>> = paths
                    .iter()
                    .map(|path| match params.get(*path) {
                        Some(PropValue::Float(v)) if v.is_finite() && range.contains(v) => {
                            Some(*v as f32)
                        }
                        _ => None,
                    })
                    .collect();
                if let Some(v) = values {
                    let matrix = if bw {
                        [photonic_core::raster::adjust::normalized_gray_weights([v[0], v[1], v[2]]);
                            3]
                    } else {
                        [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]]
                    };
                    cur = b.push(
                        IrOp::NativeTransferMatrix { matrix },
                        vec![(cur, OutPort::default())],
                    );
                } else {
                    b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        "native transfer-space matrix parameter is outside its catalog range",
                    ));
                }
            } else if fx.id.as_str() == "util.outline"
                && matches!(fx.kind,EffectKind::Unknown(tag)if tag.as_str()=="util.outline")
            {
                let scalar = |path: &str, hi: f64| match params.get(path) {
                    Some(PropValue::Float(v)) if v.is_finite() && (0.0..=hi).contains(v) => {
                        Some(*v as f32)
                    }
                    _ => None,
                };
                match (
                    scalar("params.thickness", 64.0),
                    scalar("params.r", 1.0),
                    scalar("params.g", 1.0),
                    scalar("params.b", 1.0),
                    scalar("params.opacity", 1.0),
                ) {
                    (Some(thickness), Some(r), Some(g), Some(blue), Some(opacity)) => {
                        let color = crate::color::native::encoded_srgb_to_linear_ap1_display([
                            r as f64,
                            g as f64,
                            blue as f64,
                        ])
                        .map(|v| v as f32);
                        cur = b.push(
                            IrOp::NativeOutline {
                                thickness,
                                color,
                                opacity,
                                pixel_scale: [1.0; 2],
                            },
                            vec![(cur, OutPort::default())],
                        );
                    }
                    _ => b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        "native outline parameters must be finite and within their catalog ranges",
                    )),
                }
            } else if matches!(fx.id.as_str(), "color.hue_saturation" | "color.vibrance")
                && matches!(fx.kind,EffectKind::Unknown(tag)if tag.as_str()==fx.id.as_str())
            {
                let vibrance = fx.id.as_str() == "color.vibrance";
                let scalar = |path: &str, bound: f64| match params.get(path) {
                    Some(PropValue::Float(v)) if v.is_finite() && (-bound..=bound).contains(v) => {
                        Some(*v as f32)
                    }
                    _ => None,
                };
                let controls = if vibrance {
                    scalar("params.amount", 1.0).map(|v| [v, 0.0, 0.0])
                } else {
                    match (
                        scalar("params.hue", 180.0),
                        scalar("params.saturation", 1.0),
                        scalar("params.lightness", 1.0),
                    ) {
                        (Some(h), Some(s), Some(l)) => Some([h, s, l]),
                        _ => None,
                    }
                };
                match controls {
                    Some(controls) => {
                        cur = b.push(
                            IrOp::NativeLinearHsl { vibrance, controls },
                            vec![(cur, OutPort::default())],
                        )
                    }
                    None => b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        "native linear HSL parameters must be finite and within catalog ranges",
                    )),
                }
            } else if fx.id.as_str() == "blur.surface"
                && matches!(fx.kind,EffectKind::Unknown(tag)if tag.as_str()=="blur.surface")
            {
                match (params.get("params.radius"), params.get("params.threshold")) {
                    (Some(PropValue::Float(radius)), Some(PropValue::Float(threshold)))
                        if radius.is_finite()
                            && (0.0..=64.0).contains(radius)
                            && threshold.is_finite()
                            && (0.0..=1.0).contains(threshold) =>
                    {
                        cur = b.push(
                            IrOp::NativeSurfaceBlur {
                                radius: (*radius as f32).round() as u32,
                                threshold: *threshold as f32,
                                pixel_scale: [1.0; 2],
                            },
                            vec![(cur, OutPort::default())],
                        )
                    }
                    _ => b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        "native Surface Blur requires finite radius0..64 and threshold0..1",
                    )),
                }
            } else if fx.id.as_str() == "blur.lens"
                && matches!(fx.kind,EffectKind::Unknown(tag)if tag.as_str()=="blur.lens")
            {
                match params.get("params.radius") {
                    Some(PropValue::Float(radius))
                        if radius.is_finite() && (0.0..=500.0).contains(radius) =>
                    {
                        cur = b.push(
                            IrOp::NativeLensBlur {
                                radius: *radius as f32,
                                pixel_scale: [1.0; 2],
                            },
                            vec![(cur, OutPort::default())],
                        )
                    }
                    _ => b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        "native Lens Blur requires finite radius0..500",
                    )),
                }
            } else if matches!(
                fx.id.as_str(),
                "filter.median" | "repair.dust_and_scratches"
            ) && matches!(fx.kind,EffectKind::Unknown(tag)if tag.as_str()==fx.id.as_str())
            {
                let dust = fx.id.as_str() == "repair.dust_and_scratches";
                let max = if dust { 16.0 } else { 500.0 };
                let radius = match params.get("params.radius") {
                    Some(PropValue::Float(v)) if v.is_finite() && (0.0..=max).contains(v) => {
                        Some((*v as f32).round() as u32)
                    }
                    _ => None,
                };
                let threshold = if dust {
                    match params.get("params.threshold") {
                        Some(PropValue::Float(v)) if v.is_finite() && (0.0..=255.0).contains(v) => {
                            Some(*v as f32 as u8)
                        }
                        _ => None,
                    }
                } else {
                    Some(0)
                };
                match(radius,threshold){(Some(radius),Some(threshold))=>cur=b.push(IrOp::NativeRankFilter{kind:if dust{crate::graph::ir::NativeRankKind::DustScratches}else{crate::graph::ir::NativeRankKind::Median},radius,threshold,pixel_scale:[1.0;2]},vec![(cur,OutPort::default())]),_=>b.diag(CompileDiagnostic::coded(CompileCode::ColorPipelineUnavailable,DiagSeverity::Error,scope,"native Median/Dust & Scratches requires finite parameters within its catalog ranges"))}
            } else if fx.id.as_str() == "util.unpremultiply"
                && matches!(fx.kind,EffectKind::Unknown(tag)if tag.as_str()=="util.unpremultiply")
            {
                cur = b.push(IrOp::NativeUnpremultiply, vec![(cur, OutPort::default())]);
            } else if fx.id.as_str() == "util.alpha_view"
                && matches!(fx.kind,EffectKind::Unknown(tag)if tag.as_str()=="util.alpha_view")
            {
                match params.get("params.mode") {
                    Some(PropValue::Float(v)) if v.is_finite() && (0.0..=2.0).contains(v) => {
                        cur = b.push(
                            IrOp::NativeAlphaView {
                                mode: (*v as f32).round() as u8,
                            },
                            vec![(cur, OutPort::default())],
                        )
                    }
                    _ => b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        "native Alpha View mode must be finite in0..2",
                    )),
                }
            } else if fx.id.as_str() == "color.levels"
                && matches!(fx.kind,EffectKind::Unknown(tag)if tag.as_str()=="color.levels")
            {
                let paths = [
                    "params.in_black",
                    "params.in_white",
                    "params.gamma",
                    "params.out_black",
                    "params.out_white",
                ];
                let values: Option<Vec<f32>> = paths
                    .iter()
                    .enumerate()
                    .map(|(i, path)| match params.get(*path) {
                        Some(PropValue::Float(v))
                            if v.is_finite()
                                && (if i == 2 { 0.01..=9.99 } else { 0.0..=1.0 }).contains(v) =>
                        {
                            Some(*v as f32)
                        }
                        _ => None,
                    })
                    .collect();
                if let Some(v) = values {
                    cur = b.push(
                        IrOp::NativeTransferLevels {
                            black_white: [v[0], v[1]],
                            gamma: v[2],
                            output: [v[3], v[4]],
                        },
                        vec![(cur, OutPort::default())],
                    );
                } else {
                    b.diag(CompileDiagnostic::coded(CompileCode::ColorPipelineUnavailable,DiagSeverity::Error,scope,"native Levels requires finite unit input/output ranges and gamma .01..9.99"));
                }
            } else if fx.id.as_str() == "color.curves"
                && matches!(fx.kind,EffectKind::Unknown(tag)if tag.as_str()=="color.curves")
            {
                let paths = [
                    "params.contrast",
                    "params.p0x",
                    "params.p0y",
                    "params.p1x",
                    "params.p1y",
                    "params.p2x",
                    "params.p2y",
                    "params.p3x",
                    "params.p3y",
                    "params.p4x",
                    "params.p4y",
                ];
                let values: Option<Vec<f32>> = paths
                    .iter()
                    .enumerate()
                    .map(|(i, path)| match params.get(*path) {
                        Some(PropValue::Float(v))
                            if v.is_finite()
                                && (if i == 0 { -1.0..=1.0 } else { 0.0..=1.0 }).contains(v) =>
                        {
                            Some(*v as f32)
                        }
                        _ => None,
                    })
                    .collect();
                if let Some(v) = values {
                    cur = b.push(
                        IrOp::NativeTransferCurves {
                            knots: std::array::from_fn(|i| [v[1 + 2 * i], v[2 + 2 * i]]),
                            contrast: v[0],
                        },
                        vec![(cur, OutPort::default())],
                    );
                } else {
                    b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        "native catalog Curves requires finite unit knots and contrast -1..1",
                    ));
                }
            } else if fx.id.as_str() == "color.color_balance"
                && matches!(fx.kind,EffectKind::Unknown(tag)if tag.as_str()=="color.color_balance")
            {
                let paths = [
                    "params.shadows_r",
                    "params.shadows_g",
                    "params.shadows_b",
                    "params.midtones_r",
                    "params.midtones_g",
                    "params.midtones_b",
                    "params.highlights_r",
                    "params.highlights_g",
                    "params.highlights_b",
                    "params.preserve_luminosity",
                ];
                let values: Option<Vec<f32>> = paths
                    .iter()
                    .enumerate()
                    .map(|(i, path)| match params.get(*path) {
                        Some(PropValue::Float(v))
                            if v.is_finite()
                                && (if i == 9 { 0.0..=1.0 } else { -1.0..=1.0 }).contains(v) =>
                        {
                            Some(*v as f32)
                        }
                        _ => None,
                    })
                    .collect();
                if let Some(v) = values {
                    cur = b.push(
                        IrOp::NativeColorBalance {
                            shifts: [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]],
                            preserve: v[9] > 0.5,
                        },
                        vec![(cur, OutPort::default())],
                    );
                } else {
                    b.diag(CompileDiagnostic::coded(CompileCode::ColorPipelineUnavailable,DiagSeverity::Error,scope,"native Color Balance requires finite catalog-range shifts and preserve parameter"));
                }
            } else if fx.id.as_str() == "color.photo_filter"
                && matches!(fx.kind,EffectKind::Unknown(tag)if tag.as_str()=="color.photo_filter")
            {
                let scalar = |path: &str| match params.get(path) {
                    Some(PropValue::Float(v)) if v.is_finite() && (0.0..=1.0).contains(v) => {
                        Some(*v as f32)
                    }
                    _ => None,
                };
                match (
                    scalar("params.r"),
                    scalar("params.g"),
                    scalar("params.b"),
                    scalar("params.density"),
                    scalar("params.preserve_luminosity"),
                ) {
                    (Some(red), Some(green), Some(blue), Some(density), Some(preserve)) => {
                        cur = b.push(
                            IrOp::NativePhotoFilter {
                                color: [red, green, blue],
                                density,
                                preserve: preserve > 0.5,
                            },
                            vec![(cur, OutPort::default())],
                        );
                    }
                    _ => b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        "native Photo Filter requires finite unit-range parameters",
                    )),
                }
            } else if matches!(fx.id.as_str(), "color.desaturate" | "color.invert_raster")
                && matches!(fx.kind,EffectKind::Unknown(tag)if tag.as_str()==fx.id.as_str())
            {
                cur = b.push(
                    IrOp::NativeTransferArtistic {
                        kind: if fx.id.as_str() == "color.desaturate" {
                            super::ir::NativeTransferArtisticKind::Desaturate
                        } else {
                            super::ir::NativeTransferArtisticKind::Invert
                        },
                        parameter: 0.0,
                    },
                    vec![(cur, OutPort::default())],
                );
            } else if fx.id.as_str() == "util.drop_shadow"
                && matches!(fx.kind,EffectKind::Unknown(tag) if tag.as_str()=="util.drop_shadow")
            {
                let scalar = |path: &str, lo: f64, hi: f64| match params.get(path) {
                    Some(PropValue::Float(v)) if v.is_finite() && (lo..=hi).contains(v) => {
                        Some(*v as f32)
                    }
                    _ => None,
                };
                match(scalar("params.x",-200.0,200.0),scalar("params.y",-200.0,200.0),scalar("params.radius",0.0,64.0),scalar("params.r",0.0,1.0),scalar("params.g",0.0,1.0),scalar("params.b",0.0,1.0),scalar("params.opacity",0.0,1.0)){
                    (Some(x),Some(y),Some(radius),Some(r),Some(g),Some(blue),Some(opacity))=>{let color=crate::color::native::encoded_srgb_to_linear_ap1_display([r as f64,g as f64,blue as f64]).map(|v|v as f32);cur=b.push(IrOp::NativeDropShadow{offset:[x,y],radius:radius.round()as u32,color,opacity,pixel_scale:[1.0;2]},vec![(cur,OutPort::default())]);},
                    _=>b.diag(CompileDiagnostic::coded(CompileCode::ColorPipelineUnavailable,DiagSeverity::Error,scope,"native drop shadow parameters must be finite and within their catalog ranges")),
                }
            } else if fx.id.as_str() == "geo.perspective"
                && matches!(fx.kind,EffectKind::Unknown(tag) if tag.as_str()=="geo.perspective")
            {
                let paths = [
                    "params.tl_x",
                    "params.tl_y",
                    "params.tr_x",
                    "params.tr_y",
                    "params.br_x",
                    "params.br_y",
                    "params.bl_x",
                    "params.bl_y",
                ];
                let mut corners = [0.0f32; 8];
                let mut valid = true;
                for (i, path) in paths.iter().enumerate() {
                    match params.get(path) {
                        Some(PropValue::Float(v)) if v.is_finite() && (-1.0..=2.0).contains(v) => {
                            corners[i] = *v as f32
                        }
                        _ => valid = false,
                    }
                }
                let inverse = if valid {
                    super::ops::native_perspective_inverse(corners)
                } else {
                    None
                };
                match inverse {Some(inverse)=>cur=b.push(IrOp::NativePerspective{inverse},vec![(cur,OutPort::default())]),None=>b.diag(CompileDiagnostic::coded(CompileCode::ColorPipelineUnavailable,DiagSeverity::Error,scope,"native perspective requires finite -1..=2 corners forming a nondegenerate convex quadrilateral and a numerically safe projective inverse"))}
            } else if matches!(fx.kind,EffectKind::Unknown(tag) if tag.as_str()==fx.id.as_str())
                && matches!(
                    fx.id.as_str(),
                    "stylize.emboss" | "stylize.find_edges" | "stylize.chromatic_aberration"
                )
            {
                let kind = match fx.id.as_str() {
                    "stylize.emboss" => NativeStylizeKind::Emboss,
                    "stylize.find_edges" => NativeStylizeKind::FindEdges,
                    _ => NativeStylizeKind::ChromaticAberration,
                };
                let amount = if kind == NativeStylizeKind::ChromaticAberration {
                    match params.get("params.amount") {
                        Some(PropValue::Float(v))
                            if v.is_finite() && (-64.0..=64.0).contains(v) =>
                        {
                            Some(*v as f32)
                        }
                        _ => None,
                    }
                } else {
                    Some(0.0)
                };
                match amount {Some(amount)=>cur=b.push(IrOp::NativeStylize{kind,amount,pixel_scale:[1.0;2]},vec![(cur,OutPort::default())]),
                    None=>b.diag(CompileDiagnostic::coded(CompileCode::ColorPipelineUnavailable,DiagSeverity::Error,scope,"native chromatic aberration requires finite amount in -64..=64 authored pixels")),}
            } else if matches!(fx.kind,EffectKind::Unknown(tag) if tag.as_str()==fx.id.as_str())
                && matches!(
                    fx.id.as_str(),
                    "sharpen.smart" | "filter.high_pass" | "color.clarity"
                )
            {
                let scalar = |path: &str, lo: f64, hi: f64| match params.get(path) {
                    Some(PropValue::Float(v)) if v.is_finite() && (lo..=hi).contains(v) => {
                        Some(*v as f32)
                    }
                    _ => None,
                };
                let (kind, amount, radius, threshold) = match fx.id.as_str() {
                    "sharpen.smart" => (
                        NativeDetailKind::SmartSharpen,
                        scalar("params.amount", 0.0, 10.0),
                        scalar("params.radius", 0.0, 100.0),
                        scalar("params.threshold", 0.0, 255.0).map(|v| v / 255.0),
                    ),
                    "filter.high_pass" => (
                        NativeDetailKind::HighPass,
                        Some(1.0),
                        scalar("params.radius", 0.0, 500.0),
                        Some(0.0),
                    ),
                    _ => (
                        NativeDetailKind::Clarity,
                        scalar("params.amount", -1.0, 1.0),
                        Some(0.0),
                        Some(0.0),
                    ),
                };
                match (amount, radius, threshold) {
                    (Some(amount), Some(sigma), Some(threshold)) => {
                        cur = b.push(
                            IrOp::NativeDetail {
                                kind,
                                amount,
                                sigma,
                                threshold,
                                pixel_scale: [1.0; 2],
                            },
                            vec![(cur, OutPort::default())],
                        )
                    }
                    _ => b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        "native detail parameters must be finite and within catalog ranges",
                    )),
                }
            } else if matches!(fx.kind, EffectKind::Unknown(tag) if tag.as_str()==fx.id.as_str())
                && matches!(fx.id.as_str(), "geo.pinch" | "geo.spherize" | "geo.ripple")
            {
                let kind = match fx.id.as_str() {
                    "geo.pinch" => NativeWarpKind::Pinch,
                    "geo.spherize" => NativeWarpKind::Spherize,
                    _ => NativeWarpKind::Ripple,
                };
                let path = if kind == NativeWarpKind::Ripple {
                    "params.amplitude"
                } else {
                    "params.amount"
                };
                let max = if kind == NativeWarpKind::Ripple {
                    64.0
                } else {
                    1.0
                };
                let amount = match params.get(path) {
                    Some(PropValue::Float(v)) if v.is_finite() && (-max..=max).contains(v) => {
                        Some(*v as f32)
                    }
                    _ => None,
                };
                let wavelength = if kind == NativeWarpKind::Ripple {
                    match params.get("params.wavelength") {
                        Some(PropValue::Float(v)) if v.is_finite() && (1.0..=512.0).contains(v) => {
                            Some(*v as f32)
                        }
                        _ => None,
                    }
                } else {
                    Some(1.0)
                };
                match(amount,wavelength){
                    (Some(amount),Some(wavelength))=>cur=b.push(IrOp::NativeGeometricWarp{kind,amount,wavelength,pixel_scale:[1.0;2]},vec![(cur,OutPort::default())]),
                    _=>b.diag(CompileDiagnostic::coded(CompileCode::ColorPipelineUnavailable,DiagSeverity::Error,scope,"native geometric warp parameters must be finite and within their catalog ranges")),
                }
            } else if matches!(fx.kind, EffectKind::Unknown(tag) if tag.as_str() == "blur.box")
                && fx.id.as_str() == "blur.box"
            {
                match params.get("params.radius") {
                    Some(PropValue::Float(v)) if v.is_finite() && (0.0..=500.0).contains(v) => {
                        cur = b.push(
                            IrOp::NativeBoxBlur {
                                radius: [v.round() as u32; 2],
                            },
                            vec![(cur, OutPort::default())],
                        );
                    }
                    _ => b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        "native box blur requires a finite radius in 0..=500 authored pixels",
                    )),
                }
            } else if matches!(fx.kind, EffectKind::Unknown(tag) if tag.as_str() == "stylize.mosaic")
                && fx.id.as_str() == "stylize.mosaic"
            {
                match params.get("params.block") {
                    Some(PropValue::Float(v)) if v.is_finite() && (1.0..=256.0).contains(v) => {
                        cur = b.push(
                            IrOp::NativeMosaic {
                                block: [v.round() as u32; 2],
                            },
                            vec![(cur, OutPort::default())],
                        );
                    }
                    _ => b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        "native mosaic requires finite block size in 1..=256 authored pixels",
                    )),
                }
            } else if matches!(fx.kind, EffectKind::Unknown(tag) if tag.as_str() == "blur.motion")
                && fx.id.as_str() == "blur.motion"
            {
                match native_motion_blur_op(&params) {
                    Ok(op) => cur = b.push(op, vec![(cur, OutPort::default())]),
                    Err(message) => b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        message,
                    )),
                }
            } else if fx.kind == EffectKind::Invert
                && (fx.id.as_str().is_empty() || fx.id.as_str() == "color.invert")
            {
                cur = b.push(IrOp::NativeInvert, vec![(cur, OutPort::default())]);
            } else if fx.kind == EffectKind::MaskShapeGen
                && (fx.id.as_str().is_empty() || fx.id.as_str() == "util.mask_shape")
            {
                match native_shape_op(&params, false) {
                    Ok(op) => cur = b.push(op, vec![]),
                    Err(message) => b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        message,
                    )),
                }
            } else if fx.kind == EffectKind::Glow
                && (fx.id.as_str().is_empty() || fx.id.as_str() == "stylize.glow")
            {
                match native_glow_op(&params) {
                    Ok(op) => cur = b.push(op, vec![(cur, OutPort::default())]),
                    Err(message) => b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        message,
                    )),
                }
            } else if fx.kind == EffectKind::ChromaKey
                && (fx.id.as_str().is_empty() || fx.id.as_str() == "key.chroma")
            {
                match native_chroma_key_op(&params) {
                    Ok(op) => cur = b.push(op, vec![(cur, OutPort::default())]),
                    Err(message) => b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        message,
                    )),
                }
            } else if fx.kind == EffectKind::LumaKey
                && (fx.id.as_str().is_empty() || fx.id.as_str() == "key.luma")
            {
                match native_luma_key_op(&params) {
                    Ok(op) => cur = b.push(op, vec![(cur, OutPort::default())]),
                    Err(message) => b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        message,
                    )),
                }
            } else if fx.kind == EffectKind::Blur
                && (fx.id.as_str().is_empty() || fx.id.as_str() == "blur.gaussian")
                && sigma.is_some()
            {
                cur = b.push(
                    IrOp::NativeGaussianBlur {
                        sigma: sigma.unwrap(),
                    },
                    vec![(cur, OutPort::default())],
                );
            } else if (fx.kind == EffectKind::Sharpen
                && (fx.id.as_str().is_empty() || fx.id.as_str() == "sharpen.unsharp"))
                || (fx.id.as_str() == "sharpen.unsharp_raster"
                    && matches!(fx.kind,EffectKind::Unknown(tag) if tag.as_str()=="sharpen.unsharp_raster"))
            {
                let amount = params.get("params.amount").and_then(|v| match v {
                    PropValue::Float(v) if v.is_finite() && (0.0..=10.0).contains(v) => {
                        Some(*v as f32)
                    }
                    _ => None,
                });
                match (amount, sigma.filter(|s| *s <= 100.0)) {
                    (Some(amount), Some(sigma)) => {
                        cur = b.push(
                            IrOp::NativeUnsharp { amount, sigma },
                            vec![(cur, OutPort::default())],
                        )
                    }
                    _ => b.diag(CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        scope,
                        "native unsharp requires amount 0..=10 and sigma 0..=100",
                    )),
                }
            } else {
                b.diag(CompileDiagnostic::coded(CompileCode::ColorPipelineUnavailable, DiagSeverity::Error,
                    scope, format!("native effect {} at {:?} is unavailable or has invalid parameters; only explicitly supported scene-linear operators and validated parameters can render", fx.id.as_str(), grade_owner)));
            }
            continue;
        }
        // Deflicker's gain is the verdict of a whole-range analysis job, not a
        // keyframe-resolvable param, so it is appended here rather than seeded
        // from the manifest. Absent a measurement the entries stay absent and
        // `eval_cpu` falls back to unity — an exact pass-through.
        if fx.kind == EffectKind::Deflicker {
            if let Some(gain) = scope.and_then(|c| b.deflicker.and_then(|d| d.gain(c, dt))) {
                params.entries.push((
                    PropPath::new("params.gain_r"),
                    PropValue::Float(gain[0] as f64),
                ));
                params.entries.push((
                    PropPath::new("params.gain_g"),
                    PropValue::Float(gain[1] as f64),
                ));
                params.entries.push((
                    PropPath::new("params.gain_b"),
                    PropValue::Float(gain[2] as f64),
                ));
            }
            // Rolling bands ride along on the same effect: one user-facing
            // "Deflicker", two corrections, because they are two symptoms of the
            // same shooting problem. Absent a detection these stay absent.
            if let Some(band) = scope.and_then(|c| b.deflicker.and_then(|d| d.band(c, dt))) {
                params.entries.push((
                    PropPath::new("params.band_cycles"),
                    PropValue::Float(band.cycles as f64),
                ));
                params.entries.push((
                    PropPath::new("params.band_amp"),
                    PropValue::Float(band.amplitude as f64),
                ));
                params.entries.push((
                    PropPath::new("params.band_phase"),
                    PropValue::Float(band.phase as f64),
                ));
            }
        }
        cur = b.push(
            IrOp::Effect { kind, params },
            vec![(cur, OutPort::default())],
        );
    }
    if let Some(grade) = grade {
        cur = apply_grade(b, grade, cur, dt, grade_owner);
    }
    cur
}

/// Resolve `grade` at `tick` and emit a `Grade` IR op carrying the resolved stack
/// (07 §2/§3), or return `input` unchanged when the grade is bypassed / empty /
/// fully inert. Shared by clip grades (step 2) and graph `Grade`/`Lut` nodes.
#[allow(clippy::too_many_arguments)]
fn apply_member_group_grade(
    b: &mut Builder<'_>,
    grade: &Grade,
    input: IrNodeId,
    dt: Tick,
    clip: ClipId,
    group: timeline::GroupId,
    pre: bool,
) -> IrNodeId {
    let old = (
        b.inspection_stage,
        b.inspection_member_clip,
        b.clip_output_inspection,
    );
    b.inspection_stage = if pre {
        timeline::GradeInspectionStage::GroupPre { id: group }
    } else {
        timeline::GradeInspectionStage::GroupPost { id: group }
    };
    b.inspection_member_clip = Some(clip);
    b.clip_output_inspection = false;
    let owner = if pre {
        VfxOwner::GroupPre(group)
    } else {
        VfxOwner::GroupPost(group)
    };
    let output = apply_grade(b, grade, input, dt, Some(owner));
    (
        b.inspection_stage,
        b.inspection_member_clip,
        b.clip_output_inspection,
    ) = old;
    output
}

fn inspection_clip(b: &Builder<'_>, owner: Option<VfxOwner>) -> Option<ClipId> {
    match owner {
        Some(VfxOwner::Clip(id)) => Some(id),
        Some(VfxOwner::GroupPre(id))
            if b.inspection_stage == (timeline::GradeInspectionStage::GroupPre { id }) =>
        {
            b.inspection_member_clip
        }
        Some(VfxOwner::GroupPost(id))
            if b.inspection_stage == (timeline::GradeInspectionStage::GroupPost { id }) =>
        {
            b.inspection_member_clip
        }
        _ => None,
    }
}

/// Resolve the independent/shared look with identical provenance in both color domains.
fn apply_clip_look(
    b: &mut Builder<'_>,
    project: &TimelineProject,
    clip: &Clip,
    input: IrNodeId,
    dt: Tick,
) -> IrNodeId {
    let mut cur = input;
    if let Some(look) = clip.look.as_ref() {
        let (grade, stage) = match look {
            timeline::ClipLook::Local(grade) => (
                Some(grade.as_ref()),
                photonic_render::grade::GradeStage::LocalLook,
            ),
            timeline::ClipLook::Shared(id) => (
                project.shared_looks.get(id).map(|look| &look.grade),
                photonic_render::grade::GradeStage::SharedLook { id: *id },
            ),
        };
        if let Some(grade) = grade {
            let first_diagnostic = b.diagnostics.len();
            let inspecting = b.clip_output_inspection;
            let previous_stage = b.inspection_stage;
            b.inspection_stage = match look {
                timeline::ClipLook::Local(_) => timeline::GradeInspectionStage::LocalLook,
                timeline::ClipLook::Shared(id) => {
                    timeline::GradeInspectionStage::SharedLook { id: *id }
                }
            };
            b.clip_output_inspection = false;
            cur = apply_grade(b, grade, cur, dt, Some(VfxOwner::Clip(clip.id)));
            b.clip_output_inspection = inspecting;
            b.inspection_stage = previous_stage;
            for diagnostic in &mut b.diagnostics[first_diagnostic..] {
                if let Some(grade) = diagnostic.grade.as_mut() {
                    grade.stage = Some(stage.clone());
                    diagnostic.message = grade.to_string();
                } else {
                    let label = match &stage {
                        photonic_render::grade::GradeStage::LocalLook => {
                            "Independent look".to_owned()
                        }
                        photonic_render::grade::GradeStage::SharedLook { id } => {
                            format!("Shared look {id}")
                        }
                    };
                    diagnostic.message =
                        format!("{label} on clip {}: {}", clip.id, diagnostic.message);
                }
            }
        } else if let timeline::ClipLook::Shared(id) = look {
            let diagnostic = photonic_render::grade::GradeDiagnostic {
                op: timeline::GradeOpId::nil(),
                sequence_path: b.sequence_path.clone(),
                graph_node: None,
                owner: Some(VfxOwner::Clip(clip.id)),
                stage: Some(photonic_render::grade::GradeStage::SharedLook { id: *id }),
                issue: photonic_render::grade::GradeIssue::MissingSharedLook(*id),
            };
            let mut compiled = CompileDiagnostic::coded(
                CompileCode::GradeUnresolved,
                DiagSeverity::Error,
                Some(clip.id),
                diagnostic.to_string(),
            );
            compiled.grade = Some(diagnostic);
            b.diag(compiled);
        }
    }
    cur
}

fn apply_grade(
    b: &mut Builder<'_>,
    grade: &Grade,
    input: IrNodeId,
    tick: Tick,
    owner: Option<VfxOwner>,
) -> IrNodeId {
    apply_grade_at_node(b, grade, input, tick, owner, None)
}

fn apply_grade_at_node(
    b: &mut Builder<'_>,
    grade: &Grade,
    input: IrNodeId,
    tick: Tick,
    owner: Option<VfxOwner>,
    graph_node: Option<u32>,
) -> IrNodeId {
    if grade.bypass {
        return input;
    }
    if let Some(graph) = &grade.graph {
        if let Err(error) = graph.validate(&grade.ops) {
            b.diag(CompileDiagnostic::coded(
                CompileCode::GradeUnresolved,
                DiagSeverity::Error,
                match owner {
                    Some(VfxOwner::Clip(id)) => Some(id),
                    _ => None,
                },
                error,
            ));
            return input;
        }
        fn lower(
            b: &mut Builder<'_>,
            grade: &Grade,
            graph: &GradeGraph,
            node: u32,
            input: IrNodeId,
            tick: Tick,
            owner: Option<VfxOwner>,
            memo: &mut HashMap<u32, IrNodeId>,
        ) -> IrNodeId {
            if let Some(&cached) = memo.get(&node) {
                return cached;
            }
            let result = match &graph.nodes[&node] {
                GradeGraphNode::Input => input,
                GradeGraphNode::Corrector {
                    input: upstream,
                    op,
                    ..
                } => {
                    let upstream = lower(b, grade, graph, *upstream, input, tick, owner, memo);
                    let corrector = grade
                        .ops
                        .iter()
                        .find(|candidate| candidate.id == *op)
                        .expect("validated grading graph");
                    let single = Grade {
                        ops: vec![corrector.clone()],
                        bypass: false,
                        graph: None,
                    };
                    apply_grade_at_node(b, &single, upstream, tick, owner, Some(node))
                }
                GradeGraphNode::AnimatedBezierMatte {
                    input: upstream,
                    track,
                    ..
                } => {
                    let upstream = lower(b, grade, graph, *upstream, input, tick, owner, memo);
                    match track.sample(tick) {
                        Ok(window) => b.push(
                            IrOp::BezierMatte { window },
                            vec![(upstream, OutPort::default())],
                        ),
                        Err(error) => {
                            let diagnostic = photonic_render::grade::GradeDiagnostic {
                                op: timeline::GradeOpId::nil(),
                                sequence_path: b.sequence_path.clone(),
                                graph_node: Some(node),
                                owner,
                                stage: None,
                                issue: photonic_render::grade::GradeIssue::InvalidWindowAnimation(
                                    error.into(),
                                ),
                            };
                            let mut compiled = CompileDiagnostic::coded(
                                CompileCode::GradeUnresolved,
                                DiagSeverity::Error,
                                match owner {
                                    Some(VfxOwner::Clip(id)) => Some(id),
                                    _ => None,
                                },
                                diagnostic.to_string(),
                            );
                            compiled.grade = Some(diagnostic);
                            b.diag(compiled);
                            b.push(IrOp::GradeMatteConstant { weight: 0.0 }, vec![])
                        }
                    }
                }
                GradeGraphNode::BezierMatte {
                    input: upstream,
                    window,
                    ..
                } => {
                    let upstream = lower(b, grade, graph, *upstream, input, tick, owner, memo);
                    b.push(
                        IrOp::BezierMatte {
                            window: window.clone(),
                        },
                        vec![(upstream, OutPort::default())],
                    )
                }
                GradeGraphNode::WindowMatte {
                    input: upstream,
                    op,
                    ..
                } => {
                    let upstream = lower(b, grade, graph, *upstream, input, tick, owner, memo);
                    let corrector = grade
                        .ops
                        .iter()
                        .find(|candidate| candidate.id == *op)
                        .expect("validated window source");
                    if !corrector.enabled {
                        b.push(IrOp::GradeMatteConstant { weight: 0.0 }, vec![])
                    } else {
                        match photonic_render::grade::resolve_window_matte(corrector, tick) {
                            Ok(Some(mask)) => b.push(
                                IrOp::WindowMatte { mask },
                                vec![(upstream, OutPort::default())],
                            ),
                            Ok(None) => b.push(IrOp::GradeMatteConstant { weight: 1.0 }, vec![]),
                            Err(error) => {
                                let diagnostic = photonic_render::grade::GradeDiagnostic {
                                    op: *op,
                                    sequence_path: b.sequence_path.clone(),
                                    graph_node: Some(node),
                                    owner,
                                    stage: None,
                                    issue:
                                        photonic_render::grade::GradeIssue::InvalidWindowAnimation(
                                            error.into(),
                                        ),
                                };
                                let mut compiled = CompileDiagnostic::coded(
                                    CompileCode::GradeUnresolved,
                                    DiagSeverity::Error,
                                    match owner {
                                        Some(VfxOwner::Clip(id)) => Some(id),
                                        _ => None,
                                    },
                                    diagnostic.to_string(),
                                );
                                compiled.grade = Some(diagnostic);
                                b.diag(compiled);
                                b.push(IrOp::GradeMatteConstant { weight: 0.0 }, vec![])
                            }
                        }
                    }
                }
                GradeGraphNode::QualifierMatte {
                    input: upstream,
                    op,
                    ..
                } => {
                    let upstream = lower(b, grade, graph, *upstream, input, tick, owner, memo);
                    let corrector = grade
                        .ops
                        .iter()
                        .find(|candidate| candidate.id == *op)
                        .expect("validated qualifier source");
                    let single = Grade {
                        ops: vec![corrector.clone()],
                        bypass: false,
                        graph: None,
                    };
                    let (resolved, diagnostics) =
                        photonic_render::grade::resolve_with_ids_and_native_policy(
                            &single,
                            tick,
                            |asset| b.luts.and_then(|provider| provider.lut(asset)),
                            b.working_color_domain == WorkingColorDomain::SceneLinearAcescg,
                        );
                    for mut diagnostic in diagnostics {
                        diagnostic.owner = owner;
                        diagnostic.sequence_path = b.sequence_path.clone();
                        diagnostic.graph_node = Some(node);
                        let mut compiled = CompileDiagnostic::coded(
                            CompileCode::GradeUnresolved,
                            DiagSeverity::Error,
                            match owner {
                                Some(VfxOwner::Clip(id)) => Some(id),
                                _ => None,
                            },
                            diagnostic.to_string(),
                        );
                        compiled.grade = Some(diagnostic);
                        b.diag(compiled);
                    }
                    match resolved.into_iter().next() {
                        Some((_, resolved)) => {
                            if let photonic_render::grade::ResolvedGradePayload::HslQualifier(
                                qualifier,
                            ) = resolved.payload
                            {
                                let native =
                                    b.working_color_domain == WorkingColorDomain::SceneLinearAcescg;
                                if native && b.sequence_path.len() == 1 {
                                    if let Some(clip) = inspection_clip(b, owner) {
                                        let inspection = NativeQualifierInspection {
                                            spatial: corrector.qualifier_spatial,
                                            clip,
                                            op: *op,
                                            input: upstream,
                                            qualifier: qualifier.clone(),
                                            mask: resolved.mask,
                                        };
                                        b.native_stage_grade_inputs.push(NativeStageGradeInput {
                                            clip,
                                            stage: b.inspection_stage,
                                            node: Some(node),
                                            op: *op,
                                            input: upstream,
                                            corrector: false,
                                            qualifier: Some(inspection.clone()),
                                        });
                                        if b.clip_output_inspection {
                                            b.native_graph_qualifier_inspections
                                                .push((node, inspection));
                                        }
                                    }
                                }
                                let upstream = if native {
                                    b.push(IrOp::NativeAcescct { direction: photonic_render::native_transfer::AcescctDirection::Encode }, vec![(upstream, OutPort::default())])
                                } else {
                                    upstream
                                };
                                let key = b.push(
                                    IrOp::QualifierMatte {
                                        qualifier,
                                        mask: resolved.mask,
                                        native,
                                    },
                                    vec![(upstream, OutPort::default())],
                                );
                                if let Some(spatial) =
                                    corrector.qualifier_spatial.filter(|s| !s.is_neutral())
                                {
                                    b.push(
                                        IrOp::GradeMatteRefine {
                                            refinement: spatial.refinement(),
                                        },
                                        vec![(key, OutPort::default())],
                                    )
                                } else {
                                    key
                                }
                            } else {
                                unreachable!("validated qualifier source")
                            }
                        }
                        None => b.push(IrOp::GradeMatteConstant { weight: 0.0 }, vec![]),
                    }
                }
                GradeGraphNode::MatteRefine {
                    input: upstream,
                    refinement,
                    ..
                } => {
                    let upstream = lower(b, grade, graph, *upstream, input, tick, owner, memo);
                    if refinement.is_neutral() {
                        upstream
                    } else {
                        b.push(
                            IrOp::GradeMatteRefine {
                                refinement: *refinement,
                            },
                            vec![(upstream, OutPort::default())],
                        )
                    }
                }
                GradeGraphNode::KeyMixer {
                    top, bottom, mode, ..
                } => {
                    let top = lower(b, grade, graph, *top, input, tick, owner, memo);
                    let bottom = lower(b, grade, graph, *bottom, input, tick, owner, memo);
                    b.push(
                        IrOp::GradeKeyMix { mode: *mode },
                        vec![(top, OutPort::default()), (bottom, OutPort::default())],
                    )
                }
                GradeGraphNode::MatteApply {
                    original,
                    corrected,
                    matte,
                    ..
                } => {
                    let corrected = lower(b, grade, graph, *corrected, input, tick, owner, memo);
                    let original = lower(b, grade, graph, *original, input, tick, owner, memo);
                    let matte = lower(b, grade, graph, *matte, input, tick, owner, memo);
                    b.push(
                        IrOp::GradeMatteApply,
                        vec![
                            (corrected, OutPort::default()),
                            (original, OutPort::default()),
                            (matte, OutPort::default()),
                        ],
                    )
                }
                GradeGraphNode::LayerMixer {
                    top,
                    bottom,
                    opacity,
                    ..
                } => {
                    let top = lower(b, grade, graph, *top, input, tick, owner, memo);
                    let bottom = lower(b, grade, graph, *bottom, input, tick, owner, memo);
                    b.push(
                        if b.working_color_domain == WorkingColorDomain::SceneLinearAcescg {
                            IrOp::GradeLayerMix { opacity: *opacity }
                        } else {
                            IrOp::Merge {
                                mode: BlendMode::Normal,
                                opacity: *opacity,
                            }
                        },
                        vec![(top, OutPort::default()), (bottom, OutPort::default())],
                    )
                }
                GradeGraphNode::Output { input: upstream } => {
                    lower(b, grade, graph, *upstream, input, tick, owner, memo)
                }
            };
            if b.working_color_domain == WorkingColorDomain::SceneLinearAcescg
                && b.sequence_path.len() == 1
            {
                if let Some(clip) = inspection_clip(b, owner) {
                    b.native_stage_graph_outputs.push(GradeGraphStageOutput {
                        clip,
                        stage: b.inspection_stage,
                        node,
                        image: graph.nodes[&node].output_type()
                            == timeline::GradeGraphPortType::Image,
                        output: result,
                    });
                }
            }
            if b.working_color_domain == WorkingColorDomain::SceneLinearAcescg
                && b.sequence_path.len() == 1
                && b.clip_output_inspection
            {
                if let Some(clip) = inspection_clip(b, owner) {
                    match graph.nodes[&node].output_type() {
                        photonic_core::timeline::GradeGraphPortType::Matte => {
                            b.native_graph_mattes.push((clip, node, result))
                        }
                        photonic_core::timeline::GradeGraphPortType::Image => {
                            b.native_graph_images.push((clip, node, result))
                        }
                    }
                }
            }
            memo.insert(node, result);
            result
        }
        return lower(
            b,
            grade,
            graph,
            graph.output,
            input,
            tick,
            owner,
            &mut HashMap::new(),
        );
    }
    let (ops, diagnostics) = photonic_render::grade::resolve_with_ids_and_native_policy(
        grade,
        tick,
        |asset| b.luts.and_then(|provider| provider.lut(asset)),
        b.working_color_domain == WorkingColorDomain::SceneLinearAcescg,
    );
    for mut diagnostic in diagnostics {
        diagnostic.owner = owner;
        diagnostic.sequence_path = b.sequence_path.clone();
        diagnostic.graph_node = graph_node;
        let clip = match owner {
            Some(VfxOwner::Clip(id)) => Some(id),
            _ => None,
        };
        let mut compiled = CompileDiagnostic::coded(
            CompileCode::GradeUnresolved,
            DiagSeverity::Error,
            clip,
            diagnostic.to_string(),
        );
        compiled.grade = Some(diagnostic);
        b.diag(compiled);
    }
    if b.working_color_domain == WorkingColorDomain::SceneLinearAcescg {
        let mut current = input;
        for (op_id, op) in ops {
            let mask = op.mask;
            let mask_valid = mask
                .as_ref()
                .is_none_or(|mask| photonic_render::grade_gpu::validate_native_mask(mask).is_ok());
            if let photonic_render::grade::ResolvedGradePayload::Lut3d(lut) = &op.payload {
                if mask_valid && photonic_render::grade_gpu::validate_native_lut(lut).is_ok() {
                    let asset =
                        grade.ops.iter().find(|op| op.id == op_id).and_then(|op| {
                            match op.params.base {
                                timeline::GradeOpParams::Lut3d { asset, .. } => Some(asset),
                                _ => None,
                            }
                        });
                    let binding = asset
                        .and_then(|asset| b.luts.and_then(|provider| provider.native_lut(asset)));
                    if let Some(binding) = binding {
                        if matches!(
                            binding.space,
                            timeline::color::NativeLutSpace::Acescg
                                | timeline::color::NativeLutSpace::Acescct
                        ) {
                            let log = binding.space == timeline::color::NativeLutSpace::Acescct;
                            if log {
                                current = b.push(IrOp::NativeAcescct { direction: photonic_render::native_transfer::AcescctDirection::Encode }, vec![(current, OutPort::default())]);
                            }
                            let original = current;
                            current = b.push(
                                IrOp::NativeLut3d { lut: lut.clone() },
                                vec![(current, OutPort::default())],
                            );
                            if let Some(mask) = mask {
                                current = b.push(
                                    IrOp::NativeMaskMix { mask },
                                    vec![
                                        (current, OutPort::default()),
                                        (original, OutPort::default()),
                                    ],
                                );
                            }
                            if log {
                                current = b.push(IrOp::NativeAcescct { direction: photonic_render::native_transfer::AcescctDirection::Decode }, vec![(current, OutPort::default())]);
                            }
                            continue;
                        }
                    }
                }
            }
            if let photonic_render::grade::ResolvedGradePayload::Curves(ref curves) = op.payload {
                let authored = grade.ops.iter().find(|op| op.id == op_id).and_then(|op| {
                    if let timeline::GradeOpParams::Curves {
                        master,
                        red,
                        green,
                        blue,
                        hue_vs_hue,
                        hue_vs_sat,
                        hue_vs_luma,
                        luma_vs_sat,
                        sat_vs_sat,
                    } = &op.params.base
                    {
                        Some([
                            master,
                            red,
                            green,
                            blue,
                            hue_vs_hue,
                            hue_vs_sat,
                            hue_vs_luma,
                            luma_vs_sat,
                            sat_vs_sat,
                        ])
                    } else {
                        None
                    }
                });
                let authored_valid = authored.is_some_and(|channels| {
                    channels
                        .iter()
                        .all(|points| points.iter().all(|(x, y)| x.is_finite() && y.is_finite()))
                });
                let authored_identity = authored.is_some_and(|channels| {
                    channels.iter().take(4).all(|points| {
                        points.len() < 2
                            || (points
                                .iter()
                                .all(|(x, y)| x == y && (0.0..=1.0).contains(x))
                                && points.contains(&(0.0, 0.0))
                                && points.contains(&(1.0, 1.0)))
                    }) && channels.iter().skip(4).all(|points| {
                        points.is_empty()
                            || (points
                                .iter()
                                .all(|(x, y)| (0.0..=1.0).contains(x) && *y == 0.5)
                                && points.contains(&(0.0, 0.5))
                                && points.contains(&(1.0, 0.5)))
                    })
                });
                if mask_valid
                    && authored_valid
                    && photonic_render::grade_gpu::validate_native_curves(curves).is_ok()
                {
                    if b.sequence_path.len() == 1 {
                        if let Some(clip) = inspection_clip(b, owner) {
                            b.native_stage_grade_inputs.push(NativeStageGradeInput {
                                clip,
                                stage: b.inspection_stage,
                                node: graph_node,
                                op: op_id,
                                input: current,
                                corrector: true,
                                qualifier: None,
                            });
                            if b.clip_output_inspection {
                                b.native_curve_inputs.push((clip, op_id, current));
                                if let Some(node) = graph_node {
                                    b.native_graph_curve_inputs
                                        .push((clip, node, op_id, current));
                                }
                            }
                        }
                    }
                    let identity = photonic_render::grade::curve_lut(&[]);
                    if !authored_identity
                        && ([&curves.master, &curves.red, &curves.green, &curves.blue]
                            .iter()
                            .any(|table| **table != identity)
                            || [
                                &curves.hue_vs_hue,
                                &curves.hue_vs_sat,
                                &curves.hue_vs_luma,
                                &curves.luma_vs_sat,
                                &curves.sat_vs_sat,
                            ]
                            .iter()
                            .any(|table| table.is_some()))
                    {
                        current = b.push(
                            IrOp::NativeAcescct {
                                direction:
                                    photonic_render::native_transfer::AcescctDirection::Encode,
                            },
                            vec![(current, OutPort::default())],
                        );
                        let original = current;
                        current = b.push(
                            IrOp::NativeLogCurves {
                                curves: curves.clone(),
                            },
                            vec![(current, OutPort::default())],
                        );
                        if let Some(mask) = mask {
                            current = b.push(
                                IrOp::NativeMaskMix { mask },
                                vec![
                                    (current, OutPort::default()),
                                    (original, OutPort::default()),
                                ],
                            );
                        }
                        current = b.push(
                            IrOp::NativeAcescct {
                                direction:
                                    photonic_render::native_transfer::AcescctDirection::Decode,
                            },
                            vec![(current, OutPort::default())],
                        );
                    }
                    continue;
                }
            }
            if let photonic_render::grade::ResolvedGradePayload::HslQualifier(ref qualifier) =
                op.payload
            {
                if mask_valid
                    && photonic_render::native_transfer::validate_native_qualifier(qualifier)
                        .is_ok()
                {
                    let spatial = grade
                        .ops
                        .iter()
                        .find(|op| op.id == op_id)
                        .and_then(|op| op.qualifier_spatial)
                        .filter(|s| !s.is_neutral());
                    if b.sequence_path.len() == 1 {
                        if let Some(clip) = inspection_clip(b, owner) {
                            let inspection = NativeQualifierInspection {
                                spatial,
                                clip,
                                op: op_id,
                                input: current,
                                qualifier: qualifier.clone(),
                                mask,
                            };
                            b.native_stage_grade_inputs.push(NativeStageGradeInput {
                                clip,
                                stage: b.inspection_stage,
                                node: graph_node,
                                op: op_id,
                                input: current,
                                corrector: true,
                                qualifier: Some(inspection.clone()),
                            });
                            if b.clip_output_inspection {
                                if let Some(node) = graph_node {
                                    b.native_graph_qualifier_inspections
                                        .push((node, inspection.clone()));
                                }
                                b.native_qualifier_inspections.push(inspection);
                            }
                        }
                    }
                    let cdl = qualifier.correction;
                    if cdl.slope != [1.0; 3]
                        || cdl.offset != [0.0; 3]
                        || cdl.power != [1.0; 3]
                        || cdl.sat != 1.0
                    {
                        let scene_original = current;
                        current = b.push(
                            IrOp::NativeAcescct {
                                direction:
                                    photonic_render::native_transfer::AcescctDirection::Encode,
                            },
                            vec![(current, OutPort::default())],
                        );
                        let original = current;
                        if let Some(spatial) = spatial {
                            let key = b.push(
                                IrOp::QualifierMatte {
                                    qualifier: qualifier.clone(),
                                    mask,
                                    native: true,
                                },
                                vec![(original, OutPort::default())],
                            );
                            let key = b.push(
                                IrOp::GradeMatteRefine {
                                    refinement: spatial.refinement(),
                                },
                                vec![(key, OutPort::default())],
                            );
                            current = b.push(
                                IrOp::NativeMatteCdl { cdl },
                                vec![
                                    (scene_original, OutPort::default()),
                                    (key, OutPort::default()),
                                ],
                            );
                        } else {
                            current = b.push(
                                IrOp::NativeLogQualifier {
                                    qualifier: qualifier.clone(),
                                },
                                vec![(current, OutPort::default())],
                            );
                        }
                        if let Some(mask) = mask.filter(|_| spatial.is_none()) {
                            current = b.push(
                                IrOp::NativeMaskMix { mask },
                                vec![
                                    (current, OutPort::default()),
                                    (original, OutPort::default()),
                                ],
                            );
                        }
                        if spatial.is_none() {
                            current = b.push(
                                IrOp::NativeAcescct {
                                    direction:
                                        photonic_render::native_transfer::AcescctDirection::Decode,
                                },
                                vec![(current, OutPort::default())],
                            );
                        }
                    }
                    continue;
                }
            }
            if let photonic_render::grade::ResolvedGradePayload::Cdl(cdl) = op.payload {
                if mask_valid && photonic_render::native_transfer::validate_native_cdl(&cdl).is_ok()
                {
                    if cdl.slope != [1.0; 3]
                        || cdl.offset != [0.0; 3]
                        || cdl.power != [1.0; 3]
                        || cdl.sat != 1.0
                    {
                        current = b.push(
                            IrOp::NativeAcescct {
                                direction:
                                    photonic_render::native_transfer::AcescctDirection::Encode,
                            },
                            vec![(current, OutPort::default())],
                        );
                        let original = current;
                        current = b.push(
                            IrOp::NativeLogCdl { cdl },
                            vec![(current, OutPort::default())],
                        );
                        if let Some(mask) = mask {
                            current = b.push(
                                IrOp::NativeMaskMix { mask },
                                vec![
                                    (current, OutPort::default()),
                                    (original, OutPort::default()),
                                ],
                            );
                        }
                        current = b.push(
                            IrOp::NativeAcescct {
                                direction:
                                    photonic_render::native_transfer::AcescctDirection::Decode,
                            },
                            vec![(current, OutPort::default())],
                        );
                    }
                    continue;
                }
            }
            if let photonic_render::grade::ResolvedGradePayload::Contrast { pivot, amount } =
                op.payload
            {
                if mask_valid
                    && pivot.is_finite()
                    && (0.0..=1.0).contains(&pivot)
                    && amount.is_finite()
                    && (-4.0..=4.0).contains(&amount)
                {
                    // Skip neutral contrast to preserve exact scene-linear identity.
                    if amount != 0.0 {
                        current = b.push(
                            IrOp::NativeAcescct {
                                direction:
                                    photonic_render::native_transfer::AcescctDirection::Encode,
                            },
                            vec![(current, OutPort::default())],
                        );
                        let original = current;
                        current = b.push(
                            IrOp::NativeLogContrast { pivot, amount },
                            vec![(current, OutPort::default())],
                        );
                        if let Some(mask) = mask {
                            current = b.push(
                                IrOp::NativeMaskMix { mask },
                                vec![
                                    (current, OutPort::default()),
                                    (original, OutPort::default()),
                                ],
                            );
                        }
                        current = b.push(
                            IrOp::NativeAcescct {
                                direction:
                                    photonic_render::native_transfer::AcescctDirection::Decode,
                            },
                            vec![(current, OutPort::default())],
                        );
                    }
                    continue;
                }
            }
            if let photonic_render::grade::ResolvedGradePayload::WhiteBalance { temp, tint } =
                op.payload
            {
                if mask_valid {
                    if let Ok(points) =
                        photonic_render::native_transfer::white_balance_printer_points(temp, tint)
                    {
                        // Exact neutral bypass avoids needless half-float quantization.
                        if temp != 0.0 || tint != 0.0 {
                            let original = current;
                            current = b.push(
                                IrOp::NativePrinterLights { points },
                                vec![(current, OutPort::default())],
                            );
                            if let Some(mask) = mask {
                                current = b.push(
                                    IrOp::NativeMaskMix { mask },
                                    vec![
                                        (current, OutPort::default()),
                                        (original, OutPort::default()),
                                    ],
                                );
                            }
                        }
                        continue;
                    }
                }
            }
            use photonic_render::grade::ResolvedGradePayload as P;
            let native: Result<IrOp, &str> = if !mask_valid {
                Err("native power-window geometry must be finite with positive sizes and nonnegative feather")
            } else {
                match op.payload {
                P::WhiteBalance { .. } => Err("native temperature and tint must be finite and within -1..=1"),
                P::TonalExposure(tonal) => tonal.validate().and_then(|_| if tonal.native_ap1 {Ok(IrOp::NativeTonalExposure{tonal})} else {Err("native tonal exposure requires AP1 luminance")}),
                P::LinearMatrix {rows} => photonic_render::white_balance::validate_matrix(&rows).map(|_| IrOp::NativeRgbMatrix{rows}),
                P::Exposure { stops }
                    if stops.is_finite() && (-32.0..=32.0).contains(&stops) =>
                {
                    Ok(IrOp::NativeExposure { stops })
                }
                P::Exposure { .. } => Err("native exposure must be finite and within -32..=32 stops"),
                P::LinearOffset { rgb }
                    if rgb.iter().all(|value| value.is_finite() && (-16.0..=16.0).contains(value)) =>
                {
                    Ok(IrOp::NativeLinearOffset { rgb })
                }
                P::LinearOffset { .. } => Err("native linear offset must be finite and within -16..=16"),
                P::PrinterLights { points }
                    if points.iter().all(|value| value.is_finite() && (-120.0..=120.0).contains(value)) =>
                {
                    Ok(IrOp::NativePrinterLights { points })
                }
                P::PrinterLights { .. } => Err("native printer lights must be finite and within -120..=120 points"),
                P::HighlightRolloff { knee, strength }
                    if knee.is_finite() && (0.0..=64.0).contains(&knee)
                        && strength.is_finite() && (0.0..=64.0).contains(&strength) =>
                {
                    Ok(IrOp::NativeHighlightRolloff { knee, strength })
                }
                P::HighlightRolloff { .. } => Err("native highlight roll-off parameters must be finite and within 0..=64"),
                P::SaturationVibrance { saturation, vibrance }
                    if saturation.is_finite() && (0.0..=4.0).contains(&saturation)
                        && vibrance.is_finite() && (-1.0..=1.0).contains(&vibrance) =>
                {
                    Ok(IrOp::NativeSaturationVibrance { saturation, vibrance })
                }
                P::SaturationVibrance { .. } => Err("native saturation must be within 0..=4 and vibrance within -1..=1; both must be finite"),
                _ => Err("managed grading does not yet support this corrector or mask; corrector bypassed"),
            }
            };
            match native {
                Ok(native) => {
                    let original = current;
                    current = b.push(native, vec![(current, OutPort::default())]);
                    if let Some(mask) = mask {
                        current = b.push(
                            IrOp::NativeMaskMix { mask },
                            vec![
                                (current, OutPort::default()),
                                (original, OutPort::default()),
                            ],
                        );
                    }
                }
                Err(message) => {
                    let diagnostic = photonic_render::grade::GradeDiagnostic {
                        op: op_id,
                        owner,
                        graph_node,
                        stage: None,
                        sequence_path: b.sequence_path.clone(),
                        issue: photonic_render::grade::GradeIssue::NativeCorrectionUnavailable(
                            message.into(),
                        ),
                    };
                    let mut compiled = CompileDiagnostic::coded(
                        CompileCode::ColorPipelineUnavailable,
                        DiagSeverity::Error,
                        match owner {
                            Some(VfxOwner::Clip(id)) => Some(id),
                            _ => None,
                        },
                        diagnostic.to_string(),
                    );
                    compiled.grade = Some(diagnostic);
                    b.diag(compiled);
                }
            }
        }
        return current;
    }
    if ops.is_empty() {
        input
    } else {
        b.push(
            IrOp::Grade {
                ops: ops.into_iter().map(|(_, op)| op).collect(),
            },
            vec![(input, OutPort::default())],
        )
    }
}

// ── Step 2: clip source ────────────────────────────────────────────────────────

/// Build the clip's source op (after trim + speed source-time mapping, 01 §5.1).
#[allow(clippy::too_many_arguments)]
fn build_clip_source(
    b: &mut Builder<'_>,
    project: &TimelineProject,
    seq: &Sequence,
    format_index: usize,
    format: &SequenceFormat,
    clip: &Clip,
    tick: Tick,
    quality: Quality,
    cycle: &mut HashSet<SequenceId>,
) -> IrNodeId {
    let dt = tick - clip.start;
    let src_time = clip.source_in + clip.speed.source_delta(dt);

    match &clip.source {
        ClipSource::Asset { asset } => {
            let kind = project
                .media
                .assets
                .get(asset)
                .map(|a| a.kind)
                .unwrap_or(AssetKind::Video);
            // 38 §3.5: a video source whose frame rate differs from the sequence
            // rate is conformed by nearest-covering-source-frame selection (the
            // `DecodeVideo { src_time }` below is already that, identical in
            // preview and export — 38 §3.2). State it with a per-clip Info. The
            // comparison is on the RATIONAL value, so 30/1 and 60/2 are equal.
            if kind == AssetKind::Video {
                if let Some(src_rate) = project
                    .media
                    .assets
                    .get(asset)
                    .and_then(|a| a.probe.as_ref())
                    .and_then(|p| p.video.as_ref())
                    .map(|v| v.frame_rate)
                {
                    if !rates_equal(src_rate, seq.frame_rate) {
                        b.diag_coded_once(
                            CompileCode::FrameRateConformed,
                            DiagSeverity::Info,
                            Some(clip.id),
                            format!(
                                "clip {} is {}/{} on a {}/{} sequence; frames are conformed \
                                 by nearest-covering-source-frame selection (30→24 drops, \
                                 24→30 repeats)",
                                clip.name,
                                src_rate.num,
                                src_rate.den,
                                seq.frame_rate.num,
                                seq.frame_rate.den
                            ),
                        );
                    }
                }
            }
            match kind {
                AssetKind::Image
                    if matches!(
                        seq.color,
                        timeline::color::SequenceColorConfig::NativeManaged(_)
                    ) =>
                {
                    let input = clip.native_input_color.as_ref().or_else(|| {
                        project
                            .media
                            .assets
                            .get(asset)
                            .and_then(|a| a.native_input_color.as_ref())
                    });
                    if input.is_none_or(|input| {
                        input.standard != timeline::color::NativeInputStandard::SrgbDisplay
                            || input.validate_asset_kind(AssetKind::Image).is_err()
                    }) {
                        b.diag_coded_once(CompileCode::ColorPipelineUnavailable, DiagSeverity::Error, Some(clip.id), "native still requires an explicit full-range sRGB display interpretation");
                        return b.transparent(format);
                    }
                    b.push(IrOp::NativeDecodeStill { asset: *asset }, vec![])
                }
                AssetKind::Image => b.push(IrOp::DecodeStill { asset: *asset }, vec![]),
                AssetKind::Video
                    if matches!(
                        seq.color,
                        timeline::color::SequenceColorConfig::NativeManaged(_)
                    ) =>
                {
                    let authored = clip.native_input_color.as_ref().or_else(|| {
                        project
                            .media
                            .assets
                            .get(asset)
                            .and_then(|media| media.native_input_color.as_ref())
                    });
                    let Some(input) = authored else {
                        b.diag_coded_once(
                            CompileCode::ColorPipelineUnavailable,
                            DiagSeverity::Error,
                            Some(clip.id),
                            "native video clip requires an explicit input interpretation",
                        );
                        return b.transparent(format);
                    };
                    if let Err(error) = input.validate() {
                        b.diag_coded_once(
                            CompileCode::ColorPipelineUnavailable,
                            DiagSeverity::Error,
                            Some(clip.id),
                            error,
                        );
                        return b.transparent(format);
                    }
                    if deinterlace_for_asset(project, *asset).is_some() {
                        b.diag_coded_once(
                            CompileCode::ColorPipelineUnavailable,
                            DiagSeverity::Error,
                            Some(clip.id),
                            "native interlaced input needs a qualified pre-IDT deinterlace",
                        );
                        return b.transparent(format);
                    }
                    b.push(
                        IrOp::NativeDecodeVideo {
                            asset: *asset,
                            src_time,
                            input: input.clone(),
                        },
                        vec![],
                    )
                }
                AssetKind::Video | AssetKind::Audio | AssetKind::VectorDoc | AssetKind::Lut3d => {
                    let decode = b.push(
                        IrOp::DecodeVideo {
                            asset: *asset,
                            src_time,
                            proxy: quality.proxy,
                        },
                        vec![],
                    );
                    // K-G6: auto-insert deinterlace when probe reports interlaced.
                    if let Some((method, order)) = deinterlace_for_asset(project, *asset) {
                        b.push(
                            IrOp::Deinterlace {
                                method,
                                field_order: order,
                            },
                            vec![(decode, Default::default())],
                        )
                    } else {
                        decode
                    }
                }
            }
        }
        ClipSource::Vector { asset } => {
            let vref = vector_ref_for(project, *asset);
            let doc_state = vector_state_key(*asset, format, src_time);
            b.push(
                IrOp::RasterVector {
                    vref,
                    doc_state,
                    w: format.width,
                    h: format.height,
                },
                vec![],
            )
        }
        ClipSource::NestedSequence { sequence } => build_nested_sequence(
            b,
            project,
            *sequence,
            format_index,
            format,
            src_time,
            quality,
            cycle,
            seq.frame_rate,
            clip.id,
        ),
        ClipSource::SolidColor { color } => b.push(
            IrOp::SolidColor {
                color: color_to_linear_premult(*color),
            },
            vec![],
        ),
        ClipSource::Adjustment => {
            // Reached only if an Adjustment clip is mis-placed as a source; the
            // fold loop handles real Adjustment clips (step 4). Fall back to
            // transparent so it never contributes a spurious image.
            let _ = seq;
            b.transparent(format)
        }
        ClipSource::Text { content } => {
            // G-12: a title/text clip lowers to the dedicated `TextGen` IR op,
            // its `TextClipContent` (text + shared `CaptionStyle`) resolved into
            // the same `CaptionCueRun` glyph payload the `CaptionOverlay` path
            // consumes (06 §5.3) — one text-render mechanism, not a parallel
            // path. The evaluator burns it over transparent via the glyphon
            // compositor; the clip's own `Transform2D` then places it.
            b.push(
                IrOp::TextGen {
                    block: resolve_text_block(content),
                },
                vec![],
            )
        }
        ClipSource::Unknown(_) => {
            // Forward-compat (39 §2.2): a source kind this build does not
            // understand renders as a transparent placeholder — the same inert
            // treatment as a missing/offline asset — never guessed. The original
            // `source` object is retained verbatim in the model.
            let _ = seq;
            b.diag(CompileDiagnostic::plain(format!(
                "unknown clip source {:?} renders as a placeholder (this build \
                 does not understand it)",
                clip.source.unknown_tag().unwrap_or("?")
            )));
            b.transparent(format)
        }
    }
}

/// Two frame rates are equal as RATIONAL values (30/1 == 60/2), so an
/// equivalent rate never reads as a mismatch (38 §3.5).
fn rates_equal(a: FrameRate, b: FrameRate) -> bool {
    a.num as u64 * b.den as u64 == b.num as u64 * a.den as u64
}

/// Recursively compile a nested sequence (CAP-005) and splice its program as a
/// source. Cycle-guarded: a re-entrant sequence yields a transparent placeholder
/// plus a diagnostic (never an infinite recursion / black frame).
///
/// 38 §2: the nest renders in the OUTER (parent) format — the inner sequence's
/// own `active_format`/`formats` do NOT govern (§2.3), so an inner clip reframes
/// to its host. Inner caption tracks render inside the nest (§2.3), at the inner
/// timebase (`src_time`). A rate mismatch emits one Info per nest (§2.2); a
/// reference past the inner content holds the last rendered frame + Warning
/// (§2.4). `host_rate`/`nest_clip` carry the host sequence's rate and the nest
/// clip's id so those per-nest diagnostics have a subject without another borrow.
#[allow(clippy::too_many_arguments)]
fn build_nested_sequence(
    b: &mut Builder<'_>,
    project: &TimelineProject,
    sequence: SequenceId,
    parent_format_index: usize,
    parent_format: &SequenceFormat,
    src_time: Tick,
    quality: Quality,
    cycle: &mut HashSet<SequenceId>,
    host_rate: FrameRate,
    nest_clip: ClipId,
) -> IrNodeId {
    if cycle.contains(&sequence) {
        b.diag(CompileDiagnostic::plain(format!(
            "nested-sequence cycle: {sequence} references itself; substituting transparent"
        )));
        return b.transparent(parent_format);
    }
    let Some(nested) = project.sequences.get(&sequence) else {
        b.diag(CompileDiagnostic::plain(format!(
            "nested sequence {sequence} not found; substituting transparent"
        )));
        return b.transparent(parent_format);
    };

    // 38 §2.2: rate mismatch → one Info per nest (Tick is rate-independent, so
    // this is a sampling note, not a change in what is rendered).
    if !rates_equal(nested.frame_rate, host_rate) {
        b.diag_coded_once(
            CompileCode::FrameRateConformed,
            DiagSeverity::Info,
            Some(nest_clip),
            format!(
                "nested sequence {} runs at {}/{} inside a {}/{} host; it is sampled at \
                 the requested tick (38 §2.2)",
                nested.name,
                nested.frame_rate.num,
                nested.frame_rate.den,
                host_rate.num,
                host_rate.den
            ),
        );
    }

    // 38 §2.4: if the nest references past the inner sequence's content, hold the
    // last rendered frame (fold at a FIXED `inner_end − ticks_per_frame`, which
    // is content-hash-stable across the tail so the node cache serves it once)
    // and warn. The outer clip's layout is never mutated — the compiler is a pure
    // function of the snapshot.
    let inner_end = nested.content_end();
    let fold_tick = if inner_end > Tick::ZERO && src_time >= inner_end {
        b.diag_coded_once(
            CompileCode::NestedSequenceShortened,
            DiagSeverity::Warning,
            Some(nest_clip),
            format!(
                "nested sequence {} ({} ticks) is shorter than the nest clip references; \
                 holding the last rendered frame (38 §2.4)",
                nested.name, inner_end.0
            ),
        );
        let tpf = nested.frame_rate.ticks_per_frame();
        Tick((inner_end.0 - tpf.0).max(0))
    } else {
        if inner_end == Tick::ZERO {
            // Empty inner sequence: the transparent fallback below stands, but the
            // reference is still "shortened" — warn (38 §2.4).
            b.diag_coded_once(
                CompileCode::NestedSequenceShortened,
                DiagSeverity::Warning,
                Some(nest_clip),
                format!(
                    "nested sequence {} is empty; the nest renders transparent (38 §2.4)",
                    nested.name
                ),
            );
        }
        src_time
    };

    // Arm the cycle guard around the recursive fold ONLY: every early return
    // above leaves the visited-set untouched, so a bail-out (missing nested
    // sequence referenced more than once) never poisons a sibling lower.
    // 38 §2.3: fold in the OUTER format — pass the parent's index/format through
    // so an inner clip's `reframe` entry for the OUTER format index is what
    // applies (a nest reframes to its host).
    cycle.insert(sequence);
    b.sequence_path.push(sequence);
    let program = fold_sequence(
        b,
        project,
        nested,
        parent_format_index,
        parent_format,
        fold_tick,
        quality,
        cycle,
    );
    b.sequence_path.pop();
    cycle.remove(&sequence);

    // 38 §2.3: the inner sequence's own caption tracks are part of the picture —
    // splice them here (they are invisible to the top-level `splice_captions`).
    // Captions resolve at the inner timebase (`fold_tick`, the held frame's tick
    // in the tail case), against the render (outer/parent) format. They ride ON
    // TOP of the inner fold but UNDER the outer clip's Transform2D/effects/grade,
    // which is automatic: this returns the clip's SOURCE op and `build_clip_chain`
    // appends the rest of the chain after it.
    let program = splice_captions(b, nested, parent_format, fold_tick, program);

    program.unwrap_or_else(|| b.transparent(parent_format))
}

fn native_node_scalar(
    anim: &AnimProps<GraphNodeParams>,
    path: &str,
    default: f64,
    tick: Tick,
) -> Result<f64, String> {
    let property = PropPath::new(path);
    let base = anim
        .base
        .0
        .get(path)
        .copied()
        .unwrap_or(PropValue::Float(default));
    let value = anim
        .track(&property)
        .map_or(base, |track| timeline::eval(track, &base, tick));
    match value {
        PropValue::Float(value) if value.is_finite() => Ok(value),
        _ => Err(format!(
            "native graph {path} requires a finite numeric value"
        )),
    }
}

fn native_motion_blur_op(params: &ResolvedParams) -> Result<IrOp, &'static str> {
    let number = |path| {
        params.get(path).and_then(|v| match v {
            PropValue::Float(v) if v.is_finite() => Some(*v),
            _ => None,
        })
    };
    let angle = number("params.angle").filter(|v| (0.0..=360.0).contains(v));
    let distance = number("params.distance").filter(|v| (0.0..=500.0).contains(v));
    let (Some(angle), Some(distance)) = (angle, distance) else {
        return Err("native directional motion blur requires finite angle 0..=360 degrees and distance 0..=500 authored pixels");
    };
    let radians = angle.rem_euclid(360.0).to_radians();
    let component = |v: f64| if v.abs() < 1e-10 { 0.0 } else { v as f32 };
    Ok(IrOp::NativeMotionBlur {
        delta: [
            component(distance * radians.cos()),
            component(distance * radians.sin()),
        ],
    })
}

fn native_shape_op(params: &ResolvedParams, rect: bool) -> Result<IrOp, String> {
    let scalar = |path, max: f64| match params.get(path) {
        Some(PropValue::Float(v)) if v.is_finite() && (0.0..=max).contains(v) => Ok(*v as f32),
        _ => Err(format!("native shape {path} must be finite in 0..={max}")),
    };
    let rotation = match params.get("params.rotation") {
        Some(PropValue::Float(v)) if v.is_finite() && (*v as f32).is_finite() => *v as f32,
        _ => return Err("native shape rotation must be finite radians".into()),
    };
    Ok(IrOp::NativeMaskShape {
        rect,
        center: [
            scalar("params.center_x", 1.0)?,
            scalar("params.center_y", 1.0)?,
        ],
        size: [scalar("params.size_x", 2.0)?, scalar("params.size_y", 2.0)?],
        rotation,
        feather: scalar("params.feather", 1.0)?,
    })
}

fn native_glow_op(params: &ResolvedParams) -> Result<IrOp, String> {
    let scalar = |path, max| match params.get(path) {
        Some(PropValue::Float(v)) if v.is_finite() && (0.0..=max).contains(v) => Ok(*v as f32),
        _ => Err(format!("native glow {path} must be finite in 0..={max}")),
    };
    let tint = match params.get("params.tint") {
        Some(PropValue::Color(c)) if native_swatch_color(*c).is_some() => {
            crate::color::native::encoded_srgb_to_linear_ap1_display([
                c.r as f64, c.g as f64, c.b as f64,
            ])
            .map(|v| v as f32)
        }
        _ => return Err("native glow tint requires a finite encoded-sRGB swatch in 0..=1".into()),
    };
    Ok(IrOp::NativeGlow {
        sigma: scalar("params.radius", 500.0)?,
        threshold: scalar("params.threshold", 1.0)?,
        intensity: scalar("params.intensity", 10.0)?,
        tint,
    })
}

fn native_chroma_key_op(params: &ResolvedParams) -> Result<IrOp, String> {
    let scalar = |path| match params.get(path) {
        Some(PropValue::Float(v)) if v.is_finite() && (0.0..=1.0).contains(v) => Ok(*v as f32),
        _ => Err(format!("native chroma key {path} must be finite in 0..=1")),
    };
    let color = match params.get("params.key_color") {
        Some(PropValue::Color(c)) if native_swatch_color(*c).is_some() => *c,
        _ => return Err("native chroma key requires a finite encoded-sRGB swatch in 0..=1".into()),
    };
    // Like the existing key contract, swatch alpha is not a key-distance axis.
    let key = crate::color::native::encoded_srgb_to_linear_ap1_display([
        color.r as f64,
        color.g as f64,
        color.b as f64,
    ])
    .map(|v| v as f32);
    Ok(IrOp::NativeChromaKey {
        key,
        tolerance: scalar("params.tolerance")?,
        softness: scalar("params.edge_softness")?,
        spill: scalar("params.spill_suppress")?,
    })
}

fn native_luma_key_op(params: &ResolvedParams) -> Result<IrOp, String> {
    let scalar = |path| match params.get(path) {
        Some(PropValue::Float(v)) if v.is_finite() && (0.0..=1.0).contains(v) => Ok(*v as f32),
        _ => Err(format!("native luma key {path} must be finite in 0..=1")),
    };
    let invert = match params.get("params.invert") {
        Some(PropValue::Bool(v)) => *v,
        _ => return Err("native luma key invert must be boolean".into()),
    };
    Ok(IrOp::NativeLumaKey {
        threshold: scalar("params.threshold")?,
        softness: scalar("params.softness")?,
        invert,
    })
}

/// Resolve the four-input editor Switch without evaluating dormant branches.
/// Missing selection falls back to the lowest connected input index.
fn native_switch_source(
    graph: &NodeGraph,
    node: &GraphNode,
    tick: Tick,
) -> Result<Option<(GraphNodeId, u16)>, String> {
    let property = PropPath::new("params.selected");
    let base = node
        .params
        .base
        .0
        .get("params.selected")
        .copied()
        .unwrap_or(PropValue::Enum(0));
    let value = node
        .params
        .track(&property)
        .map_or(base, |track| timeline::eval(track, &base, tick));
    let PropValue::Enum(selected) = value else {
        return Err("native Switch selected must be an enum index".into());
    };
    let mut ports = HashSet::new();
    let mut selected_source = None;
    let mut first = None;
    for edge in graph.edges.iter().filter(|edge| edge.to.0 == node.id) {
        let port = edge.to.1 .0;
        if port >= 4
            || graph
                .nodes
                .get(&edge.from.0)
                .is_some_and(|n| edge.from.1 .0 >= n.op.port_counts().1)
            || !ports.insert(port)
        {
            return Err("native Switch requires unique image input ports 0..3".into());
        }
        if first.is_none_or(|(first_port, _)| port < first_port) {
            first = Some((port, (edge.from.0, edge.from.1 .0)));
        }
        if u32::from(port) == selected {
            selected_source = Some((edge.from.0, edge.from.1 .0));
        }
    }
    Ok(selected_source.or_else(|| first.map(|(_, source)| source)))
}

#[derive(Clone, Copy)]
struct NativeGraphSourceContext<'a> {
    project: &'a TimelineProject,
    clip_start: Option<Tick>,
    clip: Option<&'a Clip>,
    sequence: SequenceId,
    format_index: usize,
    quality: Quality,
    check_online: bool,
    source_errors: Option<&'a HashMap<AssetId, String>>,
}

/// Independent MediaIn uses the asset's technical interpretation, never a host
/// clip override or its trim/speed. This first qualification requires source
/// pixels to reach the authored canvas through an explicit Resize when their
/// dimensions differ; no implicit fit may hide geometry.
fn native_media_in_op(
    context: NativeGraphSourceContext<'_>,
    asset: AssetId,
    time_source: TimeSource,
    tick: Tick,
    format: &SequenceFormat,
) -> Result<IrOp, String> {
    let media = context
        .project
        .media
        .assets
        .get(&asset)
        .ok_or("native MediaIn asset is missing")?;
    if !matches!(media.kind, AssetKind::Image | AssetKind::Video) {
        return Err("native MediaIn requires a still or video asset".into());
    }
    let timeline::AssetSource::File { path, .. } = &media.source else {
        return Err("native MediaIn requires a file-backed asset".into());
    };
    if let Some(error) = context.source_errors.and_then(|errors| errors.get(&asset)) {
        return Err(format!("native MediaIn {asset}: {error}"));
    }
    if context.check_online && !path.is_file() {
        return Err(format!("native MediaIn source {asset} is offline"));
    }
    let input = media.native_input_color.as_ref().ok_or(
        "native MediaIn needs the asset's explicit native input interpretation; \
         host clip overrides are not inherited",
    )?;
    crate::color::validate_native_asset_input(media, input, context.check_online)?;
    if deinterlace_for_asset(context.project, asset).is_some() {
        return Err(
            "native MediaIn interlaced input requires a qualified pre-IDT deinterlace".into(),
        );
    }
    let probed = media
        .probe
        .as_ref()
        .and_then(|p| p.video.as_ref())
        .map(|v| (v.width, v.height));
    let dimensions = if media.kind == AssetKind::Image && (context.check_online || probed.is_none())
    {
        image::ImageReader::open(path)
            .map_err(|e| format!("native MediaIn still dimensions: {e}"))?
            .with_guessed_format()
            .map_err(|e| format!("native MediaIn still dimensions: {e}"))?
            .into_dimensions()
            .map_err(|e| format!("native MediaIn still dimensions: {e}"))?
    } else {
        probed.ok_or("native MediaIn requires probed source dimensions")?
    };
    if dimensions.0 == 0
        || dimensions.1 == 0
        || dimensions.0 > 8192
        || dimensions.1 > 8192
        || u64::from(dimensions.0) * u64::from(dimensions.1) > 16_777_216
    {
        return Err(
            "native MediaIn source dimensions exceed the bounded source canvas limit".into(),
        );
    }
    let native_extent = dimensions != (format.width, format.height);
    if media.kind == AssetKind::Image {
        return Ok(IrOp::NativeMediaStill {
            asset,
            w: dimensions.0,
            h: dimensions.1,
            native_extent,
        });
    }
    let src_time = match (time_source, context.clip_start) {
        (TimeSource::Local, Some(start)) => Tick(
            tick.0
                .checked_sub(start.0)
                .ok_or("native MediaIn local time overflow")?,
        ),
        _ => tick,
    };
    if src_time < Tick::ZERO {
        return Err("native MediaIn requires nonnegative source time".into());
    }
    Ok(IrOp::NativeMediaVideo {
        asset,
        src_time,
        input: input.clone(),
        w: dimensions.0,
        h: dimensions.1,
        native_extent,
    })
}

/// Import only scene-linear ancestors, never the preview/output transform.
fn native_import_scene(b: &mut Builder<'_>, inner: CompiledFrame) -> Result<IrNodeId, String> {
    if inner
        .diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error)
    {
        b.diagnostics.extend(inner.diagnostics);
        return Err("native TimeOffset source cannot render accurately".into());
    }
    let output = inner
        .graph
        .output
        .ok_or("native TimeOffset source has no output")?;
    let output_node = &inner.graph.nodes[output.0 as usize];
    if !matches!(output_node.op, IrOp::NativeSdrOutput) || output_node.inputs.len() != 1 {
        return Err("native TimeOffset source requires a scene-linear boundary; nested captions remain unqualified".into());
    }
    let scene = output_node.inputs[0].0;
    let mut imported = Vec::with_capacity(output.0 as usize);
    for node in inner.graph.nodes.iter().take(output.0 as usize) {
        let inputs = node
            .inputs
            .iter()
            .map(|(id, port)| (imported[id.0 as usize], *port))
            .collect();
        imported.push(b.push(node.op.clone(), inputs));
    }
    for (key, node) in inner.native_deflicker_inputs {
        b.native_deflicker_inputs
            .push((key, imported[node.0 as usize]));
    }
    for (key, node) in inner.native_deflicker_pending {
        b.native_deflicker_pending
            .push((key, imported[node.0 as usize]));
    }
    for diagnostic in inner.diagnostics {
        b.diag(diagnostic);
    }
    Ok(imported[scene.0 as usize])
}

/// Checked native retiming keeps exact hold-segment arithmetic and the existing
/// eased integration contract. It never relies on a saturating/wrapping cast.
fn native_speed_delta(speed: &SpeedMap, delta: i64, duration: Tick) -> Result<i128, String> {
    if let SpeedMap::Keyframed { keys } = speed {
        if keys.len() > 4096 {
            return Err("native clip retiming exceeds 4096 speed keys".into());
        }
    }
    speed
        .validate_for_duration(duration)
        .map_err(|error| format!("native clip retiming invalid speed map: {error:?}"))?;
    match speed {
        SpeedMap::Constant(ratio) => Ok(delta as i128 * ratio.num as i128 / ratio.den as i128),
        SpeedMap::Keyframed { keys } if keys.is_empty() => Ok(delta as i128),
        SpeedMap::Keyframed { keys } => {
            let mut sorted = keys.clone();
            sorted.sort_by_key(|key| key.at.0);
            let eased = sorted
                .windows(2)
                .any(|pair| !matches!(pair[0].interp, timeline::Interp::Hold));
            if eased {
                // Core eased integration uses i64 interval subtraction and f64
                // quadrature. Reject the pathological minimum interval and deltas
                // beyond the exact-integer f64 range before the final tick cast.
                if delta == i64::MIN {
                    return Err("native clip retiming eased interval overflow".into());
                }
                let largest_speed = sorted
                    .iter()
                    .map(|key| key.ratio.as_f64().abs())
                    .fold(0.0, f64::max);
                if (delta as f64).abs() * largest_speed > 9_007_199_254_740_991.0 {
                    return Err(
                        "native clip retiming eased interval exceeds exact tick qualification range"
                            .into(),
                    );
                }
                let value = speed.source_delta_f64(Tick(delta)).round();
                if !value.is_finite() || value.abs() > 9_007_199_254_740_991.0 {
                    return Err(
                        "native clip retiming eased integral exceeds exact tick qualification range"
                            .into(),
                    );
                }
                return Ok(value as i128);
            }
            let (lo, hi) = if delta >= 0 {
                (0, delta as i128)
            } else {
                (delta as i128, 0)
            };
            let mut total = 0i128;
            for (index, key) in sorted.iter().enumerate() {
                let start = if index == 0 {
                    i64::MIN as i128
                } else {
                    key.at.0 as i128
                };
                let end = sorted
                    .get(index + 1)
                    .map_or(i64::MAX as i128, |next| next.at.0 as i128);
                let a = start.max(lo);
                let b = end.min(hi);
                if b > a {
                    let segment = (b - a) * key.ratio.num as i128 / key.ratio.den as i128;
                    total = total
                        .checked_add(segment)
                        .ok_or("native clip retiming speed integral overflow")?;
                }
            }
            Ok(if delta < 0 { -total } else { total })
        }
    }
}

fn native_source_time_checked(clip: &Clip, tick: Tick) -> Result<Tick, String> {
    if clip.source_in < Tick::ZERO {
        return Err("native clip retiming requires nonnegative source trim".into());
    }
    let delta = tick
        .0
        .checked_sub(clip.start.0)
        .ok_or("native clip retiming clip-local overflow")?;
    let scaled = native_speed_delta(&clip.speed, delta, clip.duration)?;
    let source = scaled + clip.source_in.0 as i128;
    let source = i64::try_from(source).map_err(|_| "native clip retiming source time overflow")?;
    if source < 0 {
        return Err("native clip retiming requires nonnegative source time".into());
    }
    Ok(Tick(source))
}

fn native_retime_clip_source(
    b: &mut Builder<'_>,
    context: NativeGraphSourceContext<'_>,
    source: IrNodeId,
    tick: Tick,
    format: &SequenceFormat,
) -> Result<IrNodeId, String> {
    let op = b.nodes[source.0 as usize].op.clone();
    if matches!(
        op,
        IrOp::NativeDecodeStill { .. }
            | IrOp::SolidColor { .. }
            | IrOp::NativeTextGen { .. }
            | IrOp::NativeDecoratedTextGen { .. }
    ) {
        return Ok(source);
    }
    let clip = context.clip.ok_or("native TimeOffset has no owning clip")?;
    let src_time = native_source_time_checked(clip, tick)?;
    if let ClipSource::NestedSequence { sequence } = clip.source {
        let nested = context
            .project
            .sequences
            .get(&sequence)
            .ok_or("native TimeOffset nest is missing")?;
        let end = nested.content_end();
        let fold_tick = if end > Tick::ZERO && src_time >= end {
            b.diag_coded_once(CompileCode::NestedSequenceShortened, DiagSeverity::Warning, Some(clip.id),
                "native nested sequence is shorter than the outer reference; holding its last frame");
            Tick((end.0 - nested.frame_rate.ticks_per_frame().0).max(0))
        } else {
            src_time
        };
        let inner = compile_native_sequence_impl(
            context.project,
            sequence,
            context.format_index,
            fold_tick,
            context.quality,
            false,
            context.check_online,
            context.source_errors,
            b.luts,
            b.deflicker,
            &b.sequence_path,
        );
        return native_import_scene(b, inner);
    }
    let op = match op {
        IrOp::NativeDecodeVideo { asset, input, .. } => IrOp::NativeDecodeVideo {
            asset,
            src_time,
            input,
        },
        IrOp::NativeRasterVector { vref, w, h, .. } => {
            let ClipSource::Vector { asset } = clip.source else {
                return Err("native TimeOffset vector source context is inconsistent".into());
            };
            IrOp::NativeRasterVector {
                vref,
                doc_state: vector_state_key(asset, format, src_time),
                w,
                h,
            }
        }
        _ => return Err("native TimeOffset ClipIn source is not qualified".into()),
    };
    Ok(b.push(op, vec![]))
}

/// Re-evaluate the pre-project program at an offset clock. Captions remain an
/// outer display stage and must not enter or be duplicated in this scene input.
fn native_retime_program(
    b: &mut Builder<'_>,
    context: NativeGraphSourceContext<'_>,
    tick: Tick,
) -> Result<IrNodeId, String> {
    if tick < Tick::ZERO {
        return Err("native TimeOffset requires nonnegative program time".into());
    }
    let inner = compile_native_sequence_stages(
        context.project,
        context.sequence,
        context.format_index,
        tick,
        context.quality,
        false,
        context.check_online,
        context.source_errors,
        b.luts,
        b.deflicker,
        &[],
        false,
    );
    native_import_scene(b, inner)
}

/// Qualified native composition subset. Never route managed pixels through the
/// legacy graph's permissive unknown-op or missing-media fallbacks.
#[cfg(test)]
fn lower_native_graph(
    b: &mut Builder<'_>,
    graph: &NodeGraph,
    format: &SequenceFormat,
    tick: Tick,
    clip_source: Option<IrNodeId>,
    program: Option<IrNodeId>,
) -> Result<IrNodeId, String> {
    lower_native_graph_with_sources(b, graph, format, tick, clip_source, program, None)
}

fn lower_native_graph_with_sources(
    b: &mut Builder<'_>,
    graph: &NodeGraph,
    format: &SequenceFormat,
    tick: Tick,
    clip_source: Option<IrNodeId>,
    program: Option<IrNodeId>,
    source_context: Option<NativeGraphSourceContext<'_>>,
) -> Result<IrNodeId, String> {
    if graph.nodes.len() > 4096 || graph.edges.len() > 8192 {
        return Err("native graph exceeds the 4096-node/8192-edge qualification limit".into());
    }
    if !matches!(
        graph.nodes.get(&graph.output).map(|n| &n.op),
        Some(GraphOp::Output)
    ) {
        return Err("native graph requires a valid Output node".into());
    }
    fn shifted_input_time(node: &timeline::GraphNode, tick: Tick) -> Result<Tick, String> {
        if let GraphOp::TimeOffset { offset } = node.op {
            if !node.params.base.0.entries.is_empty() || !node.params.tracks.is_empty() {
                return Err("native TimeOffset requires a structural constant offset".into());
            }
            return tick
                .0
                .checked_sub(offset.0)
                .map(Tick)
                .ok_or_else(|| "native TimeOffset tick overflow".into());
        }
        Ok(tick)
    }
    fn visit(
        graph: &NodeGraph,
        id: GraphNodeId,
        active: &mut HashSet<GraphNodeId>,
        seen: &mut HashSet<(GraphNodeId, Tick)>,
        order: &mut Vec<(GraphNodeId, Tick)>,
        depth: usize,
        tick: Tick,
    ) -> Result<(), String> {
        if seen.contains(&(id, tick)) {
            return Ok(());
        }
        if seen.len() + active.len() >= 8192 {
            return Err("native graph exceeds 8192 time-expanded node instances".into());
        }
        if depth > 128 || !active.insert(id) {
            return Err("native graph is cyclic or exceeds 128 levels".into());
        }
        let node = graph
            .nodes
            .get(&id)
            .ok_or("native graph references a missing node")?;
        if node.id != id {
            return Err("native graph node identity is inconsistent".into());
        }
        if matches!(node.op, GraphOp::Switch) {
            if let Some(source) = native_switch_source(graph, node, tick)? {
                visit(graph, source.0, active, seen, order, depth + 1, tick)?;
            }
            active.remove(&id);
            seen.insert((id, tick));
            order.push((id, tick));
            return Ok(());
        }
        let ports = match node.op {
            GraphOp::ClipIn
            | GraphOp::MediaIn { .. }
            | GraphOp::SolidColor
            | GraphOp::Text { .. }
            | GraphOp::VectorIn { .. } => 0,
            GraphOp::MaskShape { .. } => 0,
            GraphOp::Output
            | GraphOp::Blur
            | GraphOp::Sharpen
            | GraphOp::Invert
            | GraphOp::LumaKey
            | GraphOp::ChromaKey
            | GraphOp::Glow
            | GraphOp::Grade { .. }
            | GraphOp::Lut { .. }
            | GraphOp::Transform2D
            | GraphOp::Crop
            | GraphOp::Resize { .. }
            | GraphOp::ChannelSplit
            | GraphOp::MaskFromMatte
            | GraphOp::TimeOffset { .. } => 1,
            GraphOp::ChannelCombine => 4,
            GraphOp::Merge { mode } if super::ir::native_blend_supported(mode) => 2,
            _ => return Err(format!("native graph node {:?} is not qualified", node.op)),
        };
        let input_tick = shifted_input_time(node, tick)?;
        let mut connected = HashSet::new();
        for edge in graph.edges.iter().filter(|edge| edge.to.0 == id) {
            if graph
                .nodes
                .get(&edge.from.0)
                .is_some_and(|n| edge.from.1 .0 >= n.op.port_counts().1)
                || edge.to.1 .0 >= ports
                || !connected.insert(edge.to.1 .0)
            {
                return Err("native graph has an invalid or multiply connected image port".into());
            }
            visit(
                graph,
                edge.from.0,
                active,
                seen,
                order,
                depth + 1,
                input_tick,
            )?;
        }
        active.remove(&id);
        seen.insert((id, tick));
        order.push((id, tick));
        Ok(())
    }
    let mut order = Vec::new();
    visit(
        graph,
        graph.output,
        &mut HashSet::new(),
        &mut HashSet::new(),
        &mut order,
        0,
        tick,
    )?;
    let root_tick = tick;
    let offsets: HashSet<_> = order
        .iter()
        .filter_map(|(id, _)| match graph.nodes[id].op {
            GraphOp::TimeOffset { offset } => Some(offset),
            _ => None,
        })
        .collect();
    if offsets.len() > TIME_OFFSET_SOFT_CAP {
        b.diag(CompileDiagnostic::at(graph.id, graph.output, format!(
            "more than {TIME_OFFSET_SOFT_CAP} distinct TimeOffset values in one composition; echo/trail cost grows with each"
        )));
    }
    let mut used_channels: HashMap<(GraphNodeId, Tick), HashSet<u16>> = HashMap::new();
    for &(id, tick) in &order {
        let node = &graph.nodes[&id];
        let input_tick = shifted_input_time(node, tick)?;
        for edge in graph.edges.iter().filter(|edge| edge.to.0 == id) {
            if !matches!(
                graph.nodes.get(&edge.from.0).map(|n| &n.op),
                Some(GraphOp::ChannelSplit)
            ) {
                continue;
            }
            if matches!(node.op, GraphOp::Switch)
                && native_switch_source(graph, node, tick)? != Some((edge.from.0, edge.from.1 .0))
            {
                continue;
            }
            used_channels
                .entry((edge.from.0, input_tick))
                .or_default()
                .insert(edge.from.1 .0);
        }
    }
    let base_fallback = program.unwrap_or_else(|| b.transparent(format));
    let mut retimed_programs = HashMap::new();
    let mut retimed_clips = HashMap::new();
    let mut lowered = HashMap::new();
    let mut native_extents = HashSet::new();
    let mut extent_hashes = HashSet::new();
    let mut extent_texels = 0_u64;
    for (id, tick) in order {
        let node = &graph.nodes[&id];
        let input_tick = shifted_input_time(node, tick)?;
        if matches!(node.op, GraphOp::ChannelSplit | GraphOp::ChannelCombine)
            && (!node.params.base.0.entries.is_empty() || !node.params.tracks.is_empty())
        {
            return Err("native channel nodes have no animatable parameters".into());
        }
        let convert = |b: &mut Builder<'_>,
                       source: (GraphNodeId, u16),
                       raw: IrNodeId,
                       wants_mask: bool| {
            let source_mask = matches!(
                graph.nodes[&source.0].op,
                GraphOp::MaskShape { .. } | GraphOp::ChannelSplit | GraphOp::MaskFromMatte
            );
            match (source_mask, wants_mask) {
                (true, false) => b.push(IrOp::NativeMaskToImage, vec![(raw, OutPort::default())]),
                (false, true) => b.push(IrOp::NativeImageToMask, vec![(raw, OutPort::default())]),
                _ => raw,
            }
        };
        let mut resolved_inputs = HashMap::new();
        if !matches!(node.op, GraphOp::Switch) {
            for edge in graph.edges.iter().filter(|edge| edge.to.0 == id) {
                let source = (edge.from.0, edge.from.1 .0);
                let raw = *lowered
                    .get(&(source.0, input_tick, source.1))
                    .ok_or("native graph producer port was not lowered")?;
                if native_extents.contains(&raw) {
                    if !matches!(node.op, GraphOp::Resize { .. } | GraphOp::TimeOffset { .. }) {
                        return Err("native MediaIn with different source dimensions requires an explicit Resize before other processing".into());
                    }
                }
                resolved_inputs.insert(
                    edge.to.1 .0,
                    convert(b, source, raw, matches!(node.op, GraphOp::ChannelCombine)),
                );
            }
        }
        let input = |port: InPort| resolved_inputs.get(&port.0).copied();
        let primary = input(InPort::PRIMARY);
        let needs_fallback = match node.op {
            GraphOp::ClipIn
            | GraphOp::MediaIn { .. }
            | GraphOp::SolidColor
            | GraphOp::MaskShape { .. }
            | GraphOp::Text { .. }
            | GraphOp::VectorIn { .. }
            | GraphOp::ChannelCombine
            | GraphOp::Switch
            | GraphOp::TimeOffset { .. } => false,
            GraphOp::Merge { .. } => primary.is_none() || input(InPort::B).is_none(),
            _ => primary.is_none(),
        };
        let fallback = if tick != root_tick && program.is_some() && needs_fallback {
            if let Some(&cached) = retimed_programs.get(&tick) {
                cached
            } else {
                let context =
                    source_context.ok_or("native project TimeOffset requires a source context")?;
                let value = native_retime_program(b, context, tick)?;
                retimed_programs.insert(tick, value);
                value
            }
        } else {
            base_fallback
        };
        if matches!(node.op, GraphOp::ChannelSplit) {
            for (port, channel) in [
                (0, Channel::R),
                (1, Channel::G),
                (2, Channel::B),
                (3, Channel::A),
            ] {
                if used_channels
                    .get(&(id, tick))
                    .is_some_and(|ports| ports.contains(&port))
                {
                    let value = b.push(
                        IrOp::NativeChannelExtract { channel },
                        vec![(primary.unwrap_or(fallback), OutPort::default())],
                    );
                    lowered.insert((id, tick, port), value);
                    if port == 0 && tick == root_tick {
                        b.view_index.insert((graph.id, id), value);
                    }
                }
            }
            continue;
        }
        let output = match &node.op {
            GraphOp::Output => primary.unwrap_or(clip_source.or(program).unwrap_or(fallback)),
            GraphOp::ClipIn => {
                let source = clip_source.ok_or("ClipIn is not valid in a native project graph")?;
                if tick == root_tick {
                    source
                } else if let Some(&cached) = retimed_clips.get(&tick) {
                    cached
                } else {
                    let context = source_context
                        .ok_or("native ClipIn TimeOffset requires a source context")?;
                    let value = native_retime_clip_source(b, context, source, tick, format)?;
                    retimed_clips.insert(tick, value);
                    value
                }
            }
            GraphOp::TimeOffset { .. } => primary.unwrap_or_else(|| b.transparent(format)),
            GraphOp::Switch => match native_switch_source(graph, node, tick)? {
                Some(source) => convert(
                    b,
                    source,
                    *lowered
                        .get(&(source.0, input_tick, source.1))
                        .ok_or("native Switch selected producer was not lowered")?,
                    false,
                ),
                None => b.transparent(format),
            },
            GraphOp::ChannelSplit => b.push(
                IrOp::NativeChannelExtract {
                    channel: Channel::R,
                },
                vec![(primary.unwrap_or(fallback), OutPort::default())],
            ),
            GraphOp::ChannelCombine => {
                let mut channels = Vec::new();
                for port in 0..4 {
                    let value = if let Some(input) = input(InPort(port)) {
                        input
                    } else {
                        let a = if port == 3 { 1.0 } else { 0.0 };
                        let image = b.push(
                            IrOp::SolidColor {
                                color: LinearColor {
                                    r: 0.0,
                                    g: 0.0,
                                    b: 0.0,
                                    a,
                                },
                            },
                            vec![],
                        );
                        b.push(
                            IrOp::NativeChannelExtract {
                                channel: Channel::A,
                            },
                            vec![(image, OutPort::default())],
                        )
                    };
                    channels.push((value, OutPort::default()));
                }
                b.push(IrOp::NativeChannelCombine, channels)
            }
            GraphOp::MediaIn { asset, time_source } => {
                if !node.params.base.0.entries.is_empty() || !node.params.tracks.is_empty() {
                    return Err("native MediaIn has no animatable node parameters".into());
                }
                let context =
                    source_context.ok_or("native MediaIn requires an owning source context")?;
                let op = native_media_in_op(context, *asset, *time_source, tick, format)?;
                let extent = match &op {
                    IrOp::NativeMediaVideo {
                        w,
                        h,
                        native_extent: true,
                        ..
                    }
                    | IrOp::NativeMediaStill {
                        w,
                        h,
                        native_extent: true,
                        ..
                    } => Some((*w, *h)),
                    _ => None,
                };
                let output = b.push(op, vec![]);
                if let Some((w, h)) = extent {
                    native_extents.insert(output);
                    if extent_hashes.insert(b.nodes[output.0 as usize].content_hash) {
                        let (bw, bh) = super::ir::TextureDesc {
                            width: w,
                            height: h,
                        }
                        .bucket();
                        extent_texels += u64::from(bw) * u64::from(bh);
                        if extent_texels > 16_777_216 {
                            return Err("native mixed-resolution graph exceeds its 16777216 padded-source-texel budget".into());
                        }
                    }
                }
                output
            }
            GraphOp::VectorIn { vref } => {
                if *vref != VectorRef::WholeDocument
                    || !node.params.base.0.entries.is_empty()
                    || !node.params.tracks.is_empty()
                {
                    return Err("native VectorIn requires a static whole-document reference without extra parameters".into());
                }
                b.push(
                    IrOp::NativeRasterVector {
                        vref: *vref,
                        doc_state: vector_state_key_for_ref(*vref, format, tick),
                        w: format.width,
                        h: format.height,
                    },
                    vec![],
                )
            }
            GraphOp::Text { text } => {
                text.validate_native().map_err(str::to_string)?;
                if !node.params.base.0.entries.is_empty() || !node.params.tracks.is_empty() {
                    return Err("native Text has a static payload; animated/extra node parameters are not qualified".into());
                }
                let content = TextClipContent {
                    text: text.text.clone(),
                    style: CaptionStyle {
                        font_family: text.font_family.clone(),
                        font_size: text.font_size,
                        fill: text.fill,
                        position: text.position,
                        stroke: None,
                        ..Default::default()
                    },
                };
                let block = resolve_text_block(&content);
                super::ir::validate_native_text_block(&block).map_err(str::to_string)?;
                b.push(IrOp::NativeTextGen { block }, vec![])
            }
            GraphOp::MaskFromMatte => {
                if !node.params.base.0.entries.is_empty() || !node.params.tracks.is_empty() {
                    return Err("native subject matte has no editable parameters".into());
                }
                if u64::from(format.width) * u64::from(format.height)
                    > photonic_matte::worker::MAX_INPUT_PIXELS as u64
                {
                    return Err("native subject matte supports at most four megapixels".into());
                }
                if let Some(input) = primary {
                    b.push(IrOp::NativeMatteExtract, vec![(input, OutPort::default())])
                } else {
                    let white = b.push(
                        IrOp::SolidColor {
                            color: LinearColor {
                                r: 1.0,
                                g: 1.0,
                                b: 1.0,
                                a: 1.0,
                            },
                        },
                        vec![],
                    );
                    b.push(
                        IrOp::NativeChannelExtract {
                            channel: Channel::A,
                        },
                        vec![(white, OutPort::default())],
                    )
                }
            }
            GraphOp::SolidColor => {
                let property = PropPath::new("params.color");
                let base = node
                    .params
                    .base
                    .0
                    .get("params.color")
                    .copied()
                    .unwrap_or(PropValue::Color(Color::BLACK));
                let value = node
                    .params
                    .track(&property)
                    .map_or(base, |track| timeline::eval(track, &base, tick));
                let color = match value {
                    PropValue::Color(c) => native_swatch_color(c).ok_or(
                        "native graph solid requires a finite encoded-sRGB swatch in 0..=1",
                    )?,
                    _ => return Err("native graph solid requires a color value".into()),
                };
                b.push(IrOp::SolidColor { color }, vec![])
            }
            GraphOp::Grade { grade } => {
                apply_grade(b, grade, primary.unwrap_or(fallback), tick, None)
            }
            GraphOp::Lut { asset } => apply_grade(
                b,
                &single_lut_grade(*asset),
                primary.unwrap_or(fallback),
                tick,
                None,
            ),
            GraphOp::Transform2D => {
                let defaults = ClipTransform::default();
                let transform = ClipTransform {
                    x: native_node_scalar(&node.params, "transform.x", defaults.x, tick)?,
                    y: native_node_scalar(&node.params, "transform.y", defaults.y, tick)?,
                    scale_x: native_node_scalar(
                        &node.params,
                        "transform.scale_x",
                        defaults.scale_x,
                        tick,
                    )?,
                    scale_y: native_node_scalar(
                        &node.params,
                        "transform.scale_y",
                        defaults.scale_y,
                        tick,
                    )?,
                    rotation: native_node_scalar(
                        &node.params,
                        "transform.rotation",
                        defaults.rotation,
                        tick,
                    )?,
                    anchor_x: native_node_scalar(
                        &node.params,
                        "transform.anchor_x",
                        defaults.anchor_x,
                        tick,
                    )?,
                    anchor_y: native_node_scalar(
                        &node.params,
                        "transform.anchor_y",
                        defaults.anchor_y,
                        tick,
                    )?,
                    anchor_space: AnchorSpace::CenterOffset,
                    opacity: 1.0,
                };
                let mat = clip_transform_matrix(&transform, format);
                if !mat.is_finite() || mat.determinant().abs() < 1e-12 {
                    return Err("native graph transform must be finite and invertible".into());
                }
                b.push(
                    IrOp::Transform2DTransparent {
                        mat,
                        sampling: Sampling::Bilinear,
                    },
                    vec![(primary.unwrap_or(fallback), OutPort::default())],
                )
            }
            GraphOp::Resize { fit } => {
                // Resize dimensions are explicit intermediate pixel dimensions,
                // scaled with the processing canvas for Draft evaluation.
                // Preserve premultiplied AP1; never apply a transfer.
                let width =
                    native_node_scalar(&node.params, "params.width", format.width as f64, tick)?;
                let height =
                    native_node_scalar(&node.params, "params.height", format.height as f64, tick)?;
                if !(1.0..=8192.0).contains(&width)
                    || !(1.0..=8192.0).contains(&height)
                    || width.fract() != 0.0
                    || height.fract() != 0.0
                    || width * height > 16_777_216.0
                {
                    return Err("native graph resize requires integer dimensions 1..=8192 and at most 16777216 pixels".into());
                }
                let resized = b.push(
                    IrOp::Resize {
                        w: width as u32,
                        h: height as u32,
                        fit: map_fit(*fit),
                    },
                    vec![(primary.unwrap_or(fallback), OutPort::default())],
                );
                // Native composition stages share a fixed sequence canvas.
                // Normalize the resized logical extent before downstream grade,
                // spatial filters or output can sample bucket-padding as pixels.
                b.push(
                    IrOp::Resize {
                        w: format.width,
                        h: format.height,
                        fit: FitMode::Stretch,
                    },
                    vec![(resized, OutPort::default())],
                )
            }
            GraphOp::Crop => {
                let mut margins = [0.0_f32; 4];
                for (margin, path) in margins.iter_mut().zip([
                    "params.left",
                    "params.top",
                    "params.right",
                    "params.bottom",
                ]) {
                    let value = native_node_scalar(&node.params, path, 0.0, tick)?;
                    if !(0.0..=1.0).contains(&value) {
                        return Err("native graph crop margins must lie in 0..=1".into());
                    }
                    *margin = value as f32;
                }
                b.push(
                    IrOp::Crop {
                        left: margins[0],
                        top: margins[1],
                        right: margins[2],
                        bottom: margins[3],
                    },
                    vec![(primary.unwrap_or(fallback), OutPort::default())],
                )
            }
            GraphOp::MaskShape { shape } => {
                let rect = matches!(shape, timeline::MaskShapeKind::Rect);
                let params = resolve_effect_params(
                    EffectKind::MaskShapeGen,
                    &node.params.base.0,
                    &node.params,
                    tick,
                );
                let mut op = native_shape_op(&params, rect)?;
                if let timeline::MaskShapeKind::Polygon { vertices } = shape {
                    shape.validate_native().map_err(str::to_string)?;
                    let allowed = [
                        "params.center_x",
                        "params.center_y",
                        "params.size_x",
                        "params.size_y",
                        "params.rotation",
                        "params.feather",
                    ];
                    if node
                        .params
                        .base
                        .0
                        .entries
                        .iter()
                        .any(|(path, _)| !allowed.contains(&path.0.as_str()))
                        || node
                            .params
                            .tracks
                            .iter()
                            .any(|track| !allowed.contains(&track.property.0.as_str()))
                    {
                        return Err("native polygon supports only center, size, rotation and feather animation; vertices are static".into());
                    }
                    if let IrOp::NativeMaskShape {
                        center,
                        size,
                        rotation,
                        feather,
                        ..
                    } = op
                    {
                        op = IrOp::NativePolygonMask {
                            vertices: vertices.clone(),
                            center,
                            size,
                            rotation,
                            feather,
                        };
                    }
                }
                b.push(op, vec![])
            }
            GraphOp::Glow => {
                let params = resolve_effect_params(
                    EffectKind::Glow,
                    &node.params.base.0,
                    &node.params,
                    tick,
                );
                b.push(
                    native_glow_op(&params)?,
                    vec![(primary.unwrap_or(fallback), OutPort::default())],
                )
            }
            GraphOp::ChromaKey => {
                let params = resolve_effect_params(
                    EffectKind::ChromaKey,
                    &node.params.base.0,
                    &node.params,
                    tick,
                );
                b.push(
                    native_chroma_key_op(&params)?,
                    vec![(primary.unwrap_or(fallback), OutPort::default())],
                )
            }
            GraphOp::LumaKey => {
                let params = resolve_effect_params(
                    EffectKind::LumaKey,
                    &node.params.base.0,
                    &node.params,
                    tick,
                );
                b.push(
                    native_luma_key_op(&params)?,
                    vec![(primary.unwrap_or(fallback), OutPort::default())],
                )
            }
            GraphOp::Invert => b.push(
                IrOp::NativeInvert,
                vec![(primary.unwrap_or(fallback), OutPort::default())],
            ),
            GraphOp::Sharpen => {
                let amount = native_node_scalar(&node.params, "params.amount", 0.0, tick)?;
                let sigma = native_node_scalar(&node.params, "params.radius", 0.0, tick)?;
                if !(0.0..=10.0).contains(&amount) || !(0.0..=100.0).contains(&sigma) {
                    return Err(
                        "native graph unsharp requires amount 0..=10 and sigma 0..=100".into(),
                    );
                }
                b.push(
                    IrOp::NativeUnsharp {
                        amount: amount as f32,
                        sigma: sigma as f32,
                    },
                    vec![(primary.unwrap_or(fallback), OutPort::default())],
                )
            }
            GraphOp::Blur => {
                let params = resolve_effect_params(
                    EffectKind::Blur,
                    &node.params.base.0,
                    &node.params,
                    tick,
                );
                let sigma = match params.get("params.radius") {
                    Some(PropValue::Float(value))
                        if value.is_finite() && (0.0..=500.0).contains(value) =>
                    {
                        *value as f32
                    }
                    _ => {
                        return Err("native graph Gaussian radius must be finite in 0..=500".into())
                    }
                };
                b.push(
                    IrOp::NativeGaussianBlur { sigma },
                    vec![(primary.unwrap_or(fallback), OutPort::default())],
                )
            }
            GraphOp::Merge { mode } => {
                let bottom = input(InPort::B);
                let opacity = native_node_scalar(&node.params, "params.opacity", 1.0, tick)? as f32;
                if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
                    return Err("native graph opacity must be finite in 0..=1".into());
                }
                match (primary, bottom) {
                    (Some(top), bottom) => b.push(
                        IrOp::Merge {
                            mode: *mode,
                            opacity,
                        },
                        vec![
                            (top, OutPort::default()),
                            (bottom.unwrap_or(fallback), OutPort::default()),
                        ],
                    ),
                    (None, bottom) => bottom.unwrap_or(fallback),
                }
            }
            _ => unreachable!("reachable nodes were qualified before lowering"),
        };
        let output = if matches!(node.op, GraphOp::MaskShape { .. }) {
            b.push(
                IrOp::NativeChannelExtract {
                    channel: Channel::A,
                },
                vec![(output, OutPort::default())],
            )
        } else {
            output
        };
        lowered.insert((id, tick, 0), output);
        if tick == root_tick {
            b.view_index.insert((graph.id, id), output);
        }
    }
    Ok(lowered[&(graph.output, root_tick, 0)])
}

// ── Step 3 + 7: composition / node-graph lowering ─────────────────────────────

/// Lower a per-clip composition (08 §4): instantiate the graph, bind `ClipIn`
/// to the clip's source op, and return the node feeding `Output`. On a
/// missing-`Output`-input / cycle / type error, fall back to the plain source
/// and surface a diagnostic (02 §2 step 3, 08 §3.3 `Output` row).
#[allow(clippy::too_many_arguments)]
fn lower_composition(
    b: &mut Builder<'_>,
    project: &TimelineProject,
    seq: &Sequence,
    format_index: usize,
    format: &SequenceFormat,
    clip: &Clip,
    graph_id: GraphId,
    tick: Tick,
    quality: Quality,
    cycle: &mut HashSet<SequenceId>,
) -> IrNodeId {
    let Some(graph) = project.graphs.get(&graph_id) else {
        b.diag(CompileDiagnostic::plain(format!(
            "clip composition {graph_id} not found; using plain source"
        )));
        return build_clip_source(
            b,
            project,
            seq,
            format_index,
            format,
            clip,
            tick,
            quality,
            cycle,
        );
    };

    let mut lc = LowerCtx {
        project,
        seq,
        format_index,
        format,
        quality,
        graph,
        clip: Some(clip),
        is_project_graph: false,
        program: None,
        offsets: HashSet::new(),
        memo: HashMap::new(),
    };
    match lower_output(b, &mut lc, tick, cycle) {
        Some(node) => node,
        None => {
            b.diag(CompileDiagnostic::at(
                graph_id,
                graph.output,
                "composition Output has no input; falling back to plain clip source",
            ));
            build_clip_source(
                b,
                project,
                seq,
                format_index,
                format,
                clip,
                tick,
                quality,
                cycle,
            )
        }
    }
}

/// Step 6: splice the project graph between the fold result and `Output` (08 §5).
/// The project graph has no `ClipIn`; the program (fold result) enters wherever a
/// primary filter input is left unwired — so a bare `Grade → Output` or
/// `Vignette` graph applies to the final composite. An empty project graph
/// (`Output` with nothing wired) is a passthrough. A missing `Output` input with
/// no program to fall back on skips the splice (08 §3.3 `Output` row).
fn splice_project_graph(
    b: &mut Builder<'_>,
    project: &TimelineProject,
    program: Option<IrNodeId>,
    format: &SequenceFormat,
    tick: Tick,
) -> Option<IrNodeId> {
    let Some(graph_id) = project.project_graph else {
        return program;
    };
    let Some(graph) = project.graphs.get(&graph_id) else {
        b.diag(CompileDiagnostic::plain(format!(
            "project graph {graph_id} not found; skipping splice"
        )));
        return program;
    };

    // The active sequence is normally present when a project graph splice runs
    // (the engine only compiles a real sequence). But `project` is deserialized
    // data — a project file could carry a `project_graph` with no sequences at
    // all — so fall back to any sequence and, failing that, log-and-skip the
    // splice rather than panic.
    let Some(seq) = project
        .active_sequence
        .and_then(|id| project.sequences.get(&id))
        // Deterministic fallback: pick the first sequence in insertion order, not
        // an arbitrary HashMap iteration (which would splice a different sequence
        // run-to-run for the same project file).
        .or_else(|| {
            project
                .sequence_order
                .first()
                .and_then(|id| project.sequences.get(id))
        })
    else {
        b.diag(CompileDiagnostic::plain(
            "project graph splice requires at least one sequence; skipping splice".to_string(),
        ));
        return program;
    };

    let mut cycle = HashSet::new();
    let mut lc = LowerCtx {
        project,
        seq,
        format_index: 0,
        format,
        quality: Quality::FULL,
        graph,
        clip: None,
        is_project_graph: true,
        program,
        offsets: HashSet::new(),
        memo: HashMap::new(),
    };
    match lower_output(b, &mut lc, tick, &mut cycle) {
        Some(node) => Some(node),
        None => {
            // Empty / unsatisfied project graph: skip the splice entirely.
            program
        }
    }
}

/// Per-instantiation lowering context for one graph (composition or project).
struct LowerCtx<'a> {
    project: &'a TimelineProject,
    seq: &'a Sequence,
    format_index: usize,
    format: &'a SequenceFormat,
    quality: Quality,
    graph: &'a NodeGraph,
    /// The host clip (`ClipIn` binds to its source); `None` for the project graph.
    clip: Option<&'a Clip>,
    is_project_graph: bool,
    /// The upstream program (fold result); the project-graph's unwired-input default.
    program: Option<IrNodeId>,
    /// Distinct `TimeOffset` values seen (soft-cap diagnostic, step 7).
    offsets: HashSet<i64>,
    /// Memo of `(node, eval-tick)` → IR id within this instantiation.
    memo: HashMap<(GraphNodeId, i64), IrNodeId>,
}

/// Lower the graph's `Output` node's single input at `tick`. Returns `None` when
/// `Output` has no wired input (08 §3.3 `Output` row).
fn lower_output(
    b: &mut Builder<'_>,
    lc: &mut LowerCtx,
    tick: Tick,
    cycle: &mut HashSet<SequenceId>,
) -> Option<IrNodeId> {
    let output_id = lc.graph.output;
    let src = input_source(lc.graph, output_id, InPort::PRIMARY)?;
    Some(lower_node(b, lc, src, tick, cycle))
}

/// Find the node feeding `(node, port)` in `graph`'s edge list.
fn input_source(graph: &NodeGraph, node: GraphNodeId, port: InPort) -> Option<GraphNodeId> {
    graph
        .edges
        .iter()
        .find(|e| e.to.0 == node && e.to.1 == port)
        .map(|e| e.from.0)
}

/// Lower one graph node at evaluation time `tick`, memoized per `(node, tick)`.
fn lower_node(
    b: &mut Builder<'_>,
    lc: &mut LowerCtx,
    node_id: GraphNodeId,
    tick: Tick,
    cycle: &mut HashSet<SequenceId>,
) -> IrNodeId {
    let key = (node_id, tick.0);
    if let Some(&id) = lc.memo.get(&key) {
        return id;
    }
    let Some(node) = lc.graph.nodes.get(&node_id) else {
        return b.transparent(lc.format);
    };

    let id = lower_node_uncached(b, lc, node, tick, cycle);
    lc.memo.insert(key, id);
    b.view_index.insert((lc.graph.id, node_id), id);
    id
}

fn lower_node_uncached(
    b: &mut Builder<'_>,
    lc: &mut LowerCtx,
    node: &GraphNode,
    tick: Tick,
    cycle: &mut HashSet<SequenceId>,
) -> IrNodeId {
    // Resolve the primary input, honouring the missing-input defaults (08 §3.3).
    let primary = || -> Option<GraphNodeId> { input_source(lc.graph, node.id, InPort::PRIMARY) };

    match &node.op {
        GraphOp::Output => {
            // Nested Output is unusual; treat like passthrough of its input.
            match primary() {
                Some(src) => lower_node(b, lc, src, tick, cycle),
                None => b.transparent(lc.format),
            }
        }
        GraphOp::ClipIn => {
            // Bind to the host clip's source op at the (possibly offset) tick.
            match lc.clip {
                Some(clip) => build_clip_source(
                    b,
                    lc.project,
                    lc.seq,
                    lc.format_index,
                    lc.format,
                    clip,
                    tick,
                    lc.quality,
                    cycle,
                ),
                None => {
                    // ClipIn is invalid in the project graph (08 §5): drop it.
                    b.diag(CompileDiagnostic::at(
                        lc.graph.id,
                        node.id,
                        "ClipIn is invalid in the project graph; substituting transparent",
                    ));
                    b.transparent(lc.format)
                }
            }
        }
        GraphOp::MediaIn { asset, time_source } => {
            let kind = lc
                .project
                .media
                .assets
                .get(asset)
                .map(|a| a.kind)
                .unwrap_or(AssetKind::Video);
            match kind {
                AssetKind::Image => b.push(IrOp::DecodeStill { asset: *asset }, vec![]),
                _ => {
                    let src_time = match time_source {
                        TimeSource::Local => lc.clip.map(|clip| tick - clip.start).unwrap_or(tick),
                        TimeSource::Sequence => tick,
                    };
                    b.push(
                        IrOp::DecodeVideo {
                            asset: *asset,
                            src_time,
                            proxy: lc.quality.proxy,
                        },
                        vec![],
                    )
                }
            }
        }
        GraphOp::VectorIn { vref } => b.push(
            IrOp::RasterVector {
                vref: *vref,
                doc_state: vector_state_key_for_ref(*vref, lc.format, tick),
                w: lc.format.width,
                h: lc.format.height,
            },
            vec![],
        ),
        GraphOp::SolidColor => {
            let color = eval_node_color(&node.params, "params.color", Color::BLACK, tick);
            b.push(
                IrOp::SolidColor {
                    color: color_to_linear_premult(color),
                },
                vec![],
            )
        }
        GraphOp::Merge { mode } => {
            let a = input_source(lc.graph, node.id, InPort::A)
                .map(|s| lower_node(b, lc, s, tick, cycle));
            let bottom = input_source(lc.graph, node.id, InPort::B)
                .map(|s| lower_node(b, lc, s, tick, cycle));
            let opacity = eval_node_f32(&node.params, "params.opacity", 1.0, tick);
            match (a, bottom) {
                // Missing a → passthrough b; missing b → passthrough a (08 §3.3).
                (Some(a), Some(bt)) => b.push(
                    IrOp::Merge {
                        mode: *mode,
                        opacity,
                    },
                    vec![(a, OutPort::default()), (bt, OutPort::default())],
                ),
                (Some(a), None) => {
                    let bt = project_default_or_transparent(b, lc);
                    b.push(
                        IrOp::Merge {
                            mode: *mode,
                            opacity,
                        },
                        vec![(a, OutPort::default()), (bt, OutPort::default())],
                    )
                }
                (None, Some(bt)) => bt,
                (None, None) => project_default_or_transparent(b, lc),
            }
        }
        GraphOp::Transform2D => {
            let input = lower_primary_or_default(b, lc, primary(), tick, cycle);
            b.push(
                IrOp::Transform2D {
                    mat: graph_node_transform_matrix(&node.params, tick, lc.format),
                    sampling: Sampling::Bilinear,
                },
                vec![(input, OutPort::default())],
            )
        }
        GraphOp::Crop => {
            let input = lower_primary_or_default(b, lc, primary(), tick, cycle);
            b.push(
                IrOp::Crop {
                    left: eval_node_f32(&node.params, "params.left", 0.0, tick),
                    top: eval_node_f32(&node.params, "params.top", 0.0, tick),
                    right: eval_node_f32(&node.params, "params.right", 0.0, tick),
                    bottom: eval_node_f32(&node.params, "params.bottom", 0.0, tick),
                },
                vec![(input, OutPort::default())],
            )
        }
        GraphOp::Resize { fit } => {
            let input = lower_primary_or_default(b, lc, primary(), tick, cycle);
            b.push(
                IrOp::Resize {
                    w: eval_node_dimension(&node.params, "params.width", lc.format.width, tick),
                    h: eval_node_dimension(&node.params, "params.height", lc.format.height, tick),
                    fit: map_fit(*fit),
                },
                vec![(input, OutPort::default())],
            )
        }
        GraphOp::Grade { grade } => {
            let input = lower_primary_or_default(b, lc, primary(), tick, cycle);
            apply_grade(b, grade, input, tick, None)
        }
        GraphOp::Lut { asset } => {
            // 08 §2: `Lut` lowers to a single-op `Grade{Lut3d}` — one mechanism,
            // not a parallel LUT path. In P3 the LUT asset resolves inert (no pool
            // at compile), so this is a passthrough until the lut_provider reaches
            // compile; the chain shape is correct now.
            let input = lower_primary_or_default(b, lc, primary(), tick, cycle);
            apply_grade(b, &single_lut_grade(*asset), input, tick, None)
        }
        GraphOp::Text { .. } => {
            // 08 §2: `Text` lowers to the dedicated `TextGen` IR op (a 0-input
            // styled-text generator). The glyphon raster is P8 (blocked on the
            // still-opaque `ResolvedTextBlock` payload); the evaluator emits a
            // transparent placeholder meanwhile.
            b.push(
                IrOp::TextGen {
                    block: ResolvedTextBlock::default(),
                },
                vec![],
            )
        }
        GraphOp::MaskFromMatte => {
            // 08 §2: lowers to the dedicated `MatteExtract` IR op (U²-Net subject
            // cutout). CPU inference is P8; the evaluator passes the input through
            // (a no-op/opaque mask) until photonic-matte is wired.
            let input = lower_primary_or_default(b, lc, primary(), tick, cycle);
            b.push(
                IrOp::MatteExtract {
                    model: MatteModel::U2NetP,
                },
                vec![(input, OutPort::default())],
            )
        }
        GraphOp::ChannelSplit => {
            // 08 §2: dedicated `ChannelSplit` IR op. The current single-output
            // lowering can't yet route the four (r/g/b/a) out-ports independently,
            // so it emits the alpha channel — the canonical alpha-as-mask output
            // (08 §2 note). Per-out-port routing lands with multi-output lowering.
            let input = lower_primary_or_default(b, lc, primary(), tick, cycle);
            b.push(
                IrOp::ChannelSplit {
                    channel: Channel::A,
                },
                vec![(input, OutPort::default())],
            )
        }
        GraphOp::ChannelCombine => {
            // 08 §2: dedicated `ChannelCombine` IR op. Full four-mask (r/g/b/a)
            // wiring needs per-in-port lowering; P3 feeds the primary input and
            // defaults the rest, matching the evaluator's passthrough.
            let input = lower_primary_or_default(b, lc, primary(), tick, cycle);
            b.push(IrOp::ChannelCombine, vec![(input, OutPort::default())])
        }
        GraphOp::TimeOffset { offset } => {
            // Step 7: re-lower the upstream subgraph at t − offset. Identical
            // (subgraph, time) dedups via content hash; distinct offsets are the
            // only cost. Soft-cap the distinct-offset count with a diagnostic.
            lc.offsets.insert(offset.0);
            if lc.offsets.len() > TIME_OFFSET_SOFT_CAP {
                b.diag(CompileDiagnostic::at(
                    lc.graph.id,
                    node.id,
                    format!(
                        "more than {TIME_OFFSET_SOFT_CAP} distinct TimeOffset values in one \
                         composition; echo/trail cost grows with each"
                    ),
                ));
            }
            let shifted = tick - *offset;
            match primary() {
                Some(src) => lower_node(b, lc, src, shifted, cycle),
                None => b.transparent(lc.format),
            }
        }
        GraphOp::Switch => {
            // `selected` resolves at compile time; P3 picks the primary input
            // (or first connected) — full selected-index eval lands with node
            // params (P8).
            match primary().or_else(|| {
                lc.graph
                    .edges
                    .iter()
                    .find(|e| e.to.0 == node.id)
                    .map(|e| e.from.0)
            }) {
                Some(src) => lower_node(b, lc, src, tick, cycle),
                None => b.transparent(lc.format),
            }
        }
        GraphOp::Note { .. } => {
            // Pure annotation, never compiled (08 §2); should be unreachable as an
            // ancestor of Output, but be defensive.
            b.transparent(lc.format)
        }
        // Filter/generator effects that lower to `IrOp::Effect`. `Invert` is a
        // real evaluator pass (08 §3); the rest keep arity + ordering +
        // content-hash identity as marker nodes until their `ResolvedParams`
        // payload finalizes (P5/P7). `MaskShape` is a 0-input generator; P3 still
        // routes the missing-input default through it (harmless — the evaluator
        // ignores it), pending generator-arity lowering.
        GraphOp::Blur
        | GraphOp::Sharpen
        | GraphOp::Glow
        | GraphOp::ChromaKey
        | GraphOp::LumaKey
        | GraphOp::Invert
        | GraphOp::MaskShape { .. } => {
            let input = lower_primary_or_default(b, lc, primary(), tick, cycle);
            let kind = graph_op_effect_kind(&node.op);
            b.push(
                IrOp::Effect {
                    kind,
                    params: resolve_effect_params(kind, &node.params.base.0, &node.params, tick),
                },
                vec![(input, OutPort::default())],
            )
        }
        GraphOp::Unknown(_) => {
            // Forward-compat (39 §2.2): an op this build does not understand
            // lowers to passthrough of its primary input (an inert unary
            // filter), or the missing-input default when unwired — never
            // guessed. The original `op` object is retained verbatim in the
            // model.
            b.diag(CompileDiagnostic::at(
                lc.graph.id,
                node.id,
                "unknown graph op renders as passthrough (this build does not \
                 understand it)",
            ));
            lower_primary_or_default(b, lc, primary(), tick, cycle)
        }
    }
}

/// Build a single-op [`Grade`] carrying one `Lut3d` op for the `Lut` graph node
/// (08 §2 — `Lut` is a one-op grade, not a parallel path).
fn single_lut_grade(asset: AssetId) -> Grade {
    let mut grade = Grade::new();
    grade.ops.push(GradeOp::new(
        GradeOpKind::Lut3d,
        GradeOpParams::Lut3d {
            asset,
            intensity: 1.0,
            interp: LutInterp::Trilinear,
        },
    ));
    grade
}

/// Lower a unary op's primary input, or the missing-input default: transparent
/// black for a composition, the program for the project graph (08 §3.3 unary row
/// / §5 program-splice).
fn lower_primary_or_default(
    b: &mut Builder<'_>,
    lc: &mut LowerCtx,
    primary: Option<GraphNodeId>,
    tick: Tick,
    cycle: &mut HashSet<SequenceId>,
) -> IrNodeId {
    match primary {
        Some(src) => lower_node(b, lc, src, tick, cycle),
        None => project_default_or_transparent(b, lc),
    }
}

/// The unwired-input default: the program (fold result) in the project graph,
/// transparent black otherwise.
fn project_default_or_transparent(b: &mut Builder<'_>, lc: &LowerCtx) -> IrNodeId {
    if lc.is_project_graph {
        if let Some(program) = lc.program {
            return program;
        }
    }
    b.push(
        IrOp::SolidColor {
            color: LinearColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
        },
        vec![],
    )
}

fn map_fit(fit: photonic_core::timeline::FitMode) -> FitMode {
    use photonic_core::timeline::FitMode as G;
    match fit {
        G::Stretch => FitMode::Stretch,
        G::Contain => FitMode::Fit,
        G::Cover => FitMode::Fill,
    }
}

/// Map a filter/generator `GraphOp` to its `EffectKind` for the `IrOp::Effect`
/// lowering (08 §2). Only the effect-family ops reach here; the non-effect
/// catalog entries (Grade/Lut/Text/MaskFromMatte/Channel*) lower to their own
/// dedicated IR ops in `lower_node_uncached`.
fn graph_op_effect_kind(op: &GraphOp) -> EffectKind {
    match op {
        GraphOp::Blur => EffectKind::Blur,
        GraphOp::Sharpen => EffectKind::Sharpen,
        GraphOp::Glow => EffectKind::Glow,
        GraphOp::ChromaKey => EffectKind::ChromaKey,
        GraphOp::LumaKey => EffectKind::LumaKey,
        GraphOp::Invert => EffectKind::Invert,
        GraphOp::MaskShape { .. } => EffectKind::MaskShapeGen,
        // Unreachable: only the effect-family ops above call this.
        _ => EffectKind::Blur,
    }
}

// ── Caption overlay (step 5) ──────────────────────────────────────────────────

/// Emit one `CaptionOverlay` per enabled caption track with a cue covering `t`
/// (06 §5.3: one node per active track per compiled frame). Each node carries a
/// [`CaptionBatch`] whose words are fully cascade-resolved and whose
/// karaoke/animation state is baked at this tick — so the evaluator stays
/// time-ignorant (02 §2). Tracks with no covering cue contribute nothing; a
/// `None` program (captions over an empty sequence) roots on transparent black.
fn splice_captions(
    b: &mut Builder<'_>,
    seq: &Sequence,
    format: &SequenceFormat,
    tick: Tick,
    program: Option<IrNodeId>,
) -> Option<IrNodeId> {
    let mut cur = program;
    for track in &seq.caption_tracks {
        if !track.enabled {
            continue;
        }
        let batch = resolve_caption_batch(track, tick, format);
        if batch.cues.is_empty() {
            continue;
        }
        let input = cur.unwrap_or_else(|| b.transparent(format));
        cur = Some(b.push(
            IrOp::CaptionOverlay { cue_batch: batch },
            vec![(input, OutPort::default())],
        ));
    }
    cur
}

/// Resolve a caption track's cues covering `tick` into a [`CaptionBatch`] of
/// positioned, styled, karaoke-resolved word runs for the render text pipeline
/// (06 §5). Cues are non-overlapping (01 §4), so v1 collects at most one — the
/// loop stays general. Deterministic in `tick` (no wall-clock).
/// Shared bounds for native title/caption decoration and GUI qualification.
pub fn native_text_decoration(
    style: &CaptionStyle,
) -> Result<photonic_render::caption_decoration::CaptionDecoration, String> {
    let stroke = style
        .stroke
        .map(|(color, width)| {
            if native_swatch_color(color).is_none() {
                return Err("native outline color must be finite encoded sRGB in 0..=1".to_string());
            }
            Ok((color_to_srgb_bytes(color), width))
        })
        .transpose()?;
    let background = style
        .background
        .map(|b| {
            if native_swatch_color(b.color).is_none() {
                return Err(
                    "native background color must be finite encoded sRGB in 0..=1".to_string(),
                );
            }
            Ok((color_to_srgb_bytes(b.color), b.padding, b.corner_radius))
        })
        .transpose()?;
    let mut decoration =
        photonic_render::caption_decoration::CaptionDecoration { stroke, background };
    decoration.validate().map_err(str::to_string)?;
    if decoration
        .stroke
        .is_some_and(|(c, w)| c[3] == 0 || w == 0.0)
    {
        decoration.stroke = None;
    }
    if decoration.background.is_some_and(|(c, _, _)| c[3] == 0) {
        decoration.background = None;
    }
    Ok(decoration)
}

fn resolve_native_caption_batch(
    seq: &Sequence,
    tick: Tick,
    format: &SequenceFormat,
) -> Result<
    (
        CaptionBatch,
        Vec<photonic_render::caption_decoration::CaptionPaint>,
    ),
    String,
> {
    let mut batch = CaptionBatch::default();
    let mut paints = Vec::new();
    let (mut cue_count, mut word_count, mut text_bytes) = (0usize, 0usize, 0usize);
    for track in seq.caption_tracks.iter().filter(|t| t.enabled) {
        for cue in track
            .cues
            .iter()
            .filter(|c| c.start <= tick && tick < c.end)
        {
            cue_count += 1;
            word_count += cue.words.len();
            text_bytes += cue.words.iter().map(|w| w.text.len()).sum::<usize>();
            if cue_count > 16
                || word_count > 128
                || text_bytes > 4096
                || cue.end.0.checked_sub(cue.start.0).is_none()
                || cue
                    .position_override
                    .is_some_and(|p| p.iter().any(|x| !x.is_finite() || !(0.0..=1.0).contains(x)))
                || cue.words.iter().any(|w| {
                    w.end <= w.start
                        || w.start < cue.start
                        || w.end > cue.end
                        || w.start.0.checked_sub(ms_to_ticks(150)).is_none()
                })
            {
                return Err("native captions require bounded text, finite positions and valid cue-local word times".into());
            }
            let base = cue.style_override.as_ref().unwrap_or(&track.style);
            for style in std::iter::once(base)
                .chain(cue.words.iter().filter_map(|w| w.style_override.as_ref()))
            {
                if style.stroke != base.stroke
                    || style.background != base.background
                    || style.highlight.is_some_and(|k| {
                        native_swatch_color(k.active_color).is_none()
                            || native_swatch_color(k.inactive_color).is_none()
                    })
                    || style.animation != base.animation
                    || native_swatch_color(style.fill).is_none()
                    || style.font_family.is_empty()
                    || style.font_family.len() > 128
                    || !(1..=1000).contains(&style.weight)
                    || style
                        .position
                        .iter()
                        .any(|x| !x.is_finite() || !(0.0..=1.0).contains(x))
                    || !style.max_width.is_finite()
                    || !(0.001..=1.0).contains(&style.max_width)
                    || !style.font_size.is_finite()
                    || !(1.0..=512.0).contains(&style.font_size)
                    || (style.direction != timeline::TextDirection::Auto
                        && style.direction != base.direction)
                    || style.font_size != base.font_size
                    || style.position != base.position
                    || style.max_width != base.max_width
                {
                    return Err("native captions require bounded paint and uniform cue layout/animation/decorations".into());
                }
            }
            let decoration = native_text_decoration(base)?;
            let has_underline = std::iter::once(base)
                .chain(cue.words.iter().filter_map(|w| w.style_override.as_ref()))
                .any(|style| {
                    style
                        .highlight
                        .is_some_and(|h| h.mode == KaraokeMode::Underline)
                });
            let has_sweep = std::iter::once(base)
                .chain(cue.words.iter().filter_map(|w| w.style_override.as_ref()))
                .any(|s| {
                    s.highlight
                        .is_some_and(|k| k.mode == KaraokeMode::FillSweep)
                });
            let grouped_slide = base.animation == CaptionAnim::SlideUp
                && (decoration != Default::default() || has_underline || has_sweep);
            let opacity = if grouped_slide {
                ((tick - cue.start).0 as f32 / ms_to_ticks(200).max(1) as f32).clamp(0.0, 1.0)
            } else {
                1.0
            };
            if let Some(mut run) = resolve_cue(track, cue, tick, format) {
                // A decorated SlideUp fades the complete cue once, not each overlapping paint layer.
                if grouped_slide {
                    for (word, authored) in run.words.iter_mut().zip(&cue.words) {
                        let style = authored.style_override.as_ref().unwrap_or(base);
                        word.color = karaoke_color(style, authored, tick);
                    }
                }
                let word_opacity = if base.animation == CaptionAnim::FadeWords {
                    cue.words
                        .iter()
                        .map(|w| (fade_word_opacity(w, tick) * 255.0).round() as u8)
                        .collect()
                } else {
                    Vec::new()
                };
                let underlines = cue
                    .words
                    .iter()
                    .enumerate()
                    .filter_map(|(index, word)| {
                        let style = word.style_override.as_ref().unwrap_or(base);
                        let highlight = style.highlight?;
                        if highlight.mode != KaraokeMode::Underline
                            || tick < word.start
                            || tick >= word.end
                            || run.words[index].text.is_empty()
                        {
                            return None;
                        }
                        let (color, width) = style.stroke.unwrap_or((
                            highlight.active_color,
                            (base.font_size * 0.05).clamp(1.0, 8.0),
                        ));
                        let mut color = color_to_srgb_bytes(color);
                        if base.animation == CaptionAnim::FadeWords {
                            color[3] =
                                (color[3] as f32 * fade_word_opacity(word, tick)).round() as u8;
                        }
                        Some(photonic_render::caption_decoration::CaptionUnderline {
                            word: index,
                            color,
                            width,
                        })
                    })
                    .collect();
                let mut sweeps = Vec::new();
                for (index, word) in cue.words.iter().enumerate() {
                    let style = word.style_override.as_ref().unwrap_or(base);
                    let Some(highlight) =
                        style.highlight.filter(|k| k.mode == KaraokeMode::FillSweep)
                    else {
                        continue;
                    };
                    let alpha = if base.animation == CaptionAnim::FadeWords {
                        fade_word_opacity(word, tick)
                    } else {
                        1.0
                    };
                    let color = |value| {
                        let mut c = color_to_srgb_bytes(value);
                        c[3] = (c[3] as f32 * alpha).round() as u8;
                        c
                    };
                    if caption_sweep_word_pop(base.direction, &word.text) {
                        run.words[index].color = color(if word.start <= tick && tick < word.end {
                            highlight.active_color
                        } else {
                            highlight.inactive_color
                        });
                    } else if tick <= word.start {
                        run.words[index].color = color(highlight.inactive_color)
                    } else if tick >= word.end {
                        run.words[index].color = color(highlight.active_color)
                    } else {
                        sweeps.push(photonic_render::caption_decoration::CaptionSweep {
                            word: index,
                            fraction: ((tick - word.start).0 as f64
                                / (word.end - word.start).0 as f64)
                                as f32,
                            active: color(highlight.active_color),
                            inactive: color(highlight.inactive_color),
                        });
                        // Keep a stable word span; the evaluator suppresses this glyph fill.
                        run.words[index].color = [0; 4];
                    }
                }
                paints.push(photonic_render::caption_decoration::CaptionPaint {
                    decoration,
                    word_opacity,
                    opacity,
                    underlines,
                    sweeps,
                });
                batch.cues.push(run);
            }
        }
    }
    super::ir::validate_native_caption_batch(&batch).map_err(str::to_string)?;
    if paints.iter().all(|p| {
        p.decoration == Default::default()
            && p.underlines.is_empty()
            && p.sweeps.is_empty()
            && p.opacity == 1.0
    }) {
        paints.clear();
    }
    Ok((batch, paints))
}

fn resolve_caption_batch(
    track: &CaptionTrack,
    tick: Tick,
    format: &SequenceFormat,
) -> CaptionBatch {
    let mut cues = Vec::new();
    for cue in &track.cues {
        if cue.start <= tick && tick < cue.end {
            if let Some(run) = resolve_cue(track, cue, tick, format) {
                cues.push(run);
            }
        }
    }
    CaptionBatch { cues }
}

/// One stable notice per affected cue; consumers display/record it once rather
/// than emitting a new notification for each rendered frame or word.
pub fn caption_sweep_direction_note(track: &CaptionTrack, cue: &CaptionCue) -> Option<String> {
    let base = cue.style_override.as_ref().unwrap_or(&track.style);
    cue.words
        .iter()
        .any(|w| {
            w.style_override
                .as_ref()
                .unwrap_or(base)
                .highlight
                .is_some_and(|h| h.mode == KaraokeMode::FillSweep)
                && caption_sweep_word_pop(base.direction, &w.text)
        })
        .then(|| {
            format!(
                "Caption cue {}: FillSweep uses WordPop for RTL text",
                cue.id
            )
        })
}

/// Explicit RTL paragraphs always use direction-agnostic WordPop. Embedded
/// right-to-left runs also cannot use a left-to-right spatial split; Unicode
/// bidi levels are used rather than a guessed script/character range.
fn caption_sweep_word_pop(direction: timeline::TextDirection, text: &str) -> bool {
    if direction == timeline::TextDirection::Rtl {
        return true;
    }
    let level = match direction {
        timeline::TextDirection::Ltr => Some(unicode_bidi::Level::ltr()),
        _ => None,
    };
    unicode_bidi::BidiInfo::new(text, level)
        .levels
        .iter()
        .any(|l| l.is_rtl())
}

/// Resolve one covering cue at `tick`. Style cascades word → cue → track (01 §7,
/// each override a complete [`CaptionStyle`]); karaoke colour (06 §5.1) and
/// animation state (06 §5.2) are baked here. Returns `None` if nothing is visible
/// (e.g. Typewriter before the first character reveals).
fn resolve_cue(
    track: &CaptionTrack,
    cue: &CaptionCue,
    tick: Tick,
    _format: &SequenceFormat,
) -> Option<CaptionCueRun> {
    let cue_style: &CaptionStyle = cue.style_override.as_ref().unwrap_or(&track.style);
    let anim = cue_style.animation;

    // SlideUp (06 §5.2): whole-cue fade + upward translate over the first 200 ms,
    // both baked deterministically at compile time.
    let (cue_opacity, y_shift) = match anim {
        CaptionAnim::SlideUp => {
            let dur = ms_to_ticks(200).max(1);
            let p = ((tick - cue.start).0 as f32 / dur as f32).clamp(0.0, 1.0);
            (p, (1.0 - p) * 0.05) // slides up from +5% of frame height into place
        }
        _ => (1.0, 0.0),
    };

    let base_pos = cue.position_override.unwrap_or(cue_style.position);
    let anchor = [base_pos[0], base_pos[1] + y_shift];

    let mut words = Vec::with_capacity(cue.words.len());
    for w in &cue.words {
        // Word-level effective style: word → cue → track (each a full style).
        let eff: &CaptionStyle = w
            .style_override
            .as_ref()
            .or(cue.style_override.as_ref())
            .unwrap_or(&track.style);
        let text = reveal_text(&w.text, anim, w, tick);
        let mut color = karaoke_color(eff, w, tick);
        let mut opacity = cue_opacity;
        if let CaptionAnim::FadeWords = anim {
            opacity *= fade_word_opacity(w, tick);
        }
        color[3] = (color[3] as f32 * opacity).round().clamp(0.0, 255.0) as u8;
        words.push(CaptionWordRun {
            text,
            font_family: eff.font_family.clone(),
            font_weight: eff.weight,
            color,
        });
    }
    if words.iter().all(|w| w.text.is_empty()) {
        return None;
    }
    Some(CaptionCueRun {
        direction: cue_style.direction,
        words,
        font_size: cue_style.font_size.max(1.0),
        line_height_mul: 1.2,
        anchor,
        max_width: cue_style.max_width,
    })
}

/// Resolve a title/text clip's [`TextClipContent`] (G-12) into a
/// [`ResolvedTextBlock`] carrying a single styled [`CaptionCueRun`] — reusing the
/// caption glyph payload (06 §5.3) so titles render through the one
/// `TextGen`/glyphon mechanism. The clip's [`CaptionStyle`] (shared caption
/// styling vocabulary) supplies font / size / fill / position / wrap width; the
/// whole string is one word run (glyphon shapes embedded spaces itself). An empty
/// string resolves to `None` (nothing to shape ⇒ transparent). `TextClipContent`
/// carries a static style in v1, so there is no tick-varying prop to bake yet —
/// when keyframed title props land they resolve here, exactly as captions bake at
/// the compiled tick (02 §2).
fn resolve_text_block(content: &TextClipContent) -> ResolvedTextBlock {
    if content.text.is_empty() {
        return ResolvedTextBlock::default();
    }
    let style: &CaptionStyle = &content.style;
    ResolvedTextBlock {
        cue: Some(CaptionCueRun {
            direction: style.direction,
            words: vec![CaptionWordRun {
                text: content.text.clone(),
                font_family: style.font_family.clone(),
                font_weight: style.weight,
                color: color_to_srgb_bytes(style.fill),
            }],
            font_size: style.font_size.max(1.0),
            line_height_mul: 1.2,
            anchor: style.position,
            max_width: style.max_width,
        }),
    }
}

/// Milliseconds → ticks. `TICKS_PER_SECOND` is a multiple of 1000, so this is
/// exact (no rounding) — keeping animation timing deterministic.
fn ms_to_ticks(ms: i64) -> i64 {
    (TICKS_PER_SECOND / 1000) * ms
}

/// The word's fill colour at `tick`, resolved through its karaoke highlight
/// (06 §5.1), as sRGB straight RGBA bytes. Without a highlight it is the plain
/// fill. FillSweep's intra-glyph split is approximated at word granularity by
/// colour-lerping the sweeping word by the sweep fraction (the true per-glyph
/// split is a render follow-up).
fn karaoke_color(style: &CaptionStyle, w: &CaptionWord, tick: Tick) -> [u8; 4] {
    let Some(k) = style.highlight else {
        return color_to_srgb_bytes(style.fill);
    };
    let c = match k.mode {
        KaraokeMode::WordPop => {
            if w.start <= tick && tick < w.end {
                k.active_color
            } else {
                k.inactive_color
            }
        }
        // Glyph keeps its fill; Native resolves the active underline in CaptionPaint.
        // The Legacy glyph-only path still has no underline decoration.
        KaraokeMode::Underline => style.fill,
        KaraokeMode::FillSweep => {
            if tick < w.start {
                k.inactive_color
            } else if tick >= w.end {
                k.active_color // already-spoken words stay active (standard karaoke read)
            } else {
                let span = (w.end - w.start).0.max(1) as f32;
                let f = ((tick - w.start).0 as f32 / span).clamp(0.0, 1.0);
                lerp_color(k.inactive_color, k.active_color, f)
            }
        }
    };
    color_to_srgb_bytes(c)
}

/// FadeWords opacity (06 §5.2): ramps `0 → 1` over a 150 ms lead-in ending at
/// `w.start`, then holds at `1`.
fn fade_word_opacity(w: &CaptionWord, tick: Tick) -> f32 {
    let lead = ms_to_ticks(150).max(1);
    // Subtract in the wider domain before clamping: even a valid cue whose
    // duration fits i64 can overflow tick - (word_start - lead) near its end.
    let elapsed = tick.0 as i128 - w.start.0 as i128 + lead as i128;
    (elapsed.clamp(0, lead as i128) as f32 / lead as f32).clamp(0.0, 1.0)
}

/// Typewriter reveal (06 §5.2, 42 §6.5): the first
/// `floor(grapheme_count * clamp((t − start)/(end − start), 0, 1))` **grapheme
/// clusters** of the word; the full text for any other animation. Revealing per
/// grapheme (not per scalar) never emits a Devanagari matra without its base or
/// truncates an emoji ZWJ sequence mid-run; for pure ASCII it is byte-identical
/// at every tick, so no golden frame changes.
fn reveal_text(text: &str, anim: CaptionAnim, w: &CaptionWord, tick: Tick) -> String {
    if !matches!(anim, CaptionAnim::Typewriter) {
        return text.to_string();
    }
    let span = (w.end - w.start).0.max(1) as f32;
    let f = ((tick - w.start).0 as f32 / span).clamp(0.0, 1.0);
    let total = photonic_core::text_metrics::graphemes(text).count();
    let n = (total as f32 * f).floor() as usize;
    photonic_core::text_metrics::graphemes(text)
        .take(n)
        .collect()
}

fn lerp_color(a: Color, b: Color, f: f32) -> Color {
    let l = |x: f32, y: f32| x + (y - x) * f;
    Color {
        r: l(a.r, b.r),
        g: l(a.g, b.g),
        b: l(a.b, b.b),
        a: l(a.a, b.a),
    }
}

fn color_to_srgb_bytes(c: Color) -> [u8; 4] {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [q(c.r), q(c.g), q(c.b), q(c.a)]
}

// ── Keyframe resolution ───────────────────────────────────────────────────────

/// Evaluate an `AnimProps<ClipTransform>` at clip-relative `t` (all eight fields).
fn eval_clip_transform(anim: &AnimProps<ClipTransform>, t: Tick) -> ClipTransform {
    if anim.is_static() {
        return anim.base;
    }
    let base = anim.base;
    ClipTransform {
        x: eval_prop_f64(anim, "transform.x", base.x, t),
        y: eval_prop_f64(anim, "transform.y", base.y, t),
        scale_x: eval_prop_f64(anim, "transform.scale_x", base.scale_x, t),
        scale_y: eval_prop_f64(anim, "transform.scale_y", base.scale_y, t),
        rotation: eval_prop_f64(anim, "transform.rotation", base.rotation, t),
        anchor_space: base.anchor_space,
        anchor_x: eval_prop_f64(anim, "transform.anchor_x", base.anchor_x, t),
        anchor_y: eval_prop_f64(anim, "transform.anchor_y", base.anchor_y, t),
        opacity: eval_prop_f64(anim, "transform.opacity", base.opacity, t),
    }
}

/// Evaluate one `f64` property lane at `t`, mirroring the mixer's helper.
fn eval_prop_f64<T: timeline::PropSet>(anim: &AnimProps<T>, path: &str, base: f64, t: Tick) -> f64 {
    match anim.track(&PropPath::new(path)) {
        Some(track) => match timeline::eval(track, &PropValue::Float(base), t) {
            PropValue::Float(v) => v,
            _ => base,
        },
        None => base,
    }
}

/// Evaluate a graph node's `f32` param lane at `t` (base value from
/// `EffectParams`, overridden by an animated lane when present).
fn eval_node_f32(anim: &AnimProps<GraphNodeParams>, path: &str, default: f32, t: Tick) -> f32 {
    let base = match anim.base.0.get(path) {
        Some(PropValue::Float(v)) => *v as f32,
        _ => default,
    };
    match anim.track(&PropPath::new(path)) {
        Some(track) => match timeline::eval(track, &PropValue::Float(base as f64), t) {
            PropValue::Float(v) => v as f32,
            _ => base,
        },
        None => base,
    }
}

/// Evaluate a graph-node `f64` lane for transforms, which share the clip
/// transform property names but live in the node's open parameter bag.
fn eval_node_f64(anim: &AnimProps<GraphNodeParams>, path: &str, default: f64, t: Tick) -> f64 {
    let base = match anim.base.0.get(path) {
        Some(PropValue::Float(v)) => *v,
        _ => default,
    };
    match anim.track(&PropPath::new(path)) {
        Some(track) => match timeline::eval(track, &PropValue::Float(base), t) {
            PropValue::Float(v) => v,
            _ => base,
        },
        None => base,
    }
}

fn eval_node_dimension(
    anim: &AnimProps<GraphNodeParams>,
    path: &str,
    default: u32,
    t: Tick,
) -> u32 {
    let value = eval_node_f32(anim, path, default as f32, t);
    if value.is_finite() {
        value.round().clamp(1.0, 8192.0) as u32
    } else {
        default.max(1)
    }
}

/// Evaluate a graph node's `Color` param lane at `t`.
fn eval_node_color(
    anim: &AnimProps<GraphNodeParams>,
    path: &str,
    default: Color,
    t: Tick,
) -> Color {
    let base = match anim.base.0.get(path) {
        Some(PropValue::Color(c)) => *c,
        _ => default,
    };
    match anim.track(&PropPath::new(path)) {
        Some(track) => match timeline::eval(track, &PropValue::Color(base), t) {
            PropValue::Color(c) => c,
            _ => base,
        },
        None => base,
    }
}

fn graph_node_transform_matrix(
    anim: &AnimProps<GraphNodeParams>,
    t: Tick,
    format: &SequenceFormat,
) -> Mat3 {
    let defaults = ClipTransform::default();
    let transform = ClipTransform {
        x: eval_node_f64(anim, "transform.x", defaults.x, t),
        y: eval_node_f64(anim, "transform.y", defaults.y, t),
        scale_x: eval_node_f64(anim, "transform.scale_x", defaults.scale_x, t),
        scale_y: eval_node_f64(anim, "transform.scale_y", defaults.scale_y, t),
        rotation: eval_node_f64(anim, "transform.rotation", defaults.rotation, t),
        anchor_space: AnchorSpace::CenterOffset,
        anchor_x: eval_node_f64(anim, "transform.anchor_x", defaults.anchor_x, t),
        anchor_y: eval_node_f64(anim, "transform.anchor_y", defaults.anchor_y, t),
        opacity: defaults.opacity,
    };
    clip_transform_matrix(&transform, format)
}

/// Keyframe-resolve an effect's params into the ordered [`ResolvedParams`] bag
/// the IR carries (02 §2).
///
/// **Prefer the effect manifest** (K-B16 / 30 §2): bridged and catalogue ids
/// lower as `EffectKind::Unknown(tag)` and have empty `prop_registry` blocks, so
/// the only authoritative param list is the manifest table. Fall back to the
/// legacy registry seed for the seven v1 kinds when no manifest is found.
///
/// Order is deterministic (manifest / registry order), which `hash_op` relies on
/// for cache identity.
fn resolve_effect_params<T: PropSet>(
    kind: EffectKind,
    base: &EffectParams,
    anim: &AnimProps<T>,
    dt: Tick,
) -> ResolvedParams {
    use photonic_core::timeline::effect_manifest;

    let id = kind.effect_id();
    if let Some(m) = effect_manifest::manifest(id) {
        let mut entries = Vec::with_capacity(m.params.len());
        for spec in m.params {
            let path = PropPath::new(spec.path);
            let base_val = base.get(spec.path).copied().unwrap_or(spec.default);
            let value = match anim.track(&path) {
                Some(track) => timeline::eval(track, &base_val, dt),
                None => base_val,
            };
            entries.push((path, value));
        }
        return ResolvedParams { entries };
    }

    let seeded = EffectParams::seed(PropTargetKind::Effect(kind));
    let mut entries = Vec::with_capacity(seeded.entries.len());
    for (path, seed_default) in &seeded.entries {
        let base_val = base.get(path.as_str()).copied().unwrap_or(*seed_default);
        let value = match anim.track(path) {
            Some(track) => timeline::eval(track, &base_val, dt),
            None => base_val,
        };
        entries.push((path.clone(), value));
    }
    ResolvedParams { entries }
}

// ── Transforms & color ────────────────────────────────────────────────────────

/// Build a 3×3 affine from an evaluated [`ClipTransform`]. Center-offset anchors
/// are relative to the output-frame center; legacy absolute anchors are raw
/// output pixels. x/y are output-pixel translation offsets. Opacity drives
/// `Merge` and is not geometric.
fn clip_transform_matrix(t: &ClipTransform, format: &SequenceFormat) -> Mat3 {
    let frame_center = Vec2::new(format.width as f32 * 0.5, format.height as f32 * 0.5);
    let anchor_value = Vec2::new(t.anchor_x as f32, t.anchor_y as f32);
    let anchor = match t.anchor_space {
        AnchorSpace::Absolute => anchor_value,
        AnchorSpace::CenterOffset => frame_center + anchor_value,
    };
    let pos = Vec2::new(t.x as f32, t.y as f32);
    let scale = Vec2::new(t.scale_x as f32, t.scale_y as f32);
    Mat3::from_translation(pos)
        * Mat3::from_translation(anchor)
        * Mat3::from_angle(t.rotation as f32)
        * Mat3::from_scale(scale)
        * Mat3::from_translation(-anchor)
}

/// sRGB EOTF (gamma → scene-linear), the standard breakpoint form. A vector/
/// solid `Color` is authored in the sRGB display domain; the video graph works
/// in scene-linear premultiplied Rec.709 (D-09), so convert on the way in.
/// (03 §3.3 wants all transfer-function math consolidated into
/// `photonic-core::color`; that refactor is out of P3 scope — this mirrors the
/// existing `raster/adjust.rs` curve exactly.)
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Convert an sRGB straight-alpha [`Color`] to premultiplied scene-linear
/// [`LinearColor`] (D-09).
fn color_to_linear_premult(c: Color) -> LinearColor {
    let a = c.a.clamp(0.0, 1.0);
    LinearColor {
        r: srgb_to_linear(c.r) * a,
        g: srgb_to_linear(c.g) * a,
        b: srgb_to_linear(c.b) * a,
        a,
    }
}

// ── Vector reference resolution ───────────────────────────────────────────────

/// Resolve the `VectorRef` for a `ClipSource::Vector` asset: an embedded-vector
/// asset carries its own `VectorRef`; a file-backed one references the whole
/// external document.
fn vector_ref_for(project: &TimelineProject, asset: AssetId) -> VectorRef {
    use photonic_core::timeline::AssetSource;
    match project.media.assets.get(&asset).map(|a| &a.source) {
        Some(AssetSource::EmbeddedVector { root }) => *root,
        _ => VectorRef::WholeDocument,
    }
}

/// A `VectorStateKey` for a rasterized vector frame (02 §3 / 03 §2.5). The
/// full key hashes referenced-node state + evaluated animated props + size; P3
/// keys on `(vref discriminant, size, src_time)`, which is stable and correct
/// for a non-animated vector doc and conservative (never stale) for an animated
/// one. Referenced-node-state hashing lands with the vector-animation story.
fn vector_state_key(asset: AssetId, format: &SequenceFormat, src_time: Tick) -> VectorStateKey {
    vector_state_key_for_ref(VectorRef::WholeDocument, format, src_time).combine(asset.0.as_u128())
}

fn vector_state_key_for_ref(
    vref: VectorRef,
    format: &SequenceFormat,
    src_time: Tick,
) -> VectorStateKey {
    use xxhash_rust::xxh3::Xxh3;
    let mut h = Xxh3::new();
    h.update(&[vref_tag(&vref)]);
    match vref {
        VectorRef::Artboard(i) => h.update(&(i as u64).to_le_bytes()),
        VectorRef::Node(id) => h.update(&id.as_u128().to_le_bytes()),
        VectorRef::WholeDocument => {}
    }
    h.update(&format.width.to_le_bytes());
    h.update(&format.height.to_le_bytes());
    h.update(&src_time.0.to_le_bytes());
    VectorStateKey(h.digest128())
}

fn vref_tag(v: &VectorRef) -> u8 {
    match v {
        VectorRef::Artboard(_) => 0,
        VectorRef::Node(_) => 1,
        VectorRef::WholeDocument => 2,
    }
}

trait CombineKey {
    fn combine(self, extra: u128) -> Self;
}
impl CombineKey for VectorStateKey {
    fn combine(self, extra: u128) -> Self {
        use xxhash_rust::xxh3::Xxh3;
        let mut h = Xxh3::new();
        h.update(&self.0.to_le_bytes());
        h.update(&extra.to_le_bytes());
        VectorStateKey(h.digest128())
    }
}

// ── Content hashing ───────────────────────────────────────────────────────────

/// Content hash of `(op discriminant, resolved params, input hashes)` — the
/// cache identity of a node's result (02 §5). xxh3-128; deterministic across
/// runs (no `Instant`/random state).
pub fn content_hash(
    op: &IrOp,
    inputs: &[(IrNodeId, OutPort)],
    input_hashes: &[u128],
) -> ContentHash {
    use xxhash_rust::xxh3::Xxh3;
    let mut h = Xxh3::new();
    hash_op(&mut h, op);
    for ((_, port), ih) in inputs.iter().zip(input_hashes) {
        h.update(&[port.0]);
        h.update(&ih.to_le_bytes());
    }
    ContentHash(h.digest128())
}

fn hash_op(h: &mut xxhash_rust::xxh3::Xxh3, op: &IrOp) {
    // A per-variant tag byte, then the resolved params in a fixed order. The
    // remaining opaque resolved-param stubs (`ResolvedParams`, `ResolvedTextBlock`)
    // contribute nothing until their payloads land — extend here when they do.
    // `CaptionOverlay`'s resolved batch (incl. baked karaoke colours) IS hashed,
    // so a mid-word highlight change is a distinct cache identity (06 §5).
    let f32b = |h: &mut xxhash_rust::xxh3::Xxh3, v: f32| h.update(&v.to_bits().to_le_bytes());
    match op {
        IrOp::DecodeVideo {
            asset,
            src_time,
            proxy,
        } => {
            h.update(&[0]);
            h.update(&asset.0.as_u128().to_le_bytes());
            h.update(&src_time.0.to_le_bytes());
            h.update(&[*proxy as u8]);
        }
        IrOp::NativeDecodeVideo {
            asset,
            src_time,
            input,
        } => {
            h.update(&[25]);
            h.update(&asset.0.as_u128().to_le_bytes());
            h.update(&src_time.0.to_le_bytes());
            h.update(&serde_json::to_vec(input).expect("native input interpretation serializes"));
        }
        IrOp::NativeMediaVideo {
            asset,
            src_time,
            input,
            w,
            h: height,
            native_extent,
        } => {
            h.update(&[72]);
            h.update(&asset.0.as_u128().to_le_bytes());
            h.update(&src_time.0.to_le_bytes());
            h.update(&serde_json::to_vec(input).expect("native input serializes"));
            h.update(&w.to_le_bytes());
            h.update(&height.to_le_bytes());
            h.update(&[*native_extent as u8]);
        }
        IrOp::NativeMediaStill {
            asset,
            w,
            h: height,
            native_extent,
        } => {
            h.update(&[73]);
            h.update(&asset.0.as_u128().to_le_bytes());
            h.update(&w.to_le_bytes());
            h.update(&height.to_le_bytes());
            h.update(&[*native_extent as u8]);
        }
        IrOp::NativeDecodeStill { asset } => {
            h.update(&[34]);
            h.update(&asset.0.as_u128().to_le_bytes());
        }
        IrOp::DecodeStill { asset } => {
            h.update(&[1]);
            h.update(&asset.0.as_u128().to_le_bytes());
        }
        IrOp::NativeRasterVector {
            vref,
            doc_state,
            w,
            h: gh,
        } => {
            h.update(&[66]);
            h.update(&[vref_tag(vref)]);
            h.update(&doc_state.0.to_le_bytes());
            h.update(&w.to_le_bytes());
            h.update(&gh.to_le_bytes());
        }
        IrOp::RasterVector {
            vref,
            doc_state,
            w,
            h: gh,
        } => {
            h.update(&[2]);
            h.update(&[vref_tag(vref)]);
            h.update(&doc_state.0.to_le_bytes());
            h.update(&w.to_le_bytes());
            h.update(&gh.to_le_bytes());
        }
        IrOp::SolidColor { color } => {
            h.update(&[3]);
            f32b(h, color.r);
            f32b(h, color.g);
            f32b(h, color.b);
            f32b(h, color.a);
        }
        IrOp::Transform2D { mat, sampling } => {
            h.update(&[4]);
            for v in mat.to_cols_array() {
                f32b(h, v);
            }
            h.update(&[*sampling as u8]);
        }
        IrOp::Transform2DTransparent { mat, sampling } => {
            h.update(&[29]);
            for v in mat.to_cols_array() {
                f32b(h, v);
            }
            h.update(&[*sampling as u8]);
        }
        // Tag 20: the next free byte in this namespace (0..=19 are taken).
        // Every field is hashed — two frames of a stabilized clip differ only
        // in these numbers, so omitting any one of them would alias distinct
        // frames onto a single cache entry and freeze the correction.
        IrOp::StabilizeWarp { warp, sampling } => {
            h.update(&[20]);
            for v in warp.rotation {
                f32b(h, v);
            }
            f32b(h, warp.zoom);
            for v in warp.intrinsics {
                f32b(h, v);
            }
            for v in warp.k {
                f32b(h, v);
            }
            h.update(&[warp.fisheye as u8, warp.transparent_edges as u8]);
            h.update(&[*sampling as u8]);
        }
        IrOp::Effect { kind, params } => {
            h.update(&[5]);
            h.update(&[effect_kind_tag(*kind)]);
            hash_resolved_params(h, params);
        }
        IrOp::Grade { ops } => {
            h.update(&[6]);
            h.update(&(ops.len() as u32).to_le_bytes());
            for op in ops {
                hash_resolved_grade_op(h, op);
            }
        }
        IrOp::NativeLumaWipe {
            kind,
            softness,
            invert,
            t,
        } => {
            h.update(&[47, *kind as u8, *invert as u8]);
            f32b(h, *softness);
            f32b(h, *t);
        }
        IrOp::NativeDissolve { t } => {
            h.update(&[46]);
            f32b(h, *t);
        }
        IrOp::NativeCanvas { w, h: height } => {
            h.update(&[45]);
            h.update(&w.to_le_bytes());
            h.update(&height.to_le_bytes());
        }
        IrOp::NativeLumaKey {
            threshold,
            softness,
            invert,
        } => {
            h.update(&[50]);
            f32b(h, *threshold);
            f32b(h, *softness);
            h.update(&[*invert as u8]);
        }
        IrOp::NativeChromaKey {
            key,
            tolerance,
            softness,
            spill,
        } => {
            h.update(&[51]);
            for c in key {
                f32b(h, *c);
            }
            f32b(h, *tolerance);
            f32b(h, *softness);
            f32b(h, *spill);
        }
        IrOp::NativeGlow {
            sigma,
            threshold,
            intensity,
            tint,
        } => {
            h.update(&[52]);
            f32b(h, *sigma);
            f32b(h, *threshold);
            f32b(h, *intensity);
            for c in tint {
                f32b(h, *c);
            }
        }
        IrOp::NativeMaskShape {
            rect,
            center,
            size,
            rotation,
            feather,
        } => {
            h.update(&[53, *rect as u8]);
            for v in center.iter().chain(size) {
                f32b(h, *v);
            }
            f32b(h, *rotation);
            f32b(h, *feather);
        }
        IrOp::NativePolygonMask {
            vertices,
            center,
            size,
            rotation,
            feather,
        } => {
            h.update(&[67]);
            h.update(&(vertices.len() as u32).to_le_bytes());
            for v in vertices.iter().flatten().chain(center).chain(size) {
                f32b(h, *v);
            }
            f32b(h, *rotation);
            f32b(h, *feather);
        }
        IrOp::NativeDenoise {
            strength,
            spatial_scale,
        } => {
            h.update(&[62]);
            f32b(h, *strength);
            f32b(h, *spatial_scale);
        }
        IrOp::NativeGrain {
            amount,
            monochrome,
            seed,
            pixel_scale,
        } => {
            h.update(&[61, *monochrome as u8]);
            f32b(h, *amount);
            h.update(&seed.to_le_bytes());
            for v in pixel_scale {
                f32b(h, *v);
            }
        }
        IrOp::NativeVignette { amount, feather } => {
            h.update(&[60]);
            f32b(h, *amount);
            f32b(h, *feather);
        }
        IrOp::NativeChannelExtract { channel } => {
            h.update(&[68, *channel as u8]);
        }
        IrOp::NativeImageToMask => {
            h.update(&[69]);
        }
        IrOp::NativeMaskToImage => {
            h.update(&[70]);
        }
        IrOp::NativeChannelCombine => {
            h.update(&[71]);
        }
        IrOp::NativeInvert => {
            h.update(&[49]);
        }
        IrOp::NativeUnsharp { amount, sigma } => {
            h.update(&[48]);
            f32b(h, *amount);
            f32b(h, *sigma);
        }
        IrOp::NativeOutline {
            thickness,
            color,
            opacity,
            pixel_scale,
        } => {
            h.update(&[90]);
            f32b(h, *thickness);
            for v in color {
                f32b(h, *v);
            }
            f32b(h, *opacity);
            for v in pixel_scale {
                f32b(h, *v);
            }
        }
        IrOp::NativeLinearHsl { vibrance, controls } => {
            h.update(&[91, *vibrance as u8]);
            for v in controls {
                f32b(h, *v);
            }
        }
        IrOp::NativeSurfaceBlur {
            radius,
            threshold,
            pixel_scale,
        } => {
            h.update(&[92]);
            h.update(&radius.to_le_bytes());
            f32b(h, *threshold);
            for v in pixel_scale {
                f32b(h, *v);
            }
        }
        IrOp::NativeLensBlur {
            radius,
            pixel_scale,
        } => {
            h.update(&[93]);
            f32b(h, *radius);
            for v in pixel_scale {
                f32b(h, *v);
            }
        }
        IrOp::NativeRankFilter {
            kind,
            radius,
            threshold,
            pixel_scale,
        } => {
            h.update(&[95, *kind as u8, *threshold]);
            h.update(&radius.to_le_bytes());
            for v in pixel_scale {
                f32b(h, *v);
            }
        }
        IrOp::NativeDeflicker { gain, band } => {
            h.update(&[96]);
            for v in gain.iter().chain(band.iter()) {
                h.update(&v.to_bits().to_le_bytes());
            }
        }
        IrOp::NativeUnpremultiply => {
            h.update(&[94]);
        }
        IrOp::NativeAlphaView { mode } => {
            h.update(&[89, *mode]);
        }
        IrOp::NativeTransferLevels {
            black_white,
            gamma,
            output,
        } => {
            h.update(&[88]);
            for v in black_white.iter().chain(output.iter()) {
                f32b(h, *v);
            }
            f32b(h, *gamma);
        }
        IrOp::NativeTransferCurves { knots, contrast } => {
            h.update(&[87]);
            for v in knots.iter().flatten() {
                f32b(h, *v);
            }
            f32b(h, *contrast);
        }
        IrOp::NativeColorBalance { shifts, preserve } => {
            h.update(&[86]);
            for v in shifts.iter().flatten() {
                f32b(h, *v);
            }
            h.update(&[*preserve as u8]);
        }
        IrOp::NativePhotoFilter {
            color,
            density,
            preserve,
        } => {
            h.update(&[85]);
            for v in color {
                f32b(h, *v);
            }
            f32b(h, *density);
            h.update(&[*preserve as u8]);
        }
        IrOp::NativeTransferMatrix { matrix } => {
            h.update(&[84]);
            for v in matrix.iter().flatten() {
                f32b(h, *v);
            }
        }
        IrOp::NativeTransferArtistic { kind, parameter } => {
            h.update(&[83]);
            h.update(&(*kind as u32).to_le_bytes());
            f32b(h, *parameter);
        }
        IrOp::NativeDropShadow {
            offset,
            radius,
            color,
            opacity,
            pixel_scale,
        } => {
            h.update(&[82]);
            for v in offset {
                f32b(h, *v);
            }
            h.update(&radius.to_le_bytes());
            for v in color {
                f32b(h, *v);
            }
            f32b(h, *opacity);
            for v in pixel_scale {
                f32b(h, *v);
            }
        }
        IrOp::NativePerspective { inverse } => {
            h.update(&[81]);
            for value in inverse {
                f32b(h, *value);
            }
        }
        IrOp::NativeStylize {
            kind,
            amount,
            pixel_scale,
        } => {
            h.update(&[80]);
            h.update(&(*kind as u32).to_le_bytes());
            f32b(h, *amount);
            for v in pixel_scale {
                f32b(h, *v);
            }
        }
        IrOp::NativeDetail {
            kind,
            amount,
            sigma,
            threshold,
            pixel_scale,
        } => {
            h.update(&[79]);
            h.update(&(*kind as u32).to_le_bytes());
            f32b(h, *amount);
            f32b(h, *sigma);
            f32b(h, *threshold);
            for v in pixel_scale {
                f32b(h, *v);
            }
        }
        IrOp::NativeGeometricWarp {
            kind,
            amount,
            wavelength,
            pixel_scale,
        } => {
            h.update(&[78]);
            h.update(&(*kind as u32).to_le_bytes());
            f32b(h, *amount);
            f32b(h, *wavelength);
            for value in pixel_scale {
                f32b(h, *value);
            }
        }
        IrOp::NativeBoxBlur { radius } => {
            h.update(&[77]);
            for value in radius {
                h.update(&value.to_le_bytes());
            }
        }
        IrOp::NativeMosaic { block } => {
            h.update(&[76]);
            for value in block {
                h.update(&value.to_le_bytes());
            }
        }
        IrOp::NativeMotionBlur { delta } => {
            h.update(&[75]);
            for value in delta {
                f32b(h, *value);
            }
        }
        IrOp::NativeGaussianBlur { sigma } => {
            h.update(&[44]);
            f32b(h, *sigma);
        }
        IrOp::NativeExposure { stops } => {
            h.update(&[21]);
            f32b(h, *stops);
        }
        IrOp::NativeLinearOffset { rgb } => {
            h.update(&[26]);
            for value in rgb {
                f32b(h, *value);
            }
        }
        IrOp::NativePrinterLights { points } => {
            h.update(&[27]);
            for value in points {
                f32b(h, *value);
            }
        }
        IrOp::NativeHighlightRolloff { knee, strength } => {
            h.update(&[28]);
            f32b(h, *knee);
            f32b(h, *strength);
        }
        IrOp::NativeMaskMix { mask } => {
            h.update(&[32]);
            hash_resolved_grade_op(
                h,
                &photonic_render::grade::ResolvedGradeOp {
                    payload: photonic_render::grade::ResolvedGradePayload::Exposure { stops: 0.0 },
                    mask: Some(*mask),
                },
            );
        }
        IrOp::NativeLut3d { lut } => {
            h.update(&[31]);
            hash_resolved_grade_op(
                h,
                &photonic_render::grade::ResolvedGradeOp {
                    payload: photonic_render::grade::ResolvedGradePayload::Lut3d(lut.clone()),
                    mask: None,
                },
            );
        }
        IrOp::NativeLogCurves { curves } => {
            h.update(&[36]);
            hash_resolved_grade_op(
                h,
                &photonic_render::grade::ResolvedGradeOp {
                    payload: photonic_render::grade::ResolvedGradePayload::Curves(curves.clone()),
                    mask: None,
                },
            );
        }
        IrOp::QualifierMatte {
            qualifier,
            mask,
            native,
        } => {
            h.update(&[38, u8::from(*native)]);
            let mut key = **qualifier;
            key.correction = photonic_render::grade::ResolvedCdl {
                slope: [1.0; 3],
                offset: [0.0; 3],
                power: [1.0; 3],
                sat: 1.0,
            };
            hash_resolved_grade_op(
                h,
                &photonic_render::grade::ResolvedGradeOp {
                    payload: photonic_render::grade::ResolvedGradePayload::HslQualifier(Box::new(
                        key,
                    )),
                    mask: *mask,
                },
            );
        }
        IrOp::GradeMatteRefine { refinement } => {
            h.update(&[43, u8::from(refinement.denoise)]);
            f32b(h, refinement.grow);
            f32b(h, refinement.blur);
            for value in refinement.matte_levels {
                f32b(h, value);
            }
        }
        IrOp::GradeKeyMix { mode } => {
            h.update(&[39, *mode as u8]);
        }
        IrOp::GradeMatteApply => {
            h.update(&[40]);
        }
        IrOp::NativeTonalExposure { tonal } => {
            h.update(&[56]);
            hash_resolved_grade_op(
                h,
                &photonic_render::grade::ResolvedGradeOp {
                    payload: photonic_render::grade::ResolvedGradePayload::TonalExposure(*tonal),
                    mask: None,
                },
            );
        }
        IrOp::NativeRgbMatrix { rows } => {
            h.update(&[55]);
            for value in rows.iter().flatten() {
                f32b(h, *value);
            }
        }
        IrOp::NativeMatteCdl { cdl } => {
            h.update(&[54]);
            hash_resolved_grade_op(
                h,
                &photonic_render::grade::ResolvedGradeOp {
                    payload: photonic_render::grade::ResolvedGradePayload::Cdl(*cdl),
                    mask: None,
                },
            );
        }

        IrOp::BezierMatte { window } => {
            h.update(&[97]);
            h.update(&(window.knots.len() as u32).to_le_bytes());
            for knot in &window.knots {
                for v in knot
                    .anchor
                    .iter()
                    .chain(&knot.incoming)
                    .chain(&knot.outgoing)
                {
                    f32b(h, *v);
                }
            }
            f32b(h, window.feather);
            h.update(&[u8::from(window.invert)]);
        }
        IrOp::WindowMatte { mask } => {
            h.update(&[57]);
            hash_resolved_grade_op(
                h,
                &photonic_render::grade::ResolvedGradeOp {
                    payload: photonic_render::grade::ResolvedGradePayload::Exposure { stops: 0.0 },
                    mask: Some(*mask),
                },
            );
        }
        IrOp::GradeMatteConstant { weight } => {
            h.update(&[41]);
            f32b(h, *weight);
        }
        IrOp::GradeLayerMix { opacity } => {
            h.update(&[42]);
            f32b(h, *opacity);
        }
        IrOp::NativeLogQualifier { qualifier } => {
            h.update(&[37]);
            hash_resolved_grade_op(
                h,
                &photonic_render::grade::ResolvedGradeOp {
                    payload: photonic_render::grade::ResolvedGradePayload::HslQualifier(
                        qualifier.clone(),
                    ),
                    mask: None,
                },
            );
        }
        IrOp::NativeLogCdl { cdl } => {
            h.update(&[35]);
            hash_resolved_grade_op(
                h,
                &photonic_render::grade::ResolvedGradeOp {
                    payload: photonic_render::grade::ResolvedGradePayload::Cdl(*cdl),
                    mask: None,
                },
            );
        }
        IrOp::NativeLogContrast { pivot, amount } => {
            h.update(&[33]);
            f32b(h, *pivot);
            f32b(h, *amount);
        }
        IrOp::NativeSaturationVibrance {
            saturation,
            vibrance,
        } => {
            h.update(&[30]);
            f32b(h, *saturation);
            f32b(h, *vibrance);
        }
        IrOp::NativeAcescct { direction } => {
            h.update(&[22]);
            h.update(&[*direction as u8]);
        }
        IrOp::NativeSdrOutput => h.update(&[23]),
        IrOp::NativeSdrVideoOutput => h.update(&[24]),
        IrOp::NativeLuminanceMerge { mode, opacity } => {
            h.update(&[59]);
            h.update(&[*mode as u8]);
            f32b(h, *opacity);
        }
        IrOp::NativeBoundedMerge { mode, opacity } => {
            h.update(&[98]);
            h.update(&[*mode as u8]);
            f32b(h, *opacity);
        }
        IrOp::NativeArithmeticMerge { mode, opacity } => {
            h.update(&[58]);
            h.update(&[*mode as u8]);
            f32b(h, *opacity);
        }
        IrOp::Merge { mode, opacity } => {
            h.update(&[7]);
            h.update(&[*mode as u8]);
            f32b(h, *opacity);
        }
        IrOp::WipeMix {
            direction,
            softness,
            t,
        } => {
            h.update(&[16]);
            h.update(&[*direction as u8]);
            f32b(h, *softness);
            f32b(h, *t);
        }
        IrOp::PushMix { direction, t } => {
            h.update(&[17]);
            h.update(&[*direction as u8]);
            f32b(h, *t);
        }
        IrOp::LumaWipeMix {
            kind,
            softness,
            invert,
            t,
        } => {
            h.update(&[19]);
            h.update(&[*kind as u8]);
            f32b(h, *softness);
            h.update(&[*invert as u8]);
            f32b(h, *t);
        }
        IrOp::CaptionOverlay { cue_batch } => {
            h.update(&[8]);
            hash_caption_batch(h, cue_batch);
        }
        IrOp::Crop {
            left,
            top,
            right,
            bottom,
        } => {
            h.update(&[9]);
            f32b(h, *left);
            f32b(h, *top);
            f32b(h, *right);
            f32b(h, *bottom);
        }
        IrOp::Resize { w, h: gh, fit } => {
            h.update(&[10]);
            h.update(&w.to_le_bytes());
            h.update(&gh.to_le_bytes());
            h.update(&[*fit as u8]);
        }
        IrOp::NativeMatteExtract => {
            h.update(&[74]);
            h.update(photonic_matte::PINNED_MODEL_SHA256.as_bytes());
            h.update(b"bounded-srgb-raw-weight-v1");
        }
        IrOp::MatteExtract { model } => {
            h.update(&[11]);
            h.update(&[*model as u8]);
        }
        IrOp::NativeCaptionOutput {
            cue_batch,
            video,
            paints,
        } => {
            h.update(&[64, *video as u8]);
            hash_caption_batch(h, cue_batch);
            h.update(&(paints.len() as u32).to_le_bytes());
            for paint in paints {
                hash_caption_decoration(h, &paint.decoration);
                h.update(&(paint.word_opacity.len() as u32).to_le_bytes());
                h.update(&paint.word_opacity);
                h.update(&paint.opacity.to_le_bytes());
                h.update(&(paint.underlines.len() as u32).to_le_bytes());
                for u in &paint.underlines {
                    h.update(&(u.word as u32).to_le_bytes());
                    h.update(&u.color);
                    f32b(h, u.width);
                }
                if !paint.sweeps.is_empty() {
                    h.update(&[0x53]);
                    h.update(&(paint.sweeps.len() as u32).to_le_bytes());
                    for sweep in &paint.sweeps {
                        h.update(&(sweep.word as u32).to_le_bytes());
                        f32b(h, sweep.fraction);
                        h.update(&sweep.active);
                        h.update(&sweep.inactive);
                    }
                }
            }
        }
        IrOp::NativeDecoratedTextGen { block, decoration } => {
            h.update(&[65]);
            hash_caption_decoration(h, decoration);
            match &block.cue {
                Some(cue) => {
                    h.update(&[1]);
                    hash_caption_cue(h, cue);
                }
                None => h.update(&[0]),
            }
        }
        IrOp::NativeTextGen { block } => {
            h.update(&[63]);
            match &block.cue {
                Some(cue) => {
                    h.update(&[1]);
                    hash_caption_cue(h, cue);
                }
                None => h.update(&[0]),
            }
        }
        IrOp::TextGen { block } => {
            h.update(&[12]);
            // The resolved cue (text, font, colour, layout) IS hashed, so distinct
            // titles are distinct cache identities; an empty/absent cue hashes as a
            // bare tag (transparent).
            match &block.cue {
                Some(cue) => {
                    h.update(&[1]);
                    hash_caption_cue(h, cue);
                }
                None => h.update(&[0]),
            }
        }
        IrOp::ChannelSplit { channel } => {
            h.update(&[13]);
            h.update(&[*channel as u8]);
        }
        IrOp::ChannelCombine => {
            h.update(&[14]);
        }
        IrOp::Output { w, h: gh } => {
            h.update(&[15]);
            h.update(&w.to_le_bytes());
            h.update(&gh.to_le_bytes());
        }
        IrOp::Deinterlace {
            method,
            field_order,
        } => {
            h.update(&[18]);
            h.update(&[*method as u8]);
            h.update(&[*field_order as u8]);
        }
    }
}

/// Hash a resolved effect-param bag (K-0.2) into the content hash, in order:
/// each `(path, value)` pair contributes its path bytes and value bits. Ordered
/// iteration over the `Vec` (never a map) makes the digest deterministic, so an
/// `Effect` op's cache identity tracks its actual resolved params — two Blur
/// radii are distinct `NodeCache` entries, never a wrong-pixels collision.
/// Deterministic: only resolved bytes/bits, no pointer/time state.
fn hash_resolved_params(h: &mut xxhash_rust::xxh3::Xxh3, params: &ResolvedParams) {
    let f64b = |h: &mut xxhash_rust::xxh3::Xxh3, v: f64| h.update(&v.to_bits().to_le_bytes());
    h.update(&(params.entries.len() as u32).to_le_bytes());
    for (path, value) in &params.entries {
        h.update(&(path.as_str().len() as u32).to_le_bytes());
        h.update(path.as_str().as_bytes());
        match value {
            PropValue::Float(v) => {
                h.update(&[0]);
                f64b(h, *v);
            }
            PropValue::Vec2(v) => {
                h.update(&[1]);
                f64b(h, v[0]);
                f64b(h, v[1]);
            }
            PropValue::Color(c) => {
                h.update(&[2]);
                for ch in [c.r, c.g, c.b, c.a] {
                    h.update(&ch.to_bits().to_le_bytes());
                }
            }
            PropValue::Bool(b) => {
                h.update(&[3]);
                h.update(&[*b as u8]);
            }
            PropValue::Enum(e) => {
                h.update(&[4]);
                h.update(&e.to_le_bytes());
            }
        }
    }
}

/// Hash a resolved [`CaptionBatch`] (06 §5.3) into the content hash: cue layout
/// plus every word's text, font, weight, and baked karaoke colour. This is what
/// makes a karaoke sweep re-render — the same cue at two ticks resolves to
/// different word colours ⇒ different hash ⇒ a cache miss ⇒ a fresh composite.
/// Deterministic: only resolved bytes/f32 bits, no pointer/time state.
fn hash_caption_decoration(
    h: &mut xxhash_rust::xxh3::Xxh3,
    d: &photonic_render::caption_decoration::CaptionDecoration,
) {
    match d.stroke {
        Some((c, w)) => {
            h.update(&[1]);
            h.update(&c);
            h.update(&w.to_le_bytes());
        }
        None => h.update(&[0]),
    }
    match d.background {
        Some((c, p, r)) => {
            h.update(&[1]);
            h.update(&c);
            h.update(&p.to_le_bytes());
            h.update(&r.to_le_bytes());
        }
        None => h.update(&[0]),
    }
}

fn hash_caption_batch(h: &mut xxhash_rust::xxh3::Xxh3, batch: &CaptionBatch) {
    h.update(&(batch.cues.len() as u32).to_le_bytes());
    for cue in &batch.cues {
        hash_caption_cue(h, cue);
    }
}

/// Hash one resolved [`CaptionCueRun`] — layout plus every word's text, font,
/// weight, and baked colour — into the content hash. Shared by
/// [`hash_caption_batch`] (06 §5.3 `CaptionOverlay`) and the `TextGen` block
/// (G-12 title clips), so both text-raster paths key their cache identically.
fn hash_caption_cue(h: &mut xxhash_rust::xxh3::Xxh3, cue: &CaptionCueRun) {
    let f32b = |h: &mut xxhash_rust::xxh3::Xxh3, v: f32| h.update(&v.to_bits().to_le_bytes());
    f32b(h, cue.font_size);
    f32b(h, cue.line_height_mul);
    f32b(h, cue.anchor[0]);
    f32b(h, cue.anchor[1]);
    f32b(h, cue.max_width);
    // Auto keeps the historical byte stream/cache identity unchanged.
    if cue.direction != timeline::TextDirection::Auto {
        h.update(&[0xd1, cue.direction as u8]);
    }
    h.update(&(cue.words.len() as u32).to_le_bytes());
    for w in &cue.words {
        h.update(&(w.text.len() as u32).to_le_bytes());
        h.update(w.text.as_bytes());
        h.update(&(w.font_family.len() as u32).to_le_bytes());
        h.update(w.font_family.as_bytes());
        h.update(&w.font_weight.to_le_bytes());
        h.update(&w.color);
    }
}

/// Hash one resolved grade op (payload + mask) into the content hash so distinct
/// grades never collide in the node-result cache (02 §5). Deterministic: only
/// resolved f32 params + discriminants, no `Instant`/pointer state.
fn hash_resolved_grade_op(h: &mut xxhash_rust::xxh3::Xxh3, op: &crate::contract::ResolvedGradeOp) {
    use photonic_render::grade::ResolvedGradePayload as P;
    let f32b = |h: &mut xxhash_rust::xxh3::Xxh3, v: f32| h.update(&v.to_bits().to_le_bytes());
    let cdl = |h: &mut xxhash_rust::xxh3::Xxh3, c: &photonic_render::grade::ResolvedCdl| {
        for v in c.slope.iter().chain(&c.offset).chain(&c.power) {
            f32b(h, *v);
        }
        f32b(h, c.sat);
    };
    match &op.mask {
        None => h.update(&[0]),
        Some(m) => {
            h.update(&[
                1,
                match m.shape {
                    photonic_core::timeline::WindowShape::Ellipse => 0,
                    photonic_core::timeline::WindowShape::Rectangle => 1,
                    photonic_core::timeline::WindowShape::Gradient => 2,
                },
                m.invert as u8,
            ]);
            for v in [
                m.center[0],
                m.center[1],
                m.size[0],
                m.size[1],
                m.rotation,
                m.softness,
            ] {
                f32b(h, v);
            }
        }
    }
    match &op.payload {
        P::Exposure { stops } => {
            h.update(&[0]);
            f32b(h, *stops);
        }
        P::LinearOffset { rgb } => {
            h.update(&[7]);
            for channel in rgb {
                f32b(h, *channel);
            }
        }
        P::PrinterLights { points } => {
            h.update(&[10]);
            for point in points {
                f32b(h, *point);
            }
        }
        P::HighlightRolloff { knee, strength } => {
            h.update(&[8]);
            f32b(h, *knee);
            f32b(h, *strength);
        }
        P::SaturationVibrance {
            saturation,
            vibrance,
        } => {
            h.update(&[9]);
            f32b(h, *saturation);
            f32b(h, *vibrance);
        }
        P::Contrast { pivot, amount } => {
            h.update(&[1]);
            f32b(h, *pivot);
            f32b(h, *amount);
        }
        P::TonalExposure(tonal) => {
            h.update(&[12]);
            for value in tonal
                .stops
                .iter()
                .chain(tonal.boundaries.iter())
                .chain(std::iter::once(&tonal.softness))
            {
                f32b(h, *value);
            }
            h.update(&[u8::from(tonal.native_ap1)]);
        }
        P::LinearMatrix { rows } => {
            h.update(&[11]);
            for value in rows.iter().flatten() {
                f32b(h, *value);
            }
        }
        P::WhiteBalance { temp, tint } => {
            h.update(&[2]);
            f32b(h, *temp);
            f32b(h, *tint);
        }
        P::Cdl(c) => {
            h.update(&[3]);
            cdl(h, c);
        }
        P::Curves(c) => {
            h.update(&[4]);
            for arr in [&c.master, &c.red, &c.green, &c.blue] {
                for v in arr.iter() {
                    f32b(h, *v);
                }
            }
            for opt in [
                &c.hue_vs_hue,
                &c.hue_vs_sat,
                &c.hue_vs_luma,
                &c.luma_vs_sat,
                &c.sat_vs_sat,
            ] {
                match opt {
                    Some(a) => {
                        h.update(&[1]);
                        for v in a.iter() {
                            f32b(h, *v);
                        }
                    }
                    None => h.update(&[0]),
                }
            }
        }
        P::HslQualifier(q) => {
            h.update(&[5]);
            for v in [
                q.hue[0], q.hue[1], q.sat[0], q.sat[1], q.lum[0], q.lum[1], q.softness,
            ] {
                f32b(h, v);
            }
            cdl(h, &q.correction);
            // Preserve the original default-key identity, while all added
            // selection/refinement state participates in invalidation.
            if q.key_count != 0 || q.matte_levels != [0.0, 0.0] {
                h.update(&[0x51]);
                for value in q.matte_levels {
                    f32b(h, value);
                }
                h.update(&q.key_count.to_le_bytes());
                for key in q.keys.iter().take(q.key_count as usize) {
                    for value in key
                        .hue
                        .into_iter()
                        .chain(key.sat)
                        .chain(key.lum)
                        .chain([key.softness])
                    {
                        f32b(h, value);
                    }
                    h.update(&[u8::from(key.subtract)]);
                }
            }
        }
        P::Lut3d(l) => {
            h.update(&[6]);
            f32b(h, l.intensity);
            h.update(&[l.tetrahedral as u8]);
            h.update(&(l.table.size as u32).to_le_bytes());
            for v in l.table.domain_min.iter().chain(&l.table.domain_max) {
                f32b(h, *v);
            }
            // Digest the table samples too (K-0.5): now that LUTs resolve to real
            // tables, two distinct `.cube` files sharing size + domain must NOT
            // collide, and a LUT whose contents change under the same size/domain
            // must invalidate. The cost is a per-node xxh3 over the table (µs for a
            // 33³ LUT), off the pixel path.
            for sample in &l.table.data {
                for v in sample {
                    f32b(h, *v);
                }
            }
            if let Some(shaper) = &l.table.shaper {
                h.update(&[1]);
                h.update(&(shaper.len() as u32).to_le_bytes());
                for sample in shaper {
                    for v in sample {
                        f32b(h, *v);
                    }
                }
            } else {
                h.update(&[0]);
            }
        }
    }
}

fn effect_kind_tag(k: EffectKind) -> u8 {
    // `EffectKind` is `#[non_exhaustive]`; the wildcard covers effects added
    // after P3. Distinct tags matter only for cache-hash disambiguation, so a
    // future variant sharing tag 255 is a benign (rare) cache miss, never a
    // correctness bug — extend this map when new effects land.
    match k {
        EffectKind::Blur => 0,
        EffectKind::Sharpen => 1,
        EffectKind::Glow => 2,
        EffectKind::ChromaKey => 3,
        EffectKind::LumaKey => 4,
        EffectKind::Invert => 5,
        EffectKind::MaskShapeGen => 6,
        _ => 255,
    }
}

/// A convenience for a full [`TextureDesc`] from a format (used by callers that
/// want to pre-size the output pool bucket).
pub fn output_desc(format: &SequenceFormat) -> TextureDesc {
    TextureDesc {
        width: format.width,
        height: format.height,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_deflicker_nested_import_keeps_upstream_dependencies() {
        use super::super::deflicker::{ClipGains, GainTable, NativeDeflickerKey};
        struct NoLuts;
        impl LutProvider for NoLuts {
            fn lut(&self, _: AssetId) -> Option<std::sync::Arc<photonic_render::Lut3d>> {
                None
            }
        }
        let (mut project, outer, _, _) = native_group_look_fixture();
        let mut nested = project.sequences[&outer].clone();
        nested.id = SequenceId::new();
        let inner = nested.id;
        nested
            .master_effects
            .push(ClipEffect::new(EffectKind::Deflicker));
        project.insert_sequence(nested);
        let outer_sequence = project.sequences.get_mut(&outer).unwrap();
        outer_sequence.video_tracks[0].clips[0].source =
            ClipSource::NestedSequence { sequence: inner };
        outer_sequence
            .master_effects
            .push(ClipEffect::new(EffectKind::Deflicker));
        let key = |sequence| NativeDeflickerKey {
            sequence,
            owner: VfxOwner::Master(sequence),
            clip_context: None,
            effect_index: 0,
        };
        let mut table = GainTable::new();
        match compile_native_deflicker_input(
            &project,
            outer,
            0,
            Tick(15),
            key(outer),
            &NoLuts,
            &table,
            None,
            false,
        ) {
            Err(NativeDeflickerInputError::Pending(deps)) => assert_eq!(deps, vec![key(inner)]),
            other => panic!("nested upstream correction must be measured first: {other:?}"),
        }
        table.insert_native(
            key(inner),
            ClipGains {
                first: Tick(0),
                step: 1,
                gains: vec![[1.1; 3]],
                band: None,
                frame_ticks: 1,
            },
        );
        let frame = compile_native_deflicker_input(
            &project,
            outer,
            0,
            Tick(15),
            key(outer),
            &NoLuts,
            &table,
            None,
            false,
        )
        .unwrap();
        assert!(frame.graph.validate_working_color_domain().is_ok());
        assert!(frame
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeDeflicker { .. })));
        assert!(!frame
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeSdrOutput | IrOp::NativeSdrVideoOutput)));
    }

    #[test]
    fn native_deflicker_owner_inputs_require_only_reachable_upstream_measurements() {
        use super::super::deflicker::{ClipGains, GainTable, NativeDeflickerKey};
        struct NoLuts;
        impl LutProvider for NoLuts {
            fn lut(&self, _: AssetId) -> Option<std::sync::Arc<photonic_render::Lut3d>> {
                None
            }
        }
        let (mut project, seq, _, _) = native_group_look_fixture();
        let aid = *project.media.assets.keys().next().unwrap();
        let fx = || ClipEffect::new(EffectKind::Deflicker);
        project
            .media
            .assets
            .get_mut(&aid)
            .unwrap()
            .effects
            .push(fx());
        let sequence = project.sequences.get_mut(&seq).unwrap();
        let clip = sequence.video_tracks[0].clips[0].id;
        let track = sequence.video_tracks[0].id;
        sequence.video_tracks[0].clips[0].effects.push(fx());
        sequence.video_tracks[0].effects.push(fx());
        sequence.master_effects.push(fx());
        project
            .media
            .assets
            .get_mut(&aid)
            .unwrap()
            .effects
            .push(fx());
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].effects.push(fx());
        sequence.video_tracks[0].effects.push(fx());
        sequence.master_effects.push(fx());
        let keys: Vec<_> = [
            VfxOwner::Asset(aid),
            VfxOwner::Clip(clip),
            VfxOwner::Track(track),
            VfxOwner::Master(seq),
        ]
        .into_iter()
        .flat_map(|owner| {
            (0..2).map(move |effect_index| NativeDeflickerKey {
                sequence: seq,
                owner,
                clip_context: if matches!(owner, VfxOwner::Master(_)) {
                    None
                } else {
                    Some(clip)
                },
                effect_index,
            })
        })
        .collect();
        let empty = GainTable::new();
        let mut table = GainTable::new();
        for (i, key) in keys.iter().copied().enumerate() {
            let raw = compile_native_deflicker_input(
                &project,
                seq,
                0,
                Tick(15),
                key,
                &NoLuts,
                &empty,
                None,
                false,
            );
            if i == 0 {
                assert!(raw.is_ok(), "{raw:?}");
            } else {
                match raw {
                    Err(NativeDeflickerInputError::Pending(deps)) => {
                        for upstream in &keys[..i] {
                            assert!(deps.contains(upstream), "{deps:?} missing{upstream:?}");
                        }
                        assert!(!deps.contains(&key));
                    }
                    other => panic!("expected dependencies, got{other:?}"),
                }
            }
            let ready = compile_native_deflicker_input(
                &project,
                seq,
                0,
                Tick(15),
                key,
                &NoLuts,
                &table,
                None,
                false,
            )
            .expect("upstream measurements supplied");
            assert_eq!(
                ready.graph.output_color_encoding(),
                Ok(super::super::ir::FrameColorEncoding::SceneLinearAcescg)
            );
            let mut seen = HashSet::new();
            let mut todo = vec![ready.graph.output.unwrap()];
            while let Some(n) = todo.pop() {
                if seen.insert(n) {
                    todo.extend(
                        ready.graph.nodes[n.0 as usize]
                            .inputs
                            .iter()
                            .map(|(i, _)| *i),
                    );
                }
            }
            assert!(ready
                .native_deflicker_pending
                .iter()
                .all(|(_, n)| !seen.contains(n)));
            assert!(seen.iter().all(|n| !matches!(
                ready.graph.nodes[n.0 as usize].op,
                IrOp::NativeSdrOutput | IrOp::NativeSdrVideoOutput
            )));
            table.insert_native(
                key,
                ClipGains {
                    first: Tick(0),
                    step: 1,
                    gains: vec![[1.1; 3]],
                    band: None,
                    frame_ticks: 1,
                },
            );
        }
        let missing = NativeDeflickerKey {
            effect_index: 99,
            ..keys[0]
        };
        assert!(matches!(
            compile_native_deflicker_input(
                &project,
                seq,
                0,
                Tick(15),
                missing,
                &NoLuts,
                &table,
                None,
                false
            ),
            Err(NativeDeflickerInputError::Invalid(_))
        ));
    }

    #[test]
    fn native_vector_clip_and_graph_preserve_asset_scope_and_fail_closed() {
        let (mut p, seq, _, _) = native_group_look_fixture();
        let asset = match p.sequences[&seq].video_tracks[0].clips[0].source {
            ClipSource::Asset { asset } => asset,
            _ => unreachable!(),
        };
        let m = p.media.assets.get_mut(&asset).unwrap();
        m.kind = AssetKind::VectorDoc;
        m.native_input_color = None;
        m.source = timeline::AssetSource::EmbeddedVector {
            root: VectorRef::WholeDocument,
        };
        p.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].source =
            ClipSource::Vector { asset };
        let compile =
            |p: &TimelineProject| compile_native_preview(p, seq, 0, Tick(0), Quality::FULL);
        let mut a = compile(&p);
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        assert!(a
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeRasterVector { .. })));
        assert!(a
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op,IrOp::NativeExposure{stops} if stops==0.25)));
        assert!(a.graph.validate_working_color_domain().is_ok());
        let hash = a.graph.nodes.last().unwrap().content_hash;
        a.qualify_native_vectors(None);
        assert!(a
            .diagnostics
            .iter()
            .any(|d| d.code == Some(CompileCode::ColorPipelineUnavailable)));
        assert!(!a
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeRasterVector { .. })));
        assert_ne!(a.graph.nodes.last().unwrap().content_hash, hash);
        assert!(a.graph.validate_working_color_domain().is_ok());
        for root in [
            VectorRef::Artboard(0),
            VectorRef::Node(uuid::Uuid::new_v4()),
        ] {
            let mut bad = p.clone();
            bad.media.assets.get_mut(&asset).unwrap().source =
                timeline::AssetSource::EmbeddedVector { root };
            assert!(compile(&bad)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error));
        }
        let (mut graph, node) = NodeGraph::new_clip_composition("native logo");
        let gid = graph.id;
        graph.nodes.get_mut(&node).unwrap().op = GraphOp::VectorIn {
            vref: VectorRef::WholeDocument,
        };
        p.graphs.insert(gid, graph);
        p.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].composition = Some(gid);
        let a = compile(&p);
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        let mut a = a;
        let d = photonic_core::Document::new("empty valid logo", 64.0, 64.0);
        a.qualify_native_vectors(Some(&d));
        assert!(a.diagnostics.is_empty());
        p.graphs
            .get_mut(&gid)
            .unwrap()
            .nodes
            .get_mut(&node)
            .unwrap()
            .op = GraphOp::VectorIn {
            vref: VectorRef::Artboard(0),
        };
        assert!(compile(&p)
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error));
    }

    #[test]
    fn native_look_stage_outputs_are_distinct_and_follow_exact_link() {
        use timeline::{ClipLook, GradeInspectionStage as Stage, SharedLookId};
        let (mut project, sequence, _, look_id) = native_group_look_fixture();
        let clip = &mut project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0];
        clip.grade.as_mut().unwrap().convert_to_graph();
        let grade = clip.grade.as_ref().unwrap().clone();
        let clip_id = clip.id;
        let output = grade.graph.as_ref().unwrap().output;
        project.shared_looks.get_mut(&look_id).unwrap().grade = grade.clone();
        let compile = |p: &timeline::TimelineProject| {
            compile_native_preview(p, sequence, 0, Tick::ZERO, Quality::FULL)
        };
        let frame = compile(&project);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        let ordinary = frame
            .tap(ScopeTapPoint::GradeGraphImageOutput {
                clip: clip_id,
                node: output,
            })
            .unwrap();
        let tap = |stage| ScopeTapPoint::GradeStageGraphImageOutput {
            clip: clip_id,
            stage,
            node: output,
        };
        assert_eq!(frame.tap(tap(Stage::Clip)), Some(ordinary));
        let shared = frame.tap(tap(Stage::SharedLook { id: look_id })).unwrap();
        assert_ne!(
            shared, ordinary,
            "colliding node IDs must address distinct grade stages"
        );
        let mut cursor = shared;
        let mut stops = Vec::new();
        loop {
            let ir = &frame.graph.nodes[cursor.0 as usize];
            if let IrOp::NativeExposure { stops: value } = ir.op {
                stops.push(value);
            }
            let Some((input, _)) = ir.inputs.first() else {
                break;
            };
            cursor = *input;
        }
        assert_eq!(
            stops,
            vec![2.0, 2.0, 1.0, 0.5, 0.25],
            "look includes clip/group-pre/asset grades but excludes group-post, track and master"
        );
        assert!(frame.tap(tap(Stage::LocalLook)).is_none());
        assert!(frame
            .tap(tap(Stage::SharedLook {
                id: SharedLookId::new()
            }))
            .is_none());
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].look =
            Some(ClipLook::Local(Box::new(grade)));
        let local = compile(&project);
        assert_eq!(local.tap(tap(Stage::LocalLook)), Some(shared));
        assert!(local.tap(tap(Stage::SharedLook { id: look_id })).is_none());
        if let Some(ClipLook::Local(grade)) =
            project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0]
                .look
                .as_mut()
        {
            grade.bypass = true;
        }
        let bypass = compile(&project);
        assert!(bypass.tap(tap(Stage::LocalLook)).is_none());
        assert_eq!(bypass.tap(tap(Stage::Clip)), Some(ordinary));
    }

    #[test]
    fn native_group_member_inspection_is_ordered_typed_and_not_a_clip_alias() {
        use timeline::GradeInspectionStage as Stage;
        let (mut project, sequence, leaf, _) = native_group_look_fixture();
        let parent = project.sequences[&sequence].groups[&leaf].parent.unwrap();
        let curve = GradeOp::new(
            GradeOpKind::Curves,
            GradeOpParams::Curves {
                master: vec![],
                red: vec![],
                green: vec![],
                blue: vec![],
                hue_vs_hue: vec![],
                hue_vs_sat: vec![],
                hue_vs_luma: vec![],
                luma_vs_sat: vec![],
                sat_vs_sat: vec![],
            },
        );
        let op = curve.id;
        let seq = project.sequences.get_mut(&sequence).unwrap();
        for group in seq.groups.values_mut() {
            for grade in [&mut group.pre_grade, &mut group.post_grade]
                .into_iter()
                .flatten()
            {
                grade.ops.push(curve.clone());
                grade.convert_to_graph();
            }
        }
        let clip = &mut seq.video_tracks[0].clips[0];
        clip.grade.as_mut().unwrap().ops.push(curve.clone());
        clip.grade.as_mut().unwrap().convert_to_graph();
        let clip_id = clip.id;
        let node = clip.grade.as_ref().unwrap().graph.as_ref().unwrap().output;
        let mut other = clip.clone();
        other.id = ClipId::new();
        let other_id = other.id;
        other.source = ClipSource::SolidColor {
            color: Color::WHITE,
        };
        let mut other_track = Track::new(TrackKind::Video, "V2");
        other_track.clips.push(other);
        seq.video_tracks.push(other_track);
        let frame = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        for (stage, expected) in [
            (Stage::GroupPre { id: parent }, vec![0.5, 0.25]),
            (Stage::GroupPre { id: leaf }, vec![1.0, 0.5, 0.25]),
            (
                Stage::GroupPost { id: leaf },
                vec![3.0, 2.5, 2.0, 1.0, 0.5, 0.25],
            ),
            (
                Stage::GroupPost { id: parent },
                vec![4.0, 3.0, 2.5, 2.0, 1.0, 0.5, 0.25],
            ),
        ] {
            let tap = |clip| ScopeTapPoint::GradeStageGraphImageOutput { clip, stage, node };
            let output = frame.tap(tap(clip_id)).unwrap();
            let other = frame.tap(tap(other_id)).unwrap();
            assert_ne!(
                output, other,
                "same group must retain each member's actual input"
            );
            let mut cursor = output;
            let mut stops = Vec::new();
            loop {
                let ir = &frame.graph.nodes[cursor.0 as usize];
                if let IrOp::NativeExposure { stops: value } = ir.op {
                    stops.push(value);
                }
                let Some((input, _)) = ir.inputs.first() else {
                    break;
                };
                cursor = *input;
            }
            assert_eq!(stops, expected);
            assert_eq!(
                frame.tap(ScopeTapPoint::GradeStageInput {
                    clip: clip_id,
                    stage,
                    node: None,
                    op
                }),
                Some(output)
            );
            assert!(frame
                .tap(ScopeTapPoint::GradeStageGraphMatteOutput {
                    clip: clip_id,
                    stage,
                    node
                })
                .is_none());
        }
        assert_eq!(
            frame
                .native_curve_inputs
                .iter()
                .filter(|(c, o, _)| *c == clip_id && *o == op)
                .count(),
            1
        );
        assert!(frame
            .tap(ScopeTapPoint::NativeCurveInput { clip: clip_id, op })
            .is_some());
        let output = frame
            .native_stage_graph_outputs
            .iter()
            .find(|x| x.stage == Stage::GroupPre { id: leaf })
            .copied()
            .unwrap();
        assert!(
            GradeGraphStageOutput::find(
                &[output, output],
                output.clip,
                output.stage,
                output.node,
                output.image
            )
            .is_none(),
            "ambiguous duplicate instances must not pick the first"
        );
    }

    #[test]
    fn native_graph_image_outputs_are_scoped_to_the_ordinary_clip_grade() {
        let (mut project, sequence, _, look_id) = native_group_look_fixture();
        let clip = &mut project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0];
        clip.grade.as_mut().unwrap().convert_to_graph();
        let grade = clip.grade.as_ref().unwrap().clone();
        let graph = grade.graph.as_ref().unwrap();
        let clip_id = clip.id;
        let nodes: Vec<_> = graph.nodes.keys().copied().collect();
        project.shared_looks.get_mut(&look_id).unwrap().grade = grade.clone();
        let frame = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame.native_graph_images.len(),
            nodes.len(),
            "shared look IDs must not leak into the clip output scope"
        );
        for node in nodes {
            assert!(grade.has_image_output(node));
            assert!(!grade.has_matte_output(node));
            let output = frame
                .tap(ScopeTapPoint::GradeGraphImageOutput {
                    clip: clip_id,
                    node,
                })
                .unwrap();
            assert_eq!(
                frame.graph.node_color_encoding(output),
                Ok(super::super::ir::FrameColorEncoding::SceneLinearAcescg)
            );
            assert!(frame
                .tap(ScopeTapPoint::GradeGraphMatteOutput {
                    clip: clip_id,
                    node
                })
                .is_none());
        }
        assert!(frame
            .tap(ScopeTapPoint::GradeGraphImageOutput {
                clip: clip_id,
                node: u32::MAX
            })
            .is_none());
    }

    #[test]
    fn native_solid_uses_clip_group_look_track_master_order_and_rejects_bad_swatch() {
        let (mut project, sequence, _, _) = native_group_look_fixture();
        let clip = &mut project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0];
        clip.source = ClipSource::SolidColor {
            color: Color {
                r: 0.5,
                g: 0.5,
                b: 0.5,
                a: 0.25,
            },
        };
        let compiled = compile_native_preview(&project, sequence, 0, Tick(10), Quality::FULL);
        assert!(
            compiled.diagnostics.is_empty(),
            "{:?}",
            compiled.diagnostics
        );
        assert!(compiled.graph.validate_working_color_domain().is_ok());
        let stops: Vec<_> = compiled
            .graph
            .nodes
            .iter()
            .filter_map(|node| match node.op {
                IrOp::NativeExposure { stops } => Some(stops),
                _ => None,
            })
            .collect();
        assert_eq!(stops, vec![0.5, 1.0, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0]);
        assert!(!compiled.graph.nodes.iter().any(|node| matches!(
            node.op,
            IrOp::NativeDecodeVideo { .. } | IrOp::NativeDecodeStill { .. }
        )));
        for invalid in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
            project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].source =
                ClipSource::SolidColor {
                    color: Color {
                        r: invalid,
                        g: 0.5,
                        b: 0.5,
                        a: 1.0,
                    },
                };
            assert!(
                compile_native_preview(&project, sequence, 0, Tick(10), Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
            );
        }
    }

    #[test]
    fn native_dip_swatch_decodes_srgb_and_preserves_coverage() {
        // IEC sRGB midpoint decode, independent of the production helper.
        let linear_mid = ((0.5_f64 + 0.055) / 1.055).powf(2.4);
        for alpha in [0.0, 0.25, 0.6, 1.0] {
            let c = super::native_swatch_color(photonic_core::color::Color {
                r: 0.5,
                g: 0.5,
                b: 0.5,
                a: alpha,
            })
            .unwrap();
            assert_eq!(c.a, alpha);
            for value in [c.r, c.g, c.b] {
                assert!((value as f64 - linear_mid * alpha as f64).abs() < 1e-7);
            }
        }
        for channel in 0..4 {
            for invalid in [f32::NAN, f32::INFINITY, -0.01, 1.01] {
                let mut rgba = [0.5; 4];
                rgba[channel] = invalid;
                assert!(super::native_swatch_color(photonic_core::color::Color {
                    r: rgba[0],
                    g: rgba[1],
                    b: rgba[2],
                    a: rgba[3],
                })
                .is_none());
            }
        }
        let red = super::native_swatch_color(photonic_core::color::Color::RED).unwrap();
        // Independently rounded D65 sRGB -> D60 AP1 Bradford reference.
        for (actual, expected) in [red.r, red.g, red.b]
            .into_iter()
            .zip([0.61310, 0.07019, 0.02062])
        {
            assert!((actual - expected).abs() < 0.0002);
        }
    }

    use super::*;

    #[test]
    fn transparent_transform_has_distinct_cache_identity() {
        let mat = Mat3::from_translation(glam::Vec2::new(2.0, -1.0));
        let legacy = content_hash(
            &IrOp::Transform2D {
                mat,
                sampling: Sampling::Bilinear,
            },
            &[],
            &[],
        );
        let native = content_hash(
            &IrOp::Transform2DTransparent {
                mat,
                sampling: Sampling::Bilinear,
            },
            &[],
            &[],
        );
        assert_ne!(legacy, native);
    }

    #[test]
    fn qualifier_spatial_lowering_domains_inspection_and_failure_contract() {
        let mut grade = Grade::new();
        let mut op = GradeOp::new(
            GradeOpKind::HslQualifier,
            GradeOpParams::HslQualifier {
                hue: [0.0, 1.0],
                sat: [0.0, 1.0],
                lum: [0.2, 0.6],
                softness: 0.1,
                correction: timeline::CdlParams {
                    offset: [0.01; 3],
                    ..timeline::CdlParams::identity()
                },
                keys: vec![],
                matte_levels: [0.0; 2],
            },
        );
        op.qualifier_spatial = Some(timeline::GradeQualifierSpatial {
            denoise: true,
            grow: 0.01,
            blur: 0.005,
        });
        grade.ops.push(op);
        let clip = ClipId::new();
        let compile = |grade: &Grade, native: bool| {
            let mut b = Builder::new();
            if native {
                b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
            }
            // Inspection metadata is only owned by an ordinary outer sequence.
            b.sequence_path.push(SequenceId::new());
            let input = b.push(
                IrOp::SolidColor {
                    color: LinearColor {
                        r: 0.18,
                        g: 0.18,
                        b: 0.18,
                        a: 0.5,
                    },
                },
                vec![],
            );
            let output = apply_grade(&mut b, grade, input, Tick::ZERO, Some(VfxOwner::Clip(clip)));
            (input, output, b.finish(Some(output)))
        };
        let (_, _, result) = compile(&grade, true);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert!(result.graph.validate_working_color_domain().is_ok());
        assert_eq!(
            result
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(
                    n.op,
                    IrOp::NativeAcescct {
                        direction: photonic_render::native_transfer::AcescctDirection::Encode
                    }
                ))
                .count(),
            1
        );
        assert!(!result.graph.nodes.iter().any(|n| matches!(
            n.op,
            IrOp::NativeAcescct {
                direction: photonic_render::native_transfer::AcescctDirection::Decode
            } | IrOp::NativeLogCdl { .. }
                | IrOp::GradeMatteApply
        )));

        assert!(result
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::GradeMatteRefine { .. })));
        assert!(result
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeMatteCdl { .. })));
        assert!(!result
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeLogQualifier { .. })));
        assert_eq!(
            result.native_qualifier_inspections[0].spatial,
            grade.ops[0].qualifier_spatial
        );
        let hash = result.graph.nodes[result.graph.output.unwrap().0 as usize].content_hash;
        let mut changed = grade.clone();
        changed.ops[0].qualifier_spatial.as_mut().unwrap().blur = 0.01;
        let (_, _, other) = compile(&changed, true);
        assert_ne!(
            hash,
            other.graph.nodes[other.graph.output.unwrap().0 as usize].content_hash
        );
        let (_, _, legacy) = compile(&grade, false);
        assert!(!legacy.diagnostics.is_empty());
        changed.ops[0].qualifier_spatial.as_mut().unwrap().grow = 0.03;
        assert!(!compile(&changed, true).2.diagnostics.is_empty());
        if let GradeOpParams::HslQualifier { correction, .. } = &mut grade.ops[0].params.base {
            *correction = timeline::CdlParams::identity();
        }
        let (input, output, neutral) = compile(&grade, true);
        assert_eq!(input, output);
        assert_eq!(neutral.native_qualifier_inspections.len(), 1);
        grade.convert_to_graph();
        let (_, _, graph) = compile(&grade, true);
        assert!(graph.diagnostics.is_empty());
        assert!(graph.graph.validate_working_color_domain().is_ok());
    }

    #[test]
    fn tonal_grades_preserve_four_scope_order_and_graph_lowering() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let make = |stops| {
            let mut grade = Grade::new();
            grade.ops.push(GradeOp::new(
                GradeOpKind::TonalExposure,
                GradeOpParams::TonalExposure {
                    stops: [stops; 3],
                    boundaries: [-2.0, 2.0],
                    softness: 1.0,
                },
            ));
            grade
        };
        let asset = *project.media.assets.keys().next().unwrap();
        project.media.assets.get_mut(&asset).unwrap().grade = Some(make(0.1));
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].grade = Some(make(0.2));
        sequence.video_tracks[0].grade = Some(make(0.3));
        sequence.master_grade = Some(make(0.4));
        sequence.video_tracks[0].clips[0]
            .grade
            .as_mut()
            .unwrap()
            .convert_to_graph();
        let frame = compile_native_preview(&project, seq, 0, Tick::ZERO, Quality::FULL);
        assert!(
            frame
                .diagnostics
                .iter()
                .all(|d| d.severity != DiagSeverity::Error),
            "{:?}",
            frame.diagnostics
        );
        assert!(frame.graph.validate_working_color_domain().is_ok());
        let stops: Vec<_> = frame
            .graph
            .nodes
            .iter()
            .filter_map(|node| {
                if let IrOp::NativeTonalExposure { tonal } = node.op {
                    Some(tonal.stops[0])
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(stops, vec![0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn tonal_compiler_identity_domain_refusal_and_hash_are_explicit() {
        use photonic_render::tonal_exposure::ResolvedTonalExposure;
        let compile = |stops, boundaries, native| {
            let mut b = Builder::new();
            if native {
                b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
            }
            let input = b.push(
                IrOp::SolidColor {
                    color: LinearColor {
                        r: 0.09,
                        g: 0.09,
                        b: 0.09,
                        a: 0.5,
                    },
                },
                vec![],
            );
            let mut grade = Grade::new();
            grade.ops.push(GradeOp::new(
                GradeOpKind::TonalExposure,
                GradeOpParams::TonalExposure {
                    stops,
                    boundaries,
                    softness: 1.0,
                },
            ));
            let output = apply_grade(&mut b, &grade, input, Tick::ZERO, None);
            (input, output, b.finish(Some(output)))
        };
        for native in [false, true] {
            let (input, output, result) = compile([0.0; 3], [-2.0, 2.0], native);
            assert_eq!(input, output);
            assert!(result.diagnostics.is_empty());
            let (_, _, result) = compile([1.0, -0.5, 0.25], [-2.0, 2.0], native);
            assert!(result.diagnostics.is_empty());
            assert!(result.graph.validate_working_color_domain().is_ok());
            assert_eq!(
                result
                    .graph
                    .nodes
                    .iter()
                    .filter(|n| matches!(n.op, IrOp::NativeTonalExposure { .. }))
                    .count(),
                usize::from(native)
            );
            let (_, _, result) = compile([1.0; 3], [2.0, -2.0], native);
            assert!(!result.diagnostics.is_empty());
        }
        let tonal = ResolvedTonalExposure {
            stops: [1.0, -0.5, 0.25],
            boundaries: [-2.0, 2.0],
            softness: 1.0,
            native_ap1: true,
        };
        let baseline = content_hash(&IrOp::NativeTonalExposure { tonal }, &[], &[]);
        for varied in [
            ResolvedTonalExposure {
                stops: [1.0, 0.0, 0.25],
                ..tonal
            },
            ResolvedTonalExposure {
                boundaries: [-3.0, 2.0],
                ..tonal
            },
            ResolvedTonalExposure {
                softness: 2.0,
                ..tonal
            },
        ] {
            assert_ne!(
                baseline,
                content_hash(&IrOp::NativeTonalExposure { tonal: varied }, &[], &[])
            );
        }
        let (_, _, mut result) = compile([1.0; 3], [-2.0, 2.0], true);
        let last = result
            .graph
            .nodes
            .iter_mut()
            .find(|n| matches!(n.op, IrOp::NativeTonalExposure { .. }))
            .unwrap();
        let IrOp::NativeTonalExposure { ref mut tonal } = last.op else {
            panic!()
        };
        tonal.native_ap1 = false;
        assert!(result.graph.validate_working_color_domain().is_err());
    }

    #[test]
    fn kelvin_compiler_uses_native_matrix_and_preserves_identity_and_diagnostics() {
        let compile = |source_kelvin, target_kelvin, native| {
            let mut b = Builder::new();
            if native {
                b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
            }
            let input = b.push(
                IrOp::SolidColor {
                    color: LinearColor {
                        r: 0.18,
                        g: 0.18,
                        b: 0.18,
                        a: 0.5,
                    },
                },
                vec![],
            );
            let grade = Grade {
                ops: vec![GradeOp::new(
                    GradeOpKind::WhiteBalanceCct,
                    GradeOpParams::WhiteBalanceCct {
                        source_kelvin,
                        target_kelvin,
                    },
                )],
                bypass: false,
                graph: None,
            };
            let output = apply_grade(&mut b, &grade, input, Tick::ZERO, None);
            (input, output, b.finish(Some(output)))
        };
        for native in [false, true] {
            let (input, output, identity) = compile(6500.0, 6500.0, native);
            assert_eq!(input, output);
            assert!(identity.diagnostics.is_empty());
            let (_, _, active) = compile(3200.0, 6500.0, native);
            assert!(active.diagnostics.is_empty());
            assert!(active.graph.validate_working_color_domain().is_ok());
            assert_eq!(
                active
                    .graph
                    .nodes
                    .iter()
                    .filter(|n| matches!(n.op, IrOp::NativeRgbMatrix { .. }))
                    .count(),
                usize::from(native)
            );
            let (_, _, invalid) = compile(1500.0, 6500.0, native);
            assert!(!invalid.diagnostics.is_empty());
        }
        let a = photonic_render::white_balance::cct_matrix(3200.0, 6500.0, true).unwrap();
        let b = photonic_render::white_balance::cct_matrix(4000.0, 6500.0, true).unwrap();
        assert_ne!(
            content_hash(&IrOp::NativeRgbMatrix { rows: a }, &[], &[]),
            content_hash(&IrOp::NativeRgbMatrix { rows: b }, &[], &[])
        );
    }

    #[test]
    fn native_matte_cdl_hash_separates_fused_semantics_and_parameters() {
        let cdl = photonic_render::grade::ResolvedCdl {
            slope: [1.0; 3],
            offset: [0.04; 3],
            power: [1.0; 3],
            sat: 1.0,
        };
        let fused = content_hash(&IrOp::NativeMatteCdl { cdl }, &[], &[]);
        assert_ne!(fused, content_hash(&IrOp::NativeLogCdl { cdl }, &[], &[]));
        for changed in [
            photonic_render::grade::ResolvedCdl {
                slope: [0.9; 3],
                ..cdl
            },
            photonic_render::grade::ResolvedCdl {
                offset: [0.05; 3],
                ..cdl
            },
            photonic_render::grade::ResolvedCdl {
                power: [1.1; 3],
                ..cdl
            },
            photonic_render::grade::ResolvedCdl { sat: 0.9, ..cdl },
        ] {
            assert_ne!(
                fused,
                content_hash(&IrOp::NativeMatteCdl { cdl: changed }, &[], &[])
            );
        }
    }

    #[test]
    fn native_qualifier_neutral_window_and_cache_contract() {
        let compile = |correction: timeline::CdlParams, hue, mask| {
            let mut b = Builder::new();
            b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
            let source = b.push(
                IrOp::SolidColor {
                    color: LinearColor {
                        r: 0.18,
                        g: 0.18,
                        b: 0.18,
                        a: 0.5,
                    },
                },
                vec![],
            );
            let mut grade = Grade::new();
            let mut op = GradeOp::new(
                GradeOpKind::HslQualifier,
                GradeOpParams::HslQualifier {
                    hue,
                    sat: [0.0, 1.0],
                    lum: [0.0, 1.0],
                    softness: 0.1,
                    correction,
                    keys: vec![],
                    matte_levels: [0.0, 0.0],
                },
            );
            op.mask = mask;
            grade.ops.push(op);
            let output = apply_grade(&mut b, &grade, source, Tick::ZERO, None);
            (source, output, b.finish(Some(output)))
        };
        let (source, output, frame) = compile(timeline::CdlParams::identity(), [0.0, 1.0], None);
        assert_eq!(source, output);
        assert!(frame.diagnostics.is_empty());
        let correction = timeline::CdlParams {
            offset: [0.01; 3],
            ..timeline::CdlParams::identity()
        };
        let mask = timeline::GradeMask::PowerWindow {
            shape: timeline::WindowShape::Rectangle,
            center: [0.5; 2],
            size: [0.2; 2],
            rotation: 0.0,
            softness: 0.1,
            invert: false,
        };
        let (_, _, frame) = compile(correction, [-0.1, 0.1], Some(mask));
        assert!(frame.diagnostics.is_empty());
        assert!(frame.graph.validate_working_color_domain().is_ok());
        assert!(frame
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeMaskMix { .. })));
        let q = frame
            .graph
            .nodes
            .iter()
            .find_map(|n| {
                if let IrOp::NativeLogQualifier { qualifier } = &n.op {
                    Some(qualifier.clone())
                } else {
                    None
                }
            })
            .unwrap();
        let hash = |q| {
            content_hash(
                &IrOp::NativeLogQualifier {
                    qualifier: Box::new(q),
                },
                &[],
                &[],
            )
        };
        let baseline = hash(*q);
        for variant in 0..6 {
            let mut changed = *q;
            match variant {
                0 => changed.hue[0] -= 0.01,
                1 => changed.sat[0] += 0.01,
                2 => changed.lum[0] += 0.01,
                3 => changed.softness += 0.01,
                4 => changed.matte_levels[0] += 0.01,
                _ => {
                    changed.key_count = 1;
                    changed.keys[0].subtract = true;
                }
            }
            assert_ne!(baseline, hash(changed));
        }
        let (source, output, invalid) = compile(correction, [0.5, 0.2], None);
        assert_eq!(source, output);
        assert!(invalid
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error));
    }

    #[test]
    fn native_white_balance_lowers_gains_and_preserves_neutral_identity() {
        let compile = |temp, tint| {
            let mut builder = Builder::new();
            builder.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
            let source = builder.push(
                IrOp::SolidColor {
                    color: LinearColor {
                        r: -0.1,
                        g: 0.18,
                        b: 4.0,
                        a: 0.5,
                    },
                },
                vec![],
            );
            let mut grade = Grade::new();
            grade.ops.push(GradeOp::new(
                GradeOpKind::WhiteBalance,
                GradeOpParams::WhiteBalance { temp, tint },
            ));
            let output = apply_grade(&mut builder, &grade, source, Tick::ZERO, None);
            (source, output, builder.finish(Some(output)))
        };
        let (source, output, neutral) = compile(0.0, 0.0);
        assert_eq!(source, output);
        assert!(neutral.diagnostics.is_empty());
        let mut hashes = std::collections::HashSet::new();
        for (temp, tint) in [(-1.0, -1.0), (1.0, 1.0), (0.2, -0.3), (0.2, 0.3)] {
            let (_, output, frame) = compile(temp, tint);
            assert!(frame.diagnostics.is_empty());
            assert!(frame.graph.validate_working_color_domain().is_ok());
            let IrOp::NativePrinterLights { points } = frame.graph.nodes[output.0 as usize].op
            else {
                panic!("white balance must lower to native scene gains")
            };
            let expected = [1.0 + 0.4 * temp, 1.0 - 0.2 * tint, 1.0 - 0.4 * temp];
            for (point, gain) in points.into_iter().zip(expected) {
                assert!((2.0_f32.powf(point / 12.0) - gain).abs() < 1e-6);
            }
            assert!(hashes.insert(content_hash(
                &IrOp::NativePrinterLights { points },
                &[],
                &[]
            )));
        }
        for (temp, tint) in [
            (2.0, 0.0),
            (0.0, -2.0),
            (f32::NAN, 0.0),
            (0.0, f32::INFINITY),
        ] {
            let (source, output, frame) = compile(temp, tint);
            assert_eq!(source, output);
            assert!(frame
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error));
        }
    }

    #[test]
    fn native_curves_neutral_cache_and_secondary_family_contract() {
        let identity = photonic_render::grade::curve_lut(&[]);
        let curves = photonic_render::grade::ResolvedCurves {
            master: identity,
            red: identity,
            green: identity,
            blue: identity,
            hue_vs_hue: None,
            hue_vs_sat: None,
            hue_vs_luma: None,
            luma_vs_sat: None,
            sat_vs_sat: None,
        };
        let hash = |curves| {
            content_hash(
                &IrOp::NativeLogCurves {
                    curves: Box::new(curves),
                },
                &[],
                &[],
            )
        };
        for channel in 0..9 {
            let mut changed = curves.clone();
            match channel {
                0 => changed.master[100] += 0.01,
                1 => changed.red[100] += 0.01,
                2 => changed.green[100] += 0.01,
                3 => changed.blue[100] += 0.01,
                4 => changed.hue_vs_hue = Some([0.6; 256]),
                5 => changed.hue_vs_sat = Some([0.6; 256]),
                6 => changed.hue_vs_luma = Some([0.6; 256]),
                7 => changed.luma_vs_sat = Some([0.6; 256]),
                _ => changed.sat_vs_sat = Some([0.6; 256]),
            }
            assert_ne!(hash(curves.clone()), hash(changed));
        }
        for (shift, secondary) in [
            (0.0, None),
            (0.02, None),
            (0.0, Some(0.5)),
            (0.0, Some(0.6)),
            (0.0, Some(f32::NAN)),
        ] {
            let mut b = Builder::new();
            b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
            let source = b.push(
                IrOp::SolidColor {
                    color: LinearColor {
                        r: 0.1,
                        g: 0.2,
                        b: 0.3,
                        a: 1.0,
                    },
                },
                vec![],
            );
            let mut grade = Grade::new();
            grade.ops.push(GradeOp::new(
                GradeOpKind::Curves,
                GradeOpParams::Curves {
                    master: vec![(0.0, shift), (1.0, 1.0 + shift)],
                    red: vec![],
                    green: vec![],
                    blue: vec![],
                    hue_vs_hue: vec![],
                    hue_vs_sat: secondary
                        .map(|value| vec![(0.0, value), (1.0, value)])
                        .unwrap_or_default(),
                    hue_vs_luma: vec![],
                    luma_vs_sat: vec![],
                    sat_vs_sat: vec![],
                },
            ));
            let output = apply_grade(&mut b, &grade, source, Tick::ZERO, None);
            let frame = b.finish(Some(output));
            assert_eq!(
                !frame.diagnostics.is_empty(),
                secondary.is_some_and(|v| v.is_nan()),
                "{:?}",
                frame.diagnostics
            );
            assert!(frame.graph.validate_working_color_domain().is_ok());
            assert_eq!(
                frame
                    .graph
                    .nodes
                    .iter()
                    .any(|n| matches!(n.op, IrOp::NativeLogCurves { .. })),
                (shift != 0.0 || secondary.is_some_and(|v| v != 0.5))
                    && !secondary.is_some_and(|v| v.is_nan())
            );
            if shift == 0.0
                && (secondary.is_none_or(|v| v == 0.5) || secondary.is_some_and(|v| v.is_nan()))
            {
                assert_eq!(output, source);
            }
        }
    }

    #[test]
    fn native_cdl_and_wheels_neutral_lowering_validation_and_cache() {
        use photonic_render::grade::ResolvedCdl;
        let neutral = ResolvedCdl {
            slope: [1.0; 3],
            offset: [0.0; 3],
            power: [1.0; 3],
            sat: 1.0,
        };
        let hash = |cdl| content_hash(&IrOp::NativeLogCdl { cdl }, &[], &[]);
        for field in 0..10 {
            let mut changed = neutral;
            match field {
                0..=2 => changed.slope[field] = 1.1,
                3..=5 => changed.offset[field - 3] = 0.1,
                6..=8 => changed.power[field - 6] = 1.1,
                _ => changed.sat = 1.1,
            }
            assert_ne!(hash(neutral), hash(changed));
        }
        for (kind, params, active, invalid) in [
            (
                GradeOpKind::Cdl,
                GradeOpParams::Cdl {
                    slope: [1.0; 3],
                    offset: [0.0; 3],
                    power: [1.0; 3],
                    sat: 1.0,
                },
                false,
                false,
            ),
            (
                GradeOpKind::Wheels,
                GradeOpParams::Wheels {
                    lift: [0.0; 3],
                    gamma: [1.0; 3],
                    gain: [1.0; 3],
                    sat: 1.0,
                },
                false,
                false,
            ),
            (
                GradeOpKind::Cdl,
                GradeOpParams::Cdl {
                    slope: [1.1; 3],
                    offset: [0.02; 3],
                    power: [0.9; 3],
                    sat: 1.1,
                },
                true,
                false,
            ),
            (
                GradeOpKind::Wheels,
                GradeOpParams::Wheels {
                    lift: [-0.1; 3],
                    gamma: [1.1; 3],
                    gain: [1.1; 3],
                    sat: 1.1,
                },
                true,
                false,
            ),
            (
                GradeOpKind::Cdl,
                GradeOpParams::Cdl {
                    slope: [-0.1; 3],
                    offset: [0.0; 3],
                    power: [1.0; 3],
                    sat: 1.0,
                },
                false,
                true,
            ),
        ] {
            let mut b = Builder::new();
            b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
            let source = b.push(
                IrOp::SolidColor {
                    color: LinearColor {
                        r: 0.1,
                        g: 0.2,
                        b: 0.3,
                        a: 1.0,
                    },
                },
                vec![],
            );
            let mut grade = Grade::new();
            grade.ops.push(GradeOp::new(kind, params));
            let output = apply_grade(&mut b, &grade, source, Tick::ZERO, None);
            let frame = b.finish(Some(output));
            assert_eq!(
                !frame.diagnostics.is_empty(),
                invalid,
                "{:?}",
                frame.diagnostics
            );
            assert!(frame.graph.validate_working_color_domain().is_ok());
            assert_eq!(
                frame
                    .graph
                    .nodes
                    .iter()
                    .any(|n| matches!(n.op, IrOp::NativeLogCdl { .. })),
                active
            );
            if !active {
                assert_eq!(output, source);
            }
        }
    }

    #[test]
    fn native_contrast_cache_and_neutral_lowering_contract() {
        let hash =
            |pivot, amount| content_hash(&IrOp::NativeLogContrast { pivot, amount }, &[], &[]);
        assert_ne!(hash(0.4, 1.0), hash(0.5, 1.0));
        assert_ne!(hash(0.4, 1.0), hash(0.4, 2.0));
        for amount in [0.0, 1.0] {
            let mut builder = Builder::new();
            builder.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
            let source = builder.push(
                IrOp::SolidColor {
                    color: LinearColor {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.0,
                    },
                },
                vec![],
            );
            let mut grade = Grade::new();
            grade.ops.push(GradeOp::new(
                GradeOpKind::Contrast,
                GradeOpParams::Contrast { pivot: 0.4, amount },
            ));
            let output = apply_grade(&mut builder, &grade, source, Tick::ZERO, None);
            let frame = builder.finish(Some(output));
            assert!(frame.diagnostics.is_empty());
            assert!(frame.graph.validate_working_color_domain().is_ok());
            assert_eq!(
                frame
                    .graph
                    .nodes
                    .iter()
                    .filter(|n| matches!(n.op, IrOp::NativeLogContrast { .. }))
                    .count(),
                usize::from(amount != 0.0)
            );
            if amount == 0.0 {
                assert_eq!(output, source);
            }
        }
    }

    #[test]
    fn native_gaussian_stacks_preserve_scope_order_and_fail_closed() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let blur = |radius| {
            let mut fx =
                ClipEffect::from_manifest(timeline::EffectId::new_static("blur.gaussian")).unwrap();
            // Seed entries already contain radius; replace rather than shadow it.
            fx.params
                .base
                .entries
                .retain(|(path, _)| path.as_str() != "params.radius");
            fx.params
                .base
                .entries
                .push((PropPath::new("params.radius"), PropValue::Float(radius)));
            fx
        };
        let asset = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&asset)
            .unwrap()
            .effects
            .push(blur(1.0));
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].effects.push(blur(2.0));
        sequence.video_tracks[0].effects.push(blur(3.0));
        sequence.master_effects.push(blur(4.0));
        let frame = compile_native_preview(&project, seq, 0, Tick::ZERO, Quality::FULL);
        assert!(
            frame
                .diagnostics
                .iter()
                .all(|d| d.severity != DiagSeverity::Error),
            "{:?}",
            frame.diagnostics
        );
        assert_eq!(frame.graph.validate_working_color_domain(), Ok(()));
        let sigmas: Vec<_> = frame
            .graph
            .nodes
            .iter()
            .filter_map(|n| match n.op {
                IrOp::NativeGaussianBlur { sigma } => Some(sigma),
                _ => None,
            })
            .collect();
        assert_eq!(sigmas, vec![1.0, 2.0, 3.0, 4.0]);
        let mut legacy_id = project.clone();
        let effect = &mut legacy_id.sequences.get_mut(&seq).unwrap().master_effects[0];
        effect.id = timeline::EffectId::EMPTY;
        effect.version = 0;
        assert!(
            compile_native_preview(&legacy_id, seq, 0, Tick::ZERO, Quality::FULL)
                .diagnostics
                .iter()
                .all(|d| d.severity != DiagSeverity::Error)
        );
        let signature = content_hash(&IrOp::NativeGaussianBlur { sigma: 1.0 }, &[], &[]);
        assert_ne!(
            signature,
            content_hash(&IrOp::NativeGaussianBlur { sigma: 2.0 }, &[], &[])
        );
        assert_ne!(
            signature,
            content_hash(&IrOp::NativeExposure { stops: 1.0 }, &[], &[])
        );
        // Clip/asset keys use relative time; track/master keys use sequence time.
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].start = Tick(100);
        let clip_effect = &mut sequence.video_tracks[0].clips[0].effects[0];
        clip_effect.zone = Some((Tick(5), Tick(15)));
        let lane = clip_effect
            .params
            .track_mut(&PropPath::new("params.radius"));
        lane.insert_keyframe(timeline::Keyframe::new(
            Tick(0),
            PropValue::Float(2.0),
            timeline::Interp::Linear,
        ));
        lane.insert_keyframe(timeline::Keyframe::new(
            Tick(20),
            PropValue::Float(4.0),
            timeline::Interp::Linear,
        ));
        for (tick, expected) in [
            (104, vec![1.0, 3.0, 4.0]),
            (110, vec![1.0, 3.0, 3.0, 4.0]),
            (115, vec![1.0, 3.0, 4.0]),
        ] {
            let frame = compile_native_preview(&project, seq, 0, Tick(tick), Quality::FULL);
            assert!(frame
                .diagnostics
                .iter()
                .all(|d| d.severity != DiagSeverity::Error));
            let sigmas: Vec<_> = frame
                .graph
                .nodes
                .iter()
                .filter_map(|n| match n.op {
                    IrOp::NativeGaussianBlur { sigma } => Some(sigma),
                    _ => None,
                })
                .collect();
            assert_eq!(sigmas, expected);
        }
        project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].start = Tick::ZERO;
        for bad in [f64::NAN, -1.0, 501.0] {
            project.sequences.get_mut(&seq).unwrap().master_effects = vec![blur(bad)];
            let invalid = compile_native_preview(&project, seq, 0, Tick::ZERO, Quality::FULL);
            assert!(invalid
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error));
        }
        let mut unsupported = ClipEffect::new(EffectKind::Deflicker);
        unsupported.enabled = false;
        project.sequences.get_mut(&seq).unwrap().master_effects = vec![unsupported.clone()];
        assert!(
            compile_native_preview(&project, seq, 0, Tick::ZERO, Quality::FULL)
                .diagnostics
                .iter()
                .all(|d| d.severity != DiagSeverity::Error)
        );
        unsupported.enabled = true;
        project.sequences.get_mut(&seq).unwrap().master_effects = vec![unsupported];
        assert!(
            compile_native_preview(&project, seq, 0, Tick::ZERO, Quality::FULL)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error)
        );
    }

    #[test]
    fn caption_fade_clock_extremes_preserve_lead_in() {
        let lead = ms_to_ticks(150);
        for start in [
            i64::MIN,
            i64::MIN + lead,
            -lead,
            0,
            lead,
            i64::MAX - lead,
            i64::MAX,
        ] {
            let word = CaptionWord::new("HI", Tick(start), Tick(start.saturating_add(1)));
            for tick in [
                i64::MIN,
                i64::MAX,
                start,
                start.saturating_sub(lead),
                start.saturating_sub(lead / 2),
            ] {
                let elapsed = tick as i128 - start as i128 + lead as i128;
                let expected = if elapsed <= 0 {
                    0.0
                } else if elapsed >= lead as i128 {
                    1.0
                } else {
                    elapsed as f32 / lead as f32
                };
                assert_eq!(fade_word_opacity(&word, Tick(tick)), expected);
            }
        }
    }

    #[test]
    fn native_caption_long_clock_fade_does_not_overflow() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let mut track = CaptionTrack::new("Long-clock caption");
        track.style.stroke = None;
        track.style.animation = CaptionAnim::FadeWords;
        track.cues.push(CaptionCue::new(
            Tick(0),
            Tick(i64::MAX),
            vec![CaptionWord::new("HI", Tick(0), Tick(1))],
        ));
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.caption_tracks.push(track);
        let (batch, _) =
            resolve_native_caption_batch(sequence, Tick(i64::MAX - 1), &sequence.formats[0])
                .unwrap();
        assert_eq!(batch.cues[0].words[0].color[3], 255);
    }

    #[test]
    fn native_caption_animations_and_word_pop_resolve_exact_authored_time() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let mut track = CaptionTrack::new("Animated captions");
        track.style.stroke = None;
        track.style.position = [0.5, 0.98];
        track.cues.push(CaptionCue::new(
            Tick(0),
            Tick(ms_to_ticks(500)),
            vec![
                CaptionWord::new("e\u{301}👩🏽‍🚀", Tick(0), Tick(ms_to_ticks(200))),
                CaptionWord::new("OK", Tick(ms_to_ticks(200)), Tick(ms_to_ticks(400))),
            ],
        ));
        project
            .sequences
            .get_mut(&seq)
            .unwrap()
            .caption_tracks
            .push(track);
        let resolved = |p: &TimelineProject, ms: i64| {
            let frame = compile_native_preview(p, seq, 0, Tick(ms_to_ticks(ms)), Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            frame.graph.nodes.iter().find_map(|n| {
                if let IrOp::NativeCaptionOutput { cue_batch, .. } = &n.op {
                    Some((cue_batch.clone(), n.content_hash))
                } else {
                    None
                }
            })
        };
        for anim in [
            CaptionAnim::SlideUp,
            CaptionAnim::FadeWords,
            CaptionAnim::Typewriter,
        ] {
            project.sequences.get_mut(&seq).unwrap().caption_tracks[0]
                .style
                .animation = anim;
            let start = resolved(&project, 0);
            let mid = resolved(&project, 100).unwrap();
            let final_cue = resolved(&project, 400).unwrap();
            assert_ne!(mid.1, final_cue.1);
            match anim {
                CaptionAnim::SlideUp => {
                    let start = start.unwrap();
                    assert!((start.0.cues[0].anchor[1] - 1.03).abs() < 1e-6);
                    assert_eq!(start.0.cues[0].words[0].color[3], 0);
                    assert_eq!(mid.0.cues[0].words[0].color[3], 128);
                    assert!((mid.0.cues[0].anchor[1] - 1.005).abs() < 1e-6);
                    assert!((final_cue.0.cues[0].anchor[1] - 0.98).abs() < 1e-6);
                }
                CaptionAnim::FadeWords => {
                    assert_eq!(start.unwrap().0.cues[0].words[1].color[3], 0);
                    assert_eq!(mid.0.cues[0].words[1].color[3], 85);
                    assert_eq!(final_cue.0.cues[0].words[1].color[3], 255);
                }
                CaptionAnim::Typewriter => {
                    assert!(start.is_none());
                    assert_eq!(mid.0.cues[0].words[0].text, "e\u{301}");
                    assert_eq!(mid.0.cues[0].words[1].text, "");
                    assert_eq!(final_cue.0.cues[0].words[0].text, "e\u{301}👩🏽‍🚀");
                    assert_eq!(final_cue.0.cues[0].words[1].text, "OK");
                }
                _ => unreachable!(),
            }
            assert!(resolved(&project, 500).is_none());
        }
        let track = &mut project.sequences.get_mut(&seq).unwrap().caption_tracks[0];
        track.style.animation = CaptionAnim::None;
        track.style.highlight = Some(KaraokeStyle {
            mode: KaraokeMode::WordPop,
            active_color: Color::WHITE,
            inactive_color: Color::BLACK,
        });
        let first = resolved(&project, 0).unwrap();
        let second = resolved(&project, 200).unwrap();
        assert_ne!(first.1, second.1);
        assert_eq!(first.0.cues[0].words[0].color, [255; 4]);
        assert_eq!(first.0.cues[0].words[1].color, [0, 0, 0, 255]);
        assert_eq!(second.0.cues[0].words[0].color, [0, 0, 0, 255]);
        assert_eq!(second.0.cues[0].words[1].color, [255; 4]);
        // Unrevealed authored text is still subject to the admission budget.
        let track = &mut project.sequences.get_mut(&seq).unwrap().caption_tracks[0];
        track.style.animation = CaptionAnim::Typewriter;
        track.cues[0].words[0].text = "X".repeat(4097);
        assert!(
            compile_native_preview(&project, seq, 0, Tick(0), Quality::FULL)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error)
        );
    }

    #[test]
    fn caption_direction_resolution_hash_and_word_scope_are_explicit() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let mut track = CaptionTrack::new("Direction");
        track.style.stroke = None;
        track.cues.push(CaptionCue::new(
            Tick(0),
            Tick(200),
            vec![CaptionWord::new("Brand שלום", Tick(0), Tick(200))],
        ));
        project
            .sequences
            .get_mut(&seq)
            .unwrap()
            .caption_tracks
            .push(track);
        let mut hashes = HashSet::new();
        for direction in [
            timeline::TextDirection::Auto,
            timeline::TextDirection::Ltr,
            timeline::TextDirection::Rtl,
        ] {
            project.sequences.get_mut(&seq).unwrap().caption_tracks[0]
                .style
                .direction = direction;
            let frame = compile_native_preview(&project, seq, 0, Tick(50), Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            let (batch, op) = frame
                .graph
                .nodes
                .iter()
                .find_map(|n| {
                    if let IrOp::NativeCaptionOutput { cue_batch, .. } = &n.op {
                        Some((cue_batch, &n.op))
                    } else {
                        None
                    }
                })
                .unwrap();
            assert_eq!(batch.cues[0].direction, direction);
            assert!(hashes.insert(content_hash(op, &[], &[])));
        }
        let track = &mut project.sequences.get_mut(&seq).unwrap().caption_tracks[0];
        let mut incompatible = track.style.clone();
        incompatible.direction = timeline::TextDirection::Ltr;
        track.cues[0].words[0].style_override = Some(incompatible);
        assert!(
            compile_native_preview(&project, seq, 0, Tick(50), Quality::FULL)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error)
        );
        project.sequences.get_mut(&seq).unwrap().caption_tracks[0].cues[0].words[0]
            .style_override
            .as_mut()
            .unwrap()
            .direction = timeline::TextDirection::Auto;
        assert!(
            compile_native_preview(&project, seq, 0, Tick(50), Quality::FULL)
                .diagnostics
                .is_empty(),
            "old word style Auto inherits paragraph direction"
        );
    }

    #[test]
    fn native_fill_sweep_timing_direction_animation_and_hash_are_explicit() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let mut track = CaptionTrack::new("Spatial sweep");
        track.style.stroke = None;
        track.style.direction = timeline::TextDirection::Ltr;
        track.style.highlight = Some(KaraokeStyle {
            mode: KaraokeMode::FillSweep,
            active_color: Color::new(1.0, 0.0, 0.0, 0.5),
            inactive_color: Color::new(0.0, 0.0, 1.0, 1.0),
        });
        let mut cue = CaptionCue::new(Tick(0), Tick(500), Vec::new());
        cue.words.push(CaptionWord {
            text: "MMMM".into(),
            start: Tick(100),
            end: Tick(200),
            style_override: None,
        });
        track.cues.push(cue);
        project
            .sequences
            .get_mut(&seq)
            .unwrap()
            .caption_tracks
            .push(track);
        let mut hashes = std::collections::HashSet::new();
        for (t, fraction) in [
            (50, None),
            (100, None),
            (125, Some(0.25)),
            (150, Some(0.5)),
            (200, None),
            (250, None),
        ] {
            let sequence = &project.sequences[&seq];
            let (batch, paints) =
                resolve_native_caption_batch(sequence, Tick(t), &sequence.formats[0]).unwrap();
            if let Some(f) = fraction {
                assert_eq!(paints[0].sweeps[0].fraction, f);
                assert_eq!(paints[0].sweeps[0].active, [255, 0, 0, 128]);
                assert_eq!(batch.cues[0].words[0].color, [0; 4]);
                assert!(hashes.insert(content_hash(
                    &IrOp::NativeCaptionOutput {
                        cue_batch: batch,
                        paints,
                        video: false
                    },
                    &[],
                    &[]
                )));
            } else {
                assert!(paints.is_empty());
                assert_eq!(
                    batch.cues[0].words[0].color,
                    if t < 200 {
                        [0, 0, 255, 255]
                    } else {
                        [255, 0, 0, 128]
                    }
                );
            }
        }
        for direction in [
            timeline::TextDirection::Auto,
            timeline::TextDirection::Ltr,
            timeline::TextDirection::Rtl,
        ] {
            let track = &mut project.sequences.get_mut(&seq).unwrap().caption_tracks[0];
            track.style.direction = direction;
            track.cues[0].words[0].text = "שלום".into();
            let frame = compile_native_preview(&project, seq, 0, Tick(150), Quality::FULL);
            assert_eq!(
                frame
                    .diagnostics
                    .iter()
                    .filter(|d| d.code == Some(CompileCode::CaptionSweepDirectionFallback))
                    .count(),
                1
            );
            assert!(!frame
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error));
            let sequence = &project.sequences[&seq];
            let (batch, paints) =
                resolve_native_caption_batch(sequence, Tick(250), &sequence.formats[0]).unwrap();
            assert!(paints.is_empty());
            assert_eq!(batch.cues[0].words[0].color, [0, 0, 255, 255]);
        }
        for animation in [
            CaptionAnim::None,
            CaptionAnim::SlideUp,
            CaptionAnim::FadeWords,
            CaptionAnim::Typewriter,
        ] {
            let track = &mut project.sequences.get_mut(&seq).unwrap().caption_tracks[0];
            track.style.direction = timeline::TextDirection::Ltr;
            track.style.animation = animation;
            track.cues[0].end = Tick(ms_to_ticks(600));
            track.cues[0].words[0].text = "MMMM".into();
            track.cues[0].words[0].start = Tick(0);
            track.cues[0].words[0].end = Tick(ms_to_ticks(400));
            let sequence = &project.sequences[&seq];
            let (batch, paints) = resolve_native_caption_batch(
                sequence,
                Tick(ms_to_ticks(100)),
                &sequence.formats[0],
            )
            .unwrap();
            assert_eq!(paints[0].sweeps[0].fraction, 0.25);
            assert_eq!(paints[0].sweeps[0].active[3], 128);
            assert_eq!(
                paints[0].opacity,
                if animation == CaptionAnim::SlideUp {
                    0.5
                } else {
                    1.0
                }
            );
            assert_eq!(
                batch.cues[0].words[0].text,
                if animation == CaptionAnim::Typewriter {
                    "M"
                } else {
                    "MMMM"
                }
            );
        }
        let track = &mut project.sequences.get_mut(&seq).unwrap().caption_tracks[0];
        track.style.animation = CaptionAnim::FadeWords;
        track.cues[0].words[0].start = Tick(ms_to_ticks(200));
        let sequence = &project.sequences[&seq];
        let (batch, paints) =
            resolve_native_caption_batch(sequence, Tick(ms_to_ticks(100)), &sequence.formats[0])
                .unwrap();
        assert!(paints.is_empty());
        assert_eq!(batch.cues[0].words[0].color, [0, 0, 255, 85]);
        assert!(!caption_sweep_word_pop(
            timeline::TextDirection::Ltr,
            "123 Hello"
        ));
        assert!(caption_sweep_word_pop(
            timeline::TextDirection::Ltr,
            "مرحبا"
        ));
        assert!(caption_sweep_word_pop(
            timeline::TextDirection::Rtl,
            "Brand"
        ));
    }

    #[test]
    fn native_underline_timing_style_priority_animation_and_hash_are_explicit() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let mut track = CaptionTrack::new("Underlines");
        track.style.stroke = None;
        track.style.font_size = 40.0;
        track.style.fill = Color::new(0.2, 0.3, 0.4, 0.5);
        track.style.highlight = Some(KaraokeStyle {
            mode: KaraokeMode::Underline,
            active_color: Color::RED,
            inactive_color: Color::BLACK,
        });
        track.cues.push(CaptionCue::new(
            Tick(0),
            Tick(ms_to_ticks(500)),
            vec![
                CaptionWord::new("HI", Tick(0), Tick(ms_to_ticks(200))),
                CaptionWord::new("OK", Tick(ms_to_ticks(200)), Tick(ms_to_ticks(400))),
            ],
        ));
        project
            .sequences
            .get_mut(&seq)
            .unwrap()
            .caption_tracks
            .push(track);
        let resolve = |project: &TimelineProject, ms: i64| {
            let frame =
                compile_native_preview(project, seq, 0, Tick(ms_to_ticks(ms)), Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            assert!(frame.graph.validate_working_color_domain().is_ok());
            frame
                .graph
                .nodes
                .iter()
                .find_map(|n| {
                    if let IrOp::NativeCaptionOutput {
                        cue_batch, paints, ..
                    } = &n.op
                    {
                        Some((
                            cue_batch.clone(),
                            paints.clone(),
                            content_hash(&n.op, &[], &[]),
                        ))
                    } else {
                        None
                    }
                })
                .unwrap()
        };
        let mut hashes = Vec::new();
        for (ms, word) in [
            (0, Some(0)),
            (199, Some(0)),
            (200, Some(1)),
            (399, Some(1)),
            (400, None),
        ] {
            let (batch, paints, hash) = resolve(&project, ms);
            hashes.push(hash);
            assert!(batch.cues[0]
                .words
                .iter()
                .all(|w| w.color == [51, 77, 102, 128]));
            match word {
                Some(word) => {
                    assert_eq!(paints[0].underlines.len(), 1);
                    let u = &paints[0].underlines[0];
                    assert_eq!(u.word, word);
                    assert_eq!(u.color, [255, 0, 0, 255]);
                    assert_eq!(u.width, 2.0);
                }
                None => assert!(paints.is_empty()),
            }
        }
        assert_eq!(hashes[0], hashes[1]);
        assert_ne!(hashes[1], hashes[2]);
        assert_ne!(hashes[3], hashes[4]);
        let style = &mut project.sequences.get_mut(&seq).unwrap().caption_tracks[0].style;
        style.stroke = Some((Color::new(0.0, 1.0, 0.0, 0.5), 3.0));
        style.animation = CaptionAnim::SlideUp;
        let (batch, paints, _) = resolve(&project, 100);
        assert_eq!(paints[0].underlines[0].color, [0, 255, 0, 128]);
        assert_eq!(paints[0].underlines[0].width, 3.0);
        assert_eq!(paints[0].opacity, 0.5);
        assert_eq!(
            batch.cues[0].words[0].color[3], 128,
            "group fade is applied once after all paint layers"
        );
        assert_eq!(paints[0].scaled(0.5).underlines[0].width, 1.5);
    }

    #[test]
    fn native_caption_boundary_preserves_scopes_and_validates_effective_styles() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let mut track = CaptionTrack::new("Native captions");
        track.style.stroke = None;
        track.style.font_size = 40.0;
        track.cues.push(CaptionCue::new(
            Tick(0),
            Tick(200),
            vec![CaptionWord::new("HI", Tick(0), Tick(200))],
        ));
        project
            .sequences
            .get_mut(&seq)
            .unwrap()
            .caption_tracks
            .push(track);
        let frame = compile_native_preview(&project, seq, 0, Tick(50), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        let out = frame.graph.output.unwrap();
        assert!(matches!(
            frame.graph.nodes[out.0 as usize].op,
            IrOp::NativeCaptionOutput { video: false, .. }
        ));
        assert_ne!(frame.program_tap, Some(out));
        assert!(matches!(
            frame.graph.nodes[frame.program_tap.unwrap().0 as usize].op,
            IrOp::NativeSdrOutput
        ));
        assert!(frame.tap(ScopeTapPoint::NativeSceneProgram).is_some());
        assert_eq!(frame.graph.canvas_scale((8, 8)), glam::Vec2::new(0.5, 0.5));
        for mode in 0..8 {
            let mut copy = project.clone();
            let track = &mut copy.sequences.get_mut(&seq).unwrap().caption_tracks[0];
            match mode {
                0 => track.style.stroke = Some((Color::BLACK, 9.0)),
                1 => track.style.font_size = f32::NAN,
                2 => track.cues[0].position_override = Some([f32::NAN, 0.5]),
                3 => track.cues[0].words[0].text = "X".repeat(4097),
                4 => track.style.fill.r = f32::INFINITY,
                5 => {
                    let mut st = track.style.clone();
                    st.font_size += 1.0;
                    track.cues[0].words[0].style_override = Some(st);
                }
                6 => {
                    track.style.highlight = Some(KaraokeStyle {
                        mode: KaraokeMode::FillSweep,
                        active_color: Color::new(f32::NAN, 0.0, 0.0, 1.0),
                        inactive_color: Color::BLACK,
                    })
                }
                _ => track.style.max_width = 0.0,
            }
            assert!(
                compile_native_preview(&copy, seq, 0, Tick(50), Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error),
                "case {mode}"
            );
            track_disabled_for_native_test(&mut copy, seq);
            assert!(
                compile_native_preview(&copy, seq, 0, Tick(50), Quality::FULL)
                    .diagnostics
                    .is_empty()
            );
        }
        let end = compile_native_preview(&project, seq, 0, Tick(200), Quality::FULL);
        assert!(!end
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeCaptionOutput { .. })));
        // A complete cue override replaces the unqualified track default.
        let track = &mut project.sequences.get_mut(&seq).unwrap().caption_tracks[0];
        track.cues[0].style_override = Some(track.style.clone());
        track.style = CaptionStyle::default();
        track.style.stroke = Some((Color::BLACK, 9.0));
        assert!(
            compile_native_preview(&project, seq, 0, Tick(50), Quality::FULL)
                .diagnostics
                .is_empty()
        );
        let mut wrong = frame.graph.clone();
        if let IrOp::NativeCaptionOutput { video, .. } = &mut wrong.nodes[out.0 as usize].op {
            *video = true;
        }
        assert!(wrong.validate_working_color_domain().is_err());
        let mut wrong = frame.graph.clone();
        wrong.nodes[out.0 as usize].inputs[0].0 = IrNodeId(0);
        assert!(wrong.validate_working_color_domain().is_err());
    }
    fn track_disabled_for_native_test(project: &mut TimelineProject, seq: SequenceId) {
        project.sequences.get_mut(&seq).unwrap().caption_tracks[0].enabled = false;
    }

    #[test]
    fn native_time_offset_retimes_switch_channels_and_constants_without_extra_evaluation() {
        use timeline::{GraphEdge, Interp, Keyframe, OutPort as GOut};
        let (project, seq, _, _) = native_group_look_fixture();
        let format = &project.sequences[&seq].formats[0];
        let mut graph = NodeGraph::new_project_graph("Temporal channels");
        let mut red = GraphNode::new(GraphOp::SolidColor);
        red.params.base.0.set(
            "params.color",
            PropValue::Color(Color {
                r: 1.,
                g: 0.,
                b: 0.,
                a: 0.25,
            }),
        );
        let mut blue = GraphNode::new(GraphOp::SolidColor);
        blue.params.base.0.set(
            "params.color",
            PropValue::Color(Color {
                r: 0.,
                g: 0.,
                b: 1.,
                a: 0.75,
            }),
        );
        let mut switch = GraphNode::new(GraphOp::Switch);
        switch
            .params
            .track_mut(&PropPath::new("params.selected"))
            .insert_keyframe(Keyframe::new(Tick(0), PropValue::Enum(0), Interp::Hold));
        switch
            .params
            .track_mut(&PropPath::new("params.selected"))
            .insert_keyframe(Keyframe::new(Tick(10), PropValue::Enum(1), Interp::Hold));
        let split = GraphNode::new(GraphOp::ChannelSplit);
        let offset = GraphNode::new(GraphOp::TimeOffset { offset: Tick(5) });
        let (r, bl, sw, sp, off) = (red.id, blue.id, switch.id, split.id, offset.id);
        for n in [red, blue, switch, split, offset] {
            graph.nodes.insert(n.id, n);
        }
        graph.edges = vec![
            GraphEdge {
                from: (r, GOut(0)),
                to: (sw, InPort(0)),
            },
            GraphEdge {
                from: (bl, GOut(0)),
                to: (sw, InPort(1)),
            },
            GraphEdge {
                from: (sw, GOut(0)),
                to: (sp, InPort(0)),
            },
            GraphEdge {
                from: (sp, GOut(3)),
                to: (off, InPort(0)),
            },
            GraphEdge {
                from: (off, GOut(0)),
                to: (graph.output, InPort(0)),
            },
        ];
        let compile = |graph: &NodeGraph, tick| {
            let mut b = Builder::new();
            b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
            lower_native_graph(&mut b, graph, format, tick, None, None)
                .map(|out| b.finish(Some(out)))
        };
        for (tick, expected_alpha) in [(Tick(12), 0.25), (Tick(15), 0.75)] {
            let frame = compile(&graph, tick).unwrap();
            assert!(frame.graph.validate_working_color_domain().is_ok());
            let out = &frame.graph.nodes[frame.graph.output.unwrap().0 as usize];
            assert!(matches!(out.op, IrOp::NativeMaskToImage));
            let extract = &frame.graph.nodes[out.inputs[0].0 .0 as usize];
            assert!(matches!(
                extract.op,
                IrOp::NativeChannelExtract {
                    channel: Channel::A
                }
            ));
            let source = &frame.graph.nodes[extract.inputs[0].0 .0 as usize];
            assert!(matches!(source.op, IrOp::SolidColor { color } if color.a==expected_alpha));
            assert_eq!(
                frame
                    .graph
                    .nodes
                    .iter()
                    .filter(|n| matches!(n.op, IrOp::NativeChannelExtract { .. }))
                    .count(),
                1
            );
        }
        let first = compile(&graph, Tick(12)).unwrap();
        let second = compile(&graph, Tick(13)).unwrap();
        assert_eq!(
            first.graph.nodes[first.graph.output.unwrap().0 as usize].content_hash,
            second.graph.nodes[second.graph.output.unwrap().0 as usize].content_hash
        );
        graph
            .nodes
            .get_mut(&off)
            .unwrap()
            .params
            .base
            .0
            .set("params.offset", PropValue::Float(5.));
        assert!(compile(&graph, Tick(12))
            .unwrap_err()
            .contains("structural constant"));
        graph.nodes.get_mut(&off).unwrap().params =
            GraphNode::new(GraphOp::TimeOffset { offset: Tick(5) }).params;
        graph.nodes.get_mut(&off).unwrap().op = GraphOp::TimeOffset {
            offset: Tick(i64::MIN),
        };
        assert!(compile(&graph, Tick(12)).unwrap_err().contains("overflow"));
        graph.nodes.get_mut(&off).unwrap().op = GraphOp::TimeOffset { offset: Tick(5) };
        graph.edges[2].from = (off, GOut(0));
        assert!(compile(&graph, Tick(12)).unwrap_err().contains("cyclic"));
        graph.edges.retain(|e| e.to.0 != off);
        let blank = compile(&graph, Tick(12)).unwrap();
        assert!(
            matches!(blank.graph.nodes[blank.graph.output.unwrap().0 as usize].op,IrOp::SolidColor{color} if color.a==0.)
        );
    }

    #[test]
    fn native_time_offset_retimes_clip_trim_speed_and_pre_project_program() {
        use timeline::{GraphEdge, OutPort as GOut};
        let (mut project, seq, _, _) = native_group_look_fixture();
        let clip = &mut project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0];
        clip.start = Tick(100);
        clip.source_in = Tick(77);
        clip.speed = SpeedMap::Constant(Ratio { num: 2, den: 1 });
        let (mut graph, source) = NodeGraph::new_clip_composition("Temporal source");
        let offset = GraphNode::new(GraphOp::TimeOffset { offset: Tick(10) });
        let id = offset.id;
        graph.nodes.insert(id, offset);
        graph.edges = vec![
            GraphEdge {
                from: (source, GOut(0)),
                to: (id, InPort(0)),
            },
            GraphEdge {
                from: (id, GOut(0)),
                to: (graph.output, InPort(0)),
            },
        ];
        let gid = graph.id;
        project.graphs.insert(gid, graph);
        project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].composition = Some(gid);
        let frame = compile_native_preview(&project, seq, 0, Tick(150), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert!(frame.graph.nodes.iter().any(|n| matches!(
            n.op,
            IrOp::NativeDecodeVideo {
                src_time: Tick(157),
                ..
            }
        )));
        // Offset source does not duplicate the host asset/clip/group/track/master grades.
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativeExposure { .. }))
                .count(),
            9
        );
        let clip = &project.sequences[&seq].video_tracks[0].clips[0];
        assert_eq!(
            native_source_time_checked(clip, Tick(140)).unwrap(),
            Tick(157)
        );
        let mut bad = clip.clone();
        bad.speed = SpeedMap::Constant(Ratio {
            num: i32::MAX,
            den: 1,
        });
        assert!(native_source_time_checked(&bad, Tick(i64::MAX))
            .unwrap_err()
            .contains("overflow"));
        assert!(native_source_time_checked(clip, Tick(0))
            .unwrap_err()
            .contains("nonnegative"));
        bad.speed = SpeedMap::Keyframed { keys: vec![] };
        assert_eq!(
            native_source_time_checked(&bad, Tick(150)).unwrap(),
            Tick(127)
        );
        project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].composition = None;
        let reference = compile_native_preview(&project, seq, 0, Tick(140), Quality::FULL);
        let reference_output = &reference.graph.nodes[reference.graph.output.unwrap().0 as usize];
        let scene_hash =
            reference.graph.nodes[reference_output.inputs[0].0 .0 as usize].content_hash;
        let mut graph = NodeGraph::new_project_graph("Temporal program");
        let invert = GraphNode::new(GraphOp::Invert);
        let inv = invert.id;
        let offset = GraphNode::new(GraphOp::TimeOffset { offset: Tick(10) });
        let off = offset.id;
        graph.nodes.insert(inv, invert);
        graph.nodes.insert(off, offset);
        graph.edges = vec![
            GraphEdge {
                from: (inv, GOut(0)),
                to: (off, InPort(0)),
            },
            GraphEdge {
                from: (off, GOut(0)),
                to: (graph.output, InPort(0)),
            },
        ];
        let gid = graph.id;
        project.graphs.insert(gid, graph);
        project.project_graph = Some(gid);
        let frame = compile_native_preview(&project, seq, 0, Tick(150), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        let invert = frame
            .graph
            .nodes
            .iter()
            .find(|n| matches!(n.op, IrOp::NativeInvert { .. }))
            .unwrap();
        assert_eq!(
            frame.graph.nodes[invert.inputs[0].0 .0 as usize].content_hash,
            scene_hash
        );
        assert!(frame.graph.validate_working_color_domain().is_ok());
    }

    #[test]
    fn native_time_offset_retimes_nested_scene_without_importing_display_transforms() {
        let (mut project, outer, _, _) = native_group_look_fixture();
        let mut nested = project.sequences[&outer].clone();
        nested.id = SequenceId::new();
        let inner = nested.id;
        project.insert_sequence(nested);
        let clip = &mut project.sequences.get_mut(&outer).unwrap().video_tracks[0].clips[0];
        clip.source = ClipSource::NestedSequence { sequence: inner };
        clip.start = Tick(100);
        clip.source_in = Tick(77);
        clip.speed = SpeedMap::Constant(Ratio { num: 2, den: 1 });
        let reference = compile_native_preview(&project, inner, 0, Tick(157), Quality::FULL);
        assert!(reference.diagnostics.is_empty());
        let output = &reference.graph.nodes[reference.graph.output.unwrap().0 as usize];
        let scene_hash = reference.graph.nodes[output.inputs[0].0 .0 as usize].content_hash;
        let format = &project.sequences[&outer].formats[0];
        let clip = &project.sequences[&outer].video_tracks[0].clips[0];
        let ctx = NativeGraphSourceContext {
            project: &project,
            clip_start: Some(clip.start),
            clip: Some(clip),
            sequence: outer,
            format_index: 0,
            quality: Quality::FULL,
            check_online: false,
            source_errors: None,
        };
        let mut b = Builder::new();
        b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
        b.sequence_path = vec![outer];
        let blank = b.transparent(format);
        let source = b.push(
            IrOp::NativeCanvas { w: 16, h: 16 },
            vec![(blank, OutPort::default())],
        );
        let retimed = native_retime_clip_source(&mut b, ctx, source, Tick(140), format).unwrap();
        let frame = b.finish(Some(retimed));
        assert_eq!(
            frame.graph.nodes[retimed.0 as usize].content_hash,
            scene_hash
        );
        assert!(!frame.graph.nodes.iter().any(|n| matches!(
            n.op,
            IrOp::NativeSdrOutput | IrOp::NativeSdrVideoOutput | IrOp::NativeCaptionOutput { .. }
        )));
        assert!(frame.graph.validate_working_color_domain().is_ok());
    }

    #[test]
    fn native_decode_clock_budget_counts_only_demanded_video_sources() {
        use timeline::{GraphEdge, OutPort as GOut};
        for count in [8, 9] {
            let (mut project, seq, _, _) = native_group_look_fixture();
            let (mut graph, source) = NodeGraph::new_clip_composition("Clock budget");
            graph.edges.clear();
            let mut previous = None;
            for clock in 1..=count {
                let shifted = GraphNode::new(GraphOp::TimeOffset {
                    offset: Tick(-clock),
                });
                let id = shifted.id;
                graph.nodes.insert(id, shifted);
                graph.edges.push(GraphEdge {
                    from: (source, GOut(0)),
                    to: (id, InPort(0)),
                });
                previous = Some(if let Some(previous) = previous {
                    let merge = GraphNode::new(GraphOp::Merge {
                        mode: BlendMode::Normal,
                    });
                    let m = merge.id;
                    graph.nodes.insert(m, merge);
                    graph.edges.extend([
                        GraphEdge {
                            from: (id, GOut(0)),
                            to: (m, InPort(0)),
                        },
                        GraphEdge {
                            from: (previous, GOut(0)),
                            to: (m, InPort(1)),
                        },
                    ]);
                    m
                } else {
                    id
                });
            }
            graph.edges.push(GraphEdge {
                from: (previous.unwrap(), GOut(0)),
                to: (graph.output, InPort(0)),
            });
            let gid = graph.id;
            project.graphs.insert(gid, graph);
            project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].composition =
                Some(gid);
            let frame = compile_native_preview(&project, seq, 0, Tick(150), Quality::FULL);
            if count == 8 {
                assert!(
                    !frame
                        .diagnostics
                        .iter()
                        .any(|d| d.severity == DiagSeverity::Error),
                    "{:?}",
                    frame.diagnostics
                );
                assert_eq!(frame.graph.native_decode_requests(None).len(), 8);
                assert_eq!(
                    frame
                        .graph
                        .nodes
                        .iter()
                        .filter(|n| matches!(n.op, IrOp::NativeDecodeVideo { .. }))
                        .count(),
                    9,
                    "unconnected original source must not consume a decoder slot"
                );
            } else {
                assert!(frame
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error
                        && d.message.contains("eight independent video-clock")));
                assert!(frame.graph.native_decode_requests(None).is_empty());
            }
        }
    }

    #[test]
    fn native_malformed_speed_map_fails_before_ordinary_source_arithmetic() {
        use timeline::{Interp, SpeedKey};
        let (mut project, seq, _, _) = native_group_look_fixture();
        project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].speed =
            SpeedMap::Keyframed {
                keys: vec![
                    SpeedKey::eased(Tick(i64::MIN), Ratio { num: 1, den: 1 }, Interp::Linear),
                    SpeedKey::eased(Tick(i64::MAX), Ratio { num: 3, den: 1 }, Interp::Hold),
                ],
            };
        let result = std::panic::catch_unwind(|| {
            compile_native_preview(&project, seq, 0, Tick(10), Quality::FULL)
        });
        assert!(
            result.is_ok(),
            "malformed source map must be rejected before arithmetic"
        );
        let frame = result.unwrap();
        assert!(frame
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error && d.message.contains("speed map")));
    }

    #[test]
    fn native_time_offset_keyframed_speed_integrals_preserve_checked_timing() {
        use timeline::{Interp, SpeedKey};
        let key = |at, num, den, interp| SpeedKey::eased(Tick(at), Ratio { num, den }, interp);
        let held = SpeedMap::Keyframed {
            keys: vec![key(0, 1, 1, Interp::Hold), key(10, 2, 1, Interp::Hold)],
        };
        let linear = SpeedMap::Keyframed {
            keys: vec![key(0, 1, 1, Interp::Linear), key(10, 3, 1, Interp::Hold)],
        };
        let bezier = SpeedMap::Keyframed {
            keys: vec![
                key(
                    0,
                    1,
                    1,
                    Interp::Bezier {
                        out_handle: [1.0 / 3.0, 1.0 / 3.0],
                        in_handle: [2.0 / 3.0, 2.0 / 3.0],
                    },
                ),
                key(10, 3, 1, Interp::Hold),
            ],
        };
        let reverse = SpeedMap::Keyframed {
            keys: vec![key(0, 1, 1, Interp::Linear), key(10, -1, 1, Interp::Hold)],
        };
        for (map, expected) in [
            (held, [-4, 4, 5, 10, 20]),
            (linear, [-4, 6, 8, 20, 35]),
            (bezier, [-4, 6, 8, 20, 35]),
            (reverse, [-4, 2, 3, 0, -5]),
        ] {
            for (delta, expected) in [-4, 4, 5, 10, 15].into_iter().zip(expected) {
                let value = native_speed_delta(&map, delta, Tick(20)).unwrap();
                assert_eq!(value, expected, "{map:?} at {delta}");
                assert_eq!(value, map.source_delta(Tick(delta)).0 as i128);
            }
        }
        let fractional = SpeedMap::Keyframed {
            keys: vec![key(0, 3, 2, Interp::Hold), key(1, 3, 2, Interp::Hold)],
        };
        assert_eq!(
            native_speed_delta(&fractional, 2, Tick(20)).unwrap(),
            2,
            "preserve per-segment integer truncation"
        );
        let frozen = SpeedMap::Keyframed {
            keys: vec![key(0, 0, 1, Interp::Hold)],
        };
        assert_eq!(native_speed_delta(&frozen, i64::MIN, Tick(20)).unwrap(), 0);
        for keys in [
            vec![key(0, 1, 0, Interp::Hold)],
            vec![key(-1, 1, 1, Interp::Hold)],
            vec![key(21, 1, 1, Interp::Hold)],
            vec![key(0, 1, 1, Interp::Hold), key(0, 2, 1, Interp::Hold)],
            vec![key(
                0,
                1,
                1,
                Interp::Bezier {
                    out_handle: [f64::NAN, 0.],
                    in_handle: [1., 1.],
                },
            )],
            (0..4097).map(|i| key(i, 1, 1, Interp::Hold)).collect(),
        ] {
            assert!(native_speed_delta(&SpeedMap::Keyframed { keys }, 10, Tick(20)).is_err());
        }
        let excessive = SpeedMap::Keyframed {
            keys: vec![
                key(0, i32::MAX, 1, Interp::Linear),
                key(10, 1, 1, Interp::Hold),
            ],
        };
        assert!(native_speed_delta(&excessive, i64::MAX, Tick(20)).is_err());
        assert!(native_speed_delta(&excessive, i64::MIN, Tick(20)).is_err());
        let mut clip = Clip::new(
            ClipSource::SolidColor {
                color: Color::BLACK,
            },
            Tick(20),
            Tick(20),
        );
        clip.source_in = Tick(100);
        clip.speed = fractional;
        assert_eq!(
            native_source_time_checked(&clip, Tick(22)).unwrap(),
            Tick(102)
        );
        clip.source_in = Tick(-1);
        assert!(native_source_time_checked(&clip, Tick(22)).is_err());
    }

    #[test]
    fn native_time_offset_warns_after_four_offsets_and_bounds_temporal_expansion() {
        use timeline::{GraphEdge, OutPort as GOut};
        let (project, seq, _, _) = native_group_look_fixture();
        let format = &project.sequences[&seq].formats[0];
        let mut graph = NodeGraph::new_project_graph("Temporal budget");
        let source = GraphNode::new(GraphOp::SolidColor);
        let mut previous = source.id;
        graph.nodes.insert(previous, source);
        for offset in 1..=5 {
            let node = GraphNode::new(GraphOp::TimeOffset {
                offset: Tick(offset),
            });
            let id = node.id;
            graph.nodes.insert(id, node);
            graph.edges.push(GraphEdge {
                from: (previous, GOut(0)),
                to: (id, InPort(0)),
            });
            previous = id;
        }
        graph.edges.push(GraphEdge {
            from: (previous, GOut(0)),
            to: (graph.output, InPort(0)),
        });
        let compile = |g: &NodeGraph| {
            let mut b = Builder::new();
            b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
            lower_native_graph(&mut b, g, format, Tick(100000), None, None)
                .map(|out| b.finish(Some(out)))
        };
        let frame = compile(&graph).unwrap();
        assert_eq!(frame.diagnostics.len(), 1);
        assert!(frame.diagnostics[0].message.contains("more than 4"));
        assert_eq!(
            frame.graph.nodes.len(),
            2,
            "unchanging sources deduplicate across offsets"
        );
        let mut graph = NodeGraph::new_project_graph("Temporal fanout");
        let source = GraphNode::new(GraphOp::SolidColor);
        let mut previous = source.id;
        graph.nodes.insert(previous, source);
        for bit in 0..14 {
            let offset = GraphNode::new(GraphOp::TimeOffset {
                offset: Tick(1 << bit),
            });
            let o = offset.id;
            graph.nodes.insert(o, offset);
            let merge = GraphNode::new(GraphOp::Merge {
                mode: BlendMode::Normal,
            });
            let m = merge.id;
            graph.nodes.insert(m, merge);
            graph.edges.extend([
                GraphEdge {
                    from: (previous, GOut(0)),
                    to: (o, InPort(0)),
                },
                GraphEdge {
                    from: (previous, GOut(0)),
                    to: (m, InPort(0)),
                },
                GraphEdge {
                    from: (o, GOut(0)),
                    to: (m, InPort(1)),
                },
            ]);
            previous = m;
        }
        graph.edges.push(GraphEdge {
            from: (previous, GOut(0)),
            to: (graph.output, InPort(0)),
        });
        assert!(compile(&graph).unwrap_err().contains("8192 time-expanded"));
    }

    #[test]
    fn native_media_in_resolves_independent_time_interpretation_geometry_and_hashes() {
        use timeline::{MediaProbe, ProbedColor, VideoStreamInfo};
        let (mut project, seq, _, _) = native_group_look_fixture();
        let asset = *project.media.assets.keys().next().unwrap();
        let mut probe = MediaProbe::basic(Tick(5000), "mov", "prores");
        probe.pixel_format = Some("yuv444p10le".into());
        probe.video = Some(VideoStreamInfo {
            width: 16,
            height: 16,
            frame_rate: FrameRate::FPS_30,
            pixel_aspect: 1.,
            color: ProbedColor::default(),
            keyframe_index_cached: false,
            scan: ScanType::Progressive,
        });
        project.media.assets.get_mut(&asset).unwrap().probe = Some(probe);
        let host = &mut project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0];
        host.source = ClipSource::SolidColor {
            color: Color::WHITE,
        };
        host.start = Tick(100);
        host.duration = Tick(1000);
        host.source_in = Tick(77);
        host.speed = SpeedMap::Constant(Ratio { num: 3, den: 1 });
        let mut host_input = project.media.assets[&asset]
            .native_input_color
            .clone()
            .unwrap();
        host_input.version = 99;
        project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].native_input_color =
            Some(host_input);
        let mut hashes = HashSet::new();
        for project_scope in [false, true] {
            for time_source in [TimeSource::Local, TimeSource::Sequence] {
                let (mut graph, node) = NodeGraph::new_clip_composition("Independent plate");
                graph.nodes.get_mut(&node).unwrap().op = GraphOp::MediaIn { asset, time_source };
                let gid = graph.id;
                project.graphs.insert(gid, graph);
                project.project_graph = project_scope.then_some(gid);
                project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].composition =
                    (!project_scope).then_some(gid);
                let frame = compile_native_preview(&project, seq, 0, Tick(150), Quality::FULL);
                assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
                let op = &frame
                    .graph
                    .nodes
                    .iter()
                    .find(|n| matches!(n.op, IrOp::NativeMediaVideo { .. }))
                    .unwrap()
                    .op;
                if let IrOp::NativeMediaVideo {
                    src_time,
                    input,
                    w,
                    h,
                    ..
                } = op
                {
                    assert_eq!(
                        *src_time,
                        Tick(if !project_scope && time_source == TimeSource::Local {
                            50
                        } else {
                            150
                        })
                    );
                    assert_eq!(input.version, 1);
                    assert_eq!((*w, *h), (16, 16));
                } else {
                    unreachable!()
                }
                hashes.insert(content_hash(op, &[], &[]));
            }
        }
        assert_eq!(
            hashes.len(),
            2,
            "same resolved source time deduplicates across contexts"
        );
        let format = project.sequences[&seq].formats[0].clone();
        for case in 0..5 {
            let mut bad = project.clone();
            let media = bad.media.assets.get_mut(&asset).unwrap();
            match case {
                0 => media.native_input_color = None,
                1 => media.probe.as_mut().unwrap().video.as_mut().unwrap().width = 8193,
                2 => media.probe.as_mut().unwrap().pixel_format = Some("unsupported".into()),
                3 => {
                    media.probe.as_mut().unwrap().video.as_mut().unwrap().scan =
                        ScanType::InterlacedTopFirst
                }
                _ => media.kind = AssetKind::Audio,
            };
            assert!(
                native_media_in_op(
                    NativeGraphSourceContext {
                        project: &bad,
                        clip_start: Some(Tick(100)),
                        clip: None,
                        sequence: seq,
                        format_index: 0,
                        quality: Quality::FULL,
                        check_online: false,
                        source_errors: None
                    },
                    asset,
                    TimeSource::Sequence,
                    Tick(150),
                    &format
                )
                .is_err(),
                "case {case}"
            );
        }
        let ctx = NativeGraphSourceContext {
            project: &project,
            clip_start: Some(Tick(1)),
            clip: None,
            sequence: seq,
            format_index: 0,
            quality: Quality::FULL,
            check_online: false,
            source_errors: None,
        };
        assert!(
            native_media_in_op(ctx, asset, TimeSource::Local, Tick(i64::MIN), &format)
                .unwrap_err()
                .contains("overflow")
        );
        assert!(
            native_media_in_op(ctx, asset, TimeSource::Sequence, Tick(-1), &format)
                .unwrap_err()
                .contains("nonnegative")
        );
        let errors = HashMap::from([(asset, "decoder geometry mismatch".into())]);
        assert!(native_media_in_op(
            NativeGraphSourceContext {
                source_errors: Some(&errors),
                ..ctx
            },
            asset,
            TimeSource::Sequence,
            Tick(150),
            &format
        )
        .unwrap_err()
        .contains("decoder geometry"));
    }

    #[test]
    fn native_mixed_resolution_requires_explicit_bounded_resize() {
        use timeline::{GraphEdge, MediaProbe, OutPort as GOut, ProbedColor, VideoStreamInfo};
        let (mut project, seq, _, _) = native_group_look_fixture();
        let asset = *project.media.assets.keys().next().unwrap();
        let mut probe = MediaProbe::basic(Tick(5_000_000_000), "mov", "prores");
        probe.pixel_format = Some("yuv444p10le".into());
        probe.video = Some(VideoStreamInfo {
            width: 32,
            height: 8,
            frame_rate: FrameRate::FPS_30,
            pixel_aspect: 1.,
            color: ProbedColor::default(),
            keyframe_index_cached: false,
            scan: ScanType::Progressive,
        });
        project.media.assets.get_mut(&asset).unwrap().probe = Some(probe);
        project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].source =
            ClipSource::SolidColor {
                color: Color::BLACK,
            };
        for fit in [
            timeline::FitMode::Contain,
            timeline::FitMode::Cover,
            timeline::FitMode::Stretch,
        ] {
            let mut graph = NodeGraph::new_project_graph("Mixed source dimensions");
            let media = GraphNode::new(GraphOp::MediaIn {
                asset,
                time_source: TimeSource::Sequence,
            });
            let media_id = media.id;
            let mut resize = GraphNode::new(GraphOp::Resize { fit });
            let resize_id = resize.id;
            resize
                .params
                .base
                .0
                .set("params.width", PropValue::Float(16.));
            resize
                .params
                .base
                .0
                .set("params.height", PropValue::Float(16.));
            graph.nodes.insert(media_id, media);
            graph.nodes.insert(resize_id, resize);
            graph.edges = vec![
                GraphEdge {
                    from: (media_id, GOut(0)),
                    to: (resize_id, InPort(0)),
                },
                GraphEdge {
                    from: (resize_id, GOut(0)),
                    to: (graph.output, InPort(0)),
                },
            ];
            let gid = graph.id;
            project.graphs.insert(gid, graph);
            project.project_graph = Some(gid);
            let good = compile_native_preview(&project, seq, 0, Tick::ZERO, Quality::FULL);
            assert!(
                good.diagnostics.is_empty(),
                "explicit {fit:?} resize rejected: {:?}",
                good.diagnostics
            );
            assert!(good.graph.validate_working_color_domain().is_ok());
            assert!(good
                .graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::NativeMediaVideo { w: 32, h: 8, .. })));
            for (op, allowed) in [
                (GraphOp::TimeOffset { offset: Tick::ZERO }, true),
                (GraphOp::Switch, true),
                (GraphOp::Blur, false),
            ] {
                let mut routed = project.clone();
                let graph = routed.graphs.get_mut(&gid).unwrap();
                let route = GraphNode::new(op);
                let route_id = route.id;
                graph.nodes.insert(route_id, route);
                graph.edges[0].to = (route_id, InPort(0));
                graph.edges.push(GraphEdge {
                    from: (route_id, GOut(0)),
                    to: (resize_id, InPort(0)),
                });
                let result = compile_native_preview(&routed, seq, 0, Tick::ZERO, Quality::FULL);
                assert_eq!(
                    result.diagnostics.is_empty(),
                    allowed,
                    "{:?}",
                    result.diagnostics
                );
            }
            for (width, height) in [(8., 16.), (3., 7.), (1., 1.), (32., 8.)] {
                let mut resized_project = project.clone();
                let params = &mut resized_project
                    .graphs
                    .get_mut(&gid)
                    .unwrap()
                    .nodes
                    .get_mut(&resize_id)
                    .unwrap()
                    .params
                    .base
                    .0;
                params.set("params.width", PropValue::Float(width));
                params.set("params.height", PropValue::Float(height));
                let frame =
                    compile_native_preview(&resized_project, seq, 0, Tick::ZERO, Quality::FULL);
                assert!(
                    frame.diagnostics.is_empty(),
                    "{width}x{height}: {:?}",
                    frame.diagnostics
                );
                assert!(frame.graph.validate_working_color_domain().is_ok());
                let sizes: Vec<_> = frame
                    .graph
                    .nodes
                    .iter()
                    .filter_map(|n| match n.op {
                        IrOp::Resize { w, h, .. } => Some((w, h)),
                        _ => None,
                    })
                    .collect();
                assert!(sizes
                    .windows(2)
                    .any(|pair| pair == [(width as u32, height as u32), (16, 16)]));
                assert_ne!(
                    frame.graph.nodes.last().unwrap().content_hash,
                    good.graph.nodes.last().unwrap().content_hash
                );
            }
            for (width, height) in [
                (0., 16.),
                (8.5, 16.),
                (8193., 1.),
                (8192., 8192.),
                (f64::NAN, 8.),
            ] {
                let mut invalid = project.clone();
                let params = &mut invalid
                    .graphs
                    .get_mut(&gid)
                    .unwrap()
                    .nodes
                    .get_mut(&resize_id)
                    .unwrap()
                    .params
                    .base
                    .0;
                params.set("params.width", PropValue::Float(width));
                params.set("params.height", PropValue::Float(height));
                assert!(
                    compile_native_preview(&invalid, seq, 0, Tick::ZERO, Quality::FULL)
                        .diagnostics
                        .iter()
                        .any(|d| d.severity == DiagSeverity::Error)
                );
            }
            let mut direct = project.clone();
            let graph = direct.graphs.get_mut(&gid).unwrap();
            graph.edges = vec![GraphEdge {
                from: (media_id, GOut(0)),
                to: (graph.output, InPort(0)),
            }];
            assert!(
                compile_native_preview(&direct, seq, 0, Tick::ZERO, Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error),
                "implicit fitting must remain refused"
            );
        }
    }

    #[test]
    fn native_subject_matte_simultaneous_results_must_fit_cache() {
        for (count, size, allowed) in [
            (8, 96, true),
            (9, 96, false),
            (4, 2048, true),
            (5, 2048, false),
        ] {
            let (mut project, seq, _, _) = native_group_look_fixture();
            let format = &mut project.sequences.get_mut(&seq).unwrap().formats[0];
            format.width = size;
            format.height = size;
            let mut graph = NodeGraph::new_project_graph("Matte capacity");
            let mut previous = None;
            for index in 0..count {
                let mut solid = GraphNode::new(GraphOp::SolidColor);
                solid.params.base.0.set(
                    "params.color",
                    PropValue::Color(Color::new(index as f32 / 20.0, 0.2, 0.3, 1.0)),
                );
                let sid = solid.id;
                graph.nodes.insert(sid, solid);
                let matte = GraphNode::new(GraphOp::MaskFromMatte);
                let mid = matte.id;
                graph.nodes.insert(mid, matte);
                graph.edges.push(timeline::GraphEdge {
                    from: (sid, timeline::OutPort(0)),
                    to: (mid, InPort(0)),
                });
                previous = Some(if let Some(prev) = previous {
                    let mut merge = GraphNode::new(GraphOp::Merge {
                        mode: BlendMode::Normal,
                    });
                    merge
                        .params
                        .base
                        .0
                        .set("params.opacity", PropValue::Float(0.5));
                    let id = merge.id;
                    graph.nodes.insert(id, merge);
                    graph.edges.extend([
                        timeline::GraphEdge {
                            from: (mid, timeline::OutPort(0)),
                            to: (id, InPort(0)),
                        },
                        timeline::GraphEdge {
                            from: (prev, timeline::OutPort(0)),
                            to: (id, InPort(1)),
                        },
                    ]);
                    id
                } else {
                    mid
                });
            }
            graph.edges.push(timeline::GraphEdge {
                from: (previous.unwrap(), timeline::OutPort(0)),
                to: (graph.output, InPort(0)),
            });
            project.project_graph = Some(graph.id);
            project.graphs.insert(graph.id, graph);
            let frame = compile_native_preview(&project, seq, 0, Tick::ZERO, Quality::FULL);
            assert_eq!(
                !frame
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error),
                allowed,
                "count{count} size{size}: {:?}",
                frame.diagnostics
            );
            if !allowed {
                assert!(frame
                    .diagnostics
                    .iter()
                    .any(|d| d.message.contains("simultaneous-mask budget")));
            }
        }
    }

    #[test]
    fn native_subject_matte_ports_defaults_and_limits_are_explicit() {
        for project_scope in [false, true] {
            let (mut project, seq, _, _) = native_group_look_fixture();
            let (mut graph, source) = NodeGraph::new_clip_composition("Native subject matte");
            graph.nodes.get_mut(&source).unwrap().op = GraphOp::SolidColor;
            let node = GraphNode::new(GraphOp::MaskFromMatte);
            let id = node.id;
            graph.nodes.insert(id, node);
            graph.edges = vec![
                timeline::GraphEdge {
                    from: (source, timeline::OutPort(0)),
                    to: (id, InPort(0)),
                },
                timeline::GraphEdge {
                    from: (id, timeline::OutPort(0)),
                    to: (graph.output, InPort(0)),
                },
            ];
            let gid = graph.id;
            project.graphs.insert(gid, graph);
            if project_scope {
                project.project_graph = Some(gid);
            } else {
                project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].composition =
                    Some(gid);
            }
            let frame = compile_native_preview(&project, seq, 0, Tick::ZERO, Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            assert!(frame.graph.validate_working_color_domain().is_ok());
            assert!(frame
                .graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::NativeMatteExtract)));
            assert!(frame
                .graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::NativeMaskToImage)));
            let mut unwired = project.clone();
            unwired.graphs.get_mut(&gid).unwrap().edges.remove(0);
            let frame = compile_native_preview(&unwired, seq, 0, Tick::ZERO, Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            assert!(!frame
                .graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::NativeMatteExtract)));
            let mut invalid = project.clone();
            invalid
                .graphs
                .get_mut(&gid)
                .unwrap()
                .nodes
                .get_mut(&id)
                .unwrap()
                .params
                .base
                .0
                .set("params.invalid", timeline::PropValue::Bool(true));
            assert!(
                compile_native_preview(&invalid, seq, 0, Tick::ZERO, Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
            );
            let mut oversized = project.clone();
            let format = &mut oversized.sequences.get_mut(&seq).unwrap().formats[0];
            format.width = 5000;
            format.height = 5000;
            assert!(
                compile_native_preview(&oversized, seq, 0, Tick::ZERO, Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
            );
        }
    }

    #[test]
    fn native_channel_ports_coerce_and_route_all_four_signals() {
        for project_scope in [false, true] {
            let (mut project, seq, _, _) = native_group_look_fixture();
            let (mut graph, source) = NodeGraph::new_clip_composition("Native channels");
            graph.nodes.get_mut(&source).unwrap().op = GraphOp::SolidColor;
            let split = GraphNode::new(GraphOp::ChannelSplit);
            let sid = split.id;
            let combine = GraphNode::new(GraphOp::ChannelCombine);
            let cid = combine.id;
            graph.nodes.insert(sid, split);
            graph.nodes.insert(cid, combine);
            graph.edges = vec![
                timeline::GraphEdge {
                    from: (source, timeline::OutPort(0)),
                    to: (sid, InPort(0)),
                },
                timeline::GraphEdge {
                    from: (cid, timeline::OutPort(0)),
                    to: (graph.output, InPort(0)),
                },
            ];
            for port in 0..4 {
                graph.edges.push(timeline::GraphEdge {
                    from: (sid, timeline::OutPort(port)),
                    to: (cid, InPort(port)),
                });
            }
            let gid = graph.id;
            project.graphs.insert(gid, graph);
            if project_scope {
                project.project_graph = Some(gid);
            } else {
                project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].composition =
                    Some(gid);
            }
            let compile =
                |p: &TimelineProject| compile_native_preview(p, seq, 0, Tick(0), Quality::FULL);
            let frame = compile(&project);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            assert!(frame.graph.validate_working_color_domain().is_ok());
            let combine = frame
                .graph
                .nodes
                .iter()
                .find(|n| matches!(n.op, IrOp::NativeChannelCombine))
                .unwrap();
            for (index, (id, _)) in combine.inputs.iter().enumerate() {
                assert!(
                    matches!(frame.graph.nodes[id.0 as usize].op,IrOp::NativeChannelExtract{channel} if channel as usize==index)
                );
            }
            let mut switched = project.clone();
            let switch = GraphNode::new(GraphOp::Switch);
            let switch_id = switch.id;
            let g = switched.graphs.get_mut(&gid).unwrap();
            g.nodes.insert(switch_id, switch);
            g.edges = vec![
                timeline::GraphEdge {
                    from: (source, timeline::OutPort(0)),
                    to: (sid, InPort(0)),
                },
                timeline::GraphEdge {
                    from: (sid, timeline::OutPort(3)),
                    to: (switch_id, InPort(2)),
                },
                timeline::GraphEdge {
                    from: (switch_id, timeline::OutPort(0)),
                    to: (g.output, InPort(0)),
                },
            ];
            let only_alpha = compile(&switched);
            assert!(only_alpha.diagnostics.is_empty());
            assert_eq!(
                only_alpha
                    .graph
                    .nodes
                    .iter()
                    .filter(|n| matches!(n.op, IrOp::NativeChannelExtract { .. }))
                    .count(),
                1
            );
            assert!(only_alpha.graph.nodes.iter().any(|n| matches!(
                n.op,
                IrOp::NativeChannelExtract {
                    channel: Channel::A
                }
            )));
            assert!(only_alpha
                .graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::NativeMaskToImage)));
            let mut wrong = project.clone();
            wrong.graphs.get_mut(&gid).unwrap().edges[2].from.1 = timeline::OutPort(4);
            assert!(compile(&wrong)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error));
            let mut duplicate = project.clone();
            let edge = duplicate.graphs[&gid].edges[2].clone();
            duplicate.graphs.get_mut(&gid).unwrap().edges.push(edge);
            assert!(compile(&duplicate)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error));
            let mut image_mask = project.clone();
            image_mask.graphs.get_mut(&gid).unwrap().edges[2].from = (source, timeline::OutPort(0));
            let image_frame = compile(&image_mask);
            assert!(image_frame.diagnostics.is_empty());
            assert!(image_frame
                .graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::NativeImageToMask)));
            let graph = project.graphs.get_mut(&gid).unwrap();
            graph.nodes.get_mut(&source).unwrap().op = GraphOp::MaskShape {
                shape: timeline::MaskShapeKind::Rect,
            };
            graph.edges = vec![timeline::GraphEdge {
                from: (source, timeline::OutPort(0)),
                to: (graph.output, InPort(0)),
            }];
            let shape = compile(&project);
            assert!(shape.diagnostics.is_empty());
            assert!(shape
                .graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::NativeMaskToImage)));
            assert!(shape.graph.nodes.iter().any(|n| matches!(
                n.op,
                IrOp::NativeChannelExtract {
                    channel: Channel::A
                }
            )));
            let graph = project.graphs.get_mut(&gid).unwrap();
            graph.edges = vec![timeline::GraphEdge {
                from: (cid, timeline::OutPort(0)),
                to: (graph.output, InPort(0)),
            }];
            let defaults = compile(&project);
            assert!(defaults.diagnostics.is_empty());
            let combine = defaults
                .graph
                .nodes
                .iter()
                .find(|n| matches!(n.op, IrOp::NativeChannelCombine))
                .unwrap();
            assert_eq!(combine.inputs.len(), 4);
            for (i, (input, _)) in combine.inputs.iter().enumerate() {
                let extraction = &defaults.graph.nodes[input.0 as usize];
                assert!(matches!(
                    extraction.op,
                    IrOp::NativeChannelExtract {
                        channel: Channel::A
                    }
                ));
                let source = &defaults.graph.nodes[extraction.inputs[0].0 .0 as usize];
                assert!(
                    matches!(source.op,IrOp::SolidColor{color} if color.a==if i==3{1.0}else{0.0})
                );
            }
        }
    }

    #[test]
    fn native_graph_text_static_payload_compiles_and_hashes_its_actual_content() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let (mut graph, node) = NodeGraph::new_clip_composition("Native text");
        let gid = graph.id;
        let text = timeline::TextGen {
            text: "HI".into(),
            font_size: 40.0,
            ..Default::default()
        };
        graph.nodes.get_mut(&node).unwrap().op = GraphOp::Text { text: text.clone() };
        project.graphs.insert(gid, graph);
        project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].composition = Some(gid);
        let compile =
            |p: &TimelineProject| compile_native_preview(p, seq, 0, Tick(0), Quality::FULL);
        let a = compile(&project);
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        let source = a
            .graph
            .nodes
            .iter()
            .find(|n| matches!(n.op, IrOp::NativeTextGen { .. }))
            .unwrap();
        if let IrOp::NativeTextGen { block } = &source.op {
            assert_eq!(block.cue.as_ref().unwrap().words[0].text, "HI");
        }
        for case in 0..4 {
            let mut copy = project.clone();
            let n = copy
                .graphs
                .get_mut(&gid)
                .unwrap()
                .nodes
                .get_mut(&node)
                .unwrap();
            match case {
                0 => {
                    if let GraphOp::Text { text } = &mut n.op {
                        text.fill.r = f32::NAN;
                    }
                }
                1 => {
                    if let GraphOp::Text { text } = &mut n.op {
                        text.font_size = 0.0;
                    }
                }
                2 => {
                    if let GraphOp::Text { text } = &mut n.op {
                        text.text = "X".repeat(4097);
                    }
                }
                _ => n.params.base.0.set("params.ignored", PropValue::Float(1.0)),
            };
            assert!(
                compile(&copy)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error),
                "case{case}"
            );
        }
        let mut copy = project.clone();
        if let GraphOp::Text { text } = &mut copy
            .graphs
            .get_mut(&gid)
            .unwrap()
            .nodes
            .get_mut(&node)
            .unwrap()
            .op
        {
            text.text = "BYE".into();
        }
        let b = compile(&copy);
        let other = b
            .graph
            .nodes
            .iter()
            .find(|n| matches!(n.op, IrOp::NativeTextGen { .. }))
            .unwrap();
        assert_ne!(source.content_hash, other.content_hash);
        // The same zero-input generator may be a project graph source.
        copy.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].composition = None;
        copy.project_graph = Some(gid);
        assert!(compile(&copy).diagnostics.is_empty());
    }

    #[test]
    fn native_decorations_compile_validate_and_keep_animation_alpha_separate() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let mut content = TextClipContent::new("HI");
        content.style.background = Some(timeline::CaptionBackground {
            color: Color::new(0.0, 0.0, 1.0, 0.5),
            padding: 4.0,
            corner_radius: 5.0,
        });
        project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].source =
            ClipSource::Text {
                content: content.clone(),
            };
        let compiled = compile_native_preview(&project, seq, 0, Tick(0), Quality::FULL);
        assert!(
            compiled.diagnostics.is_empty(),
            "{:?}",
            compiled.diagnostics
        );
        assert!(compiled
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeDecoratedTextGen { .. })));
        for case in 0..3 {
            let mut bad = content.style.clone();
            match case {
                0 => bad.stroke = Some((Color::BLACK, 9.0)),
                1 => bad.background.as_mut().unwrap().padding = 129.0,
                _ => bad.background.as_mut().unwrap().color.r = f32::NAN,
            };
            assert!(native_text_decoration(&bad).is_err());
        }
        let mut track = CaptionTrack::new("Painted captions");
        track.style.background = content.style.background;
        track.style.animation = CaptionAnim::SlideUp;
        track.cues.push(CaptionCue::new(
            Tick(0),
            Tick(ms_to_ticks(1000)),
            vec![
                CaptionWord::new("HI", Tick(0), Tick(ms_to_ticks(400))),
                CaptionWord::new("OK", Tick(ms_to_ticks(400)), Tick(ms_to_ticks(800))),
            ],
        ));
        project
            .sequences
            .get_mut(&seq)
            .unwrap()
            .caption_tracks
            .push(track);
        let at = |p: &TimelineProject, ms| {
            let c = compile_native_preview(p, seq, 0, Tick(ms_to_ticks(ms)), Quality::FULL);
            assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
            c
        };
        let frame = at(&project, 100);
        let node = frame
            .graph
            .nodes
            .iter()
            .find(|n| matches!(n.op, IrOp::NativeCaptionOutput { .. }))
            .unwrap();
        let IrOp::NativeCaptionOutput {
            cue_batch, paints, ..
        } = &node.op
        else {
            unreachable!()
        };
        assert_eq!(paints[0].opacity, 0.5);
        assert_eq!(paints[0].decoration.stroke.unwrap().0[3], 255);
        assert_eq!(paints[0].decoration.background.unwrap().0[3], 128);
        assert_eq!(cue_batch.cues[0].words[0].color[3], 255);
        let mut other = node.op.clone();
        if let IrOp::NativeCaptionOutput { paints, .. } = &mut other {
            paints[0].opacity = 0.75;
        }
        assert_ne!(
            content_hash(&node.op, &[], &[]),
            content_hash(&other, &[], &[])
        );
        let mut wrong = frame.graph.clone();
        if let IrOp::NativeCaptionOutput { paints, .. } =
            &mut wrong.nodes[frame.graph.output.unwrap().0 as usize].op
        {
            paints[0].opacity = f32::NAN;
        }
        assert!(wrong.validate_working_color_domain().is_err());
        project.sequences.get_mut(&seq).unwrap().caption_tracks[0]
            .style
            .animation = CaptionAnim::FadeWords;
        let fade = at(&project, 300);
        let node = fade
            .graph
            .nodes
            .iter()
            .find(|n| matches!(n.op, IrOp::NativeCaptionOutput { .. }))
            .unwrap();
        let IrOp::NativeCaptionOutput {
            cue_batch, paints, ..
        } = &node.op
        else {
            unreachable!()
        };
        assert_eq!(paints[0].opacity, 1.0);
        assert_eq!(paints[0].word_opacity, vec![255, 85]);
        assert_eq!(cue_batch.cues[0].words[1].color[3], 85);
        let track = &mut project.sequences.get_mut(&seq).unwrap().caption_tracks[0];
        let mut different = track.style.clone();
        different.stroke = None;
        track.cues[0].words[1].style_override = Some(different);
        assert!(
            compile_native_preview(&project, seq, 0, Tick(ms_to_ticks(300)), Quality::FULL)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error)
        );
    }

    #[test]
    fn native_title_slide_uses_clip_clock_and_whole_paint_opacity() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let mut content = TextClipContent::new("Slide title");
        content.style.animation = CaptionAnim::SlideUp;
        content.style.background = Some(timeline::CaptionBackground {
            color: Color::new(0.0, 0.0, 1.0, 0.5),
            padding: 4.0,
            corner_radius: 2.0,
        });
        let sequence = project.sequences.get_mut(&seq).unwrap();
        let height = sequence.formats[0].height;
        let clip = &mut sequence.video_tracks[0].clips[0];
        clip.source = ClipSource::Text {
            content: content.clone(),
        };
        clip.start = Tick::from_seconds(1);
        clip.duration = Tick::from_seconds(2);
        let at = |project: &TimelineProject, ms: i64| {
            compile_native_preview(project, seq, 0, Tick(ms_to_ticks(1000 + ms)), Quality::FULL)
        };
        let mut hashes = Vec::new();
        for (ms, progress) in [(0, 0.0f32), (100, 0.5), (200, 1.0), (500, 1.0)] {
            let c = at(&project, ms);
            assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
            let painted = c
                .graph
                .nodes
                .iter()
                .position(|n| matches!(n.op, IrOp::NativeDecoratedTextGen { .. }))
                .unwrap();
            if progress < 1.0 {
                let shifted = c
                    .graph
                    .nodes
                    .iter()
                    .position(|n| matches!(n.op, IrOp::Transform2DTransparent { .. }))
                    .unwrap();
                assert_eq!(c.graph.nodes[shifted].inputs[0].0 .0 as usize, painted);
                let IrOp::Transform2DTransparent { mat, .. } = c.graph.nodes[shifted].op else {
                    unreachable!()
                };
                assert_eq!(
                    mat,
                    Mat3::from_translation(Vec2::new(0.0, (1.0 - progress) * 0.05 * height as f32))
                );
                assert!(c.graph.nodes.iter().any(|n|matches!(n.op,IrOp::Merge{mode:BlendMode::Normal,opacity}if opacity==progress)&&n.inputs[0].0.0 as usize==shifted));
            }
            hashes.push(c.graph.nodes[c.graph.output.unwrap().0 as usize].content_hash);
        }
        assert_ne!(hashes[0], hashes[1]);
        assert_ne!(hashes[1], hashes[2]);
        assert_eq!(hashes[2], hashes[3]);
        content.style.animation = CaptionAnim::None;
        project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].source =
            ClipSource::Text {
                content: content.clone(),
            };
        let static_frame = at(&project, 200);
        assert_eq!(
            hashes[2],
            static_frame.graph.nodes[static_frame.graph.output.unwrap().0 as usize].content_hash
        );
        for unsupported in [CaptionAnim::FadeWords, CaptionAnim::Typewriter] {
            content.style.animation = unsupported;
            project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].source =
                ClipSource::Text {
                    content: content.clone(),
                };
            assert!(at(&project, 100)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error));
        }
    }

    #[test]
    fn native_title_compiles_plain_fill_and_rejects_unqualified_styles() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let plain = TextClipContent {
            text: "HI".into(),
            style: CaptionStyle {
                stroke: None,
                font_size: 40.0,
                position: [0.5, 0.2],
                ..Default::default()
            },
        };
        project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].source =
            ClipSource::Text {
                content: plain.clone(),
            };
        let frame = compile_native_preview(&project, seq, 0, Tick::ZERO, Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert!(frame
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeTextGen { .. })));
        for invalid in 0..7 {
            let mut copy = project.clone();
            let mut content = plain.clone();
            match invalid {
                0 => content.style.stroke = Some((Color::BLACK, 9.0)),
                1 => content.style.fill.r = f32::NAN,
                2 => content.style.font_size = 0.0,
                3 => content.style.position = [-0.1, 0.5],
                4 => content.style.max_width = 0.0,
                5 => content.style.weight = 0,
                _ => content.text = "X".repeat(4097),
            }
            copy.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].source =
                ClipSource::Text { content };
            assert!(
                compile_native_preview(&copy, seq, 0, Tick::ZERO, Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error),
                "invalid case {invalid}"
            );
        }
        let source = frame
            .graph
            .nodes
            .iter()
            .find(|n| matches!(n.op, IrOp::NativeTextGen { .. }))
            .unwrap();
        let IrOp::NativeTextGen { block } = &source.op else {
            unreachable!()
        };
        assert_ne!(
            content_hash(&source.op, &[], &[]),
            content_hash(
                &IrOp::TextGen {
                    block: block.clone()
                },
                &[],
                &[]
            )
        );
        let mut wrong = FrameGraph {
            working_color_domain: WorkingColorDomain::LegacyLinearRec709,
            nodes: vec![source.clone()],
            output: Some(IrNodeId(0)),
        };
        assert!(wrong.validate_working_color_domain().is_err());
        wrong.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
        assert!(wrong.validate_working_color_domain().is_ok());
        wrong.nodes[0]
            .inputs
            .push((IrNodeId(0), OutPort::default()));
        assert!(wrong.validate_working_color_domain().is_err());
    }

    #[test]
    fn native_denoise_scopes_keys_validation_and_hash() {
        let (mut project, sequence, _, _) = native_group_look_fixture();
        let fx =
            || ClipEffect::from_manifest(timeline::EffectId::new_static("noise.reduce")).unwrap();
        let asset = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&asset)
            .unwrap()
            .effects
            .push(fx());
        let seq = project.sequences.get_mut(&sequence).unwrap();
        seq.video_tracks[0].clips[0].effects.push(fx());
        seq.video_tracks[0].effects.push(fx());
        seq.master_effects.push(fx());
        let frame = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(
                    n.op,
                    IrOp::NativeDenoise {
                        strength: 0.5,
                        spatial_scale: 1.0
                    }
                ))
                .count(),
            4
        );
        for value in [
            PropValue::Float(f64::NAN),
            PropValue::Float(-0.01),
            PropValue::Float(1.01),
            PropValue::Bool(true),
        ] {
            let mut invalid = project.clone();
            invalid.sequences.get_mut(&sequence).unwrap().master_effects[0]
                .params
                .base
                .set("params.strength", value);
            assert!(
                compile_native_preview(&invalid, sequence, 0, Tick::ZERO, Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
            );
        }
        let clip = &mut project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0];
        clip.start = Tick(100);
        clip.effects[0].zone = Some((Tick(10), Tick(20)));
        let lane = clip.effects[0]
            .params
            .track_mut(&PropPath::new("params.strength"));
        lane.insert_keyframe(timeline::Keyframe::new(
            Tick(0),
            PropValue::Float(0.0),
            timeline::Interp::Linear,
        ));
        lane.insert_keyframe(timeline::Keyframe::new(
            Tick(20),
            PropValue::Float(1.0),
            timeline::Interp::Linear,
        ));
        let frame = compile_native_preview(&project, sequence, 0, Tick(115), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert!(frame
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeDenoise { strength: 0.75, .. })));
        assert_ne!(
            content_hash(
                &IrOp::NativeDenoise {
                    strength: 0.5,
                    spatial_scale: 1.0
                },
                &[],
                &[]
            ),
            content_hash(
                &IrOp::NativeDenoise {
                    strength: 0.75,
                    spatial_scale: 1.0
                },
                &[],
                &[]
            )
        );
    }

    #[test]
    fn native_grain_scopes_keys_validation_and_seed_hash() {
        let (mut project, sequence, _, _) = native_group_look_fixture();
        let fx =
            || ClipEffect::from_manifest(timeline::EffectId::new_static("stylize.grain")).unwrap();
        let asset = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&asset)
            .unwrap()
            .effects
            .push(fx());
        let seq = project.sequences.get_mut(&sequence).unwrap();
        seq.video_tracks[0].clips[0].effects.push(fx());
        seq.video_tracks[0].effects.push(fx());
        seq.master_effects.push(fx());
        let frame = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(
                    n.op,
                    IrOp::NativeGrain {
                        amount: 0.1,
                        monochrome: true,
                        seed: 1,
                        ..
                    }
                ))
                .count(),
            4
        );
        for (path, value) in [
            ("params.amount", PropValue::Float(f64::NAN)),
            ("params.amount", PropValue::Float(1.01)),
            ("params.monochrome", PropValue::Float(-0.1)),
            ("params.seed", PropValue::Float(1000000001.0)),
            ("params.seed", PropValue::Bool(true)),
        ] {
            let mut invalid = project.clone();
            invalid.sequences.get_mut(&sequence).unwrap().master_effects[0]
                .params
                .base
                .set(path, value);
            assert!(
                compile_native_preview(&invalid, sequence, 0, Tick::ZERO, Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
            );
        }
        let clip = &mut project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0];
        clip.start = Tick(100);
        clip.effects[0].zone = Some((Tick(10), Tick(20)));
        let lane = clip.effects[0]
            .params
            .track_mut(&PropPath::new("params.seed"));
        lane.insert_keyframe(timeline::Keyframe::new(
            Tick(0),
            PropValue::Float(16777216.0),
            timeline::Interp::Linear,
        ));
        lane.insert_keyframe(timeline::Keyframe::new(
            Tick(20),
            PropValue::Float(16777218.0),
            timeline::Interp::Linear,
        ));
        let frame = compile_native_preview(&project, sequence, 0, Tick(110), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert!(frame
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeGrain { seed: 16777217, .. })));
        let op = |seed, monochrome, amount| IrOp::NativeGrain {
            amount,
            monochrome,
            seed,
            pixel_scale: [1.0; 2],
        };
        let first = content_hash(&op(16777217, true, 0.1), &[], &[]);
        for other in [
            op(16777216, true, 0.1),
            op(16777217, false, 0.1),
            op(16777217, true, 0.2),
        ] {
            assert_ne!(first, content_hash(&other, &[], &[]));
        }
    }

    #[test]
    fn native_vignette_scopes_zones_validation_and_hash() {
        let (mut project, sequence, _, _) = native_group_look_fixture();
        let fx = || {
            ClipEffect::from_manifest(timeline::EffectId::new_static("stylize.vignette")).unwrap()
        };
        let asset = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&asset)
            .unwrap()
            .effects
            .push(fx());
        let seq = project.sequences.get_mut(&sequence).unwrap();
        let mut zoned = fx();
        zoned.zone = Some((Tick(10), Tick(20)));
        seq.video_tracks[0].clips[0].effects.push(zoned);
        seq.video_tracks[0].effects.push(fx());
        seq.master_effects.push(fx());
        for (tick, count) in [(5, 3), (15, 4), (20, 3)] {
            let frame = compile_native_preview(&project, sequence, 0, Tick(tick), Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            assert!(frame.graph.validate_working_color_domain().is_ok());
            assert_eq!(
                frame
                    .graph
                    .nodes
                    .iter()
                    .filter(|n| matches!(
                        n.op,
                        IrOp::NativeVignette {
                            amount: -0.5,
                            feather: 0.5
                        }
                    ))
                    .count(),
                count
            );
        }
        for (path, value) in [
            ("params.amount", PropValue::Float(f64::NAN)),
            ("params.amount", PropValue::Float(1.01)),
            ("params.feather", PropValue::Float(-0.1)),
            ("params.feather", PropValue::Bool(true)),
        ] {
            let mut invalid = project.clone();
            invalid.sequences.get_mut(&sequence).unwrap().master_effects[0]
                .params
                .base
                .set(path, value);
            assert!(
                compile_native_preview(&invalid, sequence, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
            );
        }
        let clip = &mut project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0];
        clip.start = Tick(100);
        let lane = clip.effects[0]
            .params
            .track_mut(&PropPath::new("params.amount"));
        lane.insert_keyframe(timeline::Keyframe::new(
            Tick(0),
            PropValue::Float(-1.0),
            timeline::Interp::Linear,
        ));
        lane.insert_keyframe(timeline::Keyframe::new(
            Tick(20),
            PropValue::Float(1.0),
            timeline::Interp::Linear,
        ));
        let frame = compile_native_preview(&project, sequence, 0, Tick(115), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert!(frame
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeVignette { amount: 0.5, .. })));
        let first = content_hash(
            &IrOp::NativeVignette {
                amount: -0.5,
                feather: 0.5,
            },
            &[],
            &[],
        );
        for op in [
            IrOp::NativeVignette {
                amount: 0.5,
                feather: 0.5,
            },
            IrOp::NativeVignette {
                amount: -0.5,
                feather: 1.0,
            },
            IrOp::NativeInvert,
        ] {
            assert_ne!(first, content_hash(&op, &[], &[]));
        }
    }

    #[test]
    fn native_invert_stacks_graph_and_scope_zones_are_explicit() {
        let (mut project, sequence, _, _) = native_group_look_fixture();
        let invert =
            || ClipEffect::from_manifest(timeline::EffectId::new_static("color.invert")).unwrap();
        let asset = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&asset)
            .unwrap()
            .effects
            .push(invert());
        let seq = project.sequences.get_mut(&sequence).unwrap();
        let mut clip_effect = invert();
        clip_effect.zone = Some((Tick(10), Tick(20)));
        seq.video_tracks[0].clips[0].effects.push(clip_effect);
        seq.video_tracks[0].effects.push(invert());
        seq.master_effects.push(invert());
        for (tick, count) in [(5, 3), (15, 4), (20, 3)] {
            let frame = compile_native_preview(&project, sequence, 0, Tick(tick), Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            assert!(frame.graph.validate_working_color_domain().is_ok());
            assert_eq!(
                frame
                    .graph
                    .nodes
                    .iter()
                    .filter(|n| matches!(n.op, IrOp::NativeInvert))
                    .count(),
                count
            );
        }
        let (mut graph, source) = NodeGraph::new_clip_composition("Native invert");
        let node = GraphNode::new(GraphOp::Invert);
        let id = node.id;
        graph.nodes.insert(id, node);
        graph.edges = vec![
            timeline::GraphEdge {
                from: (source, timeline::OutPort::PRIMARY),
                to: (id, InPort::PRIMARY),
            },
            timeline::GraphEdge {
                from: (id, timeline::OutPort::PRIMARY),
                to: (graph.output, InPort::PRIMARY),
            },
        ];
        let gid = graph.id;
        project.graphs.insert(gid, graph);
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].composition =
            Some(gid);
        let frame = compile_native_preview(&project, sequence, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativeInvert))
                .count(),
            5
        );
        let fx = &mut project.sequences.get_mut(&sequence).unwrap().master_effects[0];
        fx.id = timeline::EffectId::EMPTY;
        assert!(
            compile_native_preview(&project, sequence, 0, Tick(15), Quality::FULL)
                .diagnostics
                .is_empty()
        );
        project.sequences.get_mut(&sequence).unwrap().master_effects[0].id =
            timeline::EffectId::new_static("unknown.invert");
        assert!(
            compile_native_preview(&project, sequence, 0, Tick(15), Quality::FULL)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error)
        );
        assert_ne!(
            content_hash(&IrOp::NativeInvert, &[], &[]),
            content_hash(&IrOp::NativeExposure { stops: 1.0 }, &[], &[])
        );
    }

    #[test]
    fn native_polygon_compiler_samples_geometry_hashes_vertices_and_rejects_invalid() {
        let (mut project, sequence, _, _) = native_group_look_fixture();
        let (mut graph, _) = NodeGraph::new_clip_composition("Polygon");
        let mut node = GraphNode::new(GraphOp::MaskShape {
            shape: timeline::MaskShapeKind::Polygon {
                vertices: vec![[0.0, 0.0], [1.0, 0.0], [0.5, 1.0]],
            },
        });
        let id = node.id;
        for path in [
            "params.center_x",
            "params.center_y",
            "params.size_x",
            "params.size_y",
            "params.rotation",
            "params.feather",
        ] {
            let lane = node.params.track_mut(&PropPath::new(path));
            for (tick, value) in [(0, 0.2), (10, 0.6)] {
                lane.insert_keyframe(timeline::Keyframe::new(
                    Tick(tick),
                    PropValue::Float(value),
                    timeline::Interp::Linear,
                ));
            }
        }
        graph.nodes.insert(id, node);
        graph.edges = vec![timeline::GraphEdge {
            from: (id, timeline::OutPort::PRIMARY),
            to: (graph.output, InPort::PRIMARY),
        }];
        let gid = graph.id;
        project.graphs.insert(gid, graph);
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].composition =
            Some(gid);
        let mut hashes = HashSet::new();
        for (tick, expected) in [(0, 0.2_f32), (5, 0.4), (10, 0.6)] {
            let frame = compile_native_preview(&project, sequence, 0, Tick(tick), Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            assert!(frame.graph.validate_working_color_domain().is_ok());
            let polygon = frame
                .graph
                .nodes
                .iter()
                .find(|n| matches!(n.op, IrOp::NativePolygonMask { .. }))
                .unwrap();
            let IrOp::NativePolygonMask {
                center,
                size,
                rotation,
                feather,
                ..
            } = &polygon.op
            else {
                unreachable!()
            };
            for v in center.iter().chain(size).chain([rotation, feather]) {
                assert!((*v - expected).abs() < 1e-6);
            }
            assert!(hashes.insert(polygon.content_hash));
        }
        let before = compile_native_preview(&project, sequence, 0, Tick(5), Quality::FULL).graph;
        let node = project
            .graphs
            .get_mut(&gid)
            .unwrap()
            .nodes
            .get_mut(&id)
            .unwrap();
        let GraphOp::MaskShape {
            shape: timeline::MaskShapeKind::Polygon { vertices },
        } = &mut node.op
        else {
            unreachable!()
        };
        vertices[2][0] = 0.6;
        let after = compile_native_preview(&project, sequence, 0, Tick(5), Quality::FULL).graph;
        assert_ne!(
            before.nodes.last().unwrap().content_hash,
            after.nodes.last().unwrap().content_hash
        );
        let saved = project.graphs[&gid].nodes[&id].params.clone();
        project
            .graphs
            .get_mut(&gid)
            .unwrap()
            .nodes
            .get_mut(&id)
            .unwrap()
            .params
            .base
            .0
            .set("vertices.0.x", PropValue::Float(0.5));
        assert!(
            compile_native_preview(&project, sequence, 0, Tick(5), Quality::FULL)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error)
        );
        project
            .graphs
            .get_mut(&gid)
            .unwrap()
            .nodes
            .get_mut(&id)
            .unwrap()
            .params = saved;
        for invalid in [
            vec![],
            vec![[0.0, 0.0]; 2],
            vec![[0.0, 0.0]; 129],
            vec![[f32::INFINITY, 0.0]; 3],
            vec![[-0.1, 0.0]; 3],
        ] {
            project
                .graphs
                .get_mut(&gid)
                .unwrap()
                .nodes
                .get_mut(&id)
                .unwrap()
                .op = GraphOp::MaskShape {
                shape: timeline::MaskShapeKind::Polygon { vertices: invalid },
            };
            let frame = compile_native_preview(&project, sequence, 0, Tick(5), Quality::FULL);
            assert!(frame
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error));
            assert!(!frame
                .graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::NativePolygonMask { .. })));
        }
    }

    #[test]
    fn native_shape_stacks_graph_and_scope_zones() {
        let (mut project, sequence, _, _) = native_group_look_fixture();
        let shape = || {
            ClipEffect::from_manifest(timeline::EffectId::new_static("util.mask_shape")).unwrap()
        };
        let asset = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&asset)
            .unwrap()
            .effects
            .push(shape());
        let seq = project.sequences.get_mut(&sequence).unwrap();
        let mut clip_effect = shape();
        clip_effect.zone = Some((Tick(10), Tick(20)));
        seq.video_tracks[0].clips[0].effects.push(clip_effect);
        seq.video_tracks[0].effects.push(shape());
        seq.master_effects.push(shape());
        for (tick, count) in [(5, 1), (15, 1), (20, 1)] {
            let frame = compile_native_preview(&project, sequence, 0, Tick(tick), Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            assert!(frame.graph.validate_working_color_domain().is_ok());
            assert_eq!(
                frame
                    .graph
                    .nodes
                    .iter()
                    .filter(|n| matches!(n.op, IrOp::NativeMaskShape { .. }))
                    .count(),
                count
            );
        }
        let (mut graph, _source) = NodeGraph::new_clip_composition("Native shape");
        let node = GraphNode::new(GraphOp::MaskShape {
            shape: timeline::MaskShapeKind::Ellipse,
        });
        let id = node.id;
        graph.nodes.insert(id, node);
        graph.edges = vec![timeline::GraphEdge {
            from: (id, timeline::OutPort::PRIMARY),
            to: (graph.output, InPort::PRIMARY),
        }];
        let gid = graph.id;
        project.graphs.insert(gid, graph);
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].composition =
            Some(gid);
        let frame = compile_native_preview(&project, sequence, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativeMaskShape { .. }))
                .count(),
            1
        );
        let fx = &mut project.sequences.get_mut(&sequence).unwrap().master_effects[0];
        fx.id = timeline::EffectId::EMPTY;
        assert!(
            compile_native_preview(&project, sequence, 0, Tick(15), Quality::FULL)
                .diagnostics
                .is_empty()
        );
        project.sequences.get_mut(&sequence).unwrap().master_effects[0].id =
            timeline::EffectId::new_static("unknown.shape");
        assert!(
            compile_native_preview(&project, sequence, 0, Tick(15), Quality::FULL)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error)
        );
        assert_ne!(
            content_hash(
                &IrOp::NativeMaskShape {
                    rect: false,
                    center: [0.5; 2],
                    size: [0.25; 2],
                    rotation: 0.0,
                    feather: 0.0
                },
                &[],
                &[]
            ),
            content_hash(&IrOp::NativeExposure { stops: 1.0 }, &[], &[])
        );
        for (path, value) in [
            ("params.center_x", -0.1),
            ("params.size_y", 2.1),
            ("params.feather", 1.1),
            ("params.rotation", f64::INFINITY),
        ] {
            let mut fx = shape();
            fx.params.base.set(path, PropValue::Float(value));
            let params = resolve_effect_params(
                EffectKind::MaskShapeGen,
                &fx.params.base,
                &fx.params,
                Tick::ZERO,
            );
            assert!(native_shape_op(&params, false).is_err(), "{path}");
        }
        let hash = |rect, center, size, rotation, feather| {
            content_hash(
                &IrOp::NativeMaskShape {
                    rect,
                    center,
                    size,
                    rotation,
                    feather,
                },
                &[],
                &[],
            )
        };
        let base = hash(false, [0.5; 2], [0.25; 2], 0.0, 0.0);
        assert_ne!(base, hash(true, [0.5; 2], [0.25; 2], 0.0, 0.0));
        assert_ne!(base, hash(false, [0.25; 2], [0.25; 2], 0.0, 0.0));
        assert_ne!(base, hash(false, [0.5; 2], [0.5; 2], 0.0, 0.0));
        assert_ne!(base, hash(false, [0.5; 2], [0.25; 2], 0.5, 0.0));
        assert_ne!(base, hash(false, [0.5; 2], [0.25; 2], 0.0, 0.5));
        // Generators replace their input. Earlier generators are correctly
        // pruned when a later scope supplies a fresh image; qualify each alone.
        for scope in 0..4 {
            let (mut p, seq, _, _) = native_group_look_fixture();
            let mut fx = shape();
            fx.params.base.set("params.center_x", PropValue::Float(0.5));
            fx.params.base.set("params.size_x", PropValue::Float(0.25));
            if scope == 1 {
                fx.zone = Some((Tick(10), Tick(20)));
            }
            if scope == 0 {
                p.media.assets.values_mut().next().unwrap().effects.push(fx);
            } else {
                let sequence = p.sequences.get_mut(&seq).unwrap();
                match scope {
                    1 => sequence.video_tracks[0].clips[0].effects.push(fx),
                    2 => sequence.video_tracks[0].effects.push(fx),
                    _ => sequence.master_effects.push(fx),
                }
            }
            for tick in [5, 15, 20] {
                let frame = compile_native_preview(&p, seq, 0, Tick(tick), Quality::FULL);
                assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
                let expected = if scope == 1 && tick != 15 { 0 } else { 1 };
                assert_eq!(
                    frame
                        .graph
                        .nodes
                        .iter()
                        .filter(|n| matches!(n.op, IrOp::NativeMaskShape { .. }))
                        .count(),
                    expected
                );
            }
        }
    }

    #[test]
    fn native_nonseparable_track_lowering_and_legacy_hash_separation() {
        for mode in [
            BlendMode::Hue,
            BlendMode::Saturation,
            BlendMode::Color,
            BlendMode::Luminosity,
        ] {
            let (mut project, sequence, _, _) = native_group_look_fixture();
            project.sequences.get_mut(&sequence).unwrap().video_tracks[0].blend = mode;
            project.sequences.get_mut(&sequence).unwrap().video_tracks[0].opacity = 0.5;
            let frame = compile_native_preview(&project, sequence, 0, Tick(5), Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            assert!(frame.graph.validate_working_color_domain().is_ok());
            assert!(frame.graph.nodes.iter().any(|n| matches!(n.op, IrOp::NativeBoundedMerge { mode: m, opacity } if m == mode && opacity == 0.5)));
            assert_ne!(
                content_hash(&IrOp::Merge { mode, opacity: 0.5 }, &[], &[]),
                content_hash(&IrOp::NativeBoundedMerge { mode, opacity: 0.5 }, &[], &[])
            );
            let mut legacy = Builder::new();
            let node = legacy.push(IrOp::Merge { mode, opacity: 0.5 }, vec![]);
            assert!(matches!(
                legacy.nodes[node.0 as usize].op,
                IrOp::Merge { .. }
            ));
        }
    }

    #[test]
    fn native_bounded_track_lowering_and_legacy_hash_separation() {
        for mode in [
            BlendMode::ColorDodge,
            BlendMode::ColorBurn,
            BlendMode::Divide,
            BlendMode::VividLight,
            BlendMode::HardMix,
        ] {
            let (mut project, sequence, _, _) = native_group_look_fixture();
            project.sequences.get_mut(&sequence).unwrap().video_tracks[0].blend = mode;
            project.sequences.get_mut(&sequence).unwrap().video_tracks[0].opacity = 0.5;
            let frame = compile_native_preview(&project, sequence, 0, Tick(5), Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            assert!(frame.graph.validate_working_color_domain().is_ok());
            assert!(frame.graph.nodes.iter().any(|n| matches!(n.op, IrOp::NativeBoundedMerge { mode: m, opacity } if m == mode && opacity == 0.5)));
            assert_ne!(
                content_hash(&IrOp::Merge { mode, opacity: 0.5 }, &[], &[]),
                content_hash(&IrOp::NativeBoundedMerge { mode, opacity: 0.5 }, &[], &[])
            );
            let mut legacy = Builder::new();
            let node = legacy.push(IrOp::Merge { mode, opacity: 0.5 }, vec![]);
            assert!(matches!(
                legacy.nodes[node.0 as usize].op,
                IrOp::Merge { .. }
            ));
        }
    }

    #[test]
    fn native_arithmetic_track_lowering_and_legacy_hash_separation() {
        for mode in [
            BlendMode::LinearDodge,
            BlendMode::LinearBurn,
            BlendMode::Subtract,
            BlendMode::LinearLight,
        ] {
            let (mut project, sequence, _, _) = native_group_look_fixture();
            project.sequences.get_mut(&sequence).unwrap().video_tracks[0].blend = mode;
            project.sequences.get_mut(&sequence).unwrap().video_tracks[0].opacity = 0.5;
            let frame = compile_native_preview(&project, sequence, 0, Tick(5), Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            assert!(frame.graph.validate_working_color_domain().is_ok());
            assert!(frame.graph.nodes.iter().any(|n| matches!(n.op, IrOp::NativeArithmeticMerge { mode: m, opacity } if m == mode && opacity == 0.5)));
            assert_ne!(
                content_hash(&IrOp::Merge { mode, opacity: 0.5 }, &[], &[]),
                content_hash(
                    &IrOp::NativeArithmeticMerge { mode, opacity: 0.5 },
                    &[],
                    &[]
                )
            );
            let mut legacy = Builder::new();
            let node = legacy.push(IrOp::Merge { mode, opacity: 0.5 }, vec![]);
            assert!(matches!(
                legacy.nodes[node.0 as usize].op,
                IrOp::Merge { .. }
            ));
        }
    }

    #[test]
    fn native_luminance_track_lowering_and_legacy_hash_separation() {
        for mode in [BlendMode::DarkerColor, BlendMode::LighterColor] {
            let (mut project, sequence, _, _) = native_group_look_fixture();
            project.sequences.get_mut(&sequence).unwrap().video_tracks[0].blend = mode;
            project.sequences.get_mut(&sequence).unwrap().video_tracks[0].opacity = 0.5;
            let frame = compile_native_preview(&project, sequence, 0, Tick(5), Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            assert!(frame.graph.validate_working_color_domain().is_ok());
            assert!(frame.graph.nodes.iter().any(|n| matches!(n.op, IrOp::NativeLuminanceMerge { mode: m, opacity } if m == mode && opacity == 0.5)));
            assert_ne!(
                content_hash(&IrOp::Merge { mode, opacity: 0.5 }, &[], &[]),
                content_hash(&IrOp::NativeLuminanceMerge { mode, opacity: 0.5 }, &[], &[])
            );
            let mut legacy = Builder::new();
            let node = legacy.push(IrOp::Merge { mode, opacity: 0.5 }, vec![]);
            assert!(matches!(
                legacy.nodes[node.0 as usize].op,
                IrOp::Merge { .. }
            ));
        }
    }

    #[test]
    fn native_piecewise_graph_blends_sample_opacity_and_separate_hashes() {
        let (project, sequence, _, _) = native_group_look_fixture();
        let format = &project.sequences[&sequence].formats[0];
        let mut graph = NodeGraph::new_project_graph("Native piecewise blend");
        let mut source = GraphNode::new(GraphOp::SolidColor);
        source
            .params
            .base
            .0
            .set("params.color", PropValue::Color(Color::RED));
        let source_id = source.id;
        let bottom = GraphNode::new(GraphOp::SolidColor);
        let bottom_id = bottom.id;
        let mut merge = GraphNode::new(GraphOp::Merge {
            mode: BlendMode::Overlay,
        });
        let merge_id = merge.id;
        let lane = merge.params.track_mut(&PropPath::new("params.opacity"));
        for (tick, value) in [(0, 0.0), (10, 1.0)] {
            lane.insert_keyframe(timeline::Keyframe::new(
                Tick(tick),
                PropValue::Float(value),
                timeline::Interp::Linear,
            ));
        }
        for node in [source, bottom, merge] {
            graph.nodes.insert(node.id, node);
        }
        for (from, to, port) in [
            (source_id, merge_id, InPort::PRIMARY),
            (bottom_id, merge_id, InPort::B),
            (merge_id, graph.output, InPort::PRIMARY),
        ] {
            graph.edges.push(timeline::GraphEdge {
                from: (from, timeline::OutPort::PRIMARY),
                to: (to, port),
            });
        }
        let mut hashes = HashSet::new();
        for mode in [
            BlendMode::Hue,
            BlendMode::Saturation,
            BlendMode::Color,
            BlendMode::Luminosity,
            BlendMode::ColorDodge,
            BlendMode::ColorBurn,
            BlendMode::Divide,
            BlendMode::VividLight,
            BlendMode::HardMix,
            BlendMode::Overlay,
            BlendMode::HardLight,
            BlendMode::PinLight,
            BlendMode::SoftLight,
            BlendMode::LinearDodge,
            BlendMode::LinearBurn,
            BlendMode::Subtract,
            BlendMode::LinearLight,
            BlendMode::DarkerColor,
            BlendMode::LighterColor,
        ] {
            graph.nodes.get_mut(&merge_id).unwrap().op = GraphOp::Merge { mode };
            for (tick, expected) in [(0, 0.0), (5, 0.5), (10, 1.0)] {
                let mut builder = Builder::new();
                builder.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
                let output =
                    lower_native_graph(&mut builder, &graph, format, Tick(tick), None, None)
                        .unwrap();
                let compiled = builder.finish(Some(output));
                assert!(compiled.graph.validate_working_color_domain().is_ok());
                let node = &compiled.graph.nodes[output.0 as usize];
                assert!(
                    matches!(node.op, IrOp::Merge { mode: found, opacity } | IrOp::NativeBoundedMerge { mode: found, opacity } | IrOp::NativeArithmeticMerge { mode: found, opacity } | IrOp::NativeLuminanceMerge { mode: found, opacity } if found == mode && opacity == expected)
                );
                assert!(
                    hashes.insert(node.content_hash),
                    "mode and opacity must participate in the output hash"
                );
            }
        }
    }

    #[test]
    fn native_switch_selects_animated_branch_and_prunes_dormant_graphs() {
        let (project, seq, _, _) = native_group_look_fixture();
        let format = &project.sequences[&seq].formats[0];
        let mut graph = NodeGraph::new_project_graph("Native Switch");
        let mut red = GraphNode::new(GraphOp::SolidColor);
        red.params
            .base
            .0
            .set("params.color", PropValue::Color(Color::RED));
        let red_id = red.id;
        let mut blue = GraphNode::new(GraphOp::SolidColor);
        blue.params.base.0.set(
            "params.color",
            PropValue::Color(Color {
                r: 0.0,
                g: 0.0,
                b: 1.0,
                a: 0.5,
            }),
        );
        let blue_id = blue.id;
        let mut dead = GraphNode::new(GraphOp::MaskFromMatte);
        dead.params
            .base
            .0
            .set("params.invalid", PropValue::Bool(true));
        let dead_id = dead.id;
        let switch = GraphNode::new(GraphOp::Switch);
        let switch_id = switch.id;
        for node in [red, blue, dead, switch] {
            graph.nodes.insert(node.id, node);
        }
        let edge = |from, port| timeline::GraphEdge {
            from: (from, timeline::OutPort::PRIMARY),
            to: (switch_id, InPort(port)),
        };
        graph.edges = vec![
            edge(blue_id, 2),
            edge(red_id, 0),
            edge(dead_id, 3),
            timeline::GraphEdge {
                from: (switch_id, timeline::OutPort::PRIMARY),
                to: (graph.output, InPort::PRIMARY),
            },
        ];
        let compile = |graph: &NodeGraph, tick| {
            let mut b = Builder::new();
            b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
            lower_native_graph(&mut b, graph, format, tick, None, None)
                .map(|output| b.finish(Some(output)))
        };
        for (selected, color) in [
            (0, Color::RED),
            (
                2,
                Color {
                    r: 0.0,
                    g: 0.0,
                    b: 1.0,
                    a: 0.5,
                },
            ),
            (1, Color::RED),
            (u32::MAX, Color::RED),
        ] {
            graph
                .nodes
                .get_mut(&switch_id)
                .unwrap()
                .params
                .base
                .0
                .set("params.selected", PropValue::Enum(selected));
            let frame = compile(&graph, Tick::ZERO).unwrap();
            assert!(frame.graph.validate_working_color_domain().is_ok());
            assert_eq!(
                frame.graph.nodes.len(),
                2,
                "Only the selected producer and shared transparent fallback are lowered"
            );
            let expected = native_swatch_color(color).unwrap();
            assert!(
                matches!(frame.graph.nodes[frame.graph.output.unwrap().0 as usize].op,IrOp::SolidColor {color} if color==expected)
            );
        }
        let lane = graph
            .nodes
            .get_mut(&switch_id)
            .unwrap()
            .params
            .track_mut(&PropPath::new("params.selected"));
        lane.insert_keyframe(timeline::Keyframe::new(
            Tick(0),
            PropValue::Enum(0),
            timeline::Interp::Linear,
        ));
        lane.insert_keyframe(timeline::Keyframe::new(
            Tick(10),
            PropValue::Enum(2),
            timeline::Interp::Linear,
        ));
        let before = compile(&graph, Tick(9)).unwrap();
        let after = compile(&graph, Tick(10)).unwrap();
        assert_ne!(
            before.graph.nodes[before.graph.output.unwrap().0 as usize].content_hash,
            after.graph.nodes[after.graph.output.unwrap().0 as usize].content_hash
        );
        let repeated = compile(&graph, Tick(20)).unwrap();
        assert_eq!(
            after.graph.nodes[after.graph.output.unwrap().0 as usize].content_hash,
            repeated.graph.nodes[repeated.graph.output.unwrap().0 as usize].content_hash
        );

        // Selection enum steps rather than interpolating. Unselected self-cycles
        // are dormant, but become an error when selected.
        graph.edges[0].from.0 = switch_id;
        assert!(compile(&graph, Tick(9)).is_ok());
        assert!(compile(&graph, Tick(10)).is_err());
        graph.edges[0].from.0 = blue_id;
        graph.nodes.get_mut(&switch_id).unwrap().params = GraphNode::new(GraphOp::Switch).params;
        graph
            .nodes
            .get_mut(&switch_id)
            .unwrap()
            .params
            .base
            .0
            .set("params.selected", PropValue::Enum(3));
        assert!(compile(&graph, Tick::ZERO).is_err());
        graph
            .nodes
            .get_mut(&switch_id)
            .unwrap()
            .params
            .base
            .0
            .set("params.selected", PropValue::Float(0.0));
        assert!(compile(&graph, Tick::ZERO).is_err());
        graph
            .nodes
            .get_mut(&switch_id)
            .unwrap()
            .params
            .base
            .0
            .set("params.selected", PropValue::Enum(0));
        graph.edges.push(edge(blue_id, 0));
        assert!(compile(&graph, Tick::ZERO).is_err());
        graph.edges.pop();
        graph.edges.push(edge(blue_id, 4));
        assert!(compile(&graph, Tick::ZERO).is_err());
        graph.edges.pop();
        graph.edges[2].from.1 = timeline::OutPort(1);
        assert!(compile(&graph, Tick::ZERO).is_err());
    }

    #[test]
    fn native_switch_without_connected_inputs_is_transparent_not_program() {
        let (project, seq, _, _) = native_group_look_fixture();
        let format = &project.sequences[&seq].formats[0];
        let mut graph = NodeGraph::new_project_graph("Empty Switch");
        let node = GraphNode::new(GraphOp::Switch);
        let id = node.id;
        graph.nodes.insert(id, node);
        graph.edges.push(timeline::GraphEdge {
            from: (id, timeline::OutPort::PRIMARY),
            to: (graph.output, InPort::PRIMARY),
        });
        let mut b = Builder::new();
        b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
        let source = b.push(
            IrOp::SolidColor {
                color: LinearColor {
                    r: 1.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            },
            vec![],
        );
        let output =
            lower_native_graph(&mut b, &graph, format, Tick::ZERO, None, Some(source)).unwrap();
        let frame = b.finish(Some(output));
        assert_eq!(frame.graph.nodes.len(), 2);
        assert!(
            matches!(frame.graph.nodes[frame.graph.output.unwrap().0 as usize].op,IrOp::SolidColor {color} if color.a==0.0)
        );
    }

    #[test]
    fn native_graph_solid_swatches_animation_and_zero_input_contract() {
        let (project, seq, _, _) = native_group_look_fixture();
        let format = &project.sequences[&seq].formats[0];
        let mut graph = NodeGraph::new_project_graph("Native solid");
        let mut node = GraphNode::new(GraphOp::SolidColor);
        let id = node.id;
        node.params.base.0.set(
            "params.color",
            PropValue::Color(Color {
                r: 0.2,
                g: 0.5,
                b: 0.8,
                a: 0.5,
            }),
        );
        graph.nodes.insert(id, node);
        graph.edges.push(timeline::GraphEdge {
            from: (id, timeline::OutPort::PRIMARY),
            to: (graph.output, InPort::PRIMARY),
        });
        let compile = |graph: &NodeGraph, tick| {
            let mut b = Builder::new();
            b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
            lower_native_graph(&mut b, graph, format, tick, None, None)
                .map(|output| b.finish(Some(output)))
        };
        let frame = compile(&graph, Tick::ZERO).unwrap();
        assert!(frame.graph.validate_working_color_domain().is_ok());
        let color = native_swatch_color(Color {
            r: 0.2,
            g: 0.5,
            b: 0.8,
            a: 0.5,
        })
        .unwrap();
        let out = frame
            .graph
            .nodes
            .get(frame.graph.output.unwrap().0 as usize)
            .unwrap();
        assert!(matches!(out.op,IrOp::SolidColor {color:c} if c==color));
        let lane = graph
            .nodes
            .get_mut(&id)
            .unwrap()
            .params
            .track_mut(&PropPath::new("params.color"));
        lane.insert_keyframe(timeline::Keyframe::new(
            Tick(0),
            PropValue::Color(Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }),
            timeline::Interp::Linear,
        ));
        lane.insert_keyframe(timeline::Keyframe::new(
            Tick(10),
            PropValue::Color(Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            }),
            timeline::Interp::Linear,
        ));
        let frame = compile(&graph, Tick(5)).unwrap();
        let expected = native_swatch_color(Color {
            r: 0.5,
            g: 0.5,
            b: 0.5,
            a: 0.5,
        })
        .unwrap();
        assert!(
            matches!(frame.graph.nodes.get(frame.graph.output.unwrap().0 as usize).unwrap().op,IrOp::SolidColor {color:c} if c==expected)
        );
        let node = graph.nodes.get_mut(&id).unwrap();
        node.params = GraphNode::new(GraphOp::SolidColor).params;
        for value in [
            PropValue::Float(0.5),
            PropValue::Color(Color {
                r: 2.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            }),
            PropValue::Color(Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: f32::NAN,
            }),
        ] {
            graph
                .nodes
                .get_mut(&id)
                .unwrap()
                .params
                .base
                .0
                .set("params.color", value);
            assert!(compile(&graph, Tick::ZERO).is_err());
        }
        graph.nodes.get_mut(&id).unwrap().params = GraphNode::new(GraphOp::SolidColor).params;
        assert!(compile(&graph, Tick::ZERO).is_ok());
        graph.edges.push(timeline::GraphEdge {
            from: (graph.output, timeline::OutPort::PRIMARY),
            to: (id, InPort::PRIMARY),
        });
        assert!(compile(&graph, Tick::ZERO).is_err());
    }

    #[test]
    fn native_glow_stacks_graph_and_scope_zones() {
        let (mut project, sequence, _, _) = native_group_look_fixture();
        let glow =
            || ClipEffect::from_manifest(timeline::EffectId::new_static("stylize.glow")).unwrap();
        let asset = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&asset)
            .unwrap()
            .effects
            .push(glow());
        let seq = project.sequences.get_mut(&sequence).unwrap();
        let mut clip_effect = glow();
        clip_effect.zone = Some((Tick(10), Tick(20)));
        seq.video_tracks[0].clips[0].effects.push(clip_effect);
        seq.video_tracks[0].effects.push(glow());
        seq.master_effects.push(glow());
        for (tick, count) in [(5, 3), (15, 4), (20, 3)] {
            let frame = compile_native_preview(&project, sequence, 0, Tick(tick), Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            assert!(frame.graph.validate_working_color_domain().is_ok());
            assert_eq!(
                frame
                    .graph
                    .nodes
                    .iter()
                    .filter(|n| matches!(n.op, IrOp::NativeGlow { .. }))
                    .count(),
                count
            );
        }
        let (mut graph, source) = NodeGraph::new_clip_composition("Native glow");
        let node = GraphNode::new(GraphOp::Glow);
        let id = node.id;
        graph.nodes.insert(id, node);
        graph.edges = vec![
            timeline::GraphEdge {
                from: (source, timeline::OutPort::PRIMARY),
                to: (id, InPort::PRIMARY),
            },
            timeline::GraphEdge {
                from: (id, timeline::OutPort::PRIMARY),
                to: (graph.output, InPort::PRIMARY),
            },
        ];
        let gid = graph.id;
        project.graphs.insert(gid, graph);
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].composition =
            Some(gid);
        let frame = compile_native_preview(&project, sequence, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativeGlow { .. }))
                .count(),
            5
        );
        let fx = &mut project.sequences.get_mut(&sequence).unwrap().master_effects[0];
        fx.id = timeline::EffectId::EMPTY;
        assert!(
            compile_native_preview(&project, sequence, 0, Tick(15), Quality::FULL)
                .diagnostics
                .is_empty()
        );
        project.sequences.get_mut(&sequence).unwrap().master_effects[0].id =
            timeline::EffectId::new_static("unknown.glow");
        assert!(
            compile_native_preview(&project, sequence, 0, Tick(15), Quality::FULL)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error)
        );
        assert_ne!(
            content_hash(
                &IrOp::NativeGlow {
                    sigma: 0.0,
                    threshold: 0.0,
                    intensity: 0.0,
                    tint: [0.0; 3]
                },
                &[],
                &[]
            ),
            content_hash(&IrOp::NativeExposure { stops: 1.0 }, &[], &[])
        );
        let mut fx = glow();
        fx.params.base.set(
            "params.tint",
            PropValue::Color(Color {
                r: 0.0,
                g: 1.0,
                b: 0.0,
                a: 0.25,
            }),
        );
        let params =
            resolve_effect_params(EffectKind::Glow, &fx.params.base, &fx.params, Tick::ZERO);
        let IrOp::NativeGlow { tint, .. } = native_glow_op(&params).unwrap() else {
            panic!("expected native glow")
        };
        assert!((tint[0] - 0.3395).abs() < 0.001 && (tint[1] - 0.9164).abs() < 0.001);
        for (path, value) in [
            ("params.radius", PropValue::Float(501.0)),
            ("params.threshold", PropValue::Float(f64::NAN)),
            ("params.intensity", PropValue::Float(11.0)),
            (
                "params.tint",
                PropValue::Color(Color {
                    r: -1.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                }),
            ),
        ] {
            let mut fx = glow();
            fx.params.base.set(path, value);
            let params =
                resolve_effect_params(EffectKind::Glow, &fx.params.base, &fx.params, Tick::ZERO);
            assert!(native_glow_op(&params).is_err(), "{path}");
        }
        let hash = |sigma, threshold, intensity, tint| {
            content_hash(
                &IrOp::NativeGlow {
                    sigma,
                    threshold,
                    intensity,
                    tint,
                },
                &[],
                &[],
            )
        };
        let base = hash(1.0, 0.5, 2.0, [1.0; 3]);
        assert_ne!(base, hash(2.0, 0.5, 2.0, [1.0; 3]));
        assert_ne!(base, hash(1.0, 0.25, 2.0, [1.0; 3]));
        assert_ne!(base, hash(1.0, 0.5, 3.0, [1.0; 3]));
        assert_ne!(base, hash(1.0, 0.5, 2.0, [0.5; 3]));
    }

    #[test]
    fn native_chroma_key_stacks_graph_and_scope_zones() {
        let (mut project, sequence, _, _) = native_group_look_fixture();
        let key =
            || ClipEffect::from_manifest(timeline::EffectId::new_static("key.chroma")).unwrap();
        let asset = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&asset)
            .unwrap()
            .effects
            .push(key());
        let seq = project.sequences.get_mut(&sequence).unwrap();
        let mut clip_effect = key();
        clip_effect.zone = Some((Tick(10), Tick(20)));
        seq.video_tracks[0].clips[0].effects.push(clip_effect);
        seq.video_tracks[0].effects.push(key());
        seq.master_effects.push(key());
        for (tick, count) in [(5, 3), (15, 4), (20, 3)] {
            let frame = compile_native_preview(&project, sequence, 0, Tick(tick), Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            assert!(frame.graph.validate_working_color_domain().is_ok());
            assert_eq!(
                frame
                    .graph
                    .nodes
                    .iter()
                    .filter(|n| matches!(n.op, IrOp::NativeChromaKey { .. }))
                    .count(),
                count
            );
        }
        let (mut graph, source) = NodeGraph::new_clip_composition("Native chroma key");
        let node = GraphNode::new(GraphOp::ChromaKey);
        let id = node.id;
        graph.nodes.insert(id, node);
        graph.edges = vec![
            timeline::GraphEdge {
                from: (source, timeline::OutPort::PRIMARY),
                to: (id, InPort::PRIMARY),
            },
            timeline::GraphEdge {
                from: (id, timeline::OutPort::PRIMARY),
                to: (graph.output, InPort::PRIMARY),
            },
        ];
        let gid = graph.id;
        project.graphs.insert(gid, graph);
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].composition =
            Some(gid);
        let frame = compile_native_preview(&project, sequence, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativeChromaKey { .. }))
                .count(),
            5
        );
        let fx = &mut project.sequences.get_mut(&sequence).unwrap().master_effects[0];
        fx.id = timeline::EffectId::EMPTY;
        assert!(
            compile_native_preview(&project, sequence, 0, Tick(15), Quality::FULL)
                .diagnostics
                .is_empty()
        );
        project.sequences.get_mut(&sequence).unwrap().master_effects[0].id =
            timeline::EffectId::new_static("unknown.chroma");
        assert!(
            compile_native_preview(&project, sequence, 0, Tick(15), Quality::FULL)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error)
        );
        assert_ne!(
            content_hash(
                &IrOp::NativeChromaKey {
                    key: [0.0; 3],
                    tolerance: 0.0,
                    softness: 0.0,
                    spill: 0.0
                },
                &[],
                &[]
            ),
            content_hash(&IrOp::NativeExposure { stops: 1.0 }, &[], &[])
        );
        let hash = |key, tolerance, softness, spill| {
            content_hash(
                &IrOp::NativeChromaKey {
                    key,
                    tolerance,
                    softness,
                    spill,
                },
                &[],
                &[],
            )
        };
        let base = hash([0.0, 1.0, 0.0], 0.1, 0.2, 0.3);
        assert_ne!(base, hash([1.0, 0.0, 0.0], 0.1, 0.2, 0.3));
        assert_ne!(base, hash([0.0, 1.0, 0.0], 0.2, 0.2, 0.3));
        assert_ne!(base, hash([0.0, 1.0, 0.0], 0.1, 0.4, 0.3));
        assert_ne!(base, hash([0.0, 1.0, 0.0], 0.1, 0.2, 0.5));
        let mut fx = key();
        fx.params.base.set(
            "params.key_color",
            PropValue::Color(Color {
                r: 0.0,
                g: 1.0,
                b: 0.0,
                a: 0.25,
            }),
        );
        let params = resolve_effect_params(
            EffectKind::ChromaKey,
            &fx.params.base,
            &fx.params,
            Tick::ZERO,
        );
        let IrOp::NativeChromaKey { key: converted, .. } = native_chroma_key_op(&params).unwrap()
        else {
            panic!("expected native chroma key")
        };
        assert!(
            (converted[0] - 0.3395).abs() < 0.001
                && (converted[1] - 0.9164).abs() < 0.001
                && (converted[2] - 0.1096).abs() < 0.001,
            "{converted:?}"
        );
        for (path, value) in [
            ("params.tolerance", PropValue::Float(f64::NAN)),
            ("params.edge_softness", PropValue::Float(-0.1)),
            ("params.spill_suppress", PropValue::Float(1.1)),
            (
                "params.key_color",
                PropValue::Color(Color {
                    r: 2.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                }),
            ),
            ("params.key_color", PropValue::Bool(true)),
        ] {
            let mut fx = key();
            fx.params.base.set(path, value);
            let params = resolve_effect_params(
                EffectKind::ChromaKey,
                &fx.params.base,
                &fx.params,
                Tick::ZERO,
            );
            assert!(native_chroma_key_op(&params).is_err(), "{path}");
        }
    }

    #[test]
    fn native_luma_key_stacks_graph_zones_and_invalid_parameters() {
        let (mut project, sequence, _, _) = native_group_look_fixture();
        let key = || ClipEffect::from_manifest(timeline::EffectId::new_static("key.luma")).unwrap();
        let asset = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&asset)
            .unwrap()
            .effects
            .push(key());
        let seq = project.sequences.get_mut(&sequence).unwrap();
        let mut clip_effect = key();
        clip_effect.zone = Some((Tick(10), Tick(20)));
        seq.video_tracks[0].clips[0].effects.push(clip_effect);
        seq.video_tracks[0].effects.push(key());
        seq.master_effects.push(key());
        for (tick, count) in [(5, 3), (15, 4), (20, 3)] {
            let frame = compile_native_preview(&project, sequence, 0, Tick(tick), Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            assert!(frame.graph.validate_working_color_domain().is_ok());
            assert_eq!(
                frame
                    .graph
                    .nodes
                    .iter()
                    .filter(|n| matches!(n.op, IrOp::NativeLumaKey { .. }))
                    .count(),
                count
            );
        }
        let (mut graph, source) = NodeGraph::new_clip_composition("Native luma key");
        let node = GraphNode::new(GraphOp::LumaKey);
        let id = node.id;
        graph.nodes.insert(id, node);
        graph.edges = vec![
            timeline::GraphEdge {
                from: (source, timeline::OutPort::PRIMARY),
                to: (id, InPort::PRIMARY),
            },
            timeline::GraphEdge {
                from: (id, timeline::OutPort::PRIMARY),
                to: (graph.output, InPort::PRIMARY),
            },
        ];
        let gid = graph.id;
        project.graphs.insert(gid, graph);
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].composition =
            Some(gid);
        let frame = compile_native_preview(&project, sequence, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativeLumaKey { .. }))
                .count(),
            5
        );
        let fx = &mut project.sequences.get_mut(&sequence).unwrap().master_effects[0];
        fx.id = timeline::EffectId::EMPTY;
        assert!(
            compile_native_preview(&project, sequence, 0, Tick(15), Quality::FULL)
                .diagnostics
                .is_empty()
        );
        project.sequences.get_mut(&sequence).unwrap().master_effects[0].id =
            timeline::EffectId::new_static("unknown.luma");
        assert!(
            compile_native_preview(&project, sequence, 0, Tick(15), Quality::FULL)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error)
        );
        assert_ne!(
            content_hash(
                &IrOp::NativeLumaKey {
                    threshold: 0.0,
                    softness: 0.0,
                    invert: false
                },
                &[],
                &[]
            ),
            content_hash(&IrOp::NativeExposure { stops: 1.0 }, &[], &[])
        );
        project.sequences.get_mut(&sequence).unwrap().master_effects[0].id =
            timeline::EffectId::new_static("key.luma");
        for (path, value) in [
            ("params.threshold", PropValue::Float(f64::NAN)),
            ("params.threshold", PropValue::Float(-0.1)),
            ("params.softness", PropValue::Float(1.1)),
            ("params.invert", PropValue::Float(1.0)),
        ] {
            let fx = &mut project.sequences.get_mut(&sequence).unwrap().master_effects[0];
            *fx = key();
            fx.params.base.set(PropPath::new(path), value);
            let frame = compile_native_preview(&project, sequence, 0, Tick(15), Quality::FULL);
            assert!(
                frame
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error),
                "{path}: {:?}",
                frame.diagnostics
            );
        }
        let hash = |threshold, softness, invert| {
            content_hash(
                &IrOp::NativeLumaKey {
                    threshold,
                    softness,
                    invert,
                },
                &[],
                &[],
            )
        };
        assert_ne!(hash(0.2, 0.3, false), hash(0.4, 0.3, false));
        assert_ne!(hash(0.2, 0.3, false), hash(0.2, 0.5, false));
        assert_ne!(hash(0.2, 0.3, false), hash(0.2, 0.3, true));
    }

    #[test]
    fn native_outline_catalog_scopes_keys_color_hash_and_guards() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let fx = || {
            timeline::ClipEffect::from_manifest(timeline::EffectId::new_static("util.outline"))
                .unwrap()
        };
        let aid = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&aid)
            .unwrap()
            .effects
            .push(fx());
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].effects.push(fx());
        sequence.video_tracks[0].effects.push(fx());
        sequence.master_effects.push(fx());
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativeOutline { .. }))
                .count(),
            4
        );
        for (path, a, z) in [
            ("params.thickness", 0.0, 7.5),
            ("params.r", 0.0, 1.0),
            ("params.g", 0.0, 1.0),
            ("params.b", 0.0, 1.0),
            ("params.opacity", 0.0, 1.0),
        ] {
            let mut keyed = project.clone();
            keyed.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .tracks
                .push(timeline::PropertyTrack {
                    property: PropPath::new(path),
                    keyframes: vec![
                        timeline::Keyframe::new(
                            Tick(0),
                            PropValue::Float(a),
                            timeline::Interp::Linear,
                        ),
                        timeline::Keyframe::new(
                            Tick(30),
                            PropValue::Float(z),
                            timeline::Interp::Linear,
                        ),
                    ],
                    orphaned: false,
                });
            let frame = compile_native_preview(&keyed, seq, 0, Tick(15), Quality::FULL);
            assert!(frame.diagnostics.is_empty());
            let (thickness, color, opacity) = frame
                .graph
                .nodes
                .iter()
                .rev()
                .find_map(|n| match n.op {
                    IrOp::NativeOutline {
                        thickness,
                        color,
                        opacity,
                        ..
                    } => Some((thickness, color, opacity)),
                    _ => None,
                })
                .unwrap();
            let mid = ((a + z) / 2.0) as f32;
            match path {
                "params.thickness" => assert_eq!(thickness, mid),
                "params.opacity" => assert_eq!(opacity, mid),
                _ => {
                    let mut rgb = [1.0; 3];
                    rgb[match path {
                        "params.r" => 0,
                        "params.g" => 1,
                        _ => 2,
                    }] = mid as f64;
                    assert_eq!(
                        color,
                        crate::color::native::encoded_srgb_to_linear_ap1_display(rgb)
                            .map(|v| v as f32)
                    );
                }
            }
        }
        for (path, v) in [
            ("params.thickness", 64.01),
            ("params.thickness", -0.01),
            ("params.r", 1.01),
            ("params.g", -0.01),
            ("params.b", f64::NAN),
            ("params.opacity", f64::INFINITY),
        ] {
            let mut bad = project.clone();
            bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .base
                .set(path, PropValue::Float(v));
            assert!(
                compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
            );
            bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
            assert!(
                compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .is_empty()
            );
        }
        let base = content_hash(
            &IrOp::NativeOutline {
                thickness: 2.0,
                color: [1.0; 3],
                opacity: 1.0,
                pixel_scale: [1.0; 2],
            },
            &[],
            &[],
        );
        for (thickness, color, opacity, pixel_scale) in [
            (3.0, [1.0; 3], 1.0, [1.0; 2]),
            (2.0, [0.5, 1.0, 1.0], 1.0, [1.0; 2]),
            (2.0, [1.0; 3], 0.5, [1.0; 2]),
            (2.0, [1.0; 3], 1.0, [0.5, 1.0]),
        ] {
            assert_ne!(
                base,
                content_hash(
                    &IrOp::NativeOutline {
                        thickness,
                        color,
                        opacity,
                        pixel_scale
                    },
                    &[],
                    &[]
                )
            );
        }
    }

    #[test]
    fn native_linear_hsl_catalog_scopes_keys_hash_and_guards() {
        for vibrance in [false, true] {
            let (mut project, seq, _, _) = native_group_look_fixture();
            let id = if vibrance {
                "color.vibrance"
            } else {
                "color.hue_saturation"
            };
            let fx = || timeline::ClipEffect::from_manifest(timeline::EffectId::new(id)).unwrap();
            let aid = *project.media.assets.keys().next().unwrap();
            project
                .media
                .assets
                .get_mut(&aid)
                .unwrap()
                .effects
                .push(fx());
            let sequence = project.sequences.get_mut(&seq).unwrap();
            sequence.video_tracks[0].clips[0].effects.push(fx());
            sequence.video_tracks[0].effects.push(fx());
            sequence.master_effects.push(fx());
            let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            assert_eq!(
                frame
                    .graph
                    .nodes
                    .iter()
                    .filter(|n| matches!(n.op, IrOp::NativeLinearHsl { .. }))
                    .count(),
                4
            );
            let paths = if vibrance {
                vec![("params.amount", -1.0, 0.5)]
            } else {
                vec![
                    ("params.hue", -120.0, 60.0),
                    ("params.saturation", -0.5, 1.0),
                    ("params.lightness", -0.5, 1.0),
                ]
            };
            for (index, (path, a, z)) in paths.iter().enumerate() {
                let mut keyed = project.clone();
                keyed.sequences.get_mut(&seq).unwrap().master_effects[0]
                    .params
                    .tracks
                    .push(timeline::PropertyTrack {
                        property: PropPath::new(*path),
                        keyframes: vec![
                            timeline::Keyframe::new(
                                Tick(0),
                                PropValue::Float(*a),
                                timeline::Interp::Linear,
                            ),
                            timeline::Keyframe::new(
                                Tick(30),
                                PropValue::Float(*z),
                                timeline::Interp::Linear,
                            ),
                        ],
                        orphaned: false,
                    });
                let frame = compile_native_preview(&keyed, seq, 0, Tick(15), Quality::FULL);
                assert!(frame.diagnostics.is_empty());
                let controls = frame
                    .graph
                    .nodes
                    .iter()
                    .rev()
                    .find_map(|n| match n.op {
                        IrOp::NativeLinearHsl { controls, .. } => Some(controls),
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(controls[index], ((a + z) / 2.0) as f32);
                for v in [
                    if *path == "params.hue" { 180.01 } else { 1.01 },
                    f64::NAN,
                    f64::INFINITY,
                ] {
                    let mut bad = project.clone();
                    bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                        .params
                        .base
                        .set(*path, PropValue::Float(v));
                    assert!(
                        compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                            .diagnostics
                            .iter()
                            .any(|d| d.severity == DiagSeverity::Error)
                    );
                    bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
                    assert!(
                        compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                            .diagnostics
                            .is_empty()
                    );
                }
            }
        }
        let base = content_hash(
            &IrOp::NativeLinearHsl {
                vibrance: false,
                controls: [0.0; 3],
            },
            &[],
            &[],
        );
        for (vibrance, controls) in [
            (true, [0.0; 3]),
            (false, [1.0, 0.0, 0.0]),
            (false, [0.0, 0.1, 0.0]),
            (false, [0.0, 0.0, 0.1]),
        ] {
            assert_ne!(
                base,
                content_hash(&IrOp::NativeLinearHsl { vibrance, controls }, &[], &[])
            );
        }
    }

    #[test]
    fn native_alpha_view_scopes_keys_rounding_hash_and_guards() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let fx = || {
            timeline::ClipEffect::from_manifest(timeline::EffectId::new_static("util.alpha_view"))
                .unwrap()
        };
        let aid = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&aid)
            .unwrap()
            .effects
            .push(fx());
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].effects.push(fx());
        sequence.video_tracks[0].effects.push(fx());
        sequence.master_effects.push(fx());
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativeAlphaView { .. }))
                .count(),
            4
        );
        for (value, mode) in [
            (0.0, 0),
            (0.4999999701976776, 0),
            (0.4999999999, 1),
            (0.5, 1),
            (1.4999998807907104, 1),
            (1.5, 2),
            (2.0, 2),
        ] {
            let mut p = project.clone();
            p.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .base
                .set("params.mode", PropValue::Float(value));
            let frame = compile_native_preview(&p, seq, 0, Tick(15), Quality::FULL);
            assert!(frame.diagnostics.is_empty());
            assert_eq!(
                frame.graph.nodes.iter().rev().find_map(|n| match n.op {
                    IrOp::NativeAlphaView { mode } => Some(mode),
                    _ => None,
                }),
                Some(mode)
            );
        }
        let mut keyed = project.clone();
        keyed.sequences.get_mut(&seq).unwrap().master_effects[0]
            .params
            .tracks
            .push(timeline::PropertyTrack {
                property: PropPath::new("params.mode"),
                keyframes: vec![
                    timeline::Keyframe::new(
                        Tick(0),
                        PropValue::Float(0.0),
                        timeline::Interp::Linear,
                    ),
                    timeline::Keyframe::new(
                        Tick(30),
                        PropValue::Float(2.0),
                        timeline::Interp::Linear,
                    ),
                ],
                orphaned: false,
            });
        let frame = compile_native_preview(&keyed, seq, 0, Tick(15), Quality::FULL);
        assert_eq!(
            frame.graph.nodes.iter().rev().find_map(|n| match n.op {
                IrOp::NativeAlphaView { mode } => Some(mode),
                _ => None,
            }),
            Some(1)
        );
        for value in [-0.01, 2.01, f64::NAN, f64::INFINITY] {
            let mut bad = project.clone();
            bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .base
                .set("params.mode", PropValue::Float(value));
            assert!(
                compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
            );
            bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
            assert!(
                compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .is_empty()
            );
        }
        let hashes: Vec<_> = (0..=2)
            .map(|mode| content_hash(&IrOp::NativeAlphaView { mode }, &[], &[]))
            .collect();
        assert_ne!(hashes[0], hashes[1]);
        assert_ne!(hashes[1], hashes[2]);
        assert_ne!(hashes[0], hashes[2]);
    }

    #[test]
    fn native_levels_scopes_keys_hash_and_guards() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let fx = || {
            timeline::ClipEffect::from_manifest(timeline::EffectId::new_static("color.levels"))
                .unwrap()
        };
        let aid = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&aid)
            .unwrap()
            .effects
            .push(fx());
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].effects.push(fx());
        sequence.video_tracks[0].effects.push(fx());
        sequence.master_effects.push(fx());
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativeTransferLevels { .. }))
                .count(),
            4
        );
        for path in [
            "params.in_black",
            "params.in_white",
            "params.gamma",
            "params.out_black",
            "params.out_white",
        ] {
            let (lo, hi) = if path == "params.gamma" {
                (0.01, 9.99)
            } else {
                (0.0, 1.0)
            };
            let mut keyed = project.clone();
            keyed.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .tracks
                .push(timeline::PropertyTrack {
                    property: PropPath::new(path),
                    keyframes: vec![
                        timeline::Keyframe::new(
                            Tick(0),
                            PropValue::Float(lo),
                            timeline::Interp::Linear,
                        ),
                        timeline::Keyframe::new(
                            Tick(30),
                            PropValue::Float(hi),
                            timeline::Interp::Linear,
                        ),
                    ],
                    orphaned: false,
                });
            let frame = compile_native_preview(&keyed, seq, 0, Tick(15), Quality::FULL);
            assert!(frame.diagnostics.is_empty());
            let mut midpoint = project.clone();
            midpoint.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .base
                .set(path, PropValue::Float((lo + hi) / 2.0));
            let expected = compile_native_preview(&midpoint, seq, 0, Tick(15), Quality::FULL);
            let actual: Vec<_> = frame
                .graph
                .nodes
                .iter()
                .filter_map(|n| match n.op {
                    IrOp::NativeTransferLevels {
                        black_white,
                        gamma,
                        output,
                    } => Some((black_white, gamma, output)),
                    _ => None,
                })
                .collect();
            let expected: Vec<_> = expected
                .graph
                .nodes
                .iter()
                .filter_map(|n| match n.op {
                    IrOp::NativeTransferLevels {
                        black_white,
                        gamma,
                        output,
                    } => Some((black_white, gamma, output)),
                    _ => None,
                })
                .collect();
            assert_eq!(actual, expected);
            for value in [lo - 0.01, hi + 0.01, f64::NAN, f64::INFINITY] {
                let mut bad = project.clone();
                bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                    .params
                    .base
                    .set(path, PropValue::Float(value));
                assert!(
                    compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                        .diagnostics
                        .iter()
                        .any(|d| d.severity == DiagSeverity::Error)
                );
                bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
                assert!(
                    compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                        .diagnostics
                        .is_empty()
                );
            }
        }
        let base = content_hash(
            &IrOp::NativeTransferLevels {
                black_white: [0.0, 1.0],
                gamma: 1.0,
                output: [0.0, 1.0],
            },
            &[],
            &[],
        );
        for i in 0..5 {
            let mut params = [0.0, 1.0, 1.0, 0.0, 1.0];
            params[i] = 0.1;
            assert_ne!(
                base,
                content_hash(
                    &IrOp::NativeTransferLevels {
                        black_white: [params[0], params[1]],
                        gamma: params[2],
                        output: [params[3], params[4]]
                    },
                    &[],
                    &[]
                )
            );
        }
    }

    #[test]
    fn native_catalog_curves_scopes_keys_hash_and_guards() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let fx = || {
            timeline::ClipEffect::from_manifest(timeline::EffectId::new_static("color.curves"))
                .unwrap()
        };
        let aid = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&aid)
            .unwrap()
            .effects
            .push(fx());
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].effects.push(fx());
        sequence.video_tracks[0].effects.push(fx());
        sequence.master_effects.push(fx());
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativeTransferCurves { .. }))
                .count(),
            4
        );
        for path in [
            "params.contrast",
            "params.p0x",
            "params.p0y",
            "params.p1x",
            "params.p1y",
            "params.p2x",
            "params.p2y",
            "params.p3x",
            "params.p3y",
            "params.p4x",
            "params.p4y",
        ] {
            let lo = if path == "params.contrast" { -1.0 } else { 0.0 };
            let mut keyed = project.clone();
            keyed.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .tracks
                .push(timeline::PropertyTrack {
                    property: PropPath::new(path),
                    keyframes: vec![
                        timeline::Keyframe::new(
                            Tick(0),
                            PropValue::Float(lo),
                            timeline::Interp::Linear,
                        ),
                        timeline::Keyframe::new(
                            Tick(30),
                            PropValue::Float(1.0),
                            timeline::Interp::Linear,
                        ),
                    ],
                    orphaned: false,
                });
            let frame = compile_native_preview(&keyed, seq, 0, Tick(15), Quality::FULL);
            assert!(frame.diagnostics.is_empty());
            let mut midpoint = project.clone();
            midpoint.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .base
                .set(path, PropValue::Float((lo + 1.0) / 2.0));
            let expected = compile_native_preview(&midpoint, seq, 0, Tick(15), Quality::FULL);
            let actual: Vec<_> = frame
                .graph
                .nodes
                .iter()
                .filter_map(|n| match n.op {
                    IrOp::NativeTransferCurves { knots, contrast } => Some((knots, contrast)),
                    _ => None,
                })
                .collect();
            let expected: Vec<_> = expected
                .graph
                .nodes
                .iter()
                .filter_map(|n| match n.op {
                    IrOp::NativeTransferCurves { knots, contrast } => Some((knots, contrast)),
                    _ => None,
                })
                .collect();
            assert_eq!(actual, expected);
            for value in [lo - 0.01, 1.01, f64::NAN, f64::INFINITY] {
                let mut bad = project.clone();
                bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                    .params
                    .base
                    .set(path, PropValue::Float(value));
                assert!(
                    compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                        .diagnostics
                        .iter()
                        .any(|d| d.severity == DiagSeverity::Error)
                );
                bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
                assert!(
                    compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                        .diagnostics
                        .is_empty()
                );
            }
        }
        let knots = std::array::from_fn(|i| [i as f32 * 0.25; 2]);
        let base = content_hash(
            &IrOp::NativeTransferCurves {
                knots,
                contrast: 0.0,
            },
            &[],
            &[],
        );
        for i in 0..5 {
            for j in 0..2 {
                let mut changed = knots;
                changed[i][j] = 0.1;
                assert_ne!(
                    base,
                    content_hash(
                        &IrOp::NativeTransferCurves {
                            knots: changed,
                            contrast: 0.0
                        },
                        &[],
                        &[]
                    )
                );
            }
        }
        assert_ne!(
            base,
            content_hash(
                &IrOp::NativeTransferCurves {
                    knots,
                    contrast: 0.2
                },
                &[],
                &[]
            )
        );
    }

    #[test]
    fn native_color_balance_catalog_scopes_keys_switch_hash_and_guards() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let fx = || {
            timeline::ClipEffect::from_manifest(timeline::EffectId::new_static(
                "color.color_balance",
            ))
            .unwrap()
        };
        let aid = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&aid)
            .unwrap()
            .effects
            .push(fx());
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].effects.push(fx());
        sequence.video_tracks[0].effects.push(fx());
        sequence.master_effects.push(fx());
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativeColorBalance { .. }))
                .count(),
            4
        );
        for path in [
            "params.shadows_r",
            "params.shadows_g",
            "params.shadows_b",
            "params.midtones_r",
            "params.midtones_g",
            "params.midtones_b",
            "params.highlights_r",
            "params.highlights_g",
            "params.highlights_b",
            "params.preserve_luminosity",
        ] {
            let lo = if path == "params.preserve_luminosity" {
                0.0
            } else {
                -1.0
            };
            let mut keyed = project.clone();
            keyed.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .tracks
                .push(timeline::PropertyTrack {
                    property: PropPath::new(path),
                    keyframes: vec![
                        timeline::Keyframe::new(
                            Tick(0),
                            PropValue::Float(lo),
                            timeline::Interp::Linear,
                        ),
                        timeline::Keyframe::new(
                            Tick(30),
                            PropValue::Float(1.0),
                            timeline::Interp::Linear,
                        ),
                    ],
                    orphaned: false,
                });
            let frame = compile_native_preview(&keyed, seq, 0, Tick(15), Quality::FULL);
            assert!(frame.diagnostics.is_empty());
            let mut midpoint = project.clone();
            midpoint.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .base
                .set(path, PropValue::Float((lo + 1.0) / 2.0));
            let expected = compile_native_preview(&midpoint, seq, 0, Tick(15), Quality::FULL);
            let actual: Vec<_> = frame
                .graph
                .nodes
                .iter()
                .filter_map(|n| match n.op {
                    IrOp::NativeColorBalance { shifts, preserve } => Some((shifts, preserve)),
                    _ => None,
                })
                .collect();
            let expected: Vec<_> = expected
                .graph
                .nodes
                .iter()
                .filter_map(|n| match n.op {
                    IrOp::NativeColorBalance { shifts, preserve } => Some((shifts, preserve)),
                    _ => None,
                })
                .collect();
            assert_eq!(actual, expected);
            for value in [lo - 0.01, 1.01, f64::NAN, f64::INFINITY] {
                let mut bad = project.clone();
                bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                    .params
                    .base
                    .set(path, PropValue::Float(value));
                assert!(
                    compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                        .diagnostics
                        .iter()
                        .any(|d| d.severity == DiagSeverity::Error)
                );
                bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
                assert!(
                    compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                        .diagnostics
                        .is_empty()
                );
            }
        }
        for (value, preserve) in [
            (0.0, false),
            (0.5, false),
            (0.5000000596046448, true),
            (1.0, true),
        ] {
            let mut p = project.clone();
            p.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .base
                .set("params.preserve_luminosity", PropValue::Float(value));
            let frame = compile_native_preview(&p, seq, 0, Tick(15), Quality::FULL);
            assert_eq!(
                frame.graph.nodes.iter().rev().find_map(|n| match n.op {
                    IrOp::NativeColorBalance { preserve, .. } => Some(preserve),
                    _ => None,
                }),
                Some(preserve)
            );
        }
        let base = content_hash(
            &IrOp::NativeColorBalance {
                shifts: [[0.0; 3]; 3],
                preserve: true,
            },
            &[],
            &[],
        );
        for i in 0..3 {
            for j in 0..3 {
                let mut shifts = [[0.0; 3]; 3];
                shifts[i][j] = 0.1;
                assert_ne!(
                    base,
                    content_hash(
                        &IrOp::NativeColorBalance {
                            shifts,
                            preserve: true
                        },
                        &[],
                        &[]
                    )
                );
            }
        }
        assert_ne!(
            base,
            content_hash(
                &IrOp::NativeColorBalance {
                    shifts: [[0.0; 3]; 3],
                    preserve: false
                },
                &[],
                &[]
            )
        );
    }

    #[test]
    fn native_photo_filter_catalog_scopes_keys_switch_hash_and_guards() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let fx = || {
            timeline::ClipEffect::from_manifest(timeline::EffectId::new_static(
                "color.photo_filter",
            ))
            .unwrap()
        };
        let aid = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&aid)
            .unwrap()
            .effects
            .push(fx());
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].effects.push(fx());
        sequence.video_tracks[0].effects.push(fx());
        sequence.master_effects.push(fx());
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativePhotoFilter { .. }))
                .count(),
            4
        );
        for path in [
            "params.r",
            "params.g",
            "params.b",
            "params.density",
            "params.preserve_luminosity",
        ] {
            let mut keyed = project.clone();
            keyed.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .tracks
                .push(timeline::PropertyTrack {
                    property: PropPath::new(path),
                    keyframes: vec![
                        timeline::Keyframe::new(
                            Tick(0),
                            PropValue::Float(0.0),
                            timeline::Interp::Linear,
                        ),
                        timeline::Keyframe::new(
                            Tick(30),
                            PropValue::Float(1.0),
                            timeline::Interp::Linear,
                        ),
                    ],
                    orphaned: false,
                });
            let frame = compile_native_preview(&keyed, seq, 0, Tick(15), Quality::FULL);
            assert!(frame.diagnostics.is_empty());
            let mut midpoint = project.clone();
            midpoint.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .base
                .set(path, PropValue::Float(0.5));
            let expected = compile_native_preview(&midpoint, seq, 0, Tick(15), Quality::FULL);
            let actual: Vec<_> = frame
                .graph
                .nodes
                .iter()
                .filter_map(|n| match n.op {
                    IrOp::NativePhotoFilter {
                        color,
                        density,
                        preserve,
                    } => Some((color, density, preserve)),
                    _ => None,
                })
                .collect();
            let expected: Vec<_> = expected
                .graph
                .nodes
                .iter()
                .filter_map(|n| match n.op {
                    IrOp::NativePhotoFilter {
                        color,
                        density,
                        preserve,
                    } => Some((color, density, preserve)),
                    _ => None,
                })
                .collect();
            assert_eq!(actual, expected);
            for value in [-0.01, 1.01, f64::NAN, f64::INFINITY] {
                let mut bad = project.clone();
                bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                    .params
                    .base
                    .set(path, PropValue::Float(value));
                assert!(
                    compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                        .diagnostics
                        .iter()
                        .any(|d| d.severity == DiagSeverity::Error)
                );
                bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
                assert!(
                    compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                        .diagnostics
                        .is_empty()
                );
            }
        }
        for (value, preserve) in [
            (0.0, false),
            (0.5, false),
            (0.5000000596046448, true),
            (1.0, true),
        ] {
            let mut p = project.clone();
            p.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .base
                .set("params.preserve_luminosity", PropValue::Float(value));
            let frame = compile_native_preview(&p, seq, 0, Tick(15), Quality::FULL);
            assert_eq!(
                frame.graph.nodes.iter().rev().find_map(|n| match n.op {
                    IrOp::NativePhotoFilter { preserve, .. } => Some(preserve),
                    _ => None,
                }),
                Some(preserve)
            );
        }
        let base = content_hash(
            &IrOp::NativePhotoFilter {
                color: [1.0, 0.5, 0.2],
                density: 0.25,
                preserve: true,
            },
            &[],
            &[],
        );
        for op in [
            IrOp::NativePhotoFilter {
                color: [0.9, 0.5, 0.2],
                density: 0.25,
                preserve: true,
            },
            IrOp::NativePhotoFilter {
                color: [1.0, 0.4, 0.2],
                density: 0.25,
                preserve: true,
            },
            IrOp::NativePhotoFilter {
                color: [1.0, 0.5, 0.3],
                density: 0.25,
                preserve: true,
            },
            IrOp::NativePhotoFilter {
                color: [1.0, 0.5, 0.2],
                density: 0.5,
                preserve: true,
            },
            IrOp::NativePhotoFilter {
                color: [1.0, 0.5, 0.2],
                density: 0.25,
                preserve: false,
            },
        ] {
            assert_ne!(base, content_hash(&op, &[], &[]));
        }
    }

    #[test]
    fn native_transfer_matrix_catalog_scopes_keys_hash_and_guards() {
        for (id, paths, lo, hi) in [
            (
                "color.channel_mixer",
                vec![
                    "params.rr",
                    "params.rg",
                    "params.rb",
                    "params.gr",
                    "params.gg",
                    "params.gb",
                    "params.br",
                    "params.bg",
                    "params.bb",
                ],
                -2.0,
                2.0,
            ),
            (
                "color.black_and_white",
                vec!["params.wr", "params.wg", "params.wb"],
                0.0,
                1.0,
            ),
        ] {
            let (mut project, seq, _, _) = native_group_look_fixture();
            let fx =
                || timeline::ClipEffect::from_manifest(timeline::EffectId::new_static(id)).unwrap();
            let aid = *project.media.assets.keys().next().unwrap();
            project
                .media
                .assets
                .get_mut(&aid)
                .unwrap()
                .effects
                .push(fx());
            let sequence = project.sequences.get_mut(&seq).unwrap();
            sequence.video_tracks[0].clips[0].effects.push(fx());
            sequence.video_tracks[0].effects.push(fx());
            sequence.master_effects.push(fx());
            let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
            assert!(
                frame.diagnostics.is_empty(),
                "{id}: {:?}",
                frame.diagnostics
            );
            assert_eq!(
                frame
                    .graph
                    .nodes
                    .iter()
                    .filter(|n| matches!(n.op, IrOp::NativeTransferMatrix { .. }))
                    .count(),
                4
            );
            for path in paths {
                let mut keyed = project.clone();
                keyed.sequences.get_mut(&seq).unwrap().master_effects[0]
                    .params
                    .tracks
                    .push(timeline::PropertyTrack {
                        property: PropPath::new(path),
                        keyframes: vec![
                            timeline::Keyframe::new(
                                Tick(0),
                                PropValue::Float(lo),
                                timeline::Interp::Linear,
                            ),
                            timeline::Keyframe::new(
                                Tick(30),
                                PropValue::Float(hi),
                                timeline::Interp::Linear,
                            ),
                        ],
                        orphaned: false,
                    });
                let frame = compile_native_preview(&keyed, seq, 0, Tick(15), Quality::FULL);
                assert!(frame.diagnostics.is_empty());
                let mut midpoint = project.clone();
                midpoint.sequences.get_mut(&seq).unwrap().master_effects[0]
                    .params
                    .base
                    .set(path, PropValue::Float((lo + hi) / 2.0));
                let expected = compile_native_preview(&midpoint, seq, 0, Tick(15), Quality::FULL);
                let actual: Vec<_> = frame
                    .graph
                    .nodes
                    .iter()
                    .filter_map(|n| match n.op {
                        IrOp::NativeTransferMatrix { matrix } => Some(matrix),
                        _ => None,
                    })
                    .collect();
                let expected: Vec<_> = expected
                    .graph
                    .nodes
                    .iter()
                    .filter_map(|n| match n.op {
                        IrOp::NativeTransferMatrix { matrix } => Some(matrix),
                        _ => None,
                    })
                    .collect();
                assert_eq!(actual, expected);
                for value in [lo - 0.01, hi + 0.01, f64::NAN, f64::INFINITY] {
                    let mut bad = project.clone();
                    bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                        .params
                        .base
                        .set(path, PropValue::Float(value));
                    assert!(
                        compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                            .diagnostics
                            .iter()
                            .any(|d| d.severity == DiagSeverity::Error)
                    );
                    bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
                    assert!(
                        compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                            .diagnostics
                            .is_empty()
                    );
                }
            }
        }
        let a = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let base = content_hash(&IrOp::NativeTransferMatrix { matrix: a }, &[], &[]);
        for i in 0..3 {
            for j in 0..3 {
                let mut b = a;
                b[i][j] += 0.1;
                assert_ne!(
                    base,
                    content_hash(&IrOp::NativeTransferMatrix { matrix: b }, &[], &[])
                );
            }
        }
    }

    #[test]
    fn native_deflicker_requires_analysis_and_valid_clip_contract() {
        struct Gains;
        impl DeflickerGains for Gains {
            fn gain(&self, _: ClipId, _: Tick) -> Option<[f32; 3]> {
                Some([1.2, 0.8, 1.1])
            }
            fn band(&self, _: ClipId, _: Tick) -> Option<crate::graph::rolling_bands::BandModel> {
                Some(crate::graph::rolling_bands::BandModel {
                    cycles: 2,
                    amplitude: 0.2,
                    phase: 9.0,
                    confidence: 1.0,
                })
            }
        }
        let (mut project, seq, _, _) = native_group_look_fixture();
        let fx = || {
            timeline::ClipEffect::from_manifest(timeline::EffectId::new_static("color.deflicker"))
                .unwrap()
        };
        project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0]
            .effects
            .push(fx());
        let compile = |p: &TimelineProject, g: Option<&dyn DeflickerGains>, tick| {
            compile_native_sequence_impl(
                p,
                seq,
                0,
                tick,
                Quality::FULL,
                false,
                false,
                None,
                None,
                g,
                &[],
            )
        };
        let missing = compile(&project, None, Tick(15));
        assert!(missing
            .diagnostics
            .iter()
            .any(|d| d.code == Some(CompileCode::ColorPipelineUnavailable)));
        let measured = compile(&project, Some(&Gains), Tick(15));
        assert!(
            measured.diagnostics.is_empty(),
            "{:?}",
            measured.diagnostics
        );
        let op = measured
            .graph
            .nodes
            .iter()
            .find_map(|n| match n.op {
                IrOp::NativeDeflicker { gain, band } => Some((gain, band)),
                _ => None,
            })
            .unwrap();
        let shared = 0.2126 * 1.2f32.ln() + 0.7152 * 0.8f32.ln() + 0.0722 * 1.1f32.ln();
        assert!((op.0[0] - (shared * 0.85).exp()).abs() < 1e-6);
        assert_eq!(op.0[0], op.0[1]);
        assert!((op.1[1] - 0.17).abs() < 1e-6);
        assert!(op.1[2] >= 0.0 && op.1[2] < std::f32::consts::TAU);
        let mut animated = project.clone();
        for (path, end) in [("params.amount", 1.0), ("params.chroma_amount", 1.0)] {
            let mut lane = timeline::PropertyTrack::new(PropPath::new(path));
            lane.keyframes = vec![
                timeline::Keyframe::new(Tick(0), PropValue::Float(0.0), timeline::Interp::Linear),
                timeline::Keyframe::new(Tick(30), PropValue::Float(end), timeline::Interp::Linear),
            ];
            animated.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].effects[0]
                .params
                .tracks
                .push(lane);
        }
        let zero = compile(&animated, None, Tick(0));
        assert!(zero.diagnostics.is_empty());
        assert!(!zero
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeDeflicker { .. })));
        let keyed = compile(&animated, Some(&Gains), Tick(15));
        assert!(keyed.diagnostics.is_empty());
        let half = keyed
            .graph
            .nodes
            .iter()
            .find_map(|n| match n.op {
                IrOp::NativeDeflicker { gain, .. } => Some(gain),
                _ => None,
            })
            .unwrap();
        assert!((half[0] - ((shared + (1.2f32.ln() - shared) * 0.5) * 0.5).exp()).abs() < 1e-6);
        for path in ["params.window", "params.max_change"] {
            let mut bad = project.clone();
            let mut lane = timeline::PropertyTrack::new(PropPath::new(path));
            lane.keyframes.push(timeline::Keyframe::new(
                Tick(0),
                PropValue::Float(0.5),
                timeline::Interp::Linear,
            ));
            bad.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].effects[0]
                .params
                .tracks
                .push(lane);
            assert!(compile(&bad, Some(&Gains), Tick(15)).diagnostics.is_empty());
        }
        let mut duplicate = project.clone();
        duplicate.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0]
            .effects
            .push(fx());
        let stacked = compile(&duplicate, Some(&Gains), Tick(15));
        assert!(stacked.diagnostics.is_empty());
        assert_eq!(
            stacked
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativeDeflicker { .. }))
                .count(),
            2
        );
        for owner in 0..3 {
            let mut bad = project.clone();
            bad.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0]
                .effects
                .clear();
            match owner {
                0 => bad.sequences.get_mut(&seq).unwrap().video_tracks[0]
                    .effects
                    .push(fx()),
                1 => bad
                    .sequences
                    .get_mut(&seq)
                    .unwrap()
                    .master_effects
                    .push(fx()),
                _ => bad
                    .media
                    .assets
                    .values_mut()
                    .next()
                    .unwrap()
                    .effects
                    .push(fx()),
            }
            assert!(!compile(&bad, Some(&Gains), Tick(15)).diagnostics.is_empty());
        }
        let base = content_hash(
            &IrOp::NativeDeflicker {
                gain: [1.0; 3],
                band: [0.0; 3],
            },
            &[],
            &[],
        );
        for op in [
            IrOp::NativeDeflicker {
                gain: [1.1, 1.0, 1.0],
                band: [0.0; 3],
            },
            IrOp::NativeDeflicker {
                gain: [1.0; 3],
                band: [2.0, 0.2, 0.0],
            },
            IrOp::NativeUnpremultiply,
        ] {
            assert_ne!(base, content_hash(&op, &[], &[]));
        }
    }

    #[test]
    fn native_unpremultiply_catalog_scopes_enabled_and_hash() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let fx = || {
            timeline::ClipEffect::from_manifest(timeline::EffectId::new_static(
                "util.unpremultiply",
            ))
            .unwrap()
        };
        let aid = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&aid)
            .unwrap()
            .effects
            .push(fx());
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].effects.push(fx());
        sequence.video_tracks[0].effects.push(fx());
        sequence.master_effects.push(fx());
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativeUnpremultiply))
                .count(),
            4
        );
        project.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty());
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativeUnpremultiply))
                .count(),
            3
        );
        let h = content_hash(&IrOp::NativeUnpremultiply, &[], &[]);
        for mode in 0..3 {
            assert_ne!(h, content_hash(&IrOp::NativeAlphaView { mode }, &[], &[]));
        }
        assert_ne!(h, content_hash(&IrOp::NativeInvert, &[], &[]));
    }

    #[test]
    fn native_transfer_invert_catalog_scopes_enabled_and_hash() {
        use crate::graph::ir::NativeTransferArtisticKind::*;
        let (mut project, seq, _, _) = native_group_look_fixture();
        let fx = || {
            timeline::ClipEffect::from_manifest(timeline::EffectId::new_static(
                "color.invert_raster",
            ))
            .unwrap()
        };
        let aid = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&aid)
            .unwrap()
            .effects
            .push(fx());
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].effects.push(fx());
        sequence.video_tracks[0].effects.push(fx());
        sequence.master_effects.push(fx());
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(
                    n.op,
                    IrOp::NativeTransferArtistic {
                        kind: Invert,
                        parameter: 0.0
                    }
                ))
                .count(),
            4
        );
        project.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty());
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativeTransferArtistic { kind: Invert, .. }))
                .count(),
            3
        );
        let h = content_hash(
            &IrOp::NativeTransferArtistic {
                kind: Invert,
                parameter: 0.0,
            },
            &[],
            &[],
        );
        assert_ne!(
            h,
            content_hash(
                &IrOp::NativeTransferArtistic {
                    kind: Threshold,
                    parameter: 0.0
                },
                &[],
                &[]
            )
        );
        assert_ne!(h, content_hash(&IrOp::NativeInvert, &[], &[]));
        assert_ne!(
            h,
            content_hash(
                &IrOp::NativeTransferArtistic {
                    kind: Desaturate,
                    parameter: 0.0
                },
                &[],
                &[]
            )
        );
    }

    #[test]
    fn native_desaturate_catalog_scopes_enabled_and_hash() {
        use crate::graph::ir::NativeTransferArtisticKind::*;
        let (mut project, seq, _, _) = native_group_look_fixture();
        let fx = || {
            timeline::ClipEffect::from_manifest(timeline::EffectId::new_static("color.desaturate"))
                .unwrap()
        };
        let aid = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&aid)
            .unwrap()
            .effects
            .push(fx());
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].effects.push(fx());
        sequence.video_tracks[0].effects.push(fx());
        sequence.master_effects.push(fx());
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(
                    n.op,
                    IrOp::NativeTransferArtistic {
                        kind: Desaturate,
                        parameter: 0.0
                    }
                ))
                .count(),
            4
        );
        project.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty());
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(
                    n.op,
                    IrOp::NativeTransferArtistic {
                        kind: Desaturate,
                        ..
                    }
                ))
                .count(),
            3
        );
        let h = content_hash(
            &IrOp::NativeTransferArtistic {
                kind: Desaturate,
                parameter: 0.0,
            },
            &[],
            &[],
        );
        assert_ne!(
            h,
            content_hash(
                &IrOp::NativeTransferArtistic {
                    kind: Threshold,
                    parameter: 0.0
                },
                &[],
                &[]
            )
        );
    }

    #[test]
    fn native_transfer_artistic_catalog_scopes_keys_hash_and_guards() {
        use crate::graph::ir::NativeTransferArtisticKind::*;
        for (id, kind, path, values) in [
            (
                "color.posterize",
                Posterize,
                "params.levels",
                [2.0, 3.5, 8.0, 255.0],
            ),
            (
                "color.threshold",
                Threshold,
                "params.level",
                [0.0, 0.25, 0.5, 1.0],
            ),
        ] {
            let (mut project, seq, _, _) = native_group_look_fixture();
            let fx = |v| {
                let mut f = timeline::ClipEffect::from_manifest(timeline::EffectId::new_static(id))
                    .unwrap();
                f.params.base.set(path, PropValue::Float(v));
                f
            };
            let aid = *project.media.assets.keys().next().unwrap();
            project
                .media
                .assets
                .get_mut(&aid)
                .unwrap()
                .effects
                .push(fx(values[0]));
            let sequence = project.sequences.get_mut(&seq).unwrap();
            sequence.video_tracks[0].clips[0]
                .effects
                .push(fx(values[1]));
            sequence.video_tracks[0].effects.push(fx(values[2]));
            sequence.master_effects.push(fx(values[3]));
            let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
            assert!(
                frame.diagnostics.is_empty(),
                "{id}: {:?}",
                frame.diagnostics
            );
            let params: Vec<_> = frame
                .graph
                .nodes
                .iter()
                .filter_map(|n| match n.op {
                    IrOp::NativeTransferArtistic { kind: k, parameter } if k == kind => {
                        Some(parameter)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                params,
                values.map(|v| if kind == Posterize {
                    v.round() as f32
                } else {
                    v as f32
                })
            );
            let (a, z) = if kind == Posterize {
                (2.0, 8.0)
            } else {
                (0.0, 1.0)
            };
            project.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .tracks
                .push(timeline::PropertyTrack {
                    property: PropPath::new(path),
                    keyframes: vec![
                        timeline::Keyframe::new(
                            Tick(0),
                            PropValue::Float(a),
                            timeline::Interp::Linear,
                        ),
                        timeline::Keyframe::new(
                            Tick(30),
                            PropValue::Float(z),
                            timeline::Interp::Linear,
                        ),
                    ],
                    orphaned: false,
                });
            let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
            assert!(frame.diagnostics.is_empty());
            assert!(frame.graph.nodes.iter().any(|n|matches!(n.op,IrOp::NativeTransferArtistic{kind:k,parameter}if k==kind && parameter==((a+z)/2.0)as f32)));
            project.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .tracks
                .clear();
            for v in [
                if kind == Posterize { 1.99 } else { -0.01 },
                if kind == Posterize { 255.01 } else { 1.01 },
                f64::NAN,
                f64::INFINITY,
            ] {
                let mut bad = project.clone();
                bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                    .params
                    .base
                    .set(path, PropValue::Float(v));
                assert!(
                    compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                        .diagnostics
                        .iter()
                        .any(|d| d.severity == DiagSeverity::Error)
                );
                bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
                assert!(
                    compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                        .diagnostics
                        .is_empty()
                );
            }
        }
        let base = content_hash(
            &IrOp::NativeTransferArtistic {
                kind: Posterize,
                parameter: 4.0,
            },
            &[],
            &[],
        );
        assert_ne!(
            base,
            content_hash(
                &IrOp::NativeTransferArtistic {
                    kind: Posterize,
                    parameter: 5.0
                },
                &[],
                &[]
            )
        );
        assert_ne!(
            base,
            content_hash(
                &IrOp::NativeTransferArtistic {
                    kind: Threshold,
                    parameter: 4.0
                },
                &[],
                &[]
            )
        );
    }

    #[test]
    fn native_rank_catalog_scopes_keys_rounding_hash_and_guards() {
        use crate::graph::ir::NativeRankKind;
        for dust in [false, true] {
            let (mut project, seq, _, _) = native_group_look_fixture();
            let id = if dust {
                "repair.dust_and_scratches"
            } else {
                "filter.median"
            };
            let kind = if dust {
                NativeRankKind::DustScratches
            } else {
                NativeRankKind::Median
            };
            let fx = || timeline::ClipEffect::from_manifest(timeline::EffectId::new(id)).unwrap();
            let aid = *project.media.assets.keys().next().unwrap();
            project
                .media
                .assets
                .get_mut(&aid)
                .unwrap()
                .effects
                .push(fx());
            let sequence = project.sequences.get_mut(&seq).unwrap();
            sequence.video_tracks[0].clips[0].effects.push(fx());
            sequence.video_tracks[0].effects.push(fx());
            sequence.master_effects.push(fx());
            let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            assert_eq!(frame.graph.nodes.iter().filter(|n|matches!(n.op,IrOp::NativeRankFilter{kind:k,radius,threshold,..}if k==kind&&radius==if dust{1}else{0}&&threshold==if dust{16}else{0})).count(),4);
            let paths = if dust {
                vec![("params.radius", 1.0, 6.0), ("params.threshold", 1.0, 4.0)]
            } else {
                vec![("params.radius", 1.0, 6.0)]
            };
            for (path, a, z) in paths {
                let mut keyed = project.clone();
                keyed.sequences.get_mut(&seq).unwrap().master_effects[0]
                    .params
                    .tracks
                    .push(timeline::PropertyTrack {
                        property: PropPath::new(path),
                        keyframes: vec![
                            timeline::Keyframe::new(
                                Tick(0),
                                PropValue::Float(a),
                                timeline::Interp::Linear,
                            ),
                            timeline::Keyframe::new(
                                Tick(30),
                                PropValue::Float(z),
                                timeline::Interp::Linear,
                            ),
                        ],
                        orphaned: false,
                    });
                let frame = compile_native_preview(&keyed, seq, 0, Tick(15), Quality::FULL);
                assert!(frame.diagnostics.is_empty());
                let (radius, threshold) = frame
                    .graph
                    .nodes
                    .iter()
                    .rev()
                    .find_map(|n| match n.op {
                        IrOp::NativeRankFilter {
                            radius, threshold, ..
                        } => Some((radius, threshold)),
                        _ => None,
                    })
                    .unwrap();
                if path == "params.radius" {
                    assert_eq!(radius, 4);
                } else {
                    assert_eq!(threshold, 2);
                }
            }
            for (path, v) in [
                ("params.radius", -0.1),
                ("params.radius", if dust { 16.01 } else { 500.01 }),
                ("params.radius", f64::NAN),
                (
                    if dust {
                        "params.threshold"
                    } else {
                        "params.radius"
                    },
                    f64::INFINITY,
                ),
            ] {
                let mut bad = project.clone();
                bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                    .params
                    .base
                    .set(path, PropValue::Float(v));
                assert!(
                    compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                        .diagnostics
                        .iter()
                        .any(|d| d.severity == DiagSeverity::Error)
                );
                bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
                assert!(
                    compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                        .diagnostics
                        .is_empty()
                );
            }
        }
        let hash = |kind, radius, threshold, pixel_scale| {
            content_hash(
                &IrOp::NativeRankFilter {
                    kind,
                    radius,
                    threshold,
                    pixel_scale,
                },
                &[],
                &[],
            )
        };
        let base = hash(NativeRankKind::Median, 2, 0, [1.0; 2]);
        for other in [
            hash(NativeRankKind::DustScratches, 2, 0, [1.0; 2]),
            hash(NativeRankKind::Median, 3, 0, [1.0; 2]),
            hash(NativeRankKind::DustScratches, 2, 16, [1.0; 2]),
            hash(NativeRankKind::Median, 2, 0, [0.5, 1.0]),
            hash(NativeRankKind::Median, 2, 0, [1.0, 0.5]),
        ] {
            assert_ne!(base, other);
        }
    }

    #[test]
    fn native_lens_blur_catalog_scopes_fractional_keys_hash_and_guards() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let fx = || {
            timeline::ClipEffect::from_manifest(timeline::EffectId::new_static("blur.lens"))
                .unwrap()
        };
        let aid = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&aid)
            .unwrap()
            .effects
            .push(fx());
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].effects.push(fx());
        sequence.video_tracks[0].effects.push(fx());
        sequence.master_effects.push(fx());
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op,IrOp::NativeLensBlur{radius,..}if radius==0.0))
                .count(),
            4
        );
        let mut keyed = project.clone();
        keyed.sequences.get_mut(&seq).unwrap().master_effects[0]
            .params
            .tracks
            .push(timeline::PropertyTrack {
                property: PropPath::new("params.radius"),
                keyframes: vec![
                    timeline::Keyframe::new(
                        Tick(0),
                        PropValue::Float(1.0),
                        timeline::Interp::Linear,
                    ),
                    timeline::Keyframe::new(
                        Tick(30),
                        PropValue::Float(6.5),
                        timeline::Interp::Linear,
                    ),
                ],
                orphaned: false,
            });
        let frame = compile_native_preview(&keyed, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty());
        assert_eq!(
            frame.graph.nodes.iter().rev().find_map(|n| match n.op {
                IrOp::NativeLensBlur { radius, .. } => Some(radius),
                _ => None,
            }),
            Some(3.75)
        );
        for v in [-0.1, 500.01, f64::NAN, f64::INFINITY] {
            let mut bad = project.clone();
            bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .base
                .set("params.radius", PropValue::Float(v));
            assert!(
                compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
            );
            bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
            assert!(
                compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .is_empty()
            );
        }
        let hash = |radius, pixel_scale| {
            content_hash(
                &IrOp::NativeLensBlur {
                    radius,
                    pixel_scale,
                },
                &[],
                &[],
            )
        };
        let base = hash(2.0, [1.0; 2]);
        for other in [
            hash(2.25, [1.0; 2]),
            hash(2.0, [0.5, 1.0]),
            hash(2.0, [1.0, 0.5]),
        ] {
            assert_ne!(base, other);
        }
    }

    #[test]
    fn native_surface_blur_catalog_scopes_keys_rounding_hash_and_guards() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let fx = || {
            timeline::ClipEffect::from_manifest(timeline::EffectId::new_static("blur.surface"))
                .unwrap()
        };
        let aid = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&aid)
            .unwrap()
            .effects
            .push(fx());
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].effects.push(fx());
        sequence.video_tracks[0].effects.push(fx());
        sequence.master_effects.push(fx());
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(frame.graph.nodes.iter().filter(|n|matches!(n.op,IrOp::NativeSurfaceBlur{radius:2,threshold,..}if threshold==0.25)).count(),4);
        for (path, a, z) in [("params.radius", 1.0, 6.0), ("params.threshold", 0.2, 0.8)] {
            let mut keyed = project.clone();
            keyed.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .tracks
                .push(timeline::PropertyTrack {
                    property: PropPath::new(path),
                    keyframes: vec![
                        timeline::Keyframe::new(
                            Tick(0),
                            PropValue::Float(a),
                            timeline::Interp::Linear,
                        ),
                        timeline::Keyframe::new(
                            Tick(30),
                            PropValue::Float(z),
                            timeline::Interp::Linear,
                        ),
                    ],
                    orphaned: false,
                });
            let frame = compile_native_preview(&keyed, seq, 0, Tick(15), Quality::FULL);
            assert!(frame.diagnostics.is_empty());
            let (radius, threshold) = frame
                .graph
                .nodes
                .iter()
                .rev()
                .find_map(|n| match n.op {
                    IrOp::NativeSurfaceBlur {
                        radius, threshold, ..
                    } => Some((radius, threshold)),
                    _ => None,
                })
                .unwrap();
            if path == "params.radius" {
                assert_eq!(radius, 4);
            } else {
                assert_eq!(threshold, 0.5);
            }
        }
        for (v, expected) in [(0.499999970, 0), (0.4999999999, 1)] {
            let mut rounded = project.clone();
            rounded.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .base
                .set("params.radius", PropValue::Float(v));
            let frame = compile_native_preview(&rounded, seq, 0, Tick(15), Quality::FULL);
            assert!(frame.diagnostics.is_empty());
            assert_eq!(
                frame.graph.nodes.iter().rev().find_map(|n| match n.op {
                    IrOp::NativeSurfaceBlur { radius, .. } => Some(radius),
                    _ => None,
                }),
                Some(expected)
            );
        }
        for (path, v) in [
            ("params.radius", -0.1),
            ("params.radius", 64.01),
            ("params.radius", f64::NAN),
            ("params.threshold", -0.01),
            ("params.threshold", 1.01),
            ("params.threshold", f64::INFINITY),
        ] {
            let mut bad = project.clone();
            bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .base
                .set(path, PropValue::Float(v));
            assert!(
                compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
            );
            bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
            assert!(
                compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .is_empty()
            );
        }
        let hash = |radius, threshold, pixel_scale| {
            content_hash(
                &IrOp::NativeSurfaceBlur {
                    radius,
                    threshold,
                    pixel_scale,
                },
                &[],
                &[],
            )
        };
        let base = hash(2, 0.25, [1.0; 2]);
        for other in [
            hash(3, 0.25, [1.0; 2]),
            hash(2, 0.5, [1.0; 2]),
            hash(2, 0.25, [0.5, 1.0]),
            hash(2, 0.25, [1.0, 0.5]),
        ] {
            assert_ne!(base, other);
        }
    }

    #[test]
    fn native_drop_shadow_catalog_scopes_keys_hash_color_and_guards() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let fx = || {
            timeline::ClipEffect::from_manifest(timeline::EffectId::new_static("util.drop_shadow"))
                .unwrap()
        };
        let aid = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&aid)
            .unwrap()
            .effects
            .push(fx());
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].effects.push(fx());
        sequence.video_tracks[0].effects.push(fx());
        sequence.master_effects.push(fx());
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativeDropShadow { .. }))
                .count(),
            4
        );
        for (path, a, z) in [
            ("params.x", -4.0, 8.0),
            ("params.y", -8.0, 2.0),
            ("params.radius", 0.0, 12.0),
            ("params.r", 0.0, 1.0),
            ("params.g", 0.0, 1.0),
            ("params.b", 0.0, 1.0),
            ("params.opacity", 0.0, 1.0),
        ] {
            let mut keyed = project.clone();
            keyed.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .tracks
                .push(timeline::PropertyTrack {
                    property: PropPath::new(path),
                    keyframes: vec![
                        timeline::Keyframe::new(
                            Tick(0),
                            PropValue::Float(a),
                            timeline::Interp::Linear,
                        ),
                        timeline::Keyframe::new(
                            Tick(30),
                            PropValue::Float(z),
                            timeline::Interp::Linear,
                        ),
                    ],
                    orphaned: false,
                });
            let frame = compile_native_preview(&keyed, seq, 0, Tick(15), Quality::FULL);
            assert!(
                frame.diagnostics.is_empty(),
                "{path}: {:?}",
                frame.diagnostics
            );
            let (op_offset, op_radius, op_color, op_opacity) = frame
                .graph
                .nodes
                .iter()
                .filter_map(|n| match n.op {
                    IrOp::NativeDropShadow {
                        offset,
                        radius,
                        color,
                        opacity,
                        ..
                    } => Some((offset, radius, color, opacity)),
                    _ => None,
                })
                .last()
                .unwrap();
            let mid = ((a + z) / 2.0) as f32;
            match path {
                "params.x" => assert_eq!(op_offset[0], mid),
                "params.y" => assert_eq!(op_offset[1], mid),
                "params.radius" => assert_eq!(op_radius, mid.round() as u32),
                "params.opacity" => assert_eq!(op_opacity, mid),
                _ => {
                    let mut rgb = [0.0; 3];
                    rgb[match path {
                        "params.r" => 0,
                        "params.g" => 1,
                        _ => 2,
                    }] = mid as f64;
                    let expected = crate::color::native::encoded_srgb_to_linear_ap1_display(rgb)
                        .map(|v| v as f32);
                    assert_eq!(op_color, expected);
                }
            }
        }
        for (path, v) in [
            ("params.x", 200.1),
            ("params.y", -200.1),
            ("params.radius", 64.1),
            ("params.r", 1.01),
            ("params.g", -0.01),
            ("params.b", f64::NAN),
            ("params.opacity", f64::INFINITY),
        ] {
            let mut bad = project.clone();
            bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .base
                .set(path, PropValue::Float(v));
            assert!(
                compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
            );
            bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
            assert!(
                compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .is_empty()
            );
        }
        let hash = |offset, radius, color, opacity, pixel_scale| {
            content_hash(
                &IrOp::NativeDropShadow {
                    offset,
                    radius,
                    color,
                    opacity,
                    pixel_scale,
                },
                &[],
                &[],
            )
        };
        let base = hash([0.0; 2], 2, [0.0; 3], 0.5, [1.0; 2]);
        for value in [
            hash([1.0, 0.0], 2, [0.0; 3], 0.5, [1.0; 2]),
            hash([0.0, 1.0], 2, [0.0; 3], 0.5, [1.0; 2]),
            hash([0.0; 2], 3, [0.0; 3], 0.5, [1.0; 2]),
            hash([0.0; 2], 2, [1.0; 3], 0.5, [1.0; 2]),
            hash([0.0; 2], 2, [0.0; 3], 0.6, [1.0; 2]),
            hash([0.0; 2], 2, [0.0; 3], 0.5, [0.5, 1.0]),
            hash([0.0; 2], 2, [0.0; 3], 0.5, [1.0, 0.5]),
        ] {
            assert_ne!(base, value);
        }
    }

    #[test]
    fn native_perspective_catalog_scopes_all_corner_keys_hash_and_guards() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let fx = || {
            timeline::ClipEffect::from_manifest(timeline::EffectId::new_static("geo.perspective"))
                .unwrap()
        };
        let aid = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&aid)
            .unwrap()
            .effects
            .push(fx());
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].effects.push(fx());
        sequence.video_tracks[0].effects.push(fx());
        sequence.master_effects.push(fx());
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativePerspective { .. }))
                .count(),
            4
        );
        let paths = [
            "params.tl_x",
            "params.tl_y",
            "params.tr_x",
            "params.tr_y",
            "params.br_x",
            "params.br_y",
            "params.bl_x",
            "params.bl_y",
        ];
        let identity = [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0];
        for (i, path) in paths.iter().enumerate() {
            let mut keyed = project.clone();
            let target = if identity[i] == 0.0 { 0.2 } else { 0.8 };
            keyed.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .tracks
                .push(timeline::PropertyTrack {
                    property: PropPath::new(*path),
                    keyframes: vec![
                        timeline::Keyframe::new(
                            Tick(0),
                            PropValue::Float(identity[i]),
                            timeline::Interp::Linear,
                        ),
                        timeline::Keyframe::new(
                            Tick(30),
                            PropValue::Float(target),
                            timeline::Interp::Linear,
                        ),
                    ],
                    orphaned: false,
                });
            let frame = compile_native_preview(&keyed, seq, 0, Tick(15), Quality::FULL);
            assert!(
                frame.diagnostics.is_empty(),
                "{path}: {:?}",
                frame.diagnostics
            );
            let mut corners = identity.map(|v| v as f32);
            corners[i] = ((identity[i] + target) / 2.0) as f32;
            let expected = crate::graph::ops::native_perspective_inverse(corners).unwrap();
            assert!(frame
                .graph
                .nodes
                .iter()
                .any(|n| matches!(n.op,IrOp::NativePerspective{inverse}if inverse==expected)));
        }
        for value in [-1.01, 2.01, f64::NAN, f64::INFINITY] {
            let mut bad = project.clone();
            bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .base
                .set(paths[0], PropValue::Float(value));
            assert!(
                compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
            );
            bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
            assert!(
                compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .is_empty()
            );
        }
        let mut collapsed = project.clone();
        for path in paths {
            collapsed.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .base
                .set(path, PropValue::Float(0.0));
        }
        assert!(
            compile_native_preview(&collapsed, seq, 0, Tick(15), Quality::FULL)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error)
        );
        let inverse = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let base = content_hash(&IrOp::NativePerspective { inverse }, &[], &[]);
        for i in 0..9 {
            let mut altered = inverse;
            altered[i] += 0.125;
            assert_ne!(
                base,
                content_hash(&IrOp::NativePerspective { inverse: altered }, &[], &[])
            );
        }
    }

    #[test]
    fn native_stylize_catalog_scopes_chromatic_keys_hash_and_guards() {
        for (id, kind) in [
            ("stylize.emboss", NativeStylizeKind::Emboss),
            ("stylize.find_edges", NativeStylizeKind::FindEdges),
            (
                "stylize.chromatic_aberration",
                NativeStylizeKind::ChromaticAberration,
            ),
        ] {
            let (mut project, seq, _, _) = native_group_look_fixture();
            let fx = |value| {
                let mut f = timeline::ClipEffect::from_manifest(timeline::EffectId::new_static(id))
                    .unwrap();
                if kind == NativeStylizeKind::ChromaticAberration {
                    f.params.base.set("params.amount", PropValue::Float(value));
                }
                f
            };
            let aid = *project.media.assets.keys().next().unwrap();
            project
                .media
                .assets
                .get_mut(&aid)
                .unwrap()
                .effects
                .push(fx(0.0));
            let sequence = project.sequences.get_mut(&seq).unwrap();
            sequence.video_tracks[0].clips[0].effects.push(fx(-4.0));
            sequence.video_tracks[0].effects.push(fx(8.0));
            sequence.master_effects.push(fx(64.0));
            let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
            assert!(
                frame.diagnostics.is_empty(),
                "{id}: {:?}",
                frame.diagnostics
            );
            let amounts: Vec<_> = frame
                .graph
                .nodes
                .iter()
                .filter_map(|n| match n.op {
                    IrOp::NativeStylize {
                        kind: k, amount, ..
                    } if k == kind => Some(amount),
                    _ => None,
                })
                .collect();
            assert_eq!(
                amounts,
                if kind == NativeStylizeKind::ChromaticAberration {
                    vec![0.0, -4.0, 8.0, 64.0]
                } else {
                    vec![0.0; 4]
                }
            );
            if kind == NativeStylizeKind::ChromaticAberration {
                for value in [-64.01, 64.01, f64::NAN, f64::INFINITY] {
                    let mut bad = project.clone();
                    bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                        .params
                        .base
                        .set("params.amount", PropValue::Float(value));
                    assert!(
                        compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                            .diagnostics
                            .iter()
                            .any(|d| d.severity == DiagSeverity::Error)
                    );
                    bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
                    assert!(
                        compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                            .diagnostics
                            .is_empty()
                    );
                }
                project.sequences.get_mut(&seq).unwrap().master_effects[0]
                    .params
                    .tracks
                    .push(timeline::PropertyTrack {
                        property: PropPath::new("params.amount"),
                        keyframes: vec![
                            timeline::Keyframe::new(
                                Tick(0),
                                PropValue::Float(0.0),
                                timeline::Interp::Linear,
                            ),
                            timeline::Keyframe::new(
                                Tick(30),
                                PropValue::Float(8.0),
                                timeline::Interp::Linear,
                            ),
                        ],
                        orphaned: false,
                    });
                let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
                assert!(frame
                    .graph
                    .nodes
                    .iter()
                    .any(|n| matches!(n.op, IrOp::NativeStylize { amount: 4.0, .. })));
            }
        }
        let hash = |kind, amount, pixel_scale| {
            content_hash(
                &IrOp::NativeStylize {
                    kind,
                    amount,
                    pixel_scale,
                },
                &[],
                &[],
            )
        };
        let base = hash(NativeStylizeKind::ChromaticAberration, 2.0, [1.0; 2]);
        for v in [
            hash(NativeStylizeKind::Emboss, 2.0, [1.0; 2]),
            hash(NativeStylizeKind::ChromaticAberration, 3.0, [1.0; 2]),
            hash(NativeStylizeKind::ChromaticAberration, 2.0, [0.5, 1.0]),
            hash(NativeStylizeKind::ChromaticAberration, 2.0, [1.0, 0.5]),
        ] {
            assert_ne!(base, v);
        }
    }

    #[test]
    fn native_detail_catalog_scopes_animation_hash_and_guards() {
        for id in [
            "sharpen.smart",
            "filter.high_pass",
            "color.clarity",
            "sharpen.unsharp_raster",
        ] {
            let (mut project, seq, _, _) = native_group_look_fixture();
            let path = if id == "filter.high_pass" {
                "params.radius"
            } else {
                "params.amount"
            };
            let fx = |value| {
                let mut f = timeline::ClipEffect::from_manifest(timeline::EffectId::new_static(id))
                    .unwrap();
                f.params.base.set(path, PropValue::Float(value));
                if matches!(id, "sharpen.smart" | "sharpen.unsharp_raster") {
                    f.params.base.set("params.radius", PropValue::Float(2.0));
                }
                if id == "sharpen.smart" {
                    f.params
                        .base
                        .set("params.threshold", PropValue::Float(25.5));
                }
                f
            };
            let aid = *project.media.assets.keys().next().unwrap();
            project
                .media
                .assets
                .get_mut(&aid)
                .unwrap()
                .effects
                .push(fx(0.0));
            let sequence = project.sequences.get_mut(&seq).unwrap();
            sequence.video_tracks[0].clips[0].effects.push(fx(0.25));
            sequence.video_tracks[0].effects.push(fx(0.5));
            sequence.master_effects.push(fx(1.0));
            let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
            assert!(
                frame.diagnostics.is_empty(),
                "{id}: {:?}",
                frame.diagnostics
            );
            let values: Vec<_> = frame
                .graph
                .nodes
                .iter()
                .filter_map(|n| match n.op {
                    IrOp::NativeDetail {
                        kind: NativeDetailKind::HighPass,
                        sigma,
                        ..
                    } => Some(sigma),
                    IrOp::NativeDetail { amount, .. } | IrOp::NativeUnsharp { amount, .. } => {
                        Some(amount)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(values, vec![0.0, 0.25, 0.5, 1.0], "{id}");
            let max = match id {
                "filter.high_pass" => 500.0,
                "color.clarity" => 1.0,
                _ => 10.0,
            };
            let min = if id == "color.clarity" { -1.0 } else { 0.0 };
            for value in [max + 0.01, min - 0.01, f64::NAN, f64::INFINITY] {
                let mut bad = project.clone();
                bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                    .params
                    .base
                    .set(path, PropValue::Float(value));
                assert!(
                    compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                        .diagnostics
                        .iter()
                        .any(|d| d.severity == DiagSeverity::Error)
                );
                bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
                assert!(
                    compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                        .diagnostics
                        .is_empty()
                );
            }
            if id == "sharpen.smart" {
                for (path, value) in [
                    ("params.threshold", 255.01),
                    ("params.threshold", -0.01),
                    ("params.radius", 100.01),
                ] {
                    let mut bad = project.clone();
                    bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                        .params
                        .base
                        .set(path, PropValue::Float(value));
                    assert!(
                        compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                            .diagnostics
                            .iter()
                            .any(|d| d.severity == DiagSeverity::Error)
                    );
                }
            }
            project.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .tracks
                .push(timeline::PropertyTrack {
                    property: PropPath::new(path),
                    keyframes: vec![
                        timeline::Keyframe::new(
                            Tick(0),
                            PropValue::Float(0.0),
                            timeline::Interp::Linear,
                        ),
                        timeline::Keyframe::new(
                            Tick(30),
                            PropValue::Float(0.8),
                            timeline::Interp::Linear,
                        ),
                    ],
                    orphaned: false,
                });
            let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
            assert!(frame.graph.nodes.iter().any(|n| match n.op {
                IrOp::NativeDetail {
                    kind: NativeDetailKind::HighPass,
                    sigma,
                    ..
                } => (sigma - 0.4).abs() < 0.00001,
                IrOp::NativeDetail { amount, .. } | IrOp::NativeUnsharp { amount, .. } =>
                    (amount - 0.4).abs() < 0.00001,
                _ => false,
            }));
        }
        let hash = |kind, amount, sigma, threshold, pixel_scale| {
            content_hash(
                &IrOp::NativeDetail {
                    kind,
                    amount,
                    sigma,
                    threshold,
                    pixel_scale,
                },
                &[],
                &[],
            )
        };
        let base = hash(NativeDetailKind::SmartSharpen, 1.0, 2.0, 0.1, [1.0; 2]);
        for changed in [
            hash(NativeDetailKind::HighPass, 1.0, 2.0, 0.1, [1.0; 2]),
            hash(NativeDetailKind::SmartSharpen, 2.0, 2.0, 0.1, [1.0; 2]),
            hash(NativeDetailKind::SmartSharpen, 1.0, 3.0, 0.1, [1.0; 2]),
            hash(NativeDetailKind::SmartSharpen, 1.0, 2.0, 0.2, [1.0; 2]),
            hash(NativeDetailKind::SmartSharpen, 1.0, 2.0, 0.1, [0.5, 1.0]),
        ] {
            assert_ne!(base, changed);
        }
    }

    #[test]
    fn native_geometric_catalog_scopes_keys_hash_and_invalid_parameters() {
        for (id, kind) in [
            ("geo.pinch", NativeWarpKind::Pinch),
            ("geo.spherize", NativeWarpKind::Spherize),
            ("geo.ripple", NativeWarpKind::Ripple),
        ] {
            let (mut project, seq, _, _) = native_group_look_fixture();
            let path = if kind == NativeWarpKind::Ripple {
                "params.amplitude"
            } else {
                "params.amount"
            };
            let fx = |amount| {
                let mut f = timeline::ClipEffect::from_manifest(timeline::EffectId::new_static(id))
                    .unwrap();
                f.params.base.set(path, PropValue::Float(amount));
                f
            };
            let aid = *project.media.assets.keys().next().unwrap();
            project
                .media
                .assets
                .get_mut(&aid)
                .unwrap()
                .effects
                .push(fx(0.0));
            let sequence = project.sequences.get_mut(&seq).unwrap();
            sequence.video_tracks[0].clips[0].effects.push(fx(-0.5));
            sequence.video_tracks[0].effects.push(fx(0.25));
            sequence.master_effects.push(fx(1.0));
            let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            let amounts: Vec<_> = frame
                .graph
                .nodes
                .iter()
                .filter_map(|n| match n.op {
                    IrOp::NativeGeometricWarp {
                        kind: k, amount, ..
                    } if k == kind => Some(amount),
                    _ => None,
                })
                .collect();
            assert_eq!(amounts, vec![0.0, -0.5, 0.25, 1.0]);
            let max = if kind == NativeWarpKind::Ripple {
                64.0
            } else {
                1.0
            };
            for value in [max + 0.01, -max - 0.01, f64::NAN, f64::INFINITY] {
                let mut bad = project.clone();
                bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                    .params
                    .base
                    .set(path, PropValue::Float(value));
                assert!(
                    compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                        .diagnostics
                        .iter()
                        .any(|d| d.severity == DiagSeverity::Error)
                );
                bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
                assert!(
                    compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                        .diagnostics
                        .is_empty()
                );
            }
            if kind == NativeWarpKind::Ripple {
                for value in [0.0, 512.01, f64::NAN] {
                    let mut bad = project.clone();
                    bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                        .params
                        .base
                        .set("params.wavelength", PropValue::Float(value));
                    assert!(
                        compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                            .diagnostics
                            .iter()
                            .any(|d| d.severity == DiagSeverity::Error)
                    );
                }
            }
            project.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .tracks
                .push(timeline::PropertyTrack {
                    property: PropPath::new(path),
                    keyframes: vec![
                        timeline::Keyframe::new(
                            Tick(0),
                            PropValue::Float(0.0),
                            timeline::Interp::Linear,
                        ),
                        timeline::Keyframe::new(
                            Tick(30),
                            PropValue::Float(0.8),
                            timeline::Interp::Linear,
                        ),
                    ],
                    orphaned: false,
                });
            let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
            assert!(frame.graph.nodes.iter().any(|n|matches!(n.op,IrOp::NativeGeometricWarp{amount,..} if (amount-0.4).abs()<0.00001)));
        }
        let hash = |kind, amount, wavelength, pixel_scale| {
            content_hash(
                &IrOp::NativeGeometricWarp {
                    kind,
                    amount,
                    wavelength,
                    pixel_scale,
                },
                &[],
                &[],
            )
        };
        let baseline = hash(NativeWarpKind::Ripple, 1.0, 16.0, [1.0; 2]);
        for value in [
            hash(NativeWarpKind::Pinch, 1.0, 16.0, [1.0; 2]),
            hash(NativeWarpKind::Ripple, 2.0, 16.0, [1.0; 2]),
            hash(NativeWarpKind::Ripple, 1.0, 17.0, [1.0; 2]),
            hash(NativeWarpKind::Ripple, 1.0, 16.0, [0.5, 1.0]),
            hash(NativeWarpKind::Ripple, 1.0, 16.0, [1.0, 0.5]),
        ] {
            assert_ne!(baseline, value);
        }
    }

    #[test]
    fn native_box_blur_catalog_scopes_rounding_hash_and_keyframes() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let fx = |radius| {
            let mut f =
                timeline::ClipEffect::from_manifest(timeline::EffectId::new_static("blur.box"))
                    .unwrap();
            f.params.base.set("params.radius", PropValue::Float(radius));
            f
        };
        let aid = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&aid)
            .unwrap()
            .effects
            .push(fx(0.0));
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].effects.push(fx(2.49));
        sequence.video_tracks[0].effects.push(fx(2.5));
        sequence.master_effects.push(fx(500.0));
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        let radii: Vec<_> = frame
            .graph
            .nodes
            .iter()
            .filter_map(|n| match n.op {
                IrOp::NativeBoxBlur { radius } => Some(radius),
                _ => None,
            })
            .collect();
        assert_eq!(radii, vec![[0, 0], [2, 2], [3, 3], [500, 500]]);
        let hash = |radius| content_hash(&IrOp::NativeBoxBlur { radius }, &[], &[]);
        assert_ne!(hash([2, 2]), hash([2, 3]));
        assert_ne!(hash([2, 2]), hash([3, 2]));
        for value in [500.01, -1.0, f64::NAN, f64::INFINITY] {
            let mut bad = project.clone();
            bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .base
                .set("params.radius", PropValue::Float(value));
            assert!(
                compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
            );
            bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
            assert!(
                compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .is_empty()
            );
        }
        project.sequences.get_mut(&seq).unwrap().master_effects[0]
            .params
            .tracks
            .push(timeline::PropertyTrack {
                property: PropPath::new("params.radius"),
                keyframes: vec![
                    timeline::Keyframe::new(
                        Tick(0),
                        PropValue::Float(1.0),
                        timeline::Interp::Linear,
                    ),
                    timeline::Keyframe::new(
                        Tick(30),
                        PropValue::Float(8.0),
                        timeline::Interp::Linear,
                    ),
                ],
                orphaned: false,
            });
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeBoxBlur { radius: [5, 5] })));
    }

    #[test]
    fn native_mosaic_catalog_scopes_rounding_hash_and_keyframes() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let fx = |block| {
            let mut f = timeline::ClipEffect::from_manifest(timeline::EffectId::new_static(
                "stylize.mosaic",
            ))
            .unwrap();
            f.params.base.set("params.block", PropValue::Float(block));
            f
        };
        let aid = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&aid)
            .unwrap()
            .effects
            .push(fx(1.0));
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].effects.push(fx(2.49));
        sequence.video_tracks[0].effects.push(fx(2.5));
        sequence.master_effects.push(fx(256.0));
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        let blocks: Vec<_> = frame
            .graph
            .nodes
            .iter()
            .filter_map(|n| match n.op {
                IrOp::NativeMosaic { block } => Some(block),
                _ => None,
            })
            .collect();
        assert_eq!(blocks, vec![[1, 1], [2, 2], [3, 3], [256, 256]]);
        let hash = |block| content_hash(&IrOp::NativeMosaic { block }, &[], &[]);
        assert_ne!(hash([2, 2]), hash([2, 3]));
        assert_ne!(hash([2, 2]), hash([3, 2]));
        for value in [0.0, 256.01, -1.0, f64::NAN, f64::INFINITY] {
            let mut bad = project.clone();
            bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .base
                .set("params.block", PropValue::Float(value));
            assert!(
                compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
            );
            bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
            assert!(
                compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .is_empty()
            );
        }
        project.sequences.get_mut(&seq).unwrap().master_effects[0]
            .params
            .tracks
            .push(timeline::PropertyTrack {
                property: PropPath::new("params.block"),
                keyframes: vec![
                    timeline::Keyframe::new(
                        Tick(0),
                        PropValue::Float(1.0),
                        timeline::Interp::Linear,
                    ),
                    timeline::Keyframe::new(
                        Tick(30),
                        PropValue::Float(8.0),
                        timeline::Interp::Linear,
                    ),
                ],
                orphaned: false,
            });
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeMosaic { block: [5, 5] })));
    }

    #[test]
    fn native_motion_blur_catalog_scopes_parameters_hash_and_keyframes_are_explicit() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let fx = |angle: f64, distance: f64| {
            let mut fx =
                timeline::ClipEffect::from_manifest(timeline::EffectId::new_static("blur.motion"))
                    .unwrap();
            fx.params.base.set("params.angle", PropValue::Float(angle));
            fx.params
                .base
                .set("params.distance", PropValue::Float(distance));
            fx
        };
        let aid = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&aid)
            .unwrap()
            .effects
            .push(fx(0.0, 1.0));
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0]
            .effects
            .push(fx(90.0, 2.0));
        sequence.video_tracks[0].effects.push(fx(180.0, 3.0));
        sequence.master_effects.push(fx(270.0, 4.0));
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        let delta: Vec<_> = frame
            .graph
            .nodes
            .iter()
            .filter_map(|n| match n.op {
                IrOp::NativeMotionBlur { delta } => Some(delta),
                _ => None,
            })
            .collect();
        assert_eq!(
            delta,
            vec![[1.0, 0.0], [0.0, 2.0], [-3.0, 0.0], [0.0, -4.0]]
        );
        let hash = |delta| content_hash(&IrOp::NativeMotionBlur { delta }, &[], &[]);
        assert_ne!(hash([1.0, 2.0]), hash([1.0, 3.0]));
        assert_ne!(hash([1.0, 2.0]), hash([2.0, 2.0]));
        for (path, value) in [
            ("params.angle", f64::NAN),
            ("params.angle", 361.0),
            ("params.angle", -1.0),
            ("params.distance", 501.0),
            ("params.distance", -1.0),
            ("params.distance", f64::INFINITY),
        ] {
            let mut bad = project.clone();
            bad.sequences.get_mut(&seq).unwrap().master_effects[0]
                .params
                .base
                .set(path, PropValue::Float(value));
            assert!(
                compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
            );
            bad.sequences.get_mut(&seq).unwrap().master_effects[0].enabled = false;
            assert!(
                compile_native_preview(&bad, seq, 0, Tick(15), Quality::FULL)
                    .diagnostics
                    .is_empty()
            );
        }
        let lane = timeline::PropertyTrack {
            property: PropPath::new("params.distance"),
            keyframes: vec![
                timeline::Keyframe::new(Tick(0), PropValue::Float(0.0), timeline::Interp::Linear),
                timeline::Keyframe::new(Tick(30), PropValue::Float(8.0), timeline::Interp::Linear),
            ],
            orphaned: false,
        };
        project.sequences.get_mut(&seq).unwrap().master_effects[0]
            .params
            .tracks
            .push(lane);
        let frame = compile_native_preview(&project, seq, 0, Tick(15), Quality::FULL);
        assert!(frame
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op,IrOp::NativeMotionBlur {delta} if delta==[0.0,-4.0])));
    }

    #[test]
    fn native_unsharp_graph_defaults_parameters_and_cache_are_explicit() {
        let (project, seq, _, _) = native_group_look_fixture();
        let format = &project.sequences[&seq].formats[0];
        let (mut graph, source) = NodeGraph::new_clip_composition("Native unsharp");
        let sharpen = GraphNode::new(GraphOp::Sharpen);
        let id = sharpen.id;
        graph.nodes.insert(id, sharpen);
        graph.edges = vec![
            timeline::GraphEdge {
                from: (source, timeline::OutPort::PRIMARY),
                to: (id, InPort::PRIMARY),
            },
            timeline::GraphEdge {
                from: (id, timeline::OutPort::PRIMARY),
                to: (graph.output, InPort::PRIMARY),
            },
        ];
        for (amount, radius, expected) in [
            (None, None, Some((0.0, 0.0))),
            (
                Some(PropValue::Float(2.0)),
                Some(PropValue::Float(3.0)),
                Some((2.0, 3.0)),
            ),
            (
                Some(PropValue::Bool(true)),
                Some(PropValue::Float(3.0)),
                None,
            ),
            (
                Some(PropValue::Float(1.0)),
                Some(PropValue::Float(101.0)),
                None,
            ),
        ] {
            let params = &mut graph.nodes.get_mut(&id).unwrap().params.base.0;
            params.entries.clear();
            if let Some(v) = amount {
                params.set("params.amount", v);
            }
            if let Some(v) = radius {
                params.set("params.radius", v);
            }
            let mut b = Builder::new();
            b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
            let src = b.push(
                IrOp::SolidColor {
                    color: LinearColor {
                        r: 0.2,
                        g: 0.1,
                        b: 0.0,
                        a: 0.5,
                    },
                },
                vec![],
            );
            let result = lower_native_graph(&mut b, &graph, format, Tick::ZERO, Some(src), None);
            if let Some((amount, sigma)) = expected {
                let output = result.unwrap();
                let frame = b.finish(Some(output));
                assert!(frame.graph.validate_working_color_domain().is_ok());
                assert!(frame.graph.nodes.iter().any(|node| matches!(node.op,IrOp::NativeUnsharp {amount:a,sigma:s} if a==amount && s==sigma)));
            } else {
                assert!(result.is_err());
            }
        }
        let hash = |amount, sigma| content_hash(&IrOp::NativeUnsharp { amount, sigma }, &[], &[]);
        assert_ne!(hash(1.0, 2.0), hash(2.0, 2.0));
        assert_ne!(hash(1.0, 2.0), hash(1.0, 3.0));
    }

    #[test]
    fn native_unsharp_stacks_preserve_scope_order_and_fail_closed() {
        let (mut project, seq, _, _) = native_group_look_fixture();
        let blur = |radius| {
            let mut fx =
                ClipEffect::from_manifest(timeline::EffectId::new_static("sharpen.unsharp"))
                    .unwrap();
            // Seed entries already contain radius; replace rather than shadow it.
            fx.params
                .base
                .entries
                .retain(|(path, _)| path.as_str() != "params.radius");
            fx.params
                .base
                .entries
                .push((PropPath::new("params.radius"), PropValue::Float(radius)));
            fx.params.base.set("params.amount", PropValue::Float(1.0));
            fx
        };
        let asset = *project.media.assets.keys().next().unwrap();
        project
            .media
            .assets
            .get_mut(&asset)
            .unwrap()
            .effects
            .push(blur(1.0));
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].effects.push(blur(2.0));
        sequence.video_tracks[0].effects.push(blur(3.0));
        sequence.master_effects.push(blur(4.0));
        let frame = compile_native_preview(&project, seq, 0, Tick::ZERO, Quality::FULL);
        assert!(
            frame
                .diagnostics
                .iter()
                .all(|d| d.severity != DiagSeverity::Error),
            "{:?}",
            frame.diagnostics
        );
        assert_eq!(frame.graph.validate_working_color_domain(), Ok(()));
        let sigmas: Vec<_> = frame
            .graph
            .nodes
            .iter()
            .filter_map(|n| match n.op {
                IrOp::NativeUnsharp { sigma, .. } => Some(sigma),
                _ => None,
            })
            .collect();
        assert_eq!(sigmas, vec![1.0, 2.0, 3.0, 4.0]);
        let mut legacy_id = project.clone();
        let effect = &mut legacy_id.sequences.get_mut(&seq).unwrap().master_effects[0];
        effect.id = timeline::EffectId::EMPTY;
        effect.version = 0;
        assert!(
            compile_native_preview(&legacy_id, seq, 0, Tick::ZERO, Quality::FULL)
                .diagnostics
                .iter()
                .all(|d| d.severity != DiagSeverity::Error)
        );
        let signature = content_hash(
            &IrOp::NativeUnsharp {
                sigma: 1.0,
                amount: 1.0,
            },
            &[],
            &[],
        );
        assert_ne!(
            signature,
            content_hash(
                &IrOp::NativeUnsharp {
                    sigma: 2.0,
                    amount: 1.0
                },
                &[],
                &[]
            )
        );
        assert_ne!(
            signature,
            content_hash(&IrOp::NativeExposure { stops: 1.0 }, &[], &[])
        );
        // Clip/asset keys use relative time; track/master keys use sequence time.
        let sequence = project.sequences.get_mut(&seq).unwrap();
        sequence.video_tracks[0].clips[0].start = Tick(100);
        let clip_effect = &mut sequence.video_tracks[0].clips[0].effects[0];
        clip_effect.zone = Some((Tick(5), Tick(15)));
        let lane = clip_effect
            .params
            .track_mut(&PropPath::new("params.radius"));
        lane.insert_keyframe(timeline::Keyframe::new(
            Tick(0),
            PropValue::Float(2.0),
            timeline::Interp::Linear,
        ));
        lane.insert_keyframe(timeline::Keyframe::new(
            Tick(20),
            PropValue::Float(4.0),
            timeline::Interp::Linear,
        ));
        for (tick, expected) in [
            (104, vec![1.0, 3.0, 4.0]),
            (110, vec![1.0, 3.0, 3.0, 4.0]),
            (115, vec![1.0, 3.0, 4.0]),
        ] {
            let frame = compile_native_preview(&project, seq, 0, Tick(tick), Quality::FULL);
            assert!(frame
                .diagnostics
                .iter()
                .all(|d| d.severity != DiagSeverity::Error));
            let sigmas: Vec<_> = frame
                .graph
                .nodes
                .iter()
                .filter_map(|n| match n.op {
                    IrOp::NativeUnsharp { sigma, .. } => Some(sigma),
                    _ => None,
                })
                .collect();
            assert_eq!(sigmas, expected);
        }
        project.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].start = Tick::ZERO;
        for bad in [f64::NAN, -1.0, 101.0] {
            project.sequences.get_mut(&seq).unwrap().master_effects = vec![blur(bad)];
            let invalid = compile_native_preview(&project, seq, 0, Tick::ZERO, Quality::FULL);
            assert!(invalid
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error));
        }
        let mut unsupported = ClipEffect::new(EffectKind::Deflicker);
        unsupported.enabled = false;
        project.sequences.get_mut(&seq).unwrap().master_effects = vec![unsupported.clone()];
        assert!(
            compile_native_preview(&project, seq, 0, Tick::ZERO, Quality::FULL)
                .diagnostics
                .iter()
                .all(|d| d.severity != DiagSeverity::Error)
        );
        unsupported.enabled = true;
        project.sequences.get_mut(&seq).unwrap().master_effects = vec![unsupported];
        assert!(
            compile_native_preview(&project, seq, 0, Tick::ZERO, Quality::FULL)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error)
        );
    }

    #[test]
    fn native_resize_graph_validates_dimensions_fit_and_cache_identity() {
        use timeline::{GraphEdge, OutPort as GOut};
        let (mut project, sequence, _, _) = native_group_look_fixture();
        let mut graph = NodeGraph::new_project_graph("Native resize");
        let mut node = GraphNode::new(GraphOp::Resize {
            fit: timeline::FitMode::Contain,
        });
        let id = node.id;
        node.params
            .base
            .0
            .set("params.width", PropValue::Float(12.0));
        node.params
            .base
            .0
            .set("params.height", PropValue::Float(8.0));
        graph.nodes.insert(id, node);
        graph.edges.push(GraphEdge {
            from: (id, GOut::PRIMARY),
            to: (graph.output, InPort::PRIMARY),
        });
        let graph_id = graph.id;
        project.graphs.insert(graph_id, graph);
        project.project_graph = Some(graph_id);
        let first = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        assert!(first.diagnostics.is_empty(), "{:?}", first.diagnostics);
        assert!(first.graph.validate_working_color_domain().is_ok());
        let resize = first
            .graph
            .nodes
            .iter()
            .find(|n| {
                matches!(
                    n.op,
                    IrOp::Resize {
                        w: 12,
                        h: 8,
                        fit: FitMode::Fit
                    }
                )
            })
            .unwrap();
        let first_hash = resize.content_hash;
        for fit in [timeline::FitMode::Cover, timeline::FitMode::Stretch] {
            project
                .graphs
                .get_mut(&graph_id)
                .unwrap()
                .nodes
                .get_mut(&id)
                .unwrap()
                .op = GraphOp::Resize { fit };
            let frame = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            let resize = frame
                .graph
                .nodes
                .iter()
                .find(|n| matches!(n.op, IrOp::Resize { .. }))
                .unwrap();
            assert_ne!(resize.content_hash, first_hash);
        }
        for value in [
            PropValue::Float(0.0),
            PropValue::Float(-1.0),
            PropValue::Float(8193.0),
            PropValue::Float(2.5),
            PropValue::Float(f64::NAN),
            PropValue::Bool(true),
        ] {
            let mut broken = project.clone();
            broken
                .graphs
                .get_mut(&graph_id)
                .unwrap()
                .nodes
                .get_mut(&id)
                .unwrap()
                .params
                .base
                .0
                .set("params.width", value);
            assert!(
                compile_native_preview(&broken, sequence, 0, Tick::ZERO, Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
            );
        }
        let node = project
            .graphs
            .get_mut(&graph_id)
            .unwrap()
            .nodes
            .get_mut(&id)
            .unwrap();
        node.params
            .base
            .0
            .set("params.width", PropValue::Float(8192.0));
        node.params
            .base
            .0
            .set("params.height", PropValue::Float(8192.0));
        assert!(
            compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error)
        );
    }

    #[test]
    fn native_composition_and_project_graph_preserve_stage_order() {
        use timeline::{GraphEdge, OutPort as GOut};
        let (mut project, sequence, _, _) = native_group_look_fixture();
        let exposure = |stops| {
            let mut grade = Grade::new();
            grade.ops.push(GradeOp::new(
                GradeOpKind::Exposure,
                GradeOpParams::Exposure { stops },
            ));
            GraphNode::new(GraphOp::Grade { grade })
        };
        let (mut composition, source) = NodeGraph::new_clip_composition("Native composition");
        let grade = exposure(0.125);
        let grade_id = grade.id;
        let mut blur = GraphNode::new(GraphOp::Blur);
        let blur_id = blur.id;
        blur.params
            .base
            .0
            .set("params.radius", PropValue::Float(1.5));
        composition.nodes.insert(grade_id, grade);
        composition.nodes.insert(blur_id, blur);
        composition.edges = vec![
            GraphEdge {
                from: (source, GOut::PRIMARY),
                to: (grade_id, InPort::PRIMARY),
            },
            GraphEdge {
                from: (grade_id, GOut::PRIMARY),
                to: (blur_id, InPort::PRIMARY),
            },
            GraphEdge {
                from: (blur_id, GOut::PRIMARY),
                to: (composition.output, InPort::PRIMARY),
            },
        ];
        let composition_id = composition.id;
        project.graphs.insert(composition_id, composition);
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].composition =
            Some(composition_id);
        let mut graph = NodeGraph::new_project_graph("Native project");
        let grade = exposure(7.0);
        let grade_id = grade.id;
        graph.nodes.insert(grade_id, grade);
        graph.edges.push(GraphEdge {
            from: (grade_id, GOut::PRIMARY),
            to: (graph.output, InPort::PRIMARY),
        });
        let project_graph = graph.id;
        project.graphs.insert(project_graph, graph);
        project.project_graph = Some(project_graph);
        project.active_sequence = None; // Explicit target, never accidental active-sequence fallback.
        let compiled = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        assert!(
            compiled.diagnostics.is_empty(),
            "{:?}",
            compiled.diagnostics
        );
        assert!(compiled.graph.validate_working_color_domain().is_ok());
        assert_eq!(compiled.graph.canvas_scale((8, 8)), Vec2::splat(0.5));
        let stops: Vec<_> = compiled
            .graph
            .nodes
            .iter()
            .filter_map(|n| match n.op {
                IrOp::NativeExposure { stops } => Some(stops),
                _ => None,
            })
            .collect();
        assert_eq!(
            stops,
            vec![0.125, 0.25, 0.5, 1.0, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 7.0]
        );
        assert_eq!(
            compiled
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativeDecodeVideo { .. }))
                .count(),
            1
        );
        let blur = compiled
            .graph
            .nodes
            .iter()
            .find(|n| matches!(n.op, IrOp::NativeGaussianBlur { sigma } if sigma == 1.5))
            .unwrap();
        assert!(
            matches!(compiled.graph.nodes[blur.inputs[0].0.0 as usize].op, IrOp::NativeExposure { stops } if stops == 0.125)
        );

        // The project graph belongs to the outer output, never every nest.
        let mut nested_project = project.clone();
        let inner = &nested_project.sequences[&sequence];
        let mut outer = Sequence::new("Outer native", inner.frame_rate, 16, 16);
        outer.color = inner.color.clone();
        let outer_id = outer.id;
        let mut track = Track::new(TrackKind::Video, "Nested");
        track.clips.push(Clip::new(
            ClipSource::NestedSequence { sequence },
            Tick::ZERO,
            Tick(1000),
        ));
        outer.video_tracks.push(track);
        nested_project.insert_sequence(outer);
        let nested =
            compile_native_preview(&nested_project, outer_id, 0, Tick::ZERO, Quality::FULL);
        assert!(nested.diagnostics.is_empty(), "{:?}", nested.diagnostics);
        assert!(nested.graph.validate_working_color_domain().is_ok());
        assert_eq!(
            nested
                .graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::NativeExposure { stops } if stops == 7.0))
                .count(),
            1
        );

        let mut geometric = project.clone();
        let graph = geometric.graphs.get_mut(&composition_id).unwrap();
        let mut transform = GraphNode::new(GraphOp::Transform2D);
        let transform_id = transform.id;
        transform
            .params
            .base
            .0
            .set("transform.x", PropValue::Float(1.0));
        let mut crop = GraphNode::new(GraphOp::Crop);
        let crop_id = crop.id;
        crop.params
            .base
            .0
            .set("params.left", PropValue::Float(0.25));
        graph.nodes.insert(transform_id, transform);
        graph.nodes.insert(crop_id, crop);
        graph.edges.retain(|edge| edge.to.0 != graph.output);
        graph.edges.extend([
            GraphEdge {
                from: (blur_id, GOut::PRIMARY),
                to: (transform_id, InPort::PRIMARY),
            },
            GraphEdge {
                from: (transform_id, GOut::PRIMARY),
                to: (crop_id, InPort::PRIMARY),
            },
            GraphEdge {
                from: (crop_id, GOut::PRIMARY),
                to: (graph.output, InPort::PRIMARY),
            },
        ]);
        let frame = compile_native_preview(&geometric, sequence, 0, Tick::ZERO, Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert!(frame.graph.validate_working_color_domain().is_ok());
        assert!(frame.graph.nodes.iter().any(|n| matches!(n.op, IrOp::Transform2DTransparent { mat, .. } if mat == Mat3::from_translation(Vec2::new(1.0, 0.0)))));
        assert!(frame
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::Crop { left, .. } if left == 0.25)));
        for invalid in 0..3 {
            let mut broken = geometric.clone();
            let graph = broken.graphs.get_mut(&composition_id).unwrap();
            match invalid {
                0 => graph
                    .nodes
                    .get_mut(&transform_id)
                    .unwrap()
                    .params
                    .base
                    .0
                    .set("transform.scale_x", PropValue::Float(0.0)),
                1 => graph
                    .nodes
                    .get_mut(&transform_id)
                    .unwrap()
                    .params
                    .base
                    .0
                    .set("transform.x", PropValue::Bool(true)),
                _ => graph
                    .nodes
                    .get_mut(&crop_id)
                    .unwrap()
                    .params
                    .base
                    .0
                    .set("params.left", PropValue::Float(f64::NAN)),
            }
            assert!(
                compile_native_preview(&broken, sequence, 0, Tick::ZERO, Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
            );
        }

        // Dead unsupported branches do not participate in the rendered graph.
        let graph = project.graphs.get_mut(&composition_id).unwrap();
        let dead = GraphNode::new(GraphOp::MaskFromMatte);
        graph.nodes.insert(dead.id, dead);
        assert!(
            compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL)
                .diagnostics
                .is_empty()
        );
        let valid = project.clone();
        for invalid in 0..4 {
            let mut broken = valid.clone();
            let graph = broken.graphs.get_mut(&composition_id).unwrap();
            match invalid {
                0 => graph.nodes.get_mut(&blur_id).unwrap().op = GraphOp::MaskFromMatte,
                1 => {
                    graph.edges[1].from.0 = blur_id;
                } // Reachable self-cycle.
                2 => graph.edges.push(graph.edges[0]), // Duplicate primary input.
                _ => graph.edges[0].from.1 = GOut(1),  // Unqualified output channel.
            }
            let failed = compile_native_preview(&broken, sequence, 0, Tick::ZERO, Quality::FULL);
            assert!(failed
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error));
            assert!(!failed
                .graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::NativeDecodeVideo { .. })));
        }
        let mut bad_opacity = valid.clone();
        let graph = bad_opacity.graphs.get_mut(&composition_id).unwrap();
        graph.nodes.get_mut(&blur_id).unwrap().op = GraphOp::Merge {
            mode: BlendMode::Normal,
        };
        graph
            .nodes
            .get_mut(&blur_id)
            .unwrap()
            .params
            .base
            .0
            .set("params.opacity", PropValue::Bool(true));
        assert!(
            compile_native_preview(&bad_opacity, sequence, 0, Tick::ZERO, Quality::FULL)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error)
        );

        // Missing project graphs fail even when the timeline currently has no clips.
        project
            .sequences
            .get_mut(&sequence)
            .unwrap()
            .video_tracks
            .clear();
        project.graphs.remove(&project_graph);
        assert!(
            compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error)
        );
    }

    #[test]
    fn native_dissolve_borrows_handles_and_applies_track_grade_once() {
        let (mut project, sequence, _, _) = native_group_look_fixture();
        let asset = *project.media.assets.keys().next().unwrap();
        project.media.assets.get_mut(&asset).unwrap().probe =
            Some(timeline::MediaProbe::basic(Tick(250), "mp4", "h264"));
        let track = &mut project.sequences.get_mut(&sequence).unwrap().video_tracks[0];
        track.opacity = 0.6;
        track.clips[0].duration = Tick(100);
        track.clips[0].transform.base.opacity = 0.25;
        let outgoing_id = track.clips[0].id;
        let mut incoming = track.clips[0].clone();
        incoming.id = ClipId::new();
        let incoming_id = incoming.id;
        incoming.start = Tick(100);
        incoming.transform.base.opacity = 0.5;
        incoming.transition_in = Some(timeline::Transition::new(
            TransitionKind::CrossDissolve,
            Tick(40),
        ));
        incoming.transition_in.as_mut().unwrap().params.curve = EaseCurve::Linear;
        track.clips.push(incoming);
        let frame = compile_native_preview(&project, sequence, 0, Tick(110), Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert!(frame.graph.validate_working_color_domain().is_ok());
        let dissolve = frame
            .graph
            .nodes
            .iter()
            .find(|node| matches!(node.op, IrOp::NativeDissolve { .. }))
            .unwrap();
        assert!(matches!(dissolve.op,IrOp::NativeDissolve { t } if t==0.25));
        assert!(
            matches!(frame.graph.nodes[dissolve.inputs[0].0.0 as usize].op,IrOp::Merge { opacity,.. } if opacity==0.5)
        );
        assert!(
            matches!(frame.graph.nodes[dissolve.inputs[1].0.0 as usize].op,IrOp::Merge { opacity,.. } if opacity==0.25)
        );
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|node| matches!(node.op,IrOp::NativeExposure { stops } if stops==5.0))
                .count(),
            1
        );
        assert_eq!(
            frame
                .graph
                .nodes
                .iter()
                .filter(|node| matches!(node.op,IrOp::NativeExposure { stops } if stops==6.0))
                .count(),
            1
        );
        let source_times: Vec<_> = frame
            .graph
            .nodes
            .iter()
            .filter_map(|node| match node.op {
                IrOp::NativeDecodeVideo { src_time, .. } => Some(src_time.0),
                _ => None,
            })
            .collect();
        assert!(source_times.contains(&10) && source_times.contains(&110));
        for kind in [
            TransitionKind::DipToBlack,
            TransitionKind::DipToColor,
            TransitionKind::Wipe,
            TransitionKind::Push,
            TransitionKind::LumaWipe,
        ] {
            let mut variant = project.clone();
            let tr = variant.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[1]
                .transition_in
                .as_mut()
                .unwrap();
            tr.kind = kind;
            tr.params.softness = 0.2;
            tr.params.luma_map = timeline::LumaWipeMap::Clock;
            let compiled = compile_native_preview(&variant, sequence, 0, Tick(120), Quality::FULL);
            assert!(
                compiled.diagnostics.is_empty(),
                "{kind:?}: {:?}",
                compiled.diagnostics
            );
            assert!(compiled.graph.validate_working_color_domain().is_ok());
            assert!(compiled
                .graph
                .nodes
                .iter()
                .any(|node| match (kind, &node.op) {
                    (
                        TransitionKind::DipToBlack | TransitionKind::DipToColor,
                        IrOp::NativeDissolve { t },
                    ) => *t == 0.0,
                    (TransitionKind::Wipe, IrOp::WipeMix { t, softness, .. }) =>
                        *t == 0.5 && *softness == 0.2,
                    (TransitionKind::Push, IrOp::PushMix { t, .. }) => *t == 0.5,
                    (
                        TransitionKind::LumaWipe,
                        IrOp::NativeLumaWipe {
                            kind, t, softness, ..
                        },
                    ) =>
                        *kind == super::super::luma_wipe::LumaWipeKind::Clock
                            && *t == 0.5
                            && *softness == 0.2,
                    _ => false,
                }));
        }
        let mut eased = project.clone();
        eased.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[1]
            .transition_in
            .as_mut()
            .unwrap()
            .params
            .curve = EaseCurve::EaseInOut;
        assert!(
            compile_native_preview(&eased, sequence, 0, Tick(110), Quality::FULL)
                .graph
                .nodes
                .iter()
                .any(|node| matches!(node.op,IrOp::NativeDissolve {t} if t==0.125))
        );
        for invalid in 0..3 {
            let mut variant = project.clone();
            let clip = &mut variant.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[1];
            match invalid {
                0 => clip.start = Tick(110),
                1 => {
                    let tr = clip.transition_in.as_mut().unwrap();
                    tr.kind = TransitionKind::DipToColor;
                    tr.params.color = Some(Color {
                        r: f32::NAN,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    });
                }
                _ => {
                    let tr = clip.transition_in.as_mut().unwrap();
                    tr.kind = TransitionKind::LumaWipe;
                    tr.params.softness = 0.75;
                }
            }
            assert!(
                compile_native_preview(&variant, sequence, 0, Tick(120), Quality::FULL)
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
            );
        }
        for id in [incoming_id, outgoing_id] {
            let tap = frame
                .clip_taps
                .iter()
                .find(|(clip, _)| *clip == id)
                .unwrap()
                .1;
            assert!(!matches!(
                frame.graph.nodes[tap.0 as usize].op,
                IrOp::NativeDissolve { .. }
            ));
        }
        project
            .media
            .assets
            .get_mut(&asset)
            .unwrap()
            .probe
            .as_mut()
            .unwrap()
            .duration = Tick(120);
        let shortened = compile_native_preview(&project, sequence, 0, Tick(110), Quality::FULL);
        assert!(shortened
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op,IrOp::NativeDissolve { t } if t==0.5)));
        assert!(shortened
            .diagnostics
            .iter()
            .any(|d| d.code == Some(CompileCode::TransitionHandleClipped)
                && d.severity == DiagSeverity::Info));
        project
            .media
            .assets
            .get_mut(&asset)
            .unwrap()
            .probe
            .as_mut()
            .unwrap()
            .duration = Tick(100);
        let exhausted = compile_native_preview(&project, sequence, 0, Tick(110), Quality::FULL);
        assert!(!exhausted
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::NativeDissolve { .. })));
        assert!(exhausted
            .diagnostics
            .iter()
            .any(|d| d.code == Some(CompileCode::TransitionHandleClipped)
                && d.severity == DiagSeverity::Warning));
        let clip = &mut project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[1];
        clip.transition_in = None;
        clip.transition_out = Some(timeline::Transition::new(
            TransitionKind::CrossDissolve,
            Tick(20),
        ));
        let fade = compile_native_preview(&project, sequence, 0, Tick(190), Quality::FULL);
        assert!(fade.diagnostics.is_empty(), "{:?}", fade.diagnostics);
        assert!(fade
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op,IrOp::Merge { opacity,.. } if opacity==0.5)));
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[1]
            .transition_out
            .as_mut()
            .unwrap()
            .duration = Tick(101);
        assert!(
            compile_native_preview(&project, sequence, 0, Tick(190), Quality::FULL)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error)
        );
    }

    #[test]
    fn native_catalog_outgoing_edges_follow_eased_fade_contract() {
        let (base, sid, _, _) = native_group_look_fixture();
        for gap in [false, true] {
            for curve in [
                EaseCurve::Linear,
                EaseCurve::EaseIn,
                EaseCurve::EaseOut,
                EaseCurve::EaseInOut,
            ] {
                let mut baseline = base.clone();
                let track = &mut baseline.sequences.get_mut(&sid).unwrap().video_tracks[0];
                track.clips.truncate(1);
                track.clips[0].start = Tick::ZERO;
                track.clips[0].duration = Tick(100);
                track.clips[0].transition_in = None;
                let mut transition =
                    timeline::Transition::new(TransitionKind::CrossDissolve, Tick(40));
                transition.params.curve = curve;
                track.clips[0].transition_out = Some(transition);
                if gap {
                    let mut next = track.clips[0].clone();
                    next.id = ClipId::new();
                    next.start = Tick(140);
                    next.transition_out = None;
                    track.clips.push(next);
                }
                for kind in [
                    TransitionKind::CrossDissolve,
                    TransitionKind::DipToBlack,
                    TransitionKind::DipToColor,
                    TransitionKind::Wipe,
                    TransitionKind::Push,
                    TransitionKind::LumaWipe,
                ] {
                    let mut variant = baseline.clone();
                    variant.sequences.get_mut(&sid).unwrap().video_tracks[0].clips[0]
                        .transition_out
                        .as_mut()
                        .unwrap()
                        .kind = kind;
                    for tick in [0, 59, 60, 70, 80, 90, 99, 100, 120] {
                        let reference =
                            compile_native_preview(&baseline, sid, 0, Tick(tick), Quality::FULL);
                        let frame =
                            compile_native_preview(&variant, sid, 0, Tick(tick), Quality::FULL);
                        assert!(
                            frame.diagnostics.is_empty(),
                            "{kind:?}/{curve:?}/{tick}: {:?}",
                            frame.diagnostics
                        );
                        if tick == 70 {
                            let opacity = match curve {
                                EaseCurve::Linear => 0.75,
                                EaseCurve::EaseIn => 0.9375,
                                EaseCurve::EaseOut => 0.5625,
                                EaseCurve::EaseInOut => 0.875,
                            };
                            assert!(frame.graph.nodes.iter().any(|n| matches!(n.op, IrOp::Merge { opacity: actual, .. } if actual == opacity)));
                        }
                        assert_eq!(
                            frame.graph.nodes[frame.graph.output.unwrap().0 as usize].content_hash,
                            reference.graph.nodes[reference.graph.output.unwrap().0 as usize]
                                .content_hash,
                            "outgoing {kind:?} must retain gap/end fade semantics"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn native_extended_transition_sources_keep_scope_order_and_bounded_handles() {
        let (mut base, sid, _, _) = native_group_look_fixture();
        let media = *base.media.assets.keys().next().unwrap();
        base.media.assets.get_mut(&media).unwrap().probe =
            Some(timeline::MediaProbe::basic(Tick(250), "mp4", "h264"));
        let rate = base.sequences[&sid].frame_rate;
        let mut nest = Sequence::new("source nest", rate, 16, 16);
        nest.color = base.sequences[&sid].color.clone();
        let mut nt = Track::new(TrackKind::Video, "inner");
        nt.clips
            .push(solid_clip(Color::new(0.2, 0.4, 0.8, 0.5), 0, 1000));
        nest.video_tracks.push(nt);
        let nested = nest.id;
        base.insert_sequence(nest);
        let vector = base.media.insert(MediaAsset::new(
            AssetKind::VectorDoc,
            timeline::AssetSource::EmbeddedVector {
                root: VectorRef::WholeDocument,
            },
        ));
        let track = &mut base.sequences.get_mut(&sid).unwrap().video_tracks[0];
        track.clips[0].duration = Tick(100);
        let mut incoming = track.clips[0].clone();
        incoming.id = ClipId::new();
        incoming.start = Tick(100);
        incoming.transition_in = Some(timeline::Transition::new(
            TransitionKind::CrossDissolve,
            Tick(40),
        ));
        track.clips.push(incoming);
        for source in [
            ClipSource::Text {
                content: TextClipContent::new("HI"),
            },
            ClipSource::Vector { asset: vector },
            ClipSource::NestedSequence { sequence: nested },
        ] {
            for mask in 1..=3 {
                for kind in [
                    TransitionKind::CrossDissolve,
                    TransitionKind::DipToBlack,
                    TransitionKind::DipToColor,
                    TransitionKind::Wipe,
                    TransitionKind::Push,
                    TransitionKind::LumaWipe,
                ] {
                    let mut p = base.clone();
                    let track = &mut p.sequences.get_mut(&sid).unwrap().video_tracks[0];
                    for (i, c) in track.clips.iter_mut().enumerate() {
                        if mask & (1 << i) != 0 {
                            c.source = source.clone();
                        }
                    }
                    track.clips[1].transition_in.as_mut().unwrap().kind = kind;
                    for tick in [100, 110, 120, 139, 140] {
                        let c = compile_native_preview(&p, sid, 0, Tick(tick), Quality::FULL);
                        assert!(
                            c.diagnostics.is_empty(),
                            "{source:?} mask{mask} {kind:?} {tick}: {:?}",
                            c.diagnostics
                        );
                        assert!(c.graph.validate_working_color_domain().is_ok());
                        assert_eq!(
                            c.graph
                                .nodes
                                .iter()
                                .filter(|n| matches!(n.op, IrOp::NativeSdrOutput))
                                .count(),
                            1,
                            "one outer output transform"
                        );
                        for stops in [5.0, 6.0] {
                            assert_eq!(
                                c.graph
                                    .nodes
                                    .iter()
                                    .filter(
                                        |n| matches!(n.op,IrOp::NativeExposure{stops:s}if s==stops)
                                    )
                                    .count(),
                                1,
                                "track/master applied once"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn native_nested_transition_speed_handle_clamp_and_scene_math_are_explicit() {
        let (mut p, outer, inner) = native_nest_fixture();
        let f = p.sequences[&outer].frame_rate.ticks_per_frame().0;
        p.sequences.get_mut(&inner).unwrap().video_tracks[0].clips[0].duration = Tick(10 * f);
        let tr = &mut p.sequences.get_mut(&outer).unwrap().video_tracks[0];
        tr.clips[0].duration = Tick(4 * f);
        tr.clips[0].source_in = Tick(f);
        tr.clips[0].speed = SpeedMap::Constant(Ratio::new(2, 1));
        let mut incoming = solid_clip(Color::TRANSPARENT, 4 * f, 4 * f);
        incoming.transition_in = Some(timeline::Transition::new(
            TransitionKind::CrossDissolve,
            Tick(2 * f),
        ));
        incoming.transition_in.as_mut().unwrap().params.curve = EaseCurve::Linear;
        tr.clips.push(incoming);
        assert_eq!(
            available_handle_ticks(&p, &p.sequences[&outer].video_tracks[0].clips[0]),
            Some(Tick(f / 2))
        );
        let mut c = compile_native_preview(&p, outer, 0, Tick(4 * f + f / 4), Quality::FULL);
        assert!(
            !c.diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error),
            "{:?}",
            c.diagnostics
        );
        assert!(c
            .diagnostics
            .iter()
            .any(|d| d.code == Some(CompileCode::TransitionHandleClipped)
                && d.severity == DiagSeverity::Info));
        assert!(c
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op,IrOp::NativeDissolve{t}if t==0.5)));
        let mut decoded = 0;
        for n in &mut c.graph.nodes {
            if let IrOp::NativeDecodeVideo { src_time, .. } = n.op {
                assert_eq!(src_time, Tick(19 * f / 2));
                decoded += 1;
                n.op = IrOp::SolidColor {
                    color: LinearColor {
                        r: -0.05,
                        g: 0.1,
                        b: 0.5,
                        a: 0.5,
                    },
                };
            }
        }
        assert_eq!(decoded, 1);
        let output = c.graph.output.unwrap();
        let scene = c.graph.nodes[output.0 as usize].inputs[0].0;
        c.graph.nodes.truncate(scene.0 as usize + 1);
        c.graph.output = Some(scene);
        let image = crate::graph::eval_cpu::evaluate(
            &c.graph,
            (2, 2),
            &mut crate::graph::eval_cpu::EmptyProvider,
        );
        for pixel in image.pixels {
            for (a, b) in pixel.into_iter().zip([-0.4, 0.8, 4.0, 0.25]) {
                assert!((a - b).abs() < 1e-6, "{pixel:?}");
            }
        }
        p.sequences.get_mut(&inner).unwrap().video_tracks[0].clips[0].duration = Tick(9 * f);
        let c = compile_native_preview(&p, outer, 0, Tick(4 * f + f / 4), Quality::FULL);
        assert!(c
            .diagnostics
            .iter()
            .any(|d| d.code == Some(CompileCode::TransitionHandleClipped)
                && d.severity == DiagSeverity::Warning));
        assert!(!c
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeDissolve { .. })));
        p.sequences.get_mut(&inner).unwrap().video_tracks[0].clips[0].duration = Tick(10 * f);
        let mut captions = CaptionTrack::new("unsupported nested output");
        captions.cues.push(CaptionCue::new(
            Tick::ZERO,
            Tick(10 * f),
            vec![CaptionWord::new("caption", Tick::ZERO, Tick(10 * f))],
        ));
        p.sequences
            .get_mut(&inner)
            .unwrap()
            .caption_tracks
            .push(captions);
        let c = compile_native_preview(&p, outer, 0, Tick(4 * f + f / 4), Quality::FULL);
        assert!(c
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error));
    }

    #[test]
    fn native_solid_transition_partners_have_unbounded_handles_and_scoped_grades() {
        let (mut base, sid, _, _) = native_group_look_fixture();
        let asset = *base.media.assets.keys().next().unwrap();
        base.media.assets.get_mut(&asset).unwrap().probe =
            Some(timeline::MediaProbe::basic(Tick(250), "mp4", "h264"));
        let track = &mut base.sequences.get_mut(&sid).unwrap().video_tracks[0];
        track.clips[0].duration = Tick(100);
        let mut incoming = track.clips[0].clone();
        incoming.id = ClipId::new();
        incoming.start = Tick(100);
        incoming.transition_in = Some(timeline::Transition::new(
            TransitionKind::CrossDissolve,
            Tick(40),
        ));
        track.clips.push(incoming);
        for source_mask in 1..=3 {
            for kind in [
                TransitionKind::CrossDissolve,
                TransitionKind::DipToBlack,
                TransitionKind::DipToColor,
                TransitionKind::Wipe,
                TransitionKind::Push,
                TransitionKind::LumaWipe,
            ] {
                let mut p = base.clone();
                let track = &mut p.sequences.get_mut(&sid).unwrap().video_tracks[0];
                for (index, clip) in track.clips.iter_mut().enumerate() {
                    if source_mask & (1 << index) != 0 {
                        clip.source = ClipSource::SolidColor {
                            color: Color {
                                r: 0.2,
                                g: 0.4,
                                b: 0.8,
                                a: 0.5,
                            },
                        };
                        assert_eq!(available_handle_ticks(&base, clip), None);
                    }
                }
                let tr = track.clips[1].transition_in.as_mut().unwrap();
                tr.kind = kind;
                tr.params.softness = 0.2;
                for tick in [100, 110, 120, 139, 140] {
                    let compiled = compile_native_preview(&p, sid, 0, Tick(tick), Quality::FULL);
                    assert!(
                        compiled.diagnostics.is_empty(),
                        "mask{source_mask} {kind:?} t{tick}: {:?}",
                        compiled.diagnostics
                    );
                    assert!(compiled.graph.validate_working_color_domain().is_ok());
                    for stops in [5.0, 6.0] {
                        assert_eq!(compiled.graph.nodes.iter().filter(|node| matches!(node.op,IrOp::NativeExposure {stops:s} if s==stops)).count(),1,"track/master must apply once");
                    }
                    assert_eq!(compiled.clip_taps.len(), if tick < 140 { 2 } else { 1 });
                }
            }
        }
        // Nested partners remain gated even though ordinary nested clips can render.
        let track = &mut base.sequences.get_mut(&sid).unwrap().video_tracks[0];
        track.clips[0].source = ClipSource::NestedSequence { sequence: sid };
        assert!(
            compile_native_preview(&base, sid, 0, Tick(120), Quality::FULL)
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error)
        );
    }

    #[test]
    fn native_solid_end_dissolve_retains_alpha_and_reveals_transparency() {
        use timeline::color::{NativeManagedColorConfig, SequenceColorConfig};
        let (mut project, sid) = base_project();
        project.sequences.get_mut(&sid).unwrap().color =
            SequenceColorConfig::NativeManaged(Box::new(NativeManagedColorConfig::sdr_draft()));
        let tid = add_video_track(&mut project, sid);
        let mut clip = solid_clip(
            Color {
                r: 0.3,
                g: 0.6,
                b: 0.9,
                a: 0.5,
            },
            0,
            100,
        );
        clip.transition_out = Some(timeline::Transition::new(
            TransitionKind::CrossDissolve,
            Tick(20),
        ));
        clip.transition_out.as_mut().unwrap().params.curve = EaseCurve::Linear;
        project.sequences.get_mut(&sid).unwrap().video_tracks[tid]
            .clips
            .push(clip);
        for (tick, expected) in [
            (0, 0.5),
            (79, 0.5),
            (80, 0.5),
            (90, 0.25),
            (99, 0.025),
            (100, 0.0),
        ] {
            let compiled = compile_native_preview(&project, sid, 0, Tick(tick), Quality::FULL);
            assert!(
                compiled.diagnostics.is_empty(),
                "{:?}",
                compiled.diagnostics
            );
            let pixels = super::super::eval_cpu::evaluate(
                &compiled.graph,
                (2, 1),
                &mut super::super::eval_cpu::EmptyProvider,
            );
            assert!(
                (pixels.pixels[0][3] - expected).abs() < 1e-6,
                "t{tick}: {:?}",
                pixels.pixels[0]
            );
        }
    }

    #[test]
    fn window_matte_lowers_animated_geometry_and_ignores_color_parameters() {
        let (mut project, sid, _, _) = native_group_look_fixture();
        let seq = project.sequences.get_mut(&sid).unwrap();
        seq.master_grade = None;
        seq.video_tracks[0].grade = None;
        let clip = &mut seq.video_tracks[0].clips[0];
        clip.group = None;
        clip.look = None;
        clip.start = Tick(100);
        clip.duration = Tick(200);
        clip.source = ClipSource::SolidColor {
            color: Color::WHITE,
        };
        clip.speed = SpeedMap::Constant(Ratio::new(2, 1));
        let mut op = GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 3.0 },
        );
        op.mask = Some(timeline::GradeMask::PowerWindow {
            shape: timeline::WindowShape::Ellipse,
            center: [0.25, 0.5],
            size: [0.2, 0.3],
            rotation: 0.2,
            softness: 0.25,
            invert: false,
        });
        for (at, value) in [(0, 0.25), (100, 0.75)] {
            op.params
                .track_mut(&PropPath::new("mask.center[0]"))
                .insert_keyframe(timeline::Keyframe::new(
                    Tick(at),
                    PropValue::Float(value),
                    timeline::Interp::Linear,
                ));
        }
        let id = op.id;
        let mut grade = Grade::new();
        grade.ops.push(op);
        grade.convert_to_graph();
        let key = grade
            .add_graph_utility(GradeGraphNode::WindowMatte {
                input: 0,
                op: id,
                label: String::new(),
            })
            .unwrap();
        grade
            .add_graph_utility(GradeGraphNode::MatteApply {
                original: 0,
                corrected: 0,
                matte: key,
                label: String::new(),
            })
            .unwrap();
        clip.grade = Some(grade);
        let mut hashes = Vec::new();
        for (tick, center) in [(100, 0.25), (150, 0.5), (200, 0.75), (100, 0.25)] {
            let c = compile_native_preview(&project, sid, 0, Tick(tick), Quality::FULL);
            assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
            let (index, node) = c
                .graph
                .nodes
                .iter()
                .enumerate()
                .find(|(_, n)| matches!(n.op, IrOp::WindowMatte { .. }))
                .unwrap();
            assert!(matches!(node.op,IrOp::WindowMatte {mask} if mask.center[0]==center));
            assert_eq!(
                c.graph.node_color_encoding(IrNodeId(index as u32)).unwrap(),
                crate::graph::ir::FrameColorEncoding::MatteWeight
            );
            assert!(
                !c.graph
                    .nodes
                    .iter()
                    .any(|n| matches!(n.op, IrOp::NativeExposure { .. })),
                "parked RGB correction must not run"
            );
            hashes.push(node.content_hash);
        }
        assert_ne!(hashes[0], hashes[1]);
        assert_ne!(hashes[1], hashes[2]);
        assert_eq!(hashes[0], hashes[3]);
        project.sequences.get_mut(&sid).unwrap().video_tracks[0].clips[0]
            .grade
            .as_mut()
            .unwrap()
            .ops[0]
            .params
            .base = GradeOpParams::Exposure { stops: -6.0 };
        let c = compile_native_preview(&project, sid, 0, Tick(100), Quality::FULL);
        assert_eq!(
            c.graph
                .nodes
                .iter()
                .find(|n| matches!(n.op, IrOp::WindowMatte { .. }))
                .unwrap()
                .content_hash,
            hashes[0]
        );
        project.sequences.get_mut(&sid).unwrap().video_tracks[0].clips[0]
            .grade
            .as_mut()
            .unwrap()
            .ops[0]
            .mask = None;
        let c = compile_native_preview(&project, sid, 0, Tick(100), Quality::FULL);
        assert!(c.diagnostics.is_empty());
        assert!(c
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op,IrOp::GradeMatteConstant {weight} if weight==1.0)));
        project.sequences.get_mut(&sid).unwrap().video_tracks[0].clips[0]
            .grade
            .as_mut()
            .unwrap()
            .ops[0]
            .enabled = false;
        let c = compile_native_preview(&project, sid, 0, Tick(100), Quality::FULL);
        assert!(c.diagnostics.is_empty());
        assert!(c
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op,IrOp::GradeMatteConstant {weight} if weight==0.0)));
    }

    #[test]
    fn animated_bezier_stage_clocks_hashes_and_degenerate_fail_closed() {
        use timeline::{
            BezierWindow, BezierWindowInterpolation, BezierWindowKey, BezierWindowTrack,
        };
        let make_grade = |start: i64, degenerate: bool| {
            let window = BezierWindow::default();
            let mut end = window.clone();
            for knot in &mut end.knots {
                for point in [&mut knot.anchor, &mut knot.incoming, &mut knot.outgoing] {
                    if degenerate {
                        point[0] = 1.0 - point[0];
                        point[1] = 1.0 - point[1];
                    } else {
                        point[0] += 0.2;
                    }
                }
            }
            let mut path = BezierWindowTrack::new(Tick(start), window).unwrap();
            path.keys.push(BezierWindowKey {
                at: Tick(start + 100),
                window: end,
                interpolation: BezierWindowInterpolation::Linear,
            });
            let mut grade = Grade::new();
            grade.convert_to_graph();
            let key = grade
                .add_graph_utility(GradeGraphNode::AnimatedBezierMatte {
                    input: 0,
                    track: path,
                    label: "Animated shape".into(),
                })
                .unwrap();
            grade
                .add_graph_utility(GradeGraphNode::MatteApply {
                    original: 0,
                    corrected: 0,
                    matte: key,
                    label: String::new(),
                })
                .unwrap();
            grade
        };
        for scope in 0..8 {
            let (mut project, sid, gid, look) = native_group_look_fixture();
            for media in project.media.assets.values_mut() {
                media.grade = None;
            }
            project.shared_looks.get_mut(&look).unwrap().grade = Grade::new();
            let seq = project.sequences.get_mut(&sid).unwrap();
            seq.master_grade = None;
            for group in seq.groups.values_mut() {
                group.pre_grade = None;
                group.post_grade = None;
            }
            seq.video_tracks[0].grade = None;
            let clip = &mut seq.video_tracks[0].clips[0];
            clip.start = Tick(100);
            clip.grade = None;
            let asset = match clip.source {
                ClipSource::Asset { asset } => asset,
                _ => unreachable!(),
            };
            let grade = make_grade(if scope == 5 || scope == 6 { 100 } else { 0 }, false);
            match scope {
                0 => clip.grade = Some(grade),
                1 => clip.look = Some(timeline::ClipLook::Local(Box::new(grade))),
                2 => project.shared_looks.get_mut(&look).unwrap().grade = grade,
                3 => seq.groups.get_mut(&gid).unwrap().pre_grade = Some(grade),
                4 => seq.groups.get_mut(&gid).unwrap().post_grade = Some(grade),
                5 => seq.video_tracks[0].grade = Some(grade),
                6 => seq.master_grade = Some(grade),
                7 => project.media.assets.get_mut(&asset).unwrap().grade = Some(grade),
                _ => unreachable!(),
            }
            let mut hashes = Vec::new();
            for (tick, x) in [(100, 0.5), (150, 0.6), (200, 0.7)] {
                let compiled = compile_native_preview(&project, sid, 0, Tick(tick), Quality::FULL);
                assert!(
                    compiled.diagnostics.is_empty(),
                    "scope {scope}: {:?}",
                    compiled.diagnostics
                );
                let nodes: Vec<_> = compiled
                    .graph
                    .nodes
                    .iter()
                    .filter_map(|n| match &n.op {
                        IrOp::BezierMatte { window } => Some((window, n.content_hash)),
                        _ => None,
                    })
                    .collect();
                assert_eq!(nodes.len(), 1);
                assert!(
                    (nodes[0].0.knots[0].anchor[0] - x).abs() < 1e-6,
                    "scope {scope}"
                );
                hashes.push(nodes[0].1);
            }
            assert_ne!(hashes[0], hashes[1]);
            assert_ne!(hashes[1], hashes[2]);
        }
        let (mut project, sid, _, _) = native_group_look_fixture();
        let clip = &mut project.sequences.get_mut(&sid).unwrap().video_tracks[0].clips[0];
        let cid = clip.id;
        clip.grade = Some(make_grade(0, true));
        let compiled = compile_native_preview(&project, sid, 0, Tick(50), Quality::FULL);
        assert!(compiled
            .diagnostics
            .iter()
            .any(|d| d
                .grade
                .as_ref()
                .is_some_and(|g| g.owner == Some(VfxOwner::Clip(cid))
                    && g.graph_node.is_some()
                    && matches!(
                        g.issue,
                        photonic_render::grade::GradeIssue::InvalidWindowAnimation(_)
                    ))));
        let image = crate::graph::eval_cpu::evaluate(
            &compiled.graph,
            (16, 16),
            &mut crate::graph::eval_cpu::EmptyProvider,
        );
        assert!(
            image.pixels.iter().all(|p| *p == [0.0; 4]),
            "native error boundary must produce transparent rejection, not full-frame correction"
        );
    }

    #[test]
    fn bezier_matte_hash_validation_and_lowering() {
        let window = timeline::BezierWindow::default();
        let baseline = content_hash(
            &IrOp::BezierMatte {
                window: window.clone(),
            },
            &[],
            &[],
        );
        for field in 0..8 {
            let mut changed = window.clone();
            match field {
                0..=1 => changed.knots[0].anchor[field] += 0.02,
                2..=3 => changed.knots[0].incoming[field - 2] += 0.02,
                4..=5 => changed.knots[0].outgoing[field - 4] += 0.02,
                6 => changed.feather += 0.01,
                _ => changed.invert = true,
            }
            assert_ne!(
                baseline,
                content_hash(&IrOp::BezierMatte { window: changed }, &[], &[])
            );
        }
        let (mut project, sid, _, _) = native_group_look_fixture();
        let seq = project.sequences.get_mut(&sid).unwrap();
        seq.master_grade = None;
        seq.video_tracks[0].grade = None;
        let clip = &mut seq.video_tracks[0].clips[0];
        clip.group = None;
        clip.look = None;
        clip.source = ClipSource::SolidColor {
            color: Color::WHITE,
        };
        let mut grade = Grade::new();
        grade.convert_to_graph();
        let key = grade
            .add_graph_utility(GradeGraphNode::BezierMatte {
                input: 0,
                window: window.clone(),
                label: "Cubic window".into(),
            })
            .unwrap();
        grade
            .add_graph_utility(GradeGraphNode::MatteApply {
                original: 0,
                corrected: 0,
                matte: key,
                label: String::new(),
            })
            .unwrap();
        clip.grade = Some(grade);
        let c = compile_native_preview(&project, sid, 0, Tick(0), Quality::FULL);
        assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
        let (index, node) = c
            .graph
            .nodes
            .iter()
            .enumerate()
            .find(|(_, n)| matches!(n.op, IrOp::BezierMatte { .. }))
            .unwrap();
        assert_eq!(
            c.graph.node_color_encoding(IrNodeId(index as u32)).unwrap(),
            crate::graph::ir::FrameColorEncoding::MatteWeight
        );
        assert!(matches!(&node.op,IrOp::BezierMatte{window:actual} if actual==&window));
        let mut bad = c.graph.clone();
        bad.nodes[index].inputs.clear();
        assert!(bad.validate_working_color_domain().is_err());
        bad = c.graph.clone();
        if let IrOp::BezierMatte { window } = &mut bad.nodes[index].op {
            window.knots[0].anchor[0] = f32::NAN;
        }
        assert!(bad.validate_working_color_domain().is_err());
    }

    #[test]
    fn window_matte_hash_and_domain_include_all_geometry_fields() {
        use photonic_render::grade::ResolvedMask;
        let mask = ResolvedMask {
            shape: timeline::WindowShape::Ellipse,
            center: [0.5; 2],
            size: [0.25; 2],
            rotation: 0.0,
            softness: 0.2,
            invert: false,
        };
        let baseline = content_hash(&IrOp::WindowMatte { mask }, &[], &[]);
        for field in 0..8 {
            let mut m = mask;
            match field {
                0 => m.shape = timeline::WindowShape::Rectangle,
                1 => m.center[0] += 0.1,
                2 => m.center[1] += 0.1,
                3 => m.size[0] += 0.1,
                4 => m.size[1] += 0.1,
                5 => m.rotation += 0.1,
                6 => m.softness += 0.1,
                _ => m.invert = true,
            }
            assert_ne!(
                content_hash(&IrOp::WindowMatte { mask: m }, &[], &[]),
                baseline
            );
        }
        let source = IrNode {
            op: IrOp::SolidColor {
                color: LinearColor {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
            },
            inputs: vec![],
            content_hash: ContentHash(1),
        };
        // Missing input, a matte input and invalid geometry all fail closed.
        let mut graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![
                source,
                IrNode {
                    op: IrOp::WindowMatte { mask },
                    inputs: vec![(IrNodeId(0), OutPort::default())],
                    content_hash: ContentHash(2),
                },
            ],
            output: Some(IrNodeId(0)),
        };
        assert!(graph.validate_working_color_domain().is_ok());
        graph.nodes[1].inputs.clear();
        assert!(graph.validate_working_color_domain().is_err());
        graph.nodes[1].inputs = vec![(IrNodeId(0), OutPort::default())];
        graph.nodes[0].op = IrOp::GradeMatteConstant { weight: 0.5 };
        assert!(graph.validate_working_color_domain().is_err());
        graph.nodes[0].op = IrOp::SolidColor {
            color: LinearColor {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
        };
        let mut invalid = mask;
        invalid.size[0] = 0.0;
        graph.nodes[1].op = IrOp::WindowMatte { mask: invalid };
        assert!(graph.validate_working_color_domain().is_err());
    }

    #[test]
    fn animated_window_ticks_are_clip_relative_not_source_retimed_and_hash_resolved_geometry() {
        let (mut project, sid, _, _) = native_group_look_fixture();
        let clip = &mut project.sequences.get_mut(&sid).unwrap().video_tracks[0].clips[0];
        clip.start = Tick(100);
        clip.duration = Tick(200);
        clip.source = ClipSource::SolidColor {
            color: Color::WHITE,
        };
        clip.speed = SpeedMap::Constant(Ratio::new(2, 1));
        let mut op = timeline::GradeOp::new(
            timeline::GradeOpKind::Exposure,
            timeline::GradeOpParams::Exposure { stops: 1.0 },
        );
        op.mask = Some(timeline::GradeMask::PowerWindow {
            shape: timeline::WindowShape::Rectangle,
            center: [0.25, 0.5],
            size: [0.1; 2],
            rotation: 0.0,
            softness: 0.2,
            invert: false,
        });
        for (at, value) in [(0, 0.25), (100, 0.75)] {
            op.params
                .track_mut(&PropPath::new("mask.center[0]"))
                .insert_keyframe(timeline::Keyframe::new(
                    Tick(at),
                    PropValue::Float(value),
                    timeline::Interp::Linear,
                ));
        }
        let mut grade = timeline::Grade::new();
        grade.ops.push(op);
        clip.grade = Some(grade);
        let mut hashes = Vec::new();
        for (tick, center) in [(100, 0.25), (150, 0.5), (200, 0.75), (100, 0.25)] {
            let compiled = compile_native_preview(&project, sid, 0, Tick(tick), Quality::FULL);
            assert!(
                compiled.diagnostics.is_empty(),
                "{:?}",
                compiled.diagnostics
            );
            assert!(compiled.graph.nodes.iter().any(
                |node| matches!(node.op,IrOp::NativeMaskMix {mask} if mask.center[0]==center)
            ));
            hashes
                .push(compiled.graph.nodes[compiled.graph.output.unwrap().0 as usize].content_hash);
        }
        assert_ne!(hashes[0], hashes[1]);
        assert_ne!(hashes[1], hashes[2]);
        assert_eq!(hashes[0], hashes[3]);
        let op = &mut project.sequences.get_mut(&sid).unwrap().video_tracks[0].clips[0]
            .grade
            .as_mut()
            .unwrap()
            .ops[0];
        op.kind = timeline::GradeOpKind::HslQualifier;
        op.params.base = timeline::GradeOpParams::HslQualifier {
            hue: [0.0, 1.0],
            sat: [0.0, 1.0],
            lum: [0.0, 1.0],
            softness: 0.1,
            correction: timeline::CdlParams::identity(),
            keys: Vec::new(),
            matte_levels: [0.0; 2],
        };
        let compiled = compile_native_preview(&project, sid, 0, Tick(150), Quality::FULL);
        assert!(
            compiled.diagnostics.is_empty(),
            "{:?}",
            compiled.diagnostics
        );
        assert_eq!(compiled.native_qualifier_inspections.len(), 1);
        assert_eq!(
            compiled.native_qualifier_inspections[0]
                .mask
                .unwrap()
                .center[0],
            0.5,
            "raw qualifier inspection must use the same animated window sample"
        );
    }

    #[test]
    fn native_scene_program_tap_precedes_only_the_output_transform_and_never_aliases_legacy() {
        let (mut project, sid, _, _) = native_group_look_fixture();
        for compiled in [
            compile_native_preview(&project, sid, 0, Tick::ZERO, Quality::FULL),
            compile_native_delivery(&project, sid, 0, Tick::ZERO, Quality::FULL),
        ] {
            assert!(compiled
                .diagnostics
                .iter()
                .all(|d| d.severity != DiagSeverity::Error));
            let output = compiled.tap(ScopeTapPoint::Program).unwrap();
            let scene = compiled.tap(ScopeTapPoint::NativeSceneProgram).unwrap();
            assert_eq!(compiled.graph.nodes[output.0 as usize].inputs[0].0, scene);
            assert!(matches!(
                compiled.graph.nodes[scene.0 as usize].op,
                IrOp::NativeCanvas { .. }
            ));
            assert_eq!(
                compiled.graph.node_color_encoding(scene).unwrap(),
                crate::graph::ir::FrameColorEncoding::SceneLinearAcescg
            );
            assert_ne!(scene, output);
        }
        project.sequences.get_mut(&sid).unwrap().color = Default::default();
        let legacy = compile(&project, sid, 0, Tick::ZERO, Quality::FULL, None);
        assert!(legacy.tap(ScopeTapPoint::NativeSceneProgram).is_none());
    }

    #[test]
    fn native_exposure_cache_identity_includes_stops_and_operator_kind() {
        let exposure = |stops| content_hash(&IrOp::NativeExposure { stops }, &[], &[]);
        assert_ne!(exposure(0.0), exposure(1.0));
        assert_ne!(exposure(-1.0), exposure(1.0));
        let offset = |rgb| content_hash(&IrOp::NativeLinearOffset { rgb }, &[], &[]);
        assert_ne!(offset([0.0; 3]), offset([0.1, 0.0, 0.0]));
        assert_ne!(offset([0.1, 0.0, 0.0]), offset([0.0, 0.1, 0.0]));
        assert_ne!(offset([0.0; 3]), exposure(0.0));
        let printer = |points| content_hash(&IrOp::NativePrinterLights { points }, &[], &[]);
        assert_ne!(printer([0.0; 3]), printer([12.0, 0.0, 0.0]));
        assert_ne!(printer([12.0, 0.0, 0.0]), printer([0.0, 12.0, 0.0]));
        assert_ne!(printer([0.0; 3]), offset([0.0; 3]));
        let rolloff = |knee, strength| {
            content_hash(&IrOp::NativeHighlightRolloff { knee, strength }, &[], &[])
        };
        assert_ne!(rolloff(1.0, 0.5), rolloff(2.0, 0.5));
        assert_ne!(rolloff(1.0, 0.5), rolloff(1.0, 1.0));
        assert_ne!(rolloff(1.0, 0.5), printer([0.0; 3]));
        assert_ne!(
            exposure(0.0),
            content_hash(&IrOp::Grade { ops: vec![] }, &[], &[])
        );
        let saturation = |saturation, vibrance| {
            content_hash(
                &IrOp::NativeSaturationVibrance {
                    saturation,
                    vibrance,
                },
                &[],
                &[],
            )
        };
        assert_ne!(saturation(1.0, 0.0), saturation(1.1, 0.0));
        assert_ne!(saturation(1.0, 0.0), saturation(1.0, 0.1));
        assert_ne!(saturation(1.0, 0.0), exposure(0.0));
        let transfer = |direction| content_hash(&IrOp::NativeAcescct { direction }, &[], &[]);
        assert_ne!(
            transfer(photonic_render::native_transfer::AcescctDirection::Encode),
            transfer(photonic_render::native_transfer::AcescctDirection::Decode)
        );
        assert_ne!(
            content_hash(&IrOp::NativeSdrOutput, &[], &[]),
            transfer(photonic_render::native_transfer::AcescctDirection::Decode)
        );
        assert_ne!(
            content_hash(&IrOp::NativeSdrOutput, &[], &[]),
            content_hash(&IrOp::NativeSdrVideoOutput, &[], &[])
        );
    }

    #[test]
    fn native_video_source_cache_identity_includes_input_interpretation() {
        use photonic_core::timeline::color::{
            InputMatrix, InputSignalRange, NativeInputColorInterpretation, NativeInputStandard,
        };
        let asset = AssetId::new();
        let mut input = NativeInputColorInterpretation {
            hlg_peak_nits: None,
            reference_white_nits: None,
            version: 1,
            standard: NativeInputStandard::Bt709Scene,
            range: InputSignalRange::Limited,
            matrix: InputMatrix::Bt709,
            chroma_location: None,
        };
        let hash = |input: NativeInputColorInterpretation| {
            content_hash(
                &IrOp::NativeDecodeVideo {
                    asset,
                    src_time: Tick(0),
                    input,
                },
                &[],
                &[],
            )
        };
        let limited = hash(input.clone());
        input.range = InputSignalRange::Full;
        assert_ne!(limited, hash(input.clone()));
        input.range = InputSignalRange::Limited;
        input.chroma_location = Some(photonic_core::timeline::color::NativeChromaLocation::Left);
        assert_ne!(limited, hash(input));
        assert_ne!(
            limited,
            content_hash(
                &IrOp::DecodeVideo {
                    asset,
                    src_time: Tick(0),
                    proxy: false
                },
                &[],
                &[]
            )
        );
    }

    #[test]
    fn native_source_lowering_prefers_clip_override_and_rejects_missing_input() {
        use photonic_core::timeline::color::{
            InputMatrix, InputSignalRange, NativeInputColorInterpretation, NativeInputStandard,
            NativeManagedColorConfig, SequenceColorConfig,
        };
        let (mut project, sequence_id) = base_project();
        let mut asset = MediaAsset::from_file(AssetKind::Video, "/missing/native-source.mp4");
        let asset_id = asset.id;
        let bt709 = NativeInputColorInterpretation {
            hlg_peak_nits: None,
            reference_white_nits: None,
            version: 1,
            standard: NativeInputStandard::Bt709Scene,
            range: InputSignalRange::Limited,
            matrix: InputMatrix::Bt709,
            chroma_location: None,
        };
        let bt2020 = NativeInputColorInterpretation {
            standard: NativeInputStandard::Bt2020Scene,
            matrix: InputMatrix::Bt2020NonConstant,
            ..bt709.clone()
        };
        asset.native_input_color = Some(bt709.clone());
        project.media.assets.insert(asset_id, asset);
        let mut seq = project.sequences[&sequence_id].clone();
        seq.color =
            SequenceColorConfig::NativeManaged(Box::new(NativeManagedColorConfig::sdr_draft()));
        let mut clip = Clip::new(
            ClipSource::Asset { asset: asset_id },
            Tick(0),
            Tick(1_000_000),
        );
        clip.native_input_color = Some(bt2020.clone());
        let lowered = |project: &TimelineProject, clip: &Clip| {
            let mut builder = Builder::new();
            let mut cycle = HashSet::new();
            let node = build_clip_source(
                &mut builder,
                &project,
                &seq,
                0,
                &seq.formats[0],
                clip,
                Tick(0),
                Quality::FULL,
                &mut cycle,
            );
            (
                builder.nodes[node.0 as usize].op.clone(),
                builder.diagnostics,
            )
        };
        assert!(
            matches!(lowered(&project, &clip).0, IrOp::NativeDecodeVideo { input, .. } if input == bt2020)
        );
        clip.native_input_color = None;
        assert!(
            matches!(lowered(&project, &clip).0, IrOp::NativeDecodeVideo { input, .. } if input == bt709)
        );
        project
            .media
            .assets
            .get_mut(&asset_id)
            .unwrap()
            .native_input_color = None;
        let (op, diagnostics) = lowered(&project, &clip);
        assert!(matches!(op, IrOp::SolidColor { .. }));
        assert!(diagnostics
            .iter()
            .any(|d| d.code == Some(CompileCode::ColorPipelineUnavailable)));
    }
    use photonic_core::color::Color;
    use photonic_core::timeline::{
        Clip, FrameRate, GraphEdge, GraphNode, GraphOp, InPort, MediaAsset, NodeGraph,
        OutPort as GOutPort, Sequence, Track, TrackKind,
    };

    #[test]
    fn managed_grade_lowers_scene_primaries_and_rejects_legacy_math() {
        let mut builder = Builder::new();
        builder.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
        let source = builder.push(
            IrOp::SolidColor {
                color: LinearColor {
                    r: 2.0,
                    g: 0.5,
                    b: 0.25,
                    a: 1.0,
                },
            },
            vec![],
        );
        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        ));
        grade.ops.push(GradeOp::new(
            GradeOpKind::LinearOffset,
            GradeOpParams::LinearOffset {
                rgb: [0.1, 0.0, 0.0],
            },
        ));
        grade.ops.push(GradeOp::new(
            GradeOpKind::PrinterLights,
            GradeOpParams::PrinterLights {
                points: [12.0, 0.0, -12.0],
            },
        ));
        grade.ops.push(GradeOp::new(
            GradeOpKind::HighlightRolloff,
            GradeOpParams::HighlightRolloff {
                knee: 1.0,
                strength: 0.5,
            },
        ));
        grade.ops.push(GradeOp::new(
            GradeOpKind::WhiteBalance,
            GradeOpParams::WhiteBalance {
                temp: 2.0,
                tint: 0.0,
            },
        ));
        let output = apply_grade(&mut builder, &grade, source, Tick(0), None);
        let frame = builder.finish(Some(output));
        assert_eq!(
            frame.graph.working_color_domain,
            WorkingColorDomain::SceneLinearAcescg
        );
        assert!(
            matches!(frame.graph.nodes[output.0 as usize].op, IrOp::NativeHighlightRolloff { knee, strength } if knee == 1.0 && strength == 0.5)
        );
        assert!(frame.graph.nodes.iter().any(|node| matches!(node.op, IrOp::NativePrinterLights { points } if points == [12.0, 0.0, -12.0])));
        assert!(frame.graph.nodes.iter().any(
            |node| matches!(node.op, IrOp::NativeLinearOffset { rgb } if rgb == [0.1, 0.0, 0.0])
        ));
        assert!(frame
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::NativeExposure { stops } if stops == 1.0)));
        assert!(!frame
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::Grade { .. })));
        assert!(frame
            .diagnostics
            .iter()
            .any(|diag| diag.code == Some(CompileCode::ColorPipelineUnavailable)));
    }

    #[test]
    fn managed_serial_grade_graph_uses_native_scene_operators() {
        let mut builder = Builder::new();
        builder.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
        let source = builder.push(
            IrOp::SolidColor {
                color: LinearColor {
                    r: 2.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            },
            vec![],
        );
        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        ));
        grade.ops.push(GradeOp::new(
            GradeOpKind::LinearOffset,
            GradeOpParams::LinearOffset {
                rgb: [-0.5, 0.0, 0.0],
            },
        ));
        grade.graph = Some(GradeGraph::from_stack(&grade.ops));
        let output = apply_grade(&mut builder, &grade, source, Tick(0), None);
        let frame = builder.finish(Some(output));
        assert!(frame.diagnostics.is_empty());
        assert!(frame.graph.validate_working_color_domain().is_ok());
        assert!(
            matches!(frame.graph.nodes[output.0 as usize].op, IrOp::NativeLinearOffset { rgb } if rgb == [-0.5, 0.0, 0.0])
        );
        assert!(frame
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::NativeExposure { stops } if stops == 1.0)));
        assert!(!frame
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::Grade { .. })));
    }

    #[test]
    fn invalid_native_primary_is_diagnosed_before_graph_evaluation() {
        let mut builder = Builder::new();
        builder.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
        let source = builder.push(
            IrOp::SolidColor {
                color: LinearColor {
                    r: 1.0,
                    g: 0.5,
                    b: 0.25,
                    a: 1.0,
                },
            },
            vec![],
        );
        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: f32::NAN },
        ));
        grade.ops.push(GradeOp::new(
            GradeOpKind::LinearOffset,
            GradeOpParams::LinearOffset {
                rgb: [0.1, 0.0, 0.0],
            },
        ));
        let output = apply_grade(&mut builder, &grade, source, Tick(0), None);
        let frame = builder.finish(Some(output));
        assert!(frame.diagnostics.iter().any(|finding| finding.code
            == Some(CompileCode::ColorPipelineUnavailable)
            && finding.message.contains("native exposure must be finite")));
        assert!(!frame
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::NativeExposure { .. })));
        assert!(matches!(
            frame.graph.nodes[output.0 as usize].op,
            IrOp::NativeLinearOffset { .. }
        ));
        assert!(frame.graph.validate_working_color_domain().is_ok());
    }

    #[test]
    fn serial_grading_graph_preserves_corrector_order_at_clip_stage() {
        let (mut project, sequence_id) = base_project();
        let track_index = add_video_track(&mut project, sequence_id);
        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        ));
        grade.ops.push(GradeOp::new(
            GradeOpKind::LinearOffset,
            GradeOpParams::LinearOffset {
                rgb: [0.1, 0.0, 0.0],
            },
        ));
        let mut clip = solid_clip(
            Color {
                r: 0.2,
                g: 0.2,
                b: 0.2,
                a: 1.0,
            },
            0,
            1000,
        );
        clip.grade = Some(grade.clone());
        project
            .sequences
            .get_mut(&sequence_id)
            .unwrap()
            .video_tracks[track_index]
            .clips
            .push(clip);
        let legacy = compile(&project, sequence_id, 0, Tick(0), Quality::FULL, None);
        let legacy_ops: Vec<_> = legacy
            .graph
            .nodes
            .iter()
            .filter_map(|node| match &node.op {
                IrOp::Grade { ops } => Some(ops.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        project
            .sequences
            .get_mut(&sequence_id)
            .unwrap()
            .video_tracks[track_index]
            .clips[0]
            .grade
            .as_mut()
            .unwrap()
            .graph = Some(GradeGraph::from_stack(&grade.ops));
        let converted = compile(&project, sequence_id, 0, Tick(0), Quality::FULL, None);
        let graph_ops: Vec<_> = converted
            .graph
            .nodes
            .iter()
            .filter_map(|node| match &node.op {
                IrOp::Grade { ops } => Some(ops.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert!(converted.diagnostics.is_empty());
        assert_eq!(graph_ops, legacy_ops);
        let legacy_image = crate::graph::eval_cpu::evaluate(
            &legacy.graph,
            (4, 4),
            &mut crate::graph::eval_cpu::EmptyProvider,
        );
        let converted_image = crate::graph::eval_cpu::evaluate(
            &converted.graph,
            (4, 4),
            &mut crate::graph::eval_cpu::EmptyProvider,
        );
        assert_eq!(converted_image.pixels, legacy_image.pixels);
        let mut expected = project.clone();
        let expected_grade = expected
            .sequences
            .get_mut(&sequence_id)
            .unwrap()
            .video_tracks[track_index]
            .clips[0]
            .grade
            .as_mut()
            .unwrap();
        expected_grade.graph = None;
        expected_grade.ops.pop();
        let expected_frame = compile(&expected, sequence_id, 0, Tick(0), Quality::FULL, None);
        let graph_grade = project
            .sequences
            .get_mut(&sequence_id)
            .unwrap()
            .video_tracks[track_index]
            .clips[0]
            .grade
            .as_mut()
            .unwrap();
        let second_node = graph_grade
            .graph
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find_map(|(id, node)| {
                matches!(node, GradeGraphNode::Corrector { op, .. } if *op == grade.ops[1].id)
                    .then_some(*id)
            })
            .unwrap();
        graph_grade.remove_graph_node(second_node).unwrap();
        let removed_frame = compile(&project, sequence_id, 0, Tick(0), Quality::FULL, None);
        let expected_image = crate::graph::eval_cpu::evaluate(
            &expected_frame.graph,
            (4, 4),
            &mut crate::graph::eval_cpu::EmptyProvider,
        );
        let removed_image = crate::graph::eval_cpu::evaluate(
            &removed_frame.graph,
            (4, 4),
            &mut crate::graph::eval_cpu::EmptyProvider,
        );
        assert_eq!(removed_image.pixels, expected_image.pixels);
    }

    #[test]
    fn graph_corrector_failure_identifies_its_node_and_owner() {
        let (mut project, sequence_id) = base_project();
        let track_index = add_video_track(&mut project, sequence_id);
        let missing_lut = AssetId::new();
        let mut grade = single_lut_grade(missing_lut);
        let op_id = grade.ops[0].id;
        grade.convert_to_graph();
        let node_id = grade
            .graph
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find_map(|(id, node)| {
                matches!(node, GradeGraphNode::Corrector { op, .. } if *op == op_id).then_some(*id)
            })
            .unwrap();
        let mut clip = solid_clip(Color::WHITE, 0, 1000);
        let clip_id = clip.id;
        clip.grade = Some(grade);
        project
            .sequences
            .get_mut(&sequence_id)
            .unwrap()
            .video_tracks[track_index]
            .clips
            .push(clip);
        let frame = compile(&project, sequence_id, 0, Tick(0), Quality::FULL, None);
        assert!(frame.diagnostics.iter().any(|finding| {
            finding.grade.as_ref().is_some_and(|diagnostic| {
                diagnostic.op == op_id
                    && diagnostic.graph_node == Some(node_id)
                    && diagnostic.owner == Some(VfxOwner::Clip(clip_id))
                    && matches!(diagnostic.issue, photonic_render::grade::GradeIssue::MissingLut(id) if id == missing_lut)
                    && finding.message.contains(&format!("Graph node {node_id}"))
            })
        }));
    }

    #[test]
    fn invalid_grading_graph_blocks_export_with_coded_error() {
        let (mut project, sequence_id) = base_project();
        let track_index = add_video_track(&mut project, sequence_id);
        let mut grade = Grade::new();
        grade.graph = Some(GradeGraph::from_stack(&[GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        )]));
        let mut clip = solid_clip(
            Color {
                r: 0.2,
                g: 0.2,
                b: 0.2,
                a: 1.0,
            },
            0,
            1000,
        );
        clip.grade = Some(grade);
        project
            .sequences
            .get_mut(&sequence_id)
            .unwrap()
            .video_tracks[track_index]
            .clips
            .push(clip);
        let frame = compile(&project, sequence_id, 0, Tick(0), Quality::FULL, None);
        assert!(frame
            .diagnostics
            .iter()
            .any(|d| d.code == Some(CompileCode::GradeUnresolved)
                && d.severity == DiagSeverity::Error));
    }

    #[test]
    fn parallel_grading_graph_mixes_corrected_and_original_branches() {
        let (mut project, sequence_id) = base_project();
        let track_index = add_video_track(&mut project, sequence_id);
        let op = GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        );
        let mut grade = Grade::new();
        grade.convert_to_graph();
        grade.add_graph_corrector(op, true).unwrap();
        let mut clip = solid_clip(
            Color {
                r: 0.2,
                g: 0.2,
                b: 0.2,
                a: 1.0,
            },
            0,
            1000,
        );
        clip.grade = Some(grade);
        project
            .sequences
            .get_mut(&sequence_id)
            .unwrap()
            .video_tracks[track_index]
            .clips
            .push(clip);
        let frame = compile(&project, sequence_id, 0, Tick(0), Quality::FULL, None);
        assert!(frame.diagnostics.is_empty());
        assert!(frame.graph.nodes.iter().any(|node| matches!(node.op,
            IrOp::Merge { opacity, .. } if opacity == 0.5)));
        let image = crate::graph::eval_cpu::evaluate(
            &frame.graph,
            (4, 4),
            &mut crate::graph::eval_cpu::EmptyProvider,
        );
        let mut clean_project = project.clone();
        clean_project
            .sequences
            .get_mut(&sequence_id)
            .unwrap()
            .video_tracks[track_index]
            .clips[0]
            .grade = None;
        let clean = compile(&clean_project, sequence_id, 0, Tick(0), Quality::FULL, None);
        let clean_image = crate::graph::eval_cpu::evaluate(
            &clean.graph,
            (4, 4),
            &mut crate::graph::eval_cpu::EmptyProvider,
        );
        let mut full_project = project;
        full_project
            .sequences
            .get_mut(&sequence_id)
            .unwrap()
            .video_tracks[track_index]
            .clips[0]
            .grade
            .as_mut()
            .unwrap()
            .graph = None;
        let full = compile(&full_project, sequence_id, 0, Tick(0), Quality::FULL, None);
        let full_image = crate::graph::eval_cpu::evaluate(
            &full.graph,
            (4, 4),
            &mut crate::graph::eval_cpu::EmptyProvider,
        );
        let mixed = image.pixels[0][0];
        let ungraded = clean_image.pixels[0][0];
        let graded = full_image.pixels[0][0];
        assert!(
            ungraded < mixed && mixed < graded,
            "{ungraded} < {mixed} < {graded}"
        );
    }

    #[test]
    fn group_pre_clip_and_group_post_grades_keep_their_stage_order() {
        use photonic_core::timeline::{
            Grade, GradeOp, GradeOpKind, GradeOpParams, GroupKind, GroupNode,
        };
        use photonic_render::grade::ResolvedGradePayload;

        let exposure = |stops| {
            let mut grade = Grade::new();
            grade.ops.push(GradeOp::new(
                GradeOpKind::Exposure,
                GradeOpParams::Exposure { stops },
            ));
            grade
        };
        let (mut project, sequence_id) = base_project();
        let track_index = add_video_track(&mut project, sequence_id);
        let sequence = project.sequences.get_mut(&sequence_id).unwrap();
        let mut group = GroupNode::new(GroupKind::Normal);
        group.pre_grade = Some(exposure(1.0));
        group.post_grade = Some(exposure(3.0));
        let mut parent = GroupNode::new(GroupKind::Normal);
        parent.pre_grade = Some(exposure(0.5));
        parent.post_grade = Some(exposure(4.0));
        group.parent = Some(parent.id);
        let group_id = group.id;
        sequence.groups.insert(parent.id, parent);
        sequence.groups.insert(group_id, group);
        let mut clip = solid_clip(
            Color {
                r: 0.2,
                g: 0.2,
                b: 0.2,
                a: 1.0,
            },
            0,
            1000,
        );
        clip.group = Some(group_id);
        clip.grade = Some(exposure(2.0));
        sequence.video_tracks[track_index].clips.push(clip);
        let mut sibling = solid_clip(
            Color {
                r: 0.2,
                g: 0.2,
                b: 0.2,
                a: 1.0,
            },
            1000,
            1000,
        );
        sibling.group = Some(group_id);
        sequence.video_tracks[track_index].clips.push(sibling);
        let compiled = compile(&project, sequence_id, 0, Tick(0), Quality::FULL, None);
        let stages: Vec<f32> = compiled
            .graph
            .nodes
            .iter()
            .filter_map(|node| {
                let IrOp::Grade { ops } = &node.op else {
                    return None;
                };
                match &ops[0].payload {
                    ResolvedGradePayload::Exposure { stops } => Some(*stops),
                    _ => None,
                }
            })
            .collect();
        assert_eq!(stages, vec![0.5, 1.0, 2.0, 3.0, 4.0]);
        let sibling_frame = compile(&project, sequence_id, 0, Tick(1000), Quality::FULL, None);
        let sibling_stages: Vec<f32> = sibling_frame
            .graph
            .nodes
            .iter()
            .filter_map(|node| {
                let IrOp::Grade { ops } = &node.op else {
                    return None;
                };
                match &ops[0].payload {
                    ResolvedGradePayload::Exposure { stops } => Some(*stops),
                    _ => None,
                }
            })
            .collect();
        assert_eq!(sibling_stages, vec![0.5, 1.0, 3.0, 4.0]);
        let clean = compile_with_luts_and_opts(
            &project,
            sequence_id,
            0,
            Tick(0),
            Quality::FULL,
            None,
            None,
            true,
        );
        assert!(!clean
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::Grade { .. })));
    }

    #[test]
    fn shared_look_stage_resolves_and_missing_reference_blocks_export() {
        use photonic_core::timeline::{
            ClipLook, Grade, GradeOp, GradeOpKind, GradeOpParams, SharedLook, SharedLookId,
        };
        use photonic_render::grade::{GradeIssue, GradeStage, ResolvedGradePayload};

        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        ));
        let id = SharedLookId::new();
        let (mut project, sequence_id) = base_project();
        let track_index = add_video_track(&mut project, sequence_id);
        let sequence = project.sequences.get_mut(&sequence_id).unwrap();
        let mut clip = solid_clip(Color::WHITE, 0, 1000);
        clip.look = Some(ClipLook::Shared(id));
        sequence.video_tracks[track_index].clips.push(clip);
        let missing = compile(&project, sequence_id, 0, Tick(0), Quality::FULL, None);
        assert!(missing.diagnostics.iter().any(|diagnostic| diagnostic
            .grade
            .as_ref()
            .is_some_and(
                |grade| matches!(grade.issue, GradeIssue::MissingSharedLook(found) if found == id)
            )));
        project.shared_looks.insert(
            id,
            SharedLook {
                versions: Vec::new(),
                active_version: None,
                id,
                name: "Scene".into(),
                grade: grade.clone(),
            },
        );
        let resolved = compile(&project, sequence_id, 0, Tick(0), Quality::FULL, None);
        assert!(resolved
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.grade.is_none()));
        assert!(resolved.graph.nodes.iter().any(|node| matches!(&node.op, IrOp::Grade { ops } if matches!(ops[0].payload, ResolvedGradePayload::Exposure { stops } if stops == 1.0))));
        project
            .sequences
            .get_mut(&sequence_id)
            .unwrap()
            .video_tracks[track_index]
            .clips[0]
            .look = Some(ClipLook::Local(Box::new(grade)));
        let local = compile(&project, sequence_id, 0, Tick(0), Quality::FULL, None);
        assert_eq!(
            resolved.graph.nodes[resolved.graph.output.unwrap().0 as usize].content_hash,
            local.graph.nodes[local.graph.output.unwrap().0 as usize].content_hash
        );
        let mut missing_lut = Grade::new();
        missing_lut.ops.push(GradeOp::new(
            GradeOpKind::Lut3d,
            GradeOpParams::Lut3d {
                asset: AssetId::new(),
                intensity: 1.0,
                interp: timeline::LutInterp::Trilinear,
            },
        ));
        project.shared_looks.get_mut(&id).unwrap().grade = missing_lut.clone();
        project
            .sequences
            .get_mut(&sequence_id)
            .unwrap()
            .video_tracks[track_index]
            .clips[0]
            .look = Some(ClipLook::Shared(id));
        let shared_failure = compile(&project, sequence_id, 0, Tick(0), Quality::FULL, None);
        assert!(shared_failure.diagnostics.iter().any(|finding| finding.grade.as_ref()
            .is_some_and(|grade| matches!(grade.stage, Some(GradeStage::SharedLook { id: found }) if found == id))));
        project
            .sequences
            .get_mut(&sequence_id)
            .unwrap()
            .video_tracks[track_index]
            .clips[0]
            .look = Some(ClipLook::Local(Box::new(missing_lut)));
        let local_failure = compile(&project, sequence_id, 0, Tick(0), Quality::FULL, None);
        assert!(local_failure.diagnostics.iter().any(|finding| finding
            .grade
            .as_ref()
            .is_some_and(|grade| matches!(grade.stage, Some(GradeStage::LocalLook)))));
    }

    #[test]
    fn legacy_sdr_rejects_tagged_hdr_source_before_decode() {
        use photonic_core::timeline::{
            AssetKind, Clip, ClipSource, MediaAsset, MediaProbe, ProbedColor, VideoStreamInfo,
        };
        let (mut project, sequence_id) = base_project();
        let track_index = add_video_track(&mut project, sequence_id);
        let mut asset = MediaAsset::from_file(AssetKind::Video, "/tmp/pq.mov");
        let mut probe = MediaProbe::basic(Tick(1000), "mov", "prores");
        probe.video = Some(VideoStreamInfo {
            width: 16,
            height: 16,
            frame_rate: FrameRate::FPS_30,
            pixel_aspect: 1.0,
            color: ProbedColor {
                transfer: Some("smpte2084".into()),
                ..Default::default()
            },
            keyframe_index_cached: false,
            scan: Default::default(),
        });
        asset.probe = Some(probe);
        let id = asset.id;
        project.media.insert(asset);
        let clip = Clip::new(ClipSource::Asset { asset: id }, Tick(0), Tick(1000));
        let clip_id = clip.id;
        project
            .sequences
            .get_mut(&sequence_id)
            .unwrap()
            .video_tracks[track_index]
            .clips
            .push(clip);
        let frame = compile(&project, sequence_id, 0, Tick(0), Quality::FULL, None);
        assert!(frame.diagnostics.iter().any(|finding| finding.code
            == Some(CompileCode::ColorPipelineUnavailable)
            && finding.clip == Some(clip_id)
            && finding.message.contains("smpte2084")));
        assert!(!frame
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::DecodeVideo { asset, .. } if asset == id)));
        let peek = compile_asset_peek(&project, id, Tick(0), Quality::FULL, 16, 16);
        assert!(peek
            .diagnostics
            .iter()
            .any(|finding| finding.code == Some(CompileCode::ColorPipelineUnavailable)));

        for (color, expected) in [
            (
                ProbedColor {
                    matrix: Some("bt2020nc".into()),
                    ..Default::default()
                },
                "bt2020nc matrix",
            ),
            (
                ProbedColor {
                    primaries: Some("smpte432".into()),
                    ..Default::default()
                },
                "smpte432 primaries",
            ),
        ] {
            project
                .media
                .assets
                .get_mut(&id)
                .unwrap()
                .probe
                .as_mut()
                .unwrap()
                .video
                .as_mut()
                .unwrap()
                .color = color;
            let frame = compile(&project, sequence_id, 0, Tick(0), Quality::FULL, None);
            assert!(frame.diagnostics.iter().any(|finding| finding.code
                == Some(CompileCode::ColorPipelineUnavailable)
                && finding.message.contains(expected)));
            assert!(!frame
                .graph
                .nodes
                .iter()
                .any(|node| matches!(node.op, IrOp::DecodeVideo { asset, .. } if asset == id)));
            let peek = compile_asset_peek(&project, id, Tick(0), Quality::FULL, 16, 16);
            assert!(peek.diagnostics.iter().any(|finding| finding.code
                == Some(CompileCode::ColorPipelineUnavailable)
                && finding.message.contains(expected)));
        }
    }

    #[test]
    fn missing_group_lut_identifies_the_group_stage() {
        use photonic_core::timeline::{GroupKind, GroupNode};

        let (mut project, sequence_id) = base_project();
        let track_index = add_video_track(&mut project, sequence_id);
        let sequence = project.sequences.get_mut(&sequence_id).unwrap();
        let mut group = GroupNode::new(GroupKind::Normal);
        group.pre_grade = Some(single_lut_grade(AssetId::new()));
        let group_id = group.id;
        sequence.groups.insert(group_id, group);
        for start in [0, 1000] {
            let mut clip = solid_clip(Color::WHITE, start, 1000);
            clip.group = Some(group_id);
            sequence.video_tracks[track_index].clips.push(clip);
        }
        let compiled = compile(&project, sequence_id, 0, Tick(0), Quality::FULL, None);
        assert!(compiled.diagnostics.iter().any(|diagnostic| {
            diagnostic.grade.as_ref().is_some_and(|grade| {
                grade.owner == Some(VfxOwner::GroupPre(group_id))
                    && matches!(
                        grade.issue,
                        photonic_render::grade::GradeIssue::MissingLut(_)
                    )
            })
        }));
    }

    #[test]
    fn window_shape_changes_grade_cache_key() {
        use photonic_core::timeline::WindowShape;
        use photonic_render::grade::{ResolvedGradePayload, ResolvedMask};

        let digest = |shape| {
            let mut hash = xxhash_rust::xxh3::Xxh3::new();
            hash_resolved_grade_op(
                &mut hash,
                &crate::contract::ResolvedGradeOp {
                    payload: ResolvedGradePayload::Exposure { stops: 1.0 },
                    mask: Some(ResolvedMask {
                        shape,
                        center: [0.5, 0.5],
                        size: [0.25, 0.25],
                        rotation: 0.0,
                        softness: 0.1,
                        invert: false,
                    }),
                },
            );
            hash.digest128()
        };
        assert_ne!(digest(WindowShape::Ellipse), digest(WindowShape::Rectangle));
        assert_ne!(digest(WindowShape::Ellipse), digest(WindowShape::Gradient));
        assert_ne!(
            digest(WindowShape::Rectangle),
            digest(WindowShape::Gradient)
        );
    }

    #[test]
    fn linear_offset_channels_invalidate_grade_cache_key() {
        use photonic_render::grade::ResolvedGradePayload;
        let digest = |rgb: [f32; 3]| {
            let mut hash = xxhash_rust::xxh3::Xxh3::new();
            hash_resolved_grade_op(
                &mut hash,
                &crate::contract::ResolvedGradeOp {
                    payload: ResolvedGradePayload::LinearOffset { rgb },
                    mask: None,
                },
            );
            hash.digest()
        };
        let neutral = digest([0.0; 3]);
        for rgb in [[0.1, 0.0, 0.0], [0.0, 0.1, 0.0], [0.0, 0.0, 0.1]] {
            assert_ne!(digest(rgb), neutral);
        }
    }

    #[test]
    fn highlight_rolloff_controls_invalidate_grade_cache_key() {
        use photonic_render::grade::ResolvedGradePayload;
        let digest = |knee, strength| {
            let mut hash = xxhash_rust::xxh3::Xxh3::new();
            hash_resolved_grade_op(
                &mut hash,
                &crate::contract::ResolvedGradeOp {
                    payload: ResolvedGradePayload::HighlightRolloff { knee, strength },
                    mask: None,
                },
            );
            hash.digest()
        };
        assert_ne!(digest(1.0, 0.5), digest(1.5, 0.5));
        assert_ne!(digest(1.0, 0.5), digest(1.0, 0.8));
    }

    #[test]
    fn saturation_vibrance_controls_invalidate_grade_cache_key() {
        use photonic_render::grade::ResolvedGradePayload;
        let digest = |saturation, vibrance| {
            let mut hash = xxhash_rust::xxh3::Xxh3::new();
            hash_resolved_grade_op(
                &mut hash,
                &crate::contract::ResolvedGradeOp {
                    payload: ResolvedGradePayload::SaturationVibrance {
                        saturation,
                        vibrance,
                    },
                    mask: None,
                },
            );
            hash.digest()
        };
        assert_ne!(digest(1.0, 0.0), digest(1.2, 0.0));
        assert_ne!(digest(1.0, 0.0), digest(1.0, 0.3));
    }

    #[test]
    fn advanced_curve_changes_invalidate_grade_cache_key() {
        use photonic_render::grade::{ResolvedCurves, ResolvedGradePayload};
        let identity = photonic_render::grade::curve_lut(&[]);
        let base = ResolvedCurves {
            master: identity,
            red: identity,
            green: identity,
            blue: identity,
            hue_vs_hue: None,
            hue_vs_sat: None,
            hue_vs_luma: None,
            luma_vs_sat: None,
            sat_vs_sat: None,
        };
        let digest = |curves: ResolvedCurves| {
            let mut hash = xxhash_rust::xxh3::Xxh3::new();
            hash_resolved_grade_op(
                &mut hash,
                &crate::contract::ResolvedGradeOp {
                    payload: ResolvedGradePayload::Curves(Box::new(curves)),
                    mask: None,
                },
            );
            hash.digest()
        };
        let original = digest(base.clone());
        let changed = photonic_render::grade::curve_lut(&[(0.0, 0.5), (1.0, 0.7)]);
        let mut hue_luma = base.clone();
        hue_luma.hue_vs_luma = Some(changed);
        let mut luma_sat = base.clone();
        luma_sat.luma_vs_sat = Some(changed);
        let mut sat_sat = base;
        sat_sat.sat_vs_sat = Some(changed);
        for key in [digest(hue_luma), digest(luma_sat), digest(sat_sat)] {
            assert_ne!(key, original);
        }
    }

    // ---- Task 5: Typewriter reveal by grapheme cluster (42 §6.5) ----

    fn caption_word(text: &str, start: i64, end: i64) -> CaptionWord {
        CaptionWord::new(text, Tick(start), Tick(end))
    }

    #[test]
    fn reveal_ascii_is_byte_identical_per_scalar() {
        // "hello" over [0, 500] reveals one more char per 100-tick step — pins
        // the no-regression claim for ASCII.
        let w = caption_word("hello", 0, 500);
        let steps = ["h", "he", "hel", "hell", "hello"];
        for (i, want) in steps.iter().enumerate() {
            let tick = Tick(((i + 1) as i64) * 100);
            assert_eq!(
                reveal_text("hello", CaptionAnim::Typewriter, &w, tick),
                *want
            );
        }
    }

    #[test]
    fn reveal_never_splits_a_devanagari_cluster() {
        // नमस्ते — every revealed prefix is a whole number of grapheme clusters,
        // so the output never begins with a combining matra/virama.
        let text = "\u{0928}\u{092E}\u{0938}\u{094D}\u{0924}\u{0947}";
        let clusters: Vec<&str> = photonic_core::text_metrics::graphemes(text).collect();
        let w = caption_word(text, 0, 600);
        for step in 0..=12 {
            let tick = Tick(step * 50);
            let out = reveal_text(text, CaptionAnim::Typewriter, &w, tick);
            // Output equals the first k whole clusters for some k.
            let k = photonic_core::text_metrics::graphemes(&out).count();
            let expected: String = clusters.iter().take(k).copied().collect();
            assert_eq!(out, expected);
            // Never starts with the virama (U+094D) or matra (U+0947).
            if let Some(c) = out.chars().next() {
                assert!(c != '\u{094D}' && c != '\u{0947}');
            }
        }
    }

    #[test]
    fn reveal_emoji_zwj_is_all_or_nothing() {
        // A ZWJ family emoji is one grapheme cluster: the reveal is either empty
        // or the whole sequence, never a partial ZWJ run.
        let text = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        let w = caption_word(text, 0, 400);
        for step in 0..=8 {
            let tick = Tick(step * 50);
            let out = reveal_text(text, CaptionAnim::Typewriter, &w, tick);
            assert!(out.is_empty() || out == text, "partial ZWJ run: {out:?}");
        }
    }

    #[test]
    fn reveal_non_typewriter_returns_full_text() {
        let text = "\u{65E5}\u{672C}\u{8A9E}";
        let w = caption_word(text, 0, 300);
        for anim in [
            CaptionAnim::None,
            CaptionAnim::FadeWords,
            CaptionAnim::SlideUp,
        ] {
            for step in 0..=6 {
                assert_eq!(reveal_text(text, anim, &w, Tick(step * 50)), text);
            }
        }
    }

    fn base_project() -> (TimelineProject, SequenceId) {
        let mut project = TimelineProject::new();
        let seq = Sequence::new("seq", FrameRate::FPS_30, 320, 180);
        let id = seq.id;
        project.insert_sequence(seq);
        (project, id)
    }

    fn add_video_track(project: &mut TimelineProject, seq_id: SequenceId) -> usize {
        let seq = project.sequences.get_mut(&seq_id).unwrap();
        seq.video_tracks.push(Track::new(TrackKind::Video, "V1"));
        seq.video_tracks.len() - 1
    }

    fn solid_clip(color: Color, start: i64, dur: i64) -> Clip {
        Clip::new(ClipSource::SolidColor { color }, Tick(start), Tick(dur))
    }

    fn assert_point_close(actual: Vec2, expected: Vec2) {
        assert!(
            (actual - expected).length() < 1e-4,
            "actual {actual:?}, expected {expected:?}"
        );
    }

    /// The whole point of the compile-pass integration: a measured clip gets its
    /// solved gain injected into the lowered `Effect` params, and an unmeasured
    /// one does not (so it lowers as an exact pass-through).
    #[test]
    fn deflicker_gain_is_injected_only_for_measured_clips() {
        use crate::graph::deflicker::{ClipGains, GainTable};

        let (mut project, seq_id) = base_project();
        let tk = add_video_track(&mut project, seq_id);
        let mut clip = solid_clip(Color::WHITE, 0, Tick::from_seconds(2).0);
        clip.effects.push(photonic_core::timeline::ClipEffect::new(
            EffectKind::Deflicker,
        ));
        let clip_id = clip.id;
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(clip);

        let gain_of = |out: &CompiledFrame| -> Option<f32> {
            out.graph.nodes.iter().find_map(|n| match &n.op {
                IrOp::Effect {
                    kind: EffectKind::Deflicker,
                    params,
                } => params.get("params.gain_r").map(|v| match v {
                    PropValue::Float(f) => *f as f32,
                    _ => f32::NAN,
                }),
                _ => None,
            })
        };

        // No store: the effect lowers, but carries no gain.
        let bare = compile(&project, seq_id, 0, Tick(0), Quality::PREVIEW, None);
        assert!(
            bare.graph.nodes.iter().any(|n| matches!(
                &n.op,
                IrOp::Effect {
                    kind: EffectKind::Deflicker,
                    ..
                }
            )),
            "the deflicker effect should still lower without a measurement"
        );
        assert_eq!(gain_of(&bare), None, "no measurement ⇒ no gain param");

        // With a measurement, the gain for this tick is injected verbatim.
        let mut table = GainTable::new();
        table.insert(
            clip_id,
            ClipGains {
                first: Tick(0),
                step: Tick::from_seconds(1).0,
                gains: vec![[1.25, 1.25, 1.25], [0.80, 0.80, 0.80]],
                band: None,
                frame_ticks: 1,
            },
        );
        let measured = compile_full(
            &project,
            seq_id,
            0,
            Tick(0),
            Quality::PREVIEW,
            None,
            None,
            false,
            Some(&table),
            None,
        );
        assert_eq!(gain_of(&measured), Some(1.25));

        // A later tick picks up the later sample — the gain really is per-frame.
        let later = compile_full(
            &project,
            seq_id,
            0,
            Tick::from_seconds(1),
            Quality::PREVIEW,
            None,
            None,
            false,
            Some(&table),
            None,
        );
        assert_eq!(gain_of(&later), Some(0.80));

        // A different clip's id must not pick up this clip's gains.
        let mut other = GainTable::new();
        other.insert(
            photonic_core::timeline::ClipId::new(),
            ClipGains {
                first: Tick(0),
                step: 1,
                gains: vec![[2.0; 3]],
                band: None,
                frame_ticks: 1,
            },
        );
        let unrelated = compile_full(
            &project,
            seq_id,
            0,
            Tick(0),
            Quality::PREVIEW,
            None,
            None,
            false,
            Some(&other),
            None,
        );
        assert_eq!(gain_of(&unrelated), None);
    }

    #[test]
    fn default_clip_transform_matrix_is_identity() {
        let format = SequenceFormat::new("test", 320, 180);
        assert_eq!(
            clip_transform_matrix(&ClipTransform::default(), &format),
            Mat3::IDENTITY
        );
    }

    #[test]
    fn zero_anchor_rotation_preserves_frame_center() {
        let transform = ClipTransform {
            rotation: 0.4,
            ..ClipTransform::default()
        };
        let format = SequenceFormat::new("test", 320, 180);
        let center = Vec2::new(160.0, 90.0);
        assert_point_close(
            clip_transform_matrix(&transform, &format).transform_point2(center),
            center,
        );
    }

    #[test]
    fn clip_position_offsets_frame_center() {
        let transform = ClipTransform {
            x: 12.0,
            y: -7.0,
            ..ClipTransform::default()
        };
        let format = SequenceFormat::new("test", 320, 180);
        let center = Vec2::new(160.0, 90.0);
        assert_point_close(
            clip_transform_matrix(&transform, &format).transform_point2(center),
            center + Vec2::new(12.0, -7.0),
        );
    }

    #[test]
    fn nonzero_anchor_is_relative_to_frame_center() {
        let transform = ClipTransform {
            rotation: 0.4,
            anchor_x: 20.0,
            anchor_y: -10.0,
            ..ClipTransform::default()
        };
        let format = SequenceFormat::new("test", 320, 180);
        let pivot = Vec2::new(180.0, 80.0);
        assert_point_close(
            clip_transform_matrix(&transform, &format).transform_point2(pivot),
            pivot,
        );
    }

    #[test]
    fn absolute_anchor_preserves_legacy_top_left_pivot() {
        let transform = ClipTransform {
            rotation: 0.4,
            anchor_space: AnchorSpace::Absolute,
            anchor_x: 0.0,
            anchor_y: 0.0,
            ..ClipTransform::default()
        };
        let format = SequenceFormat::new("test", 320, 180);
        let top_left = Vec2::ZERO;
        assert_point_close(
            clip_transform_matrix(&transform, &format).transform_point2(top_left),
            top_left,
        );
        let center = Vec2::new(160.0, 90.0);
        assert!(
            (clip_transform_matrix(&transform, &format).transform_point2(center) - center).length()
                > 1.0
        );
    }

    #[test]
    fn animated_transform_preserves_anchor_space() {
        let mut anim = AnimProps::new(ClipTransform {
            anchor_space: AnchorSpace::Absolute,
            ..ClipTransform::default()
        });
        let mut track = timeline::PropertyTrack::new("transform.anchor_x");
        track.insert_keyframe(timeline::Keyframe::new(
            Tick(0),
            PropValue::Float(4.0),
            timeline::Interp::Linear,
        ));
        anim.tracks.push(track);
        assert_eq!(
            eval_clip_transform(&anim, Tick(0)).anchor_space,
            AnchorSpace::Absolute
        );
    }

    #[test]
    fn skip_clip_looks_omits_clip_effect_ops() {
        // K-B5: clean compile must not emit Effect IR for the clip stack, while
        // the full compile does — proving the bypass path is real, not a no-op.
        let (mut project, seq_id) = base_project();
        let tk = add_video_track(&mut project, seq_id);
        let mut clip = solid_clip(Color::WHITE, 0, Tick::from_seconds(2).0);
        clip.effects
            .push(photonic_core::timeline::ClipEffect::new(EffectKind::Invert));
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(clip);

        let full = compile(&project, seq_id, 0, Tick(0), Quality::PREVIEW, None);
        let clean = compile_with_luts_and_opts(
            &project,
            seq_id,
            0,
            Tick(0),
            Quality::PREVIEW,
            None,
            None,
            true,
        );
        let has_effect = |g: &crate::graph::ir::FrameGraph| {
            g.nodes.iter().any(|n| matches!(n.op, IrOp::Effect { .. }))
        };
        assert!(
            has_effect(&full.graph),
            "full compile must lower the Invert effect"
        );
        assert!(
            !has_effect(&clean.graph),
            "skip_clip_looks must fold out the clip effect stack"
        );
        assert!(
            full.graph.nodes.len() > clean.graph.nodes.len(),
            "clean graph should be strictly smaller (fewer ops)"
        );
    }

    #[test]
    fn compare_clean_preserves_stabilization_provider() {
        use crate::graph::stabilize::{
            BiasEstimate, FrameCorrection, StabilizationAnalysis, StabilizationCache,
            StabilizationDiagnostics,
        };
        use photonic_core::timeline::{
            LensProfileRef, MotionBinding, MotionFormat, MotionSourceRef, StabilizationSpec,
        };
        use std::path::PathBuf;

        let (mut project, seq_id) = base_project();
        let tk = add_video_track(&mut project, seq_id);
        let mut clip = solid_clip(Color::WHITE, 0, Tick::from_seconds(2).0);
        let mut spec = StabilizationSpec::new(MotionBinding {
            source: MotionSourceRef::Sidecar {
                path: PathBuf::from("/missing/stabilization.gcsv"),
                rel_path: None,
                format: MotionFormat::Gcsv,
            },
            sync: Default::default(),
            lens: LensProfileRef::RotationOnly,
        });
        spec.analysis_key = Some("analysis-key".into());
        clip.stabilization = Some(spec);
        let clip_id = clip.id;
        let source_range = crate::graph::stabilize::source_time_range(&clip);
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(clip);

        let mut stabilization = StabilizationCache::default();
        stabilization.insert(
            clip_id,
            "analysis-key".into(),
            StabilizationAnalysis {
                frames: vec![FrameCorrection {
                    rotation: [0.999, 0.0, 0.045, 0.0, 1.0, 0.0, -0.045, 0.0, 0.999],
                    zoom: 1.1,
                }],
                diagnostics: StabilizationDiagnostics {
                    bias: BiasEstimate::ZERO,
                    sample_rate_hz: None,
                    clock_drift_ppm: 0.0,
                    sync_residual_ns: 0.0,
                    anchors_used: 0,
                    max_required_zoom: 1.1,
                    infeasible_range: None,
                    mean_gravity_confidence: None,
                    horizon_lock_unavailable: false,
                },
                fps: 30.0,
                source_start_s: source_range.0,
                source_end_s: source_range.1,
                width: 320.0,
                height: 180.0,
                intrinsics: [0.5, 0.5, 0.5, 0.5],
                k: [0.0; 4],
                fisheye: false,
            },
        );

        let full = compile_full(
            &project,
            seq_id,
            0,
            Tick(0),
            Quality::PREVIEW,
            None,
            None,
            false,
            None,
            Some(&stabilization),
        );
        let clean = compile_with_providers(
            &project,
            seq_id,
            0,
            Tick(0),
            Quality::PREVIEW,
            None,
            None,
            Some(&stabilization),
            true,
        );
        let has_stabilization = |frame: &CompiledFrame| {
            frame
                .graph
                .nodes
                .iter()
                .any(|node| matches!(node.op, IrOp::StabilizeWarp { .. }))
        };
        assert!(
            has_stabilization(&full),
            "full compile must lower stabilization"
        );
        assert!(
            has_stabilization(&clean),
            "compare-clean must preserve stabilization while skipping only clip looks"
        );
    }

    #[test]
    fn bare_solid_clip_is_solid_transform_output() {
        let (mut project, seq_id) = base_project();
        let tk = add_video_track(&mut project, seq_id);
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(solid_clip(Color::BLACK, 0, Tick::from_seconds(2).0));

        let out = compile(&project, seq_id, 0, Tick(0), Quality::PREVIEW, None);
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        // Single opaque track ⇒ no Merge. Chain: SolidColor → Transform2D → Output.
        let ops: Vec<&str> = out.graph.nodes.iter().map(|n| op_name(&n.op)).collect();
        assert_eq!(ops, vec!["SolidColor", "Transform2D", "Output"]);
        let output = out.graph.output.unwrap();
        assert!(matches!(
            out.graph.nodes[output.0 as usize].op,
            IrOp::Output { .. }
        ));
    }

    #[test]
    fn composition_media_in_local_time_uses_host_clip_offset() {
        fn source_time(time_source: TimeSource) -> Tick {
            let (mut project, seq_id) = base_project();
            let graph_media = GraphNode::new(GraphOp::MediaIn {
                asset: AssetId::new(),
                time_source,
            });
            let graph_output = GraphNode::new(GraphOp::Output);
            let media_id = graph_media.id;
            let output_id = graph_output.id;
            let graph = NodeGraph {
                id: GraphId::new(),
                name: "media-time-source".into(),
                nodes: HashMap::from([(media_id, graph_media), (output_id, graph_output)]),
                edges: vec![GraphEdge {
                    from: (media_id, GOutPort::PRIMARY),
                    to: (output_id, InPort::PRIMARY),
                }],
                output: output_id,
                ui: HashMap::new(),
            };
            let graph_id = graph.id;
            project.graphs.insert(graph_id, graph);

            let tk = add_video_track(&mut project, seq_id);
            let mut clip = solid_clip(Color::WHITE, 1_000_000, 1_000_000);
            clip.composition = Some(graph_id);
            project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
                .clips
                .push(clip);

            let compiled = compile(&project, seq_id, 0, Tick(1_250_000), Quality::FULL, None);
            compiled
                .graph
                .nodes
                .iter()
                .find_map(|node| match &node.op {
                    IrOp::DecodeVideo { src_time, .. } => Some(*src_time),
                    _ => None,
                })
                .expect("MediaIn must lower to a video decode")
        }

        assert_eq!(source_time(TimeSource::Local), Tick(250_000));
        assert_eq!(source_time(TimeSource::Sequence), Tick(1_250_000));
    }

    #[test]
    fn composition_geometry_nodes_lower_resolved_parameters() {
        let (mut project, seq_id) = base_project();
        let clip_in = GraphNode::new(GraphOp::ClipIn);
        let mut transform = GraphNode::new(GraphOp::Transform2D);
        transform
            .params
            .base
            .0
            .set("transform.x", PropValue::Float(12.0));
        transform
            .params
            .base
            .0
            .set("transform.scale_x", PropValue::Float(1.5));
        let mut crop = GraphNode::new(GraphOp::Crop);
        crop.params.base.0.set("params.left", PropValue::Float(0.1));
        crop.params
            .base
            .0
            .set("params.bottom", PropValue::Float(0.2));
        let mut resize = GraphNode::new(GraphOp::Resize {
            fit: photonic_core::timeline::FitMode::Contain,
        });
        resize
            .params
            .base
            .0
            .set("params.width", PropValue::Float(80.0));
        resize
            .params
            .base
            .0
            .set("params.height", PropValue::Float(40.0));
        let output = GraphNode::new(GraphOp::Output);
        let (clip_id, transform_id, crop_id, resize_id, output_id) =
            (clip_in.id, transform.id, crop.id, resize.id, output.id);
        let graph = NodeGraph {
            id: GraphId::new(),
            name: "geometry-params".into(),
            nodes: HashMap::from([
                (clip_id, clip_in),
                (transform_id, transform),
                (crop_id, crop),
                (resize_id, resize),
                (output_id, output),
            ]),
            edges: vec![
                GraphEdge {
                    from: (clip_id, GOutPort::PRIMARY),
                    to: (transform_id, InPort::PRIMARY),
                },
                GraphEdge {
                    from: (transform_id, GOutPort::PRIMARY),
                    to: (crop_id, InPort::PRIMARY),
                },
                GraphEdge {
                    from: (crop_id, GOutPort::PRIMARY),
                    to: (resize_id, InPort::PRIMARY),
                },
                GraphEdge {
                    from: (resize_id, GOutPort::PRIMARY),
                    to: (output_id, InPort::PRIMARY),
                },
            ],
            output: output_id,
            ui: HashMap::new(),
        };
        let graph_id = graph.id;
        project.graphs.insert(graph_id, graph);
        let tk = add_video_track(&mut project, seq_id);
        let mut clip = solid_clip(Color::WHITE, 0, Tick::from_seconds(2).0);
        clip.composition = Some(graph_id);
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(clip);

        let compiled = compile(&project, seq_id, 0, Tick(0), Quality::FULL, None);
        assert!(
            compiled.diagnostics.is_empty(),
            "{:?}",
            compiled.diagnostics
        );
        assert!(compiled.graph.nodes.iter().any(|node| {
            matches!(&node.op, IrOp::Transform2D { mat, .. } if *mat != Mat3::IDENTITY)
        }));
        assert!(compiled.graph.nodes.iter().any(|node| {
            matches!(
                &node.op,
                IrOp::Crop {
                    left: 0.1,
                    top: 0.0,
                    right: 0.0,
                    bottom: 0.2
                }
            )
        }));
        assert!(compiled.graph.nodes.iter().any(|node| {
            matches!(
                &node.op,
                IrOp::Resize {
                    w: 80,
                    h: 40,
                    fit: FitMode::Fit
                }
            )
        }));
    }

    #[test]
    fn empty_sequence_outputs_transparent() {
        let (project, seq_id) = base_project();
        let out = compile(&project, seq_id, 0, Tick(0), Quality::PREVIEW, None);
        // Only a transparent SolidColor feeding Output.
        let ops: Vec<&str> = out.graph.nodes.iter().map(|n| op_name(&n.op)).collect();
        assert_eq!(ops, vec!["SolidColor", "Output"]);
    }

    #[test]
    fn two_opaque_tracks_fold_with_one_merge() {
        let (mut project, seq_id) = base_project();
        for _ in 0..2 {
            let tk = add_video_track(&mut project, seq_id);
            project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
                .clips
                .push(solid_clip(Color::BLACK, 0, Tick::from_seconds(2).0));
        }
        let out = compile(&project, seq_id, 0, Tick(0), Quality::PREVIEW, None);
        let merges = out
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.op, IrOp::Merge { .. }))
            .count();
        assert_eq!(merges, 1, "two tracks fold with exactly one Merge");
    }

    #[test]
    fn opacity_zero_clip_is_dead_branch() {
        let (mut project, seq_id) = base_project();
        let tk = add_video_track(&mut project, seq_id);
        let mut clip = solid_clip(Color::BLACK, 0, Tick::from_seconds(2).0);
        clip.transform.base.opacity = 0.0;
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(clip);
        let out = compile(&project, seq_id, 0, Tick(0), Quality::PREVIEW, None);
        // The invisible clip folds away → empty program → transparent → Output.
        let ops: Vec<&str> = out.graph.nodes.iter().map(|n| op_name(&n.op)).collect();
        assert_eq!(ops, vec!["SolidColor", "Output"]);
    }

    #[test]
    fn adjustment_clip_rewraps_the_composite_below() {
        let (mut project, seq_id) = base_project();
        // Bottom track: a solid.
        let tk0 = add_video_track(&mut project, seq_id);
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk0]
            .clips
            .push(solid_clip(Color::BLACK, 0, Tick::from_seconds(2).0));
        // Top track: an Adjustment clip with an effect → re-roots the stack below.
        let tk1 = add_video_track(&mut project, seq_id);
        let mut adj = Clip::new(ClipSource::Adjustment, Tick(0), Tick::from_seconds(2));
        adj.effects
            .push(photonic_core::timeline::ClipEffect::new(EffectKind::Blur));
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk1]
            .clips
            .push(adj);

        let out = compile(&project, seq_id, 0, Tick(0), Quality::PREVIEW, None);
        // The Effect node's input is the bottom solid (re-root), and no Merge is
        // introduced by the Adjustment (it replaces the accumulator).
        let effect = out
            .graph
            .nodes
            .iter()
            .find(|n| matches!(n.op, IrOp::Effect { .. }))
            .expect("adjustment effect node present");
        let input = effect.inputs[0].0;
        // Its input chain traces back to the bottom clip through that clip's
        // own Transform2D (re-root, not a fresh source).
        assert!(matches!(
            out.graph.nodes[input.0 as usize].op,
            IrOp::Transform2D { .. }
        ));
        assert!(!out
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::Merge { .. })));
    }

    /// K-0.2 Step A: the resolved effect params are folded into the content
    /// hash, so two clips that differ ONLY in a Blur radius compile to distinct
    /// `Effect` cache identities. Without this, the two radii would collide in
    /// `NodeCache` and Step B's real Blur kernel would sample the wrong cached
    /// pixels. Also asserts determinism (the same radius hashes stably).
    #[test]
    fn blur_radius_participates_in_content_hash() {
        fn effect_hash_for_radius(radius: f64) -> u128 {
            let (mut project, seq_id) = base_project();
            let tk = add_video_track(&mut project, seq_id);
            let mut clip = solid_clip(Color::BLACK, 0, Tick::from_seconds(2).0);
            let mut eff = photonic_core::timeline::ClipEffect::new(EffectKind::Blur);
            eff.params
                .base
                .set("params.radius", PropValue::Float(radius));
            clip.effects.push(eff);
            project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
                .clips
                .push(clip);
            let out = compile(&project, seq_id, 0, Tick(0), Quality::PREVIEW, None);
            out.graph
                .nodes
                .iter()
                .find(|n| matches!(n.op, IrOp::Effect { .. }))
                .expect("a Blur Effect node is present")
                .content_hash
                .0
        }
        let h10 = effect_hash_for_radius(10.0);
        let h50 = effect_hash_for_radius(50.0);
        assert_ne!(
            h10, h50,
            "two Blur clips differing only in radius must not share a content hash"
        );
        assert_eq!(
            h10,
            effect_hash_for_radius(10.0),
            "the same radius must hash deterministically"
        );
    }

    #[test]
    fn composition_splices_clip_source_and_keeps_chain() {
        let (mut project, seq_id) = base_project();
        let tk = add_video_track(&mut project, seq_id);
        // A ClipIn → Output composition (the fixed v1 seed).
        let (graph, _clip_in) = NodeGraph::new_clip_composition("comp");
        let gid = graph.id;
        project.graphs.insert(gid, graph);
        let mut clip = solid_clip(Color::BLACK, 0, Tick::from_seconds(2).0);
        clip.composition = Some(gid);
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(clip);

        let out = compile(&project, seq_id, 0, Tick(0), Quality::PREVIEW, None);
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        // ClipIn binds to the solid source; the still-applied Transform2D rides
        // on top, so the shape matches the plain-clip chain.
        let ops: Vec<&str> = out.graph.nodes.iter().map(|n| op_name(&n.op)).collect();
        assert_eq!(ops, vec!["SolidColor", "Transform2D", "Output"]);
    }

    #[test]
    fn composition_with_merge_over_program() {
        // Composition: SolidColor(a) merged over ClipIn(b) → Output. The comp's
        // SolidColor is WHITE and the host clip's source is BLACK, so the two
        // sources stay distinct (identical colors would content-hash dedup to a
        // single node — see `identical_solid_colors_dedup_to_one_node`; that is
        // correct behaviour, so this fixture keeps them distinct on purpose to
        // prove the Merge really pulls a *second* source).
        let (mut project, seq_id) = base_project();
        let tk = add_video_track(&mut project, seq_id);

        let clip_in = GraphNode::new(GraphOp::ClipIn);
        let mut solid = GraphNode::new(GraphOp::SolidColor);
        solid.params.base.0.set(
            "params.color",
            PropValue::Color(Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            }),
        );
        let merge = GraphNode::new(GraphOp::Merge {
            mode: BlendMode::Normal,
        });
        let output = GraphNode::new(GraphOp::Output);
        let (ci, so, mg, ou) = (clip_in.id, solid.id, merge.id, output.id);
        let mut nodes = std::collections::HashMap::new();
        for n in [clip_in, solid, merge, output] {
            nodes.insert(n.id, n);
        }
        let graph = NodeGraph {
            id: GraphId::new(),
            name: "comp".into(),
            nodes,
            edges: vec![
                GraphEdge {
                    from: (so, GOutPort::PRIMARY),
                    to: (mg, InPort::A),
                },
                GraphEdge {
                    from: (ci, GOutPort::PRIMARY),
                    to: (mg, InPort::B),
                },
                GraphEdge {
                    from: (mg, GOutPort::PRIMARY),
                    to: (ou, InPort::PRIMARY),
                },
            ],
            output: ou,
            ui: std::collections::HashMap::new(),
        };
        let gid = graph.id;
        project.graphs.insert(gid, graph);
        let mut clip = solid_clip(Color::BLACK, 0, Tick::from_seconds(2).0);
        clip.composition = Some(gid);
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(clip);

        let out = compile(&project, seq_id, 0, Tick(0), Quality::PREVIEW, None);
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

        // Exactly one Merge — the composition's. A single fully-opaque clip folds
        // without a track-fold Merge, so this is unambiguously the comp's node.
        let merge_idx = out
            .graph
            .nodes
            .iter()
            .position(|n| matches!(n.op, IrOp::Merge { .. }))
            .expect("composition Merge present");
        assert_eq!(
            out.graph
                .nodes
                .iter()
                .filter(|n| matches!(n.op, IrOp::Merge { .. }))
                .count(),
            1,
            "only the composition's Merge — no spurious track-fold Merge"
        );

        // The Merge pulls two DISTINCT sources: the comp's white SolidColor and
        // the clip's black source bound via ClipIn (02 §2 step 3 — ClipIn binds
        // to the host clip's source op).
        let merge = &out.graph.nodes[merge_idx];
        assert_eq!(merge.inputs.len(), 2, "binary Merge has both inputs wired");
        for (input, _) in &merge.inputs {
            assert!(
                matches!(
                    out.graph.nodes[input.0 as usize].op,
                    IrOp::SolidColor { .. }
                ),
                "each Merge input is a SolidColor source"
            );
        }
        let solids = out
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.op, IrOp::SolidColor { .. }))
            .count();
        assert_eq!(
            solids, 2,
            "comp SolidColor + clip source via ClipIn stay distinct"
        );

        // Source-substitution (08 §4): the composition replaces ONLY the source
        // op; the still-applied default chain rides on top. So the IR Output's
        // input is the clip's default Transform2D, and that Transform2D's input
        // is the composition's Merge — the comp's Output feeds the default chain,
        // it does not become the terminal output itself.
        let ir_out = out.graph.output.expect("compiled Output present");
        let xf = out.graph.nodes[ir_out.0 as usize].inputs[0].0;
        assert!(
            matches!(out.graph.nodes[xf.0 as usize].op, IrOp::Transform2D { .. }),
            "default Transform2D rides on top of the composition Output"
        );
        let xf_in = out.graph.nodes[xf.0 as usize].inputs[0].0;
        assert_eq!(
            xf_in.0 as usize, merge_idx,
            "the still-applied default chain sits directly on the composition's Merge"
        );
    }

    #[test]
    fn project_graph_filter_applies_to_program() {
        let (mut project, seq_id) = base_project();
        let tk = add_video_track(&mut project, seq_id);
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(solid_clip(Color::BLACK, 0, Tick::from_seconds(2).0));

        // Project graph: Blur → Output; Blur's input is unwired, so the program
        // (fold result) feeds it (08 §5 program-splice).
        let blur = GraphNode::new(GraphOp::Blur);
        let output = GraphNode::new(GraphOp::Output);
        let (bl, ou) = (blur.id, output.id);
        let mut nodes = std::collections::HashMap::new();
        nodes.insert(bl, blur);
        nodes.insert(ou, output);
        let pg = NodeGraph {
            id: GraphId::new(),
            name: "pg".into(),
            nodes,
            edges: vec![GraphEdge {
                from: (bl, GOutPort::PRIMARY),
                to: (ou, InPort::PRIMARY),
            }],
            output: ou,
            ui: std::collections::HashMap::new(),
        };
        let pgid = pg.id;
        project.graphs.insert(pgid, pg);
        project.project_graph = Some(pgid);

        let out = compile(&project, seq_id, 0, Tick(0), Quality::PREVIEW, None);
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        // An Effect (Blur) node whose input traces to the clip's Transform2D.
        let effect = out
            .graph
            .nodes
            .iter()
            .find(|n| matches!(n.op, IrOp::Effect { .. }))
            .expect("project-graph filter present");
        let input = effect.inputs[0].0;
        assert!(matches!(
            out.graph.nodes[input.0 as usize].op,
            IrOp::Transform2D { .. }
        ));
        // Output's input is the filter (splice sits between program and Output).
        let output = out.graph.output.unwrap();
        let out_in = out.graph.nodes[output.0 as usize].inputs[0].0;
        assert!(matches!(
            out.graph.nodes[out_in.0 as usize].op,
            IrOp::Effect { .. }
        ));
    }

    #[test]
    fn nested_sequence_cycle_is_guarded() {
        let (mut project, seq_id) = base_project();
        // A self-nesting clip: sequence contains a clip whose source is itself.
        let tk = add_video_track(&mut project, seq_id);
        let nested = Clip::new(
            ClipSource::NestedSequence { sequence: seq_id },
            Tick(0),
            Tick::from_seconds(2),
        );
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(nested);
        let out = compile(&project, seq_id, 0, Tick(0), Quality::PREVIEW, None);
        assert!(
            out.diagnostics.iter().any(|d| d.message.contains("cycle")),
            "self-nesting must produce a cycle diagnostic, got {:?}",
            out.diagnostics
        );
    }

    /// A `ClipSource::NestedSequence` clip splices its inner sequence's composite
    /// as the clip's *source* (02 §2 step 3 / CAP-005). The inner sequence here is
    /// a real 2-clip composite — an opaque red backdrop under a half-opacity blue —
    /// and it must ride through the outer clip unchanged, proving the recursive
    /// compile lowers the inner program (not a transparent fallback / single clip).
    #[test]
    fn nested_sequence_composites_inner_as_one_clip() {
        let mut project = TimelineProject::new();

        // Inner sequence: bottom opaque red, top blue at 0.5 opacity. Premultiplied
        // linear `over` (0 and 1 map through sRGB→linear unchanged):
        //   top_eff = (0,0,1,1)·0.5 = (0,0,0.5,0.5)
        //   out     = top_eff + red·(1−0.5) = (0.5, 0, 0.5, 1.0)
        let mut inner = Sequence::new("inner", FrameRate::FPS_30, 4, 4);
        let inner_id = inner.id;
        let mut bottom = Track::new(TrackKind::Video, "V1");
        bottom.clips.push(solid_clip(
            Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            0,
            Tick::from_seconds(2).0,
        ));
        let mut top = Track::new(TrackKind::Video, "V2");
        let mut blue = solid_clip(
            Color {
                r: 0.0,
                g: 0.0,
                b: 1.0,
                a: 1.0,
            },
            0,
            Tick::from_seconds(2).0,
        );
        blue.transform.base.opacity = 0.5;
        top.clips.push(blue);
        inner.video_tracks.push(bottom);
        inner.video_tracks.push(top);
        project.insert_sequence(inner);

        // Outer sequence: one clip whose source is the inner sequence.
        let mut outer = Sequence::new("outer", FrameRate::FPS_30, 4, 4);
        let outer_id = outer.id;
        let mut ot = Track::new(TrackKind::Video, "V1");
        ot.clips.push(Clip::new(
            ClipSource::NestedSequence { sequence: inner_id },
            Tick(0),
            Tick::from_seconds(2),
        ));
        outer.video_tracks.push(ot);
        project.insert_sequence(outer);

        let out = compile(&project, outer_id, 0, Tick(0), Quality::FULL, None);
        assert!(
            out.diagnostics.is_empty(),
            "clean nested compile has no diagnostics, got {:?}",
            out.diagnostics
        );
        // Both inner clips were lowered as the nested source (two distinct solids),
        // not collapsed to a single clip or a transparent fallback.
        let solids = out
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.op, IrOp::SolidColor { .. }))
            .count();
        assert!(
            solids >= 2,
            "inner composite lowers both source solids, got {solids}"
        );

        // The composited pixels match the inner sequence's blend, evaluated through
        // the deterministic CPU reference (03 §6).
        let img = crate::graph::eval_cpu::evaluate(
            &out.graph,
            (4, 4),
            &mut crate::graph::eval_cpu::EmptyProvider,
        );
        for p in &img.pixels {
            assert!((p[0] - 0.5).abs() < 1e-4, "r={}", p[0]);
            assert!(p[1].abs() < 1e-4, "g={}", p[1]);
            assert!((p[2] - 0.5).abs() < 1e-4, "b={}", p[2]);
            assert!((p[3] - 1.0).abs() < 1e-4, "a={}", p[3]);
        }
    }

    #[test]
    fn identical_solid_colors_dedup_to_one_node() {
        // Two tracks with the exact same solid color: content-hash dedup means
        // one SolidColor node feeds both fold inputs.
        let (mut project, seq_id) = base_project();
        for _ in 0..2 {
            let tk = add_video_track(&mut project, seq_id);
            let mut clip = solid_clip(
                Color {
                    r: 0.2,
                    g: 0.4,
                    b: 0.6,
                    a: 1.0,
                },
                0,
                Tick::from_seconds(2).0,
            );
            // Make the top one semi-transparent so a Merge is forced but the
            // SolidColor + Transform2D still dedup.
            clip.transform.base.opacity = 1.0;
            project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
                .clips
                .push(clip);
        }
        let out = compile(&project, seq_id, 0, Tick(0), Quality::PREVIEW, None);
        let solids = out
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.op, IrOp::SolidColor { .. }))
            .count();
        // Both tracks share one deduped SolidColor and one deduped Transform2D.
        assert_eq!(solids, 1, "identical solids dedup");
    }

    #[test]
    fn fit_long_edge_caps_draft_size() {
        assert_eq!(fit_long_edge(1920, 1080, DRAFT_MAX_LONG_EDGE), (960, 540));
        assert_eq!(fit_long_edge(640, 360, DRAFT_MAX_LONG_EDGE), (640, 360));
        assert_eq!(fit_long_edge(1080, 1920, DRAFT_MAX_LONG_EDGE), (540, 960));
    }

    #[test]
    fn compile_asset_peek_emits_decode_and_output() {
        use photonic_core::timeline::{AssetKind, MediaAsset, Sequence, TimelineProject};
        let mut project = TimelineProject::new();
        let asset = MediaAsset::from_file(AssetKind::Video, "/tmp/x.mp4");
        let id = asset.id;
        project.media.insert(asset);
        let seq = Sequence::new("S", FrameRate::FPS_30, 1920, 1080);
        project.sequences.insert(seq.id, seq);
        let compiled = compile_asset_peek(&project, id, Tick::ZERO, Quality::PREVIEW, 640, 360);
        assert!(compiled
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::DecodeVideo { asset: a, .. } if a == id)));
        assert!(compiled
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::Output { w: 640, h: 360 })));
    }

    #[test]
    fn native_asset_peek_has_explicit_source_and_display_transform() {
        use photonic_core::timeline::color::{
            InputMatrix, InputSignalRange, NativeInputColorInterpretation, NativeInputStandard,
        };
        let mut project = TimelineProject::new();
        let mut asset = MediaAsset::from_file(AssetKind::Video, "/tmp/native-peek.mp4");
        let input = NativeInputColorInterpretation {
            hlg_peak_nits: None,
            reference_white_nits: None,
            version: 1,
            standard: NativeInputStandard::Bt709Scene,
            range: InputSignalRange::Limited,
            matrix: InputMatrix::Bt709,
            chroma_location: None,
        };
        asset.native_input_color = Some(input.clone());
        let id = asset.id;
        project.media.insert(asset);
        let compiled = compile_asset_peek(&project, id, Tick::ZERO, Quality::PREVIEW, 640, 360);
        assert!(compiled.diagnostics.is_empty());
        assert_eq!(
            compiled.graph.working_color_domain,
            WorkingColorDomain::SceneLinearAcescg
        );
        assert!(matches!(
            compiled.graph.nodes[0].op,
            IrOp::NativeDecodeVideo { asset, input: ref authored, .. } if asset == id && authored == &input
        ));
        assert!(matches!(compiled.graph.nodes[1].op, IrOp::NativeSdrOutput));
        assert_eq!(
            compiled.graph.output_color_encoding(),
            Ok(crate::graph::ir::FrameColorEncoding::SrgbDisplay)
        );
        project
            .media
            .assets
            .get_mut(&id)
            .unwrap()
            .native_input_color
            .as_mut()
            .unwrap()
            .version = 2;
        let invalid = compile_asset_peek(&project, id, Tick::ZERO, Quality::FULL, 640, 360);
        assert!(invalid
            .diagnostics
            .iter()
            .any(|d| d.code == Some(CompileCode::ColorPipelineUnavailable)));
        assert!(!invalid.graph.nodes.iter().any(|node| matches!(
            node.op,
            IrOp::DecodeVideo { .. } | IrOp::NativeDecodeVideo { .. }
        )));
        use photonic_core::timeline::{MediaProbe, ProbedColor, VideoStreamInfo};
        let media = project.media.assets.get_mut(&id).unwrap();
        media.native_input_color.as_mut().unwrap().version = 1;
        let mut probe = MediaProbe::basic(Tick(1_000), "mov", "prores");
        probe.video = Some(VideoStreamInfo {
            width: 640,
            height: 360,
            frame_rate: FrameRate::FPS_30,
            pixel_aspect: 1.0,
            color: ProbedColor::default(),
            keyframe_index_cached: false,
            scan: ScanType::InterlacedTopFirst,
        });
        media.probe = Some(probe);
        let interlaced = compile_asset_peek(&project, id, Tick::ZERO, Quality::FULL, 640, 360);
        assert!(interlaced
            .diagnostics
            .iter()
            .any(|d| d.code == Some(CompileCode::ColorPipelineUnavailable)
                && d.message.contains("pre-IDT deinterlace")));
    }

    fn native_group_look_fixture() -> (
        TimelineProject,
        SequenceId,
        timeline::GroupId,
        timeline::SharedLookId,
    ) {
        use timeline::color::{
            InputMatrix, InputSignalRange, NativeInputColorInterpretation, NativeInputStandard,
            NativeManagedColorConfig, SequenceColorConfig,
        };
        let exposure = |stops| {
            let mut grade = Grade::new();
            grade.ops.push(GradeOp::new(
                GradeOpKind::Exposure,
                GradeOpParams::Exposure { stops },
            ));
            grade
        };
        let mut project = TimelineProject::new();
        let mut asset = MediaAsset::from_file(AssetKind::Video, "/tmp/native-stage-fixture.mp4");
        asset.native_input_color = Some(NativeInputColorInterpretation {
            hlg_peak_nits: None,
            reference_white_nits: None,
            version: 1,
            standard: NativeInputStandard::Bt709Scene,
            range: InputSignalRange::Limited,
            matrix: InputMatrix::Bt709,
            chroma_location: None,
        });
        asset.grade = Some(exposure(0.25));
        let asset_id = asset.id;
        project.media.insert(asset);
        let mut seq = Sequence::new("native stages", FrameRate::FPS_30, 16, 16);
        seq.color =
            SequenceColorConfig::NativeManaged(Box::new(NativeManagedColorConfig::sdr_draft()));
        let mut parent = timeline::GroupNode::new(timeline::GroupKind::Normal);
        parent.pre_grade = Some(exposure(0.5));
        parent.post_grade = Some(exposure(4.0));
        let mut group = timeline::GroupNode::new(timeline::GroupKind::Normal);
        group.parent = Some(parent.id);
        group.pre_grade = Some(exposure(1.0));
        group.post_grade = Some(exposure(3.0));
        let group_id = group.id;
        seq.groups.insert(parent.id, parent);
        seq.groups.insert(group.id, group);
        let look_id = timeline::SharedLookId::new();
        project.shared_looks.insert(
            look_id,
            timeline::SharedLook {
                versions: Vec::new(),
                active_version: None,
                id: look_id,
                name: "shared".into(),
                grade: exposure(2.5),
            },
        );
        let mut clip = Clip::new(
            ClipSource::Asset { asset: asset_id },
            Tick::ZERO,
            Tick(1000),
        );
        clip.group = Some(group_id);
        clip.grade = Some(exposure(2.0));
        clip.look = Some(timeline::ClipLook::Shared(look_id));
        let mut track = Track::new(TrackKind::Video, "V1");
        track.grade = Some(exposure(5.0));
        track.clips.push(clip);
        seq.video_tracks.push(track);
        seq.master_grade = Some(exposure(6.0));
        let sequence = seq.id;
        project.insert_sequence(seq);
        (project, sequence, group_id, look_id)
    }

    #[test]
    fn native_lut_lowering_obeys_declared_scene_or_log_coordinates() {
        use timeline::color::{LutColorInterpretation, LutColorSpace, LutPurpose, NativeLutSpace};
        struct Provider {
            table: std::sync::Arc<photonic_render::Lut3d>,
            space: NativeLutSpace,
        }
        impl LutProvider for Provider {
            fn lut(&self, _: AssetId) -> Option<std::sync::Arc<photonic_render::Lut3d>> {
                None
            }
            fn native_lut(&self, _: AssetId) -> Option<NativeLutBinding> {
                Some(NativeLutBinding {
                    table: self.table.clone(),
                    space: self.space,
                })
            }
        }
        for space in [NativeLutSpace::Acescg, NativeLutSpace::Acescct] {
            let (mut project, sequence, _, _) = native_group_look_fixture();
            let mut asset = MediaAsset::from_file(AssetKind::Lut3d, "/tmp/native-fixture.cube");
            asset.lut_full_hash = Some("verified-fixture".into());
            let declaration = LutColorSpace::Native {
                transform_revision: 1,
                space,
            };
            asset.lut_color = Some(LutColorInterpretation {
                version: 1,
                purpose: LutPurpose::Creative,
                input: declaration.clone(),
                output: declaration,
            });
            let id = asset.id;
            project.media.insert(asset);
            let mut grade = Grade::new();
            grade.ops.push(GradeOp::new(
                GradeOpKind::Lut3d,
                GradeOpParams::Lut3d {
                    asset: id,
                    intensity: 1.0,
                    interp: timeline::LutInterp::Tetrahedral,
                },
            ));
            grade.ops[0].mask = Some(timeline::GradeMask::PowerWindow {
                shape: timeline::WindowShape::Ellipse,
                center: [0.5; 2],
                size: [0.25; 2],
                rotation: 0.0,
                softness: 0.1,
                invert: false,
            });
            project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].grade =
                Some(grade);
            let provider = Provider {
                table: std::sync::Arc::new(photonic_render::Lut3d::identity(2)),
                space,
            };
            for delivery in [false, true] {
                let frame = compile_native_sequence(
                    &project,
                    sequence,
                    0,
                    Tick::ZERO,
                    Quality::FULL,
                    delivery,
                    false,
                    None,
                    Some(&provider),
                );
                assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
                assert!(frame.graph.validate_working_color_domain().is_ok());
                assert!(frame
                    .graph
                    .nodes
                    .iter()
                    .any(|n| matches!(n.op, IrOp::NativeLut3d { .. })));
                let mask_node = frame
                    .graph
                    .nodes
                    .iter()
                    .position(|n| matches!(n.op, IrOp::NativeMaskMix { .. }))
                    .unwrap();
                assert_eq!(
                    frame
                        .graph
                        .node_color_encoding(IrNodeId(mask_node as u32))
                        .unwrap(),
                    if space == NativeLutSpace::Acescct {
                        crate::graph::ir::FrameColorEncoding::Acescct
                    } else {
                        crate::graph::ir::FrameColorEncoding::SceneLinearAcescg
                    }
                );
                let transfers = frame
                    .graph
                    .nodes
                    .iter()
                    .filter(|n| matches!(n.op, IrOp::NativeAcescct { .. }))
                    .count();
                assert_eq!(
                    transfers,
                    if space == NativeLutSpace::Acescct {
                        2
                    } else {
                        0
                    }
                );
                assert!(!frame
                    .graph
                    .nodes
                    .iter()
                    .any(|n| matches!(n.op, IrOp::Grade { .. })));
            }
            project.media.assets.get_mut(&id).unwrap().lut_full_hash = None;
            let missing_pin = compile_native_sequence(
                &project,
                sequence,
                0,
                Tick::ZERO,
                Quality::FULL,
                false,
                false,
                None,
                Some(&provider),
            );
            assert!(missing_pin
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error));
            assert!(!missing_pin.graph.nodes.iter().any(|n| matches!(
                n.op,
                IrOp::NativeDecodeVideo { .. } | IrOp::NativeLut3d { .. }
            )));
        }
    }

    #[test]
    fn native_group_and_shared_look_stages_match_preview_and_delivery() {
        let (project, sequence, _, _) = native_group_look_fixture();
        for compiled in [
            compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL),
            compile_native_delivery(&project, sequence, 0, Tick::ZERO, Quality::FULL),
        ] {
            assert!(
                compiled.diagnostics.is_empty(),
                "{:?}",
                compiled.diagnostics
            );
            assert!(compiled.graph.validate_working_color_domain().is_ok());
            let stages: Vec<_> = compiled
                .graph
                .nodes
                .iter()
                .filter_map(|node| match node.op {
                    IrOp::NativeExposure { stops } => Some(stops),
                    _ => None,
                })
                .collect();
            assert_eq!(stages, [0.25, 0.5, 1.0, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0]);
        }
    }

    #[test]
    fn native_group_offsets_surround_clip_exposure_numerically() {
        let (mut project, sequence, group, _) = native_group_look_fixture();
        let offset = |value| {
            let mut grade = Grade::new();
            grade.ops.push(GradeOp::new(
                GradeOpKind::LinearOffset,
                GradeOpParams::LinearOffset { rgb: [value; 3] },
            ));
            grade
        };
        for media in project.media.assets.values_mut() {
            media.grade = None;
        }
        let seq = project.sequences.get_mut(&sequence).unwrap();
        seq.master_grade = None;
        seq.video_tracks[0].grade = None;
        let clip = &mut seq.video_tracks[0].clips[0];
        clip.look = None;
        let clip_id = clip.id;
        if let GradeOpParams::Exposure { stops } =
            &mut clip.grade.as_mut().unwrap().ops[0].params.base
        {
            *stops = 1.0;
        }
        let node = seq.groups.get_mut(&group).unwrap();
        node.parent = None;
        node.pre_grade = Some(offset(0.2));
        node.post_grade = Some(offset(0.3));
        let compiled = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        assert!(
            compiled.diagnostics.is_empty(),
            "{:?}",
            compiled.diagnostics
        );
        let mut graph = compiled.graph.clone();
        for node in &mut graph.nodes {
            if matches!(node.op, IrOp::NativeDecodeVideo { .. }) {
                node.op = IrOp::SolidColor {
                    color: crate::graph::ir::LinearColor {
                        r: 0.1,
                        g: 0.1,
                        b: 0.1,
                        a: 1.0,
                    },
                };
            }
        }
        let tap = compiled.tap(ScopeTapPoint::Clip(clip_id)).unwrap();
        graph.nodes.truncate(tap.0 as usize + 1);
        graph.output = Some(tap);
        assert!(
            graph.validate_working_color_domain().is_ok(),
            "{:?}",
            graph.validate_working_color_domain()
        );
        let image = crate::graph::eval_cpu::evaluate(
            &graph,
            (16, 16),
            &mut crate::graph::eval_cpu::EmptyProvider,
        );
        for pixel in image.pixels {
            for value in &pixel[..3] {
                assert!((value - 0.9).abs() < 1e-6, "{pixel:?}");
            }
            assert_eq!(pixel[3], 1.0);
        }
    }

    fn native_nest_fixture() -> (TimelineProject, SequenceId, SequenceId) {
        let (mut project, inner, _, _) = native_group_look_fixture();
        for asset in project.media.assets.values_mut() {
            asset.grade = None;
        }
        let exposure = || {
            let mut grade = Grade::new();
            grade.ops.push(GradeOp::new(
                GradeOpKind::Exposure,
                GradeOpParams::Exposure { stops: 1.0 },
            ));
            grade
        };
        let seq = project.sequences.get_mut(&inner).unwrap();
        seq.groups.clear();
        seq.video_tracks[0].grade = None;
        seq.video_tracks[0].clips[0].group = None;
        seq.video_tracks[0].clips[0].look = None;
        seq.video_tracks[0].clips[0].grade = Some(exposure());
        seq.master_grade = Some(exposure());
        let mut outer = Sequence::new("outer", FrameRate::FPS_30, 16, 16);
        outer.color = seq.color.clone();
        outer.master_grade = Some(exposure());
        let mut track = Track::new(TrackKind::Video, "nest");
        let mut clip = Clip::new(
            ClipSource::NestedSequence { sequence: inner },
            Tick::ZERO,
            Tick(10000),
        );
        clip.grade = Some(exposure());
        track.clips.push(clip);
        outer.video_tracks.push(track);
        let outer_id = outer.id;
        project.insert_sequence(outer);
        (project, outer_id, inner)
    }

    #[test]
    fn native_nests_keep_scene_stages_and_apply_output_once() {
        let (project, outer, _) = native_nest_fixture();
        for tick in [Tick::ZERO, Tick(2000)] {
            for mut compiled in [
                compile_native_preview(&project, outer, 0, tick, Quality::FULL),
                compile_native_delivery(&project, outer, 0, tick, Quality::FULL),
            ] {
                assert!(
                    !compiled
                        .diagnostics
                        .iter()
                        .any(|d| d.severity == DiagSeverity::Error),
                    "{:?}",
                    compiled.diagnostics
                );
                assert_eq!(
                    compiled
                        .graph
                        .nodes
                        .iter()
                        .filter(|n| matches!(
                            n.op,
                            IrOp::NativeSdrOutput | IrOp::NativeSdrVideoOutput
                        ))
                        .count(),
                    1
                );
                for node in &mut compiled.graph.nodes {
                    if let IrOp::NativeDecodeVideo { src_time, .. } = node.op {
                        assert_eq!(src_time, Tick::ZERO); // Tail holds inner frame zero.
                        node.op = IrOp::SolidColor {
                            color: LinearColor {
                                r: -0.05,
                                g: 0.1,
                                b: 0.5,
                                a: 0.5,
                            },
                        };
                    }
                }
                let output = compiled.graph.output.unwrap();
                let scene = compiled.graph.nodes[output.0 as usize].inputs[0].0;
                compiled.graph.nodes.truncate(scene.0 as usize + 1);
                compiled.graph.output = Some(scene);
                let image = crate::graph::eval_cpu::evaluate(
                    &compiled.graph,
                    (2, 2),
                    &mut crate::graph::eval_cpu::EmptyProvider,
                );
                for pixel in image.pixels {
                    assert_eq!(pixel, [-0.8, 1.6, 8.0, 0.5]);
                }
                if tick != Tick::ZERO {
                    assert!(compiled
                        .diagnostics
                        .iter()
                        .any(|d| d.code == Some(CompileCode::NestedSequenceShortened)));
                }
            }
        }
    }

    #[test]
    fn native_nests_refuse_cycles_missing_sources_and_mismatched_boundaries() {
        for case in 0..4 {
            let (mut project, outer, inner) = native_nest_fixture();
            match case {
                0 => {
                    project.sequences.get_mut(&inner).unwrap().video_tracks[0].clips[0].source =
                        ClipSource::NestedSequence { sequence: outer }
                }
                1 => {
                    project.sequences.remove(&inner);
                }
                2 => project.sequences.get_mut(&inner).unwrap().formats[0].width = 32,
                _ => {
                    project.sequences.get_mut(&inner).unwrap().color =
                        timeline::color::SequenceColorConfig::LegacySdr
                }
            }
            for compiled in [
                compile_native_preview(&project, outer, 0, Tick::ZERO, Quality::FULL),
                compile_native_delivery(&project, outer, 0, Tick::ZERO, Quality::FULL),
            ] {
                assert!(
                    compiled
                        .diagnostics
                        .iter()
                        .any(|d| d.severity == DiagSeverity::Error),
                    "case {case}"
                );
                assert!(!compiled
                    .graph
                    .nodes
                    .iter()
                    .any(|n| matches!(n.op, IrOp::NativeDecodeVideo { .. })));
            }
        }
    }

    #[test]
    fn native_still_timeline_requires_matching_explicit_interpretation() {
        use timeline::color::{
            InputMatrix, InputSignalRange, NativeInputColorInterpretation, NativeInputStandard,
        };
        let (mut project, sequence, _, _) = native_group_look_fixture();
        let asset = project.sequences[&sequence].video_tracks[0].clips[0]
            .source
            .asset()
            .unwrap();
        project.media.assets.get_mut(&asset).unwrap().kind = AssetKind::Image;
        for standard in [
            NativeInputStandard::Bt709Scene,
            NativeInputStandard::SrgbDisplay,
        ] {
            project
                .media
                .assets
                .get_mut(&asset)
                .unwrap()
                .native_input_color = Some(NativeInputColorInterpretation {
                hlg_peak_nits: None,
                reference_white_nits: None,
                version: 1,
                standard,
                range: InputSignalRange::Full,
                matrix: if standard == NativeInputStandard::SrgbDisplay {
                    InputMatrix::Rgb
                } else {
                    InputMatrix::Bt709
                },
                chroma_location: None,
            });
            for compiled in [
                compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL),
                compile_native_delivery(&project, sequence, 0, Tick::ZERO, Quality::FULL),
            ] {
                if standard == NativeInputStandard::SrgbDisplay {
                    assert!(
                        !compiled
                            .diagnostics
                            .iter()
                            .any(|d| d.severity == DiagSeverity::Error),
                        "{:?}",
                        compiled.diagnostics
                    );
                    assert!(compiled
                        .graph
                        .nodes
                        .iter()
                        .any(|n| matches!(n.op, IrOp::NativeDecodeStill { .. })));
                    assert!(compiled.graph.validate_working_color_domain().is_ok());
                } else {
                    assert!(compiled
                        .diagnostics
                        .iter()
                        .any(|d| d.severity == DiagSeverity::Error));
                }
            }
        }
    }

    #[test]
    fn native_typed_matte_graph_lowers_key_domain_and_preserves_cache_dependencies() {
        let (mut project, sequence, _, _) = native_group_look_fixture();
        let mut grade = Grade::new();
        let key = GradeOp::new(
            GradeOpKind::HslQualifier,
            GradeOpParams::HslQualifier {
                hue: [0.0, 1.0],
                sat: [0.0, 1.0],
                lum: [0.0, 1.0],
                softness: 0.0,
                correction: timeline::CdlParams::identity(),
                keys: vec![],
                matte_levels: [0.0, 0.0],
            },
        );
        let key_id = key.id;
        grade.ops.push(key);
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        ));
        grade.convert_to_graph();
        grade
            .add_graph_corrector(
                GradeOp::new(
                    GradeOpKind::Exposure,
                    GradeOpParams::Exposure { stops: -0.5 },
                ),
                true,
            )
            .unwrap();
        let graph = grade.graph.as_ref().unwrap();
        let original = graph
            .nodes
            .iter()
            .find(|(_, node)| matches!(node, GradeGraphNode::Input))
            .map(|(&id, _)| id)
            .unwrap();
        let corrected = match graph.nodes[&graph.output] {
            GradeGraphNode::Output { input } => input,
            _ => unreachable!(),
        };
        let matte = grade
            .add_graph_utility(GradeGraphNode::QualifierMatte {
                input: original,
                op: key_id,
                label: String::new(),
            })
            .unwrap();
        let refined = grade
            .add_graph_utility(GradeGraphNode::MatteRefine {
                input: matte,
                refinement: Default::default(),
                label: "Refine".into(),
            })
            .unwrap();
        let mixed = grade
            .add_graph_utility(GradeGraphNode::KeyMixer {
                top: refined,
                bottom: matte,
                mode: timeline::GradeKeyMixMode::Multiply,
                label: String::new(),
            })
            .unwrap();
        grade
            .add_graph_utility(GradeGraphNode::MatteApply {
                original,
                corrected,
                matte: mixed,
                label: String::new(),
            })
            .unwrap();
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].grade =
            Some(grade.clone());
        let first = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        assert!(
            first
                .diagnostics
                .iter()
                .all(|d| d.severity != DiagSeverity::Error),
            "{:?}",
            first.diagnostics
        );
        assert_eq!(first.graph.validate_working_color_domain(), Ok(()));
        assert!(first
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::GradeLayerMix { .. })));
        let (key_index, key_node) = first
            .graph
            .nodes
            .iter()
            .enumerate()
            .find(|(_, node)| matches!(node.op, IrOp::QualifierMatte { native: true, .. }))
            .unwrap();
        assert_eq!(
            first
                .graph
                .node_color_encoding(IrNodeId(key_index as u32))
                .unwrap(),
            crate::graph::ir::FrameColorEncoding::MatteWeight
        );
        assert!(
            !first
                .graph
                .nodes
                .iter()
                .any(|node| matches!(node.op, IrOp::GradeMatteRefine { .. })),
            "neutral refinement must be a passthrough"
        );
        let mut spatial = grade.clone();
        if let GradeGraphNode::MatteRefine { refinement, .. } = spatial
            .graph
            .as_mut()
            .unwrap()
            .nodes
            .get_mut(&refined)
            .unwrap()
        {
            refinement.blur = 0.005;
        }
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].grade =
            Some(spatial.clone());
        let blurred = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        let blur_hash = blurred
            .graph
            .nodes
            .iter()
            .find(|node| matches!(node.op, IrOp::GradeMatteRefine { .. }))
            .unwrap()
            .content_hash;
        assert_eq!(blurred.graph.validate_working_color_domain(), Ok(()));
        if let GradeGraphNode::MatteRefine { refinement, .. } = spatial
            .graph
            .as_mut()
            .unwrap()
            .nodes
            .get_mut(&refined)
            .unwrap()
        {
            refinement.blur = 0.01;
        }
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].grade =
            Some(spatial);
        let stronger = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        assert_ne!(
            blur_hash,
            stronger
                .graph
                .nodes
                .iter()
                .find(|node| matches!(node.op, IrOp::GradeMatteRefine { .. }))
                .unwrap()
                .content_hash
        );
        let source_input = first
            .tap(ScopeTapPoint::NativeGraphQualifierInput {
                clip: project.sequences[&sequence].video_tracks[0].clips[0].id,
                node: matte,
                op: key_id,
            })
            .expect("key utility exposes its scene input");
        assert_eq!(
            first.graph.node_color_encoding(source_input).unwrap(),
            crate::graph::ir::FrameColorEncoding::SceneLinearAcescg
        );
        let key_hash = key_node.content_hash;
        if let GradeOpParams::HslQualifier { correction, .. } = &mut grade.ops[0].params.base {
            correction.offset = [0.1; 3];
        }
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].grade = Some(grade);
        let changed = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        let changed_key = changed
            .graph
            .nodes
            .iter()
            .find(|node| matches!(node.op, IrOp::QualifierMatte { .. }))
            .unwrap();
        assert_eq!(
            key_hash, changed_key.content_hash,
            "a key-only output must ignore the source qualifier's CDL"
        );
        assert_ne!(
            first.graph.nodes.last().unwrap().content_hash,
            changed.graph.nodes.last().unwrap().content_hash,
            "the image correction still depends on its CDL"
        );
    }

    #[test]
    fn copied_look_does_not_hide_clip_qualifier_input() {
        let (mut project, sequence, _, _) = native_group_look_fixture();
        let clip_id = project.sequences[&sequence].video_tracks[0].clips[0].id;
        let clip = project.sequences.get_mut(&sequence).unwrap().video_tracks[0]
            .clips
            .iter_mut()
            .find(|clip| clip.id == clip_id)
            .unwrap();
        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 0.7 },
        ));
        let key = GradeOp::new(
            GradeOpKind::HslQualifier,
            GradeOpParams::HslQualifier {
                hue: [0.0, 1.0],
                sat: [0.0, 1.0],
                lum: [0.0, 1.0],
                softness: 0.1,
                correction: timeline::CdlParams::identity(),
                keys: vec![],
                matte_levels: [0.0, 0.0],
            },
        );
        let op = key.id;
        grade.ops.push(key);
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: -0.3 },
        ));
        clip.grade = Some(grade.clone());
        clip.look = Some(timeline::ClipLook::Local(Box::new(grade.clone())));
        let frame = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        assert!(
            frame
                .tap(ScopeTapPoint::NativeQualifierInput { clip: clip_id, op })
                .is_some(),
            "an operator copied into a different look stage must not hide the ordinary clip input"
        );
        use timeline::GradeInspectionStage as Stage;
        let point = |stage, node| ScopeTapPoint::GradeStageInput {
            clip: clip_id,
            stage,
            node,
            op,
        };
        let ordinary = frame
            .tap(ScopeTapPoint::NativeQualifierInput { clip: clip_id, op })
            .unwrap();
        assert_eq!(frame.tap(point(Stage::Clip, None)), Some(ordinary));
        let look = frame.tap(point(Stage::LocalLook, None)).unwrap();
        assert_ne!(look, ordinary);
        assert!(frame.tap(point(Stage::LocalLook, Some(123456))).is_none());
        assert!(frame
            .tap(point(
                Stage::SharedLook {
                    id: timeline::SharedLookId::new()
                },
                None
            ))
            .is_none());
        let mut dependencies = Vec::new();
        let mut cursor = look;
        loop {
            let n = &frame.graph.nodes[cursor.0 as usize];
            if let IrOp::NativeExposure { stops } = n.op {
                dependencies.push(stops);
            }
            let Some((input, _)) = n.inputs.first() else {
                break;
            };
            cursor = *input;
        }
        assert_eq!(dependencies, vec![0.7, -0.3, 0.7, 1.0, 0.5, 0.25]);
        // In graph mode, key-only utilities don't make an unambiguous image
        // corrector ambiguous, but must have their own exact-node address.
        grade.convert_to_graph();
        let input = *grade
            .graph
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|(_, n)| matches!(n, timeline::GradeGraphNode::Input))
            .unwrap()
            .0;
        let corrector = *grade
            .graph
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|(_, n)| matches!(n,timeline::GradeGraphNode::Corrector {op: id,..} if *id==op))
            .unwrap()
            .0;
        let image = match grade.graph.as_ref().unwrap().nodes[&grade.graph.as_ref().unwrap().output]
        {
            timeline::GradeGraphNode::Output { input } => input,
            _ => unreachable!(),
        };
        let key = grade
            .add_graph_utility(timeline::GradeGraphNode::QualifierMatte {
                input,
                op,
                label: String::new(),
            })
            .unwrap();
        grade
            .add_graph_utility(timeline::GradeGraphNode::MatteApply {
                original: input,
                corrected: image,
                matte: key,
                label: String::new(),
            })
            .unwrap();
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].look =
            Some(timeline::ClipLook::Local(Box::new(grade)));
        let graph = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        assert!(graph.diagnostics.is_empty(), "{:?}", graph.diagnostics);
        assert_eq!(
            graph.tap(point(Stage::LocalLook, None)),
            graph.tap(point(Stage::LocalLook, Some(corrector)))
        );
        assert_ne!(
            graph.tap(point(Stage::LocalLook, Some(key))),
            graph.tap(point(Stage::LocalLook, Some(corrector)))
        );
        assert!(graph.tap(point(Stage::Clip, Some(key))).is_none());
    }

    #[test]
    fn native_qualifier_input_tap_excludes_own_and_later_corrections() {
        let (mut project, sequence, _, _) = native_group_look_fixture();
        let clip_id = project.sequences[&sequence].video_tracks[0].clips[0].id;
        let clip = project.sequences.get_mut(&sequence).unwrap().video_tracks[0]
            .clips
            .iter_mut()
            .find(|clip| clip.id == clip_id)
            .unwrap();
        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 0.7 },
        ));
        let key = GradeOp::new(
            GradeOpKind::HslQualifier,
            GradeOpParams::HslQualifier {
                hue: [0.0, 1.0],
                sat: [0.0, 1.0],
                lum: [0.0, 1.0],
                softness: 0.1,
                correction: timeline::CdlParams::identity(),
                keys: vec![],
                matte_levels: [0.0, 0.0],
            },
        );
        let op = key.id;
        grade.ops.push(key);
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: -0.3 },
        ));
        clip.grade = Some(grade.clone());
        let frame = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        assert!(
            frame
                .diagnostics
                .iter()
                .all(|d| d.severity != DiagSeverity::Error),
            "{:?}",
            frame.diagnostics
        );
        let point = ScopeTapPoint::NativeQualifierInput { clip: clip_id, op };
        let input = frame
            .tap(point)
            .expect("neutral qualifier still has an exact input tap");
        assert!(
            matches!(frame.graph.nodes[input.0 as usize].op, IrOp::NativeExposure { stops } if stops == 0.7)
        );
        assert_eq!(
            frame.graph.node_color_encoding(input).unwrap(),
            crate::graph::ir::FrameColorEncoding::SceneLinearAcescg
        );
        assert!(!frame
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeLogQualifier { .. })));
        assert_eq!(frame.resolve_tap(point), Some((point, input)));
        assert!(frame
            .tap(ScopeTapPoint::NativeQualifierInput {
                clip: clip_id,
                op: GradeOpId::new()
            })
            .is_none());
        assert_eq!(
            frame
                .resolve_tap(ScopeTapPoint::NativeQualifierInput {
                    clip: clip_id,
                    op: GradeOpId::new()
                })
                .unwrap()
                .0,
            ScopeTapPoint::Program
        );
        grade.convert_to_graph();
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].grade =
            Some(grade.clone());
        let serial = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        let serial_input = serial
            .tap(point)
            .expect("serial graph corrector has its upstream input");
        assert!(
            matches!(serial.graph.nodes[serial_input.0 as usize].op, IrOp::NativeExposure { stops } if stops == 0.7)
        );
        let graph = grade.graph.as_mut().unwrap();
        let corrector = graph
            .nodes
            .iter()
            .find_map(|(&id, node)| {
                matches!(node, GradeGraphNode::Corrector { op: candidate, .. } if *candidate == op)
                    .then_some(id)
            })
            .unwrap();
        graph
            .nodes
            .get_mut(&corrector)
            .unwrap()
            .set_input("image", 0)
            .unwrap();
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].grade =
            Some(grade.clone());
        let rewired = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        let rewired_input = rewired.tap(point).expect("rewired graph has an input");
        assert!(
            !matches!(rewired.graph.nodes[rewired_input.0 as usize].op, IrOp::NativeExposure { stops } if stops == 0.7)
        );
        assert_ne!(
            serial.graph.nodes[serial_input.0 as usize].content_hash,
            rewired.graph.nodes[rewired_input.0 as usize].content_hash
        );
        let graph = grade.graph.as_mut().unwrap();
        let duplicate = graph.next_id;
        graph.next_id += 2;
        graph.nodes.insert(
            duplicate,
            GradeGraphNode::Corrector {
                input: 1,
                op,
                label: "Repeated operator".into(),
            },
        );
        let old_output = match graph.nodes[&graph.output] {
            GradeGraphNode::Output { input } => input,
            _ => unreachable!(),
        };
        graph.nodes.insert(
            duplicate + 1,
            GradeGraphNode::LayerMixer {
                top: duplicate,
                bottom: old_output,
                opacity: 0.5,
                label: String::new(),
            },
        );
        graph
            .nodes
            .get_mut(&graph.output)
            .unwrap()
            .set_input("image", duplicate + 1)
            .unwrap();
        assert_eq!(graph.validate(&grade.ops), Ok(()));
        assert!(!grade.has_unambiguous_corrector_input(op));
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].grade = Some(grade);
        let repeated = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        assert!(
            repeated.tap(point).is_none(),
            "operator-only inspection must reject ambiguous graph instances"
        );
        let point_for = |node, operator| match point {
            ScopeTapPoint::NativeQualifierInput { clip, .. } => {
                ScopeTapPoint::NativeGraphQualifierInput {
                    clip,
                    node,
                    op: operator,
                }
            }
            ScopeTapPoint::NativeCurveInput { clip, .. } => ScopeTapPoint::NativeGraphCurveInput {
                clip,
                node,
                op: operator,
            },
            _ => unreachable!(),
        };
        let first = repeated
            .tap(point_for(corrector, op))
            .expect("explicit original instance input");
        let second = repeated
            .tap(point_for(duplicate, op))
            .expect("explicit repeated instance input");
        assert_ne!(
            repeated.graph.nodes[first.0 as usize].content_hash,
            repeated.graph.nodes[second.0 as usize].content_hash
        );
        assert!(
            repeated
                .tap(point_for(duplicate, GradeOpId::new()))
                .is_none(),
            "replaced operators cannot satisfy a stale node request"
        );
    }

    #[test]
    fn native_curve_input_tap_excludes_own_and_later_corrections() {
        let (mut project, sequence, _, _) = native_group_look_fixture();
        let clip_id = project.sequences[&sequence].video_tracks[0].clips[0].id;
        let clip = project.sequences.get_mut(&sequence).unwrap().video_tracks[0]
            .clips
            .iter_mut()
            .find(|clip| clip.id == clip_id)
            .unwrap();
        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 0.7 },
        ));
        let key = GradeOp::new(
            GradeOpKind::Curves,
            GradeOpParams::Curves {
                master: vec![],
                red: vec![],
                green: vec![],
                blue: vec![],
                hue_vs_hue: vec![],
                hue_vs_sat: vec![],
                hue_vs_luma: vec![],
                luma_vs_sat: vec![],
                sat_vs_sat: vec![],
            },
        );
        let op = key.id;
        grade.ops.push(key);
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: -0.3 },
        ));
        clip.grade = Some(grade.clone());
        let frame = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        assert!(
            frame
                .diagnostics
                .iter()
                .all(|d| d.severity != DiagSeverity::Error),
            "{:?}",
            frame.diagnostics
        );
        let point = ScopeTapPoint::NativeCurveInput { clip: clip_id, op };
        let input = frame
            .tap(point)
            .expect("neutral curve still has an exact input tap");
        assert!(
            matches!(frame.graph.nodes[input.0 as usize].op, IrOp::NativeExposure { stops } if stops == 0.7)
        );
        assert_eq!(
            frame.graph.node_color_encoding(input).unwrap(),
            crate::graph::ir::FrameColorEncoding::SceneLinearAcescg
        );
        assert!(!frame
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::NativeLogCurves { .. })));
        assert_eq!(frame.resolve_tap(point), Some((point, input)));
        assert!(frame
            .tap(ScopeTapPoint::NativeCurveInput {
                clip: clip_id,
                op: GradeOpId::new()
            })
            .is_none());
        assert_eq!(
            frame
                .resolve_tap(ScopeTapPoint::NativeCurveInput {
                    clip: clip_id,
                    op: GradeOpId::new()
                })
                .unwrap()
                .0,
            ScopeTapPoint::Program
        );
        grade.convert_to_graph();
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].grade =
            Some(grade.clone());
        let serial = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        let serial_input = serial
            .tap(point)
            .expect("serial graph corrector has its upstream input");
        assert!(
            matches!(serial.graph.nodes[serial_input.0 as usize].op, IrOp::NativeExposure { stops } if stops == 0.7)
        );
        let graph = grade.graph.as_mut().unwrap();
        let corrector = graph
            .nodes
            .iter()
            .find_map(|(&id, node)| {
                matches!(node, GradeGraphNode::Corrector { op: candidate, .. } if *candidate == op)
                    .then_some(id)
            })
            .unwrap();
        graph
            .nodes
            .get_mut(&corrector)
            .unwrap()
            .set_input("image", 0)
            .unwrap();
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].grade =
            Some(grade.clone());
        let rewired = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        let rewired_input = rewired.tap(point).expect("rewired graph has an input");
        assert!(
            !matches!(rewired.graph.nodes[rewired_input.0 as usize].op, IrOp::NativeExposure { stops } if stops == 0.7)
        );
        assert_ne!(
            serial.graph.nodes[serial_input.0 as usize].content_hash,
            rewired.graph.nodes[rewired_input.0 as usize].content_hash
        );
        let graph = grade.graph.as_mut().unwrap();
        let duplicate = graph.next_id;
        graph.next_id += 2;
        graph.nodes.insert(
            duplicate,
            GradeGraphNode::Corrector {
                input: 1,
                op,
                label: "Repeated operator".into(),
            },
        );
        let old_output = match graph.nodes[&graph.output] {
            GradeGraphNode::Output { input } => input,
            _ => unreachable!(),
        };
        graph.nodes.insert(
            duplicate + 1,
            GradeGraphNode::LayerMixer {
                top: duplicate,
                bottom: old_output,
                opacity: 0.5,
                label: String::new(),
            },
        );
        graph
            .nodes
            .get_mut(&graph.output)
            .unwrap()
            .set_input("image", duplicate + 1)
            .unwrap();
        assert_eq!(graph.validate(&grade.ops), Ok(()));
        assert!(!grade.has_unambiguous_corrector_input(op));
        project.sequences.get_mut(&sequence).unwrap().video_tracks[0].clips[0].grade = Some(grade);
        let repeated = compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL);
        assert!(
            repeated.tap(point).is_none(),
            "operator-only inspection must reject ambiguous graph instances"
        );
        let point_for = |node, operator| match point {
            ScopeTapPoint::NativeQualifierInput { clip, .. } => {
                ScopeTapPoint::NativeGraphQualifierInput {
                    clip,
                    node,
                    op: operator,
                }
            }
            ScopeTapPoint::NativeCurveInput { clip, .. } => ScopeTapPoint::NativeGraphCurveInput {
                clip,
                node,
                op: operator,
            },
            _ => unreachable!(),
        };
        let first = repeated
            .tap(point_for(corrector, op))
            .expect("explicit original instance input");
        let second = repeated
            .tap(point_for(duplicate, op))
            .expect("explicit repeated instance input");
        assert_ne!(
            repeated.graph.nodes[first.0 as usize].content_hash,
            repeated.graph.nodes[second.0 as usize].content_hash
        );
        assert!(
            repeated
                .tap(point_for(duplicate, GradeOpId::new()))
                .is_none(),
            "replaced operators cannot satisfy a stale node request"
        );
    }

    #[test]
    fn native_unsupported_shared_look_refuses_both_outputs() {
        let (mut project, sequence, _, look) = native_group_look_fixture();
        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::WhiteBalance,
            GradeOpParams::WhiteBalance {
                temp: 2.0,
                tint: 0.0,
            },
        ));
        let op_id = grade.ops[0].id;
        grade.graph = Some(timeline::GradeGraph::from_stack(&grade.ops));
        project.shared_looks.get_mut(&look).unwrap().grade = grade;
        for compiled in [
            compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL),
            compile_native_delivery(&project, sequence, 0, Tick::ZERO, Quality::FULL),
        ] {
            assert!(compiled
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error && d.message.contains("Shared look")));
            let diagnostic = compiled
                .diagnostics
                .iter()
                .find_map(|d| d.grade.as_ref())
                .unwrap();
            assert_eq!(diagnostic.op, op_id);
            assert_eq!(diagnostic.sequence_path, [sequence]);
            assert!(diagnostic.graph_node.is_some());
            assert_eq!(
                diagnostic.stage,
                Some(photonic_render::grade::GradeStage::SharedLook { id: look })
            );
            assert!(matches!(
                diagnostic.issue,
                photonic_render::grade::GradeIssue::NativeCorrectionUnavailable(_)
            ));
            let wire = serde_json::to_string(diagnostic).unwrap();
            assert_eq!(
                serde_json::from_str::<photonic_render::grade::GradeDiagnostic>(&wire).unwrap(),
                *diagnostic
            );
            assert!(!compiled
                .graph
                .nodes
                .iter()
                .any(|node| matches!(node.op, IrOp::NativeDecodeVideo { .. })));
        }
    }

    #[test]
    fn native_group_ancestry_errors_refuse_rendering() {
        let (project, sequence, group, _) = native_group_look_fixture();
        for parent in [Some(group), Some(timeline::GroupId::new())] {
            let mut project = project.clone();
            project
                .sequences
                .get_mut(&sequence)
                .unwrap()
                .groups
                .get_mut(&group)
                .unwrap()
                .parent = parent;
            for compiled in [
                compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL),
                compile_native_delivery(&project, sequence, 0, Tick::ZERO, Quality::FULL),
            ] {
                assert!(compiled
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error
                        && d.message.contains("group reference")));
                assert!(!compiled
                    .graph
                    .nodes
                    .iter()
                    .any(|node| matches!(node.op, IrOp::NativeDecodeVideo { .. })));
            }
        }
    }

    #[test]
    fn native_missing_shared_look_retains_provenance_and_refuses_rendering() {
        let (mut project, sequence, _, look) = native_group_look_fixture();
        project.shared_looks.remove(&look);
        for compiled in [
            compile_native_preview(&project, sequence, 0, Tick::ZERO, Quality::FULL),
            compile_native_delivery(&project, sequence, 0, Tick::ZERO, Quality::FULL),
        ] {
            assert!(compiled.diagnostics.iter().any(|d| d.grade.as_ref().is_some_and(|grade|
                grade.sequence_path == [sequence] && matches!(grade.issue, photonic_render::grade::GradeIssue::MissingSharedLook(id) if id == look)
            )));
            assert!(!compiled
                .graph
                .nodes
                .iter()
                .any(|node| matches!(node.op, IrOp::NativeDecodeVideo { .. })));
        }
    }

    #[test]
    fn native_timeline_preview_renders_only_qualified_clip() {
        use photonic_core::timeline::color::{
            InputMatrix, InputSignalRange, NativeInputColorInterpretation, NativeInputStandard,
            NativeManagedColorConfig, SequenceColorConfig,
        };
        let mut project = TimelineProject::new();
        let mut asset = MediaAsset::from_file(AssetKind::Video, "/tmp/native-preview.mp4");
        let asset_id = asset.id;
        asset.native_input_color = Some(NativeInputColorInterpretation {
            hlg_peak_nits: None,
            reference_white_nits: None,
            version: 1,
            standard: NativeInputStandard::Bt709Scene,
            range: InputSignalRange::Limited,
            matrix: InputMatrix::Bt709,
            chroma_location: None,
        });
        project.media.insert(asset);
        let mut seq = Sequence::new("native", FrameRate::FPS_30, 640, 360);
        seq.color =
            SequenceColorConfig::NativeManaged(Box::new(NativeManagedColorConfig::sdr_draft()));
        let seq_id = seq.id;
        let mut track = Track::new(TrackKind::Video, "V1");
        track.clips.push(Clip::new(
            ClipSource::Asset { asset: asset_id },
            Tick::ZERO,
            Tick(1_000_000),
        ));
        seq.video_tracks.push(track);
        project.insert_sequence(seq);
        let preview = compile_native_preview(&project, seq_id, 0, Tick::ZERO, Quality::PREVIEW);
        assert!(preview.diagnostics.is_empty(), "{:?}", preview.diagnostics);
        assert!(preview.graph.validate_working_color_domain().is_ok());
        assert_eq!(
            preview.graph.output_color_encoding(),
            Ok(crate::graph::ir::FrameColorEncoding::SrgbDisplay)
        );
        let clip_id = project.sequences[&seq_id].video_tracks[0].clips[0].id;
        let program_tap = preview.tap(ScopeTapPoint::Program).unwrap();
        let clip_tap = preview.tap(ScopeTapPoint::Clip(clip_id)).unwrap();
        assert_eq!(
            preview.graph.node_color_encoding(program_tap),
            Ok(crate::graph::ir::FrameColorEncoding::SrgbDisplay)
        );
        assert_eq!(
            preview.graph.node_color_encoding(clip_tap),
            Ok(crate::graph::ir::FrameColorEncoding::SceneLinearAcescg)
        );
        assert!(preview
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::NativeDecodeVideo { .. })));
        let mut serial = Grade::new();
        serial.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        ));
        serial.graph = Some(timeline::GradeGraph::from_stack(&serial.ops));
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[0].clips[0].grade = Some(serial);
        let graphed = compile_native_preview(&project, seq_id, 0, Tick::ZERO, Quality::PREVIEW);
        assert!(graphed.diagnostics.is_empty(), "{:?}", graphed.diagnostics);
        assert!(graphed
            .graph
            .nodes
            .iter()
            .any(|node| { matches!(node.op, IrOp::NativeExposure { stops } if stops == 1.0) }));
        let original_source = project.media.assets[&asset_id].source.clone();
        project.media.assets.get_mut(&asset_id).unwrap().source = timeline::AssetSource::File {
            path: std::env::temp_dir()
                .join(format!("photonic-native-missing-{}", uuid::Uuid::new_v4())),
            rel_path: None,
        };
        let offline = compile_native_preview_live(&project, seq_id, 0, Tick::ZERO, Quality::FULL);
        assert!(offline.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == Some(CompileCode::ColorPipelineUnavailable)
                && diagnostic.message.contains("offline")
        }));
        assert!(!offline
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::NativeDecodeVideo { .. })));
        let live_path =
            std::env::temp_dir().join(format!("photonic-native-live-{}", uuid::Uuid::new_v4()));
        std::fs::write(&live_path, b"source placeholder").unwrap();
        project.media.assets.get_mut(&asset_id).unwrap().source = timeline::AssetSource::File {
            path: live_path.clone(),
            rel_path: None,
        };
        let unprobed = compile_native_preview_live(&project, seq_id, 0, Tick::ZERO, Quality::FULL);
        assert!(unprobed.diagnostics.iter().any(|diagnostic| {
            diagnostic.message.contains("probed source pixel format")
                && diagnostic.code == Some(CompileCode::ColorPipelineUnavailable)
        }));
        let mut valid_probe = timeline::MediaProbe::basic(Tick(1_000_000), "mov", "test");
        valid_probe.pixel_format = Some("yuv444p".into());
        project.media.assets.get_mut(&asset_id).unwrap().probe = Some(valid_probe);
        let live = compile_native_preview_live(&project, seq_id, 0, Tick::ZERO, Quality::FULL);
        assert!(live.diagnostics.is_empty(), "{:?}", live.diagnostics);
        let mut unsupported_probe = timeline::MediaProbe::basic(Tick(1_000_000), "mov", "test");
        unsupported_probe.pixel_format = Some("gbrp".into());
        project.media.assets.get_mut(&asset_id).unwrap().probe = Some(unsupported_probe);
        let unsupported =
            compile_native_preview_live(&project, seq_id, 0, Tick::ZERO, Quality::FULL);
        assert!(unsupported.diagnostics.iter().any(|diagnostic| {
            diagnostic.message.contains("gbrp")
                && diagnostic.code == Some(CompileCode::ColorPipelineUnavailable)
        }));
        assert!(!unsupported
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::NativeDecodeVideo { .. })));
        project.media.assets.get_mut(&asset_id).unwrap().probe = None;
        std::fs::remove_file(&live_path).unwrap();
        project.media.assets.get_mut(&asset_id).unwrap().source = original_source;
        let delivery = compile_native_delivery(&project, seq_id, 0, Tick::ZERO, Quality::FULL);
        assert!(
            delivery.diagnostics.is_empty(),
            "{:?}",
            delivery.diagnostics
        );
        assert!(delivery.graph.validate_working_color_domain().is_ok());
        assert_eq!(
            delivery.graph.output_color_encoding(),
            Ok(crate::graph::ir::FrameColorEncoding::Bt709Video)
        );
        assert!(matches!(
            delivery.graph.nodes.last().unwrap().op,
            IrOp::NativeSdrVideoOutput
        ));
        let gap = compile_native_delivery(&project, seq_id, 0, Tick(2_000_000), Quality::FULL);
        assert!(gap.diagnostics.is_empty());
        assert_eq!(
            gap.graph.output_color_encoding(),
            Ok(crate::graph::ir::FrameColorEncoding::Bt709Video)
        );
        if let SequenceColorConfig::NativeManaged(config) =
            &mut project.sequences.get_mut(&seq_id).unwrap().color
        {
            config.export = photonic_core::timeline::color::NativeOutputTransform::SrgbSdr;
        }
        let wrong_output = compile_native_delivery(&project, seq_id, 0, Tick::ZERO, Quality::FULL);
        assert!(wrong_output
            .diagnostics
            .iter()
            .any(|d| d.code == Some(CompileCode::ColorPipelineUnavailable)));
        assert!(!wrong_output
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::NativeDecodeVideo { .. })));
        if let SequenceColorConfig::NativeManaged(config) =
            &mut project.sequences.get_mut(&seq_id).unwrap().color
        {
            config.export = photonic_core::timeline::color::NativeOutputTransform::Bt709VideoSdr;
        }

        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        ));
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[0].clips[0].grade = Some(grade);
        let graded = compile_native_preview(&project, seq_id, 0, Tick::ZERO, Quality::PREVIEW);
        assert!(graded.diagnostics.is_empty(), "{:?}", graded.diagnostics);
        assert!(graded
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::NativeExposure { stops } if stops == 1.0)));

        let exposure = |stops| {
            let mut grade = Grade::new();
            grade.ops.push(GradeOp::new(
                GradeOpKind::Exposure,
                GradeOpParams::Exposure { stops },
            ));
            grade
        };
        project.media.assets.get_mut(&asset_id).unwrap().grade = Some(exposure(2.0));
        let seq = project.sequences.get_mut(&seq_id).unwrap();
        seq.video_tracks[0].grade = Some(exposure(3.0));
        seq.master_grade = Some(exposure(4.0));
        let scoped = compile_native_preview(&project, seq_id, 0, Tick::ZERO, Quality::PREVIEW);
        assert!(scoped.diagnostics.is_empty(), "{:?}", scoped.diagnostics);
        let stops: Vec<_> = scoped
            .graph
            .nodes
            .iter()
            .filter_map(|node| match node.op {
                IrOp::NativeExposure { stops } => Some(stops),
                _ => None,
            })
            .collect();
        assert_eq!(stops, [2.0, 1.0, 3.0, 4.0]);

        let mut upper = Track::new(TrackKind::Video, "V2");
        upper.clips.push(Clip::new(
            ClipSource::Asset { asset: asset_id },
            Tick::ZERO,
            Tick(1_000_000),
        ));
        project
            .sequences
            .get_mut(&seq_id)
            .unwrap()
            .video_tracks
            .push(upper);
        let layered = compile_native_preview(&project, seq_id, 0, Tick::ZERO, Quality::PREVIEW);
        assert!(layered.diagnostics.is_empty(), "{:?}", layered.diagnostics);
        assert!(layered.graph.validate_working_color_domain().is_ok());
        assert_eq!(layered.clip_taps.len(), 2);
        assert!(layered.graph.nodes.iter().any(|node| matches!(
            node.op,
            IrOp::Merge {
                mode: BlendMode::Normal,
                ..
            }
        )));
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[1].opacity = 0.5;
        let mixed = compile_native_preview(&project, seq_id, 0, Tick::ZERO, Quality::PREVIEW);
        assert!(mixed.diagnostics.is_empty(), "{:?}", mixed.diagnostics);
        assert!(mixed.graph.nodes.iter().any(|node| matches!(node.op, IrOp::Merge { mode: BlendMode::Normal, opacity } if opacity == 0.5)));
        for mode in [
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
            project.sequences.get_mut(&seq_id).unwrap().video_tracks[1].blend = mode;
            let blended = compile_native_preview(&project, seq_id, 0, Tick::ZERO, Quality::PREVIEW);
            assert!(blended.diagnostics.is_empty(), "{:?}", blended.diagnostics);
            assert!(blended.graph.validate_working_color_domain().is_ok());
            assert!(blended.graph.nodes.iter().any(|node| matches!(node.op, IrOp::Merge { mode: found, opacity } if found == mode && opacity == 0.5)));
        }
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[1].blend = BlendMode::Color;

        let color_blend = compile_native_preview(&project, seq_id, 0, Tick::ZERO, Quality::PREVIEW);
        assert!(
            color_blend.diagnostics.is_empty(),
            "{:?}",
            color_blend.diagnostics
        );
        assert!(color_blend.graph.validate_working_color_domain().is_ok());
        assert!(color_blend.graph.nodes.iter().any(|node| matches!(
            node.op,
            IrOp::NativeBoundedMerge {
                mode: BlendMode::Color,
                ..
            }
        )));
        project
            .sequences
            .get_mut(&seq_id)
            .unwrap()
            .video_tracks
            .pop();

        let mut adjustment_track = Track::new(TrackKind::Video, "Adjustment");
        let mut adjustment = Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick(1_000_000));
        adjustment.grade = Some(exposure(0.25));
        adjustment_track.clips.push(adjustment);
        project
            .sequences
            .get_mut(&seq_id)
            .unwrap()
            .video_tracks
            .push(adjustment_track);
        let adjusted = compile_native_preview(&project, seq_id, 0, Tick::ZERO, Quality::PREVIEW);
        assert!(
            adjusted.diagnostics.is_empty(),
            "{:?}",
            adjusted.diagnostics
        );
        let order: Vec<_> = adjusted
            .graph
            .nodes
            .iter()
            .filter_map(|node| match node.op {
                IrOp::NativeExposure { stops } => Some(stops),
                _ => None,
            })
            .collect();
        assert_eq!(order, [2.0, 1.0, 3.0, 0.25, 4.0]);
        project
            .sequences
            .get_mut(&seq_id)
            .unwrap()
            .video_tracks
            .pop();

        project.sequences.get_mut(&seq_id).unwrap().master_grade = Some(exposure(100.0));
        let unsupported = compile_native_preview(&project, seq_id, 0, Tick::ZERO, Quality::PREVIEW);
        assert!(unsupported
            .diagnostics
            .iter()
            .any(|d| d.code == Some(CompileCode::ColorPipelineUnavailable)));
        assert!(!unsupported
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::NativeDecodeVideo { .. })));
        project.sequences.get_mut(&seq_id).unwrap().master_grade = Some(exposure(4.0));

        project.sequences.get_mut(&seq_id).unwrap().video_tracks[0].clips[0]
            .transform
            .base
            .x = 2.0;
        let reframed = compile_native_preview(&project, seq_id, 0, Tick::ZERO, Quality::PREVIEW);
        assert!(
            reframed.diagnostics.is_empty(),
            "{:?}",
            reframed.diagnostics
        );
        assert!(reframed
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::Transform2DTransparent { .. })));
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[0].clips[0]
            .transform
            .base
            .opacity = 0.5;
        let translucent = compile_native_preview(&project, seq_id, 0, Tick::ZERO, Quality::PREVIEW);
        assert!(
            translucent.diagnostics.is_empty(),
            "{:?}",
            translucent.diagnostics
        );
        assert!(translucent.graph.nodes.iter().any(|node| matches!(node.op, IrOp::Merge { mode: BlendMode::Normal, opacity } if opacity == 0.5)));
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[0].clips[0]
            .transform
            .base
            .opacity = f64::NAN;
        let refused = compile_native_preview(&project, seq_id, 0, Tick::ZERO, Quality::PREVIEW);
        assert!(refused
            .diagnostics
            .iter()
            .any(|d| d.code == Some(CompileCode::ColorPipelineUnavailable)));
        assert!(!refused
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::NativeDecodeVideo { .. })));

        project.sequences.get_mut(&seq_id).unwrap().video_tracks[0].clips[0]
            .transform
            .base
            .x = 0.0;
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[0].clips[0]
            .transform
            .base
            .opacity = 1.0;
        project
            .media
            .assets
            .get_mut(&asset_id)
            .unwrap()
            .native_input_color = None;
        let missing = compile_native_preview(&project, seq_id, 0, Tick::ZERO, Quality::PREVIEW);
        assert!(missing
            .diagnostics
            .iter()
            .any(|d| d.code == Some(CompileCode::ColorPipelineUnavailable)));
        assert!(!missing
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::NativeDecodeVideo { .. })));
    }

    #[test]
    fn compile_is_deterministic_across_runs() {
        let (mut project, seq_id) = base_project();
        let tk = add_video_track(&mut project, seq_id);
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(solid_clip(
                Color {
                    r: 0.1,
                    g: 0.2,
                    b: 0.3,
                    a: 1.0,
                },
                0,
                Tick::from_seconds(2).0,
            ));
        let a = compile(&project, seq_id, 0, Tick(500), Quality::PREVIEW, None);
        let b = compile(&project, seq_id, 0, Tick(500), Quality::PREVIEW, None);
        let ha: Vec<u128> = a.graph.nodes.iter().map(|n| n.content_hash.0).collect();
        let hb: Vec<u128> = b.graph.nodes.iter().map(|n| n.content_hash.0).collect();
        assert_eq!(ha, hb, "content hashes are run-stable");
    }

    #[test]
    fn asset_video_clip_emits_decode_video_at_mapped_src_time() {
        let (mut project, seq_id) = base_project();
        let asset = MediaAsset::from_file(AssetKind::Video, "/tmp/x.mp4");
        let aid = asset.id;
        project.media.insert(asset);
        let tk = add_video_track(&mut project, seq_id);
        let mut clip = Clip::new(
            ClipSource::Asset { asset: aid },
            Tick(0),
            Tick::from_seconds(4),
        );
        clip.source_in = Tick(1000);
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(clip);
        let out = compile(&project, seq_id, 0, Tick(500), Quality::PREVIEW, None);
        let decode = out
            .graph
            .nodes
            .iter()
            .find_map(|n| match &n.op {
                IrOp::DecodeVideo {
                    src_time, proxy, ..
                } => Some((*src_time, *proxy)),
                _ => None,
            })
            .expect("decode node present");
        // src_time = source_in(1000) + (tick 500 − clip.start 0) at 1× speed = 1500.
        assert_eq!(decode.0, Tick(1500));
        assert!(decode.1, "preview quality requests proxy");
    }

    #[test]
    fn distinct_grades_produce_distinct_hashes() {
        use photonic_core::timeline::grade::{Grade, GradeOp, GradeOpKind, GradeOpParams};
        let grade_node_hash = |stops: f32| -> u128 {
            let (mut project, seq_id) = base_project();
            let tk = add_video_track(&mut project, seq_id);
            let mut clip = solid_clip(
                Color {
                    r: 0.5,
                    g: 0.5,
                    b: 0.5,
                    a: 1.0,
                },
                0,
                Tick::from_seconds(2).0,
            );
            let mut grade = Grade::new();
            grade.ops.push(GradeOp::new(
                GradeOpKind::Exposure,
                GradeOpParams::Exposure { stops },
            ));
            clip.grade = Some(grade);
            project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
                .clips
                .push(clip);
            let out = compile(&project, seq_id, 0, Tick(0), Quality::FULL, None);
            out.graph
                .nodes
                .iter()
                .find_map(|n| match &n.op {
                    IrOp::Grade { .. } => Some(n.content_hash.0),
                    _ => None,
                })
                .expect("grade node present")
        };
        // Different resolved params ⇒ different content hash (cache-correct).
        assert_ne!(grade_node_hash(1.0), grade_node_hash(2.0));
        // Same params ⇒ stable hash (determinism, 02 §2).
        assert_eq!(grade_node_hash(1.5), grade_node_hash(1.5));
    }

    #[test]
    fn text_node_lowers_to_textgen() {
        // Project graph: Text → Output. `Text` is a 0-input generator lowering to
        // the dedicated `TextGen` IR op (08 §2).
        let (mut project, seq_id) = base_project();
        let tk = add_video_track(&mut project, seq_id);
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(solid_clip(Color::BLACK, 0, Tick::from_seconds(2).0));

        let text = GraphNode::new(GraphOp::Text {
            text: photonic_core::timeline::TextGen::default(),
        });
        let output = GraphNode::new(GraphOp::Output);
        let (tx, ou) = (text.id, output.id);
        let mut nodes = std::collections::HashMap::new();
        nodes.insert(tx, text);
        nodes.insert(ou, output);
        let pg = NodeGraph {
            id: GraphId::new(),
            name: "pg".into(),
            nodes,
            edges: vec![GraphEdge {
                from: (tx, GOutPort::PRIMARY),
                to: (ou, InPort::PRIMARY),
            }],
            output: ou,
            ui: std::collections::HashMap::new(),
        };
        let pgid = pg.id;
        project.graphs.insert(pgid, pg);
        project.project_graph = Some(pgid);

        let out = compile(&project, seq_id, 0, Tick(0), Quality::PREVIEW, None);
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        let output = out.graph.output.unwrap();
        let out_in = out.graph.nodes[output.0 as usize].inputs[0].0;
        assert!(
            matches!(out.graph.nodes[out_in.0 as usize].op, IrOp::TextGen { .. }),
            "Text lowered to TextGen feeding Output"
        );
    }

    #[test]
    fn channel_and_matte_nodes_lower_to_dedicated_ir() {
        // Composition: ClipIn → ChannelSplit → MaskFromMatte → Output. Verifies
        // both dedicated IR lowerings (08 §2 §3.4) land as ChannelSplit +
        // MatteExtract, not generic Effect nodes.
        let (mut project, seq_id) = base_project();
        let tk = add_video_track(&mut project, seq_id);

        let clip_in = GraphNode::new(GraphOp::ClipIn);
        let split = GraphNode::new(GraphOp::ChannelSplit);
        let matte = GraphNode::new(GraphOp::MaskFromMatte);
        let output = GraphNode::new(GraphOp::Output);
        let (ci, sp, ma, ou) = (clip_in.id, split.id, matte.id, output.id);
        let mut nodes = std::collections::HashMap::new();
        for n in [clip_in, split, matte, output] {
            nodes.insert(n.id, n);
        }
        let graph = NodeGraph {
            id: GraphId::new(),
            name: "comp".into(),
            nodes,
            edges: vec![
                GraphEdge {
                    from: (ci, GOutPort::PRIMARY),
                    to: (sp, InPort::PRIMARY),
                },
                GraphEdge {
                    from: (sp, GOutPort::PRIMARY),
                    to: (ma, InPort::PRIMARY),
                },
                GraphEdge {
                    from: (ma, GOutPort::PRIMARY),
                    to: (ou, InPort::PRIMARY),
                },
            ],
            output: ou,
            ui: std::collections::HashMap::new(),
        };
        let gid = graph.id;
        project.graphs.insert(gid, graph);
        let mut clip = solid_clip(Color::BLACK, 0, Tick::from_seconds(2).0);
        clip.composition = Some(gid);
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(clip);

        let out = compile(&project, seq_id, 0, Tick(0), Quality::PREVIEW, None);
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::ChannelSplit { .. })),
            "ChannelSplit lowered to its dedicated IR op"
        );
        assert!(
            out.graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::MatteExtract { .. })),
            "MaskFromMatte lowered to MatteExtract"
        );
        assert!(
            !out.graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::Effect { .. })),
            "no generic Effect placeholders for these ops"
        );
    }

    #[test]
    fn dip_to_black_midpoint_is_black() {
        // Two adjacent solids; the second dips-to-black in. At the exact midpoint
        // the frame is black (through-color), regardless of either clip's color.
        let (mut project, seq_id) = base_project();
        let tk = add_video_track(&mut project, seq_id);
        let clips = &mut project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk].clips;
        clips.push(Clip::new(
            ClipSource::SolidColor {
                color: Color {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
            },
            Tick(0),
            Tick(100),
        ));
        let mut b = Clip::new(
            ClipSource::SolidColor {
                color: Color {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
            },
            Tick(100),
            Tick(100),
        );
        b.transition_in = Some(photonic_core::timeline::Transition::new(
            TransitionKind::DipToBlack,
            Tick(40),
        ));
        clips.push(b);

        // t=120 → raw 0.5 → EaseInOut 0.5 → second phase opacity 0 → pure black.
        let out = compile(&project, seq_id, 0, Tick(120), Quality::FULL, None);
        let img = crate::graph::eval_cpu::evaluate(
            &out.graph,
            (4, 4),
            &mut crate::graph::eval_cpu::EmptyProvider,
        );
        for p in &img.pixels {
            for (c, &v) in p[..3].iter().enumerate() {
                assert!(v.abs() < 1e-4, "dip midpoint black, channel {c} = {v}");
            }
        }
    }

    // ── Caption overlay resolution (06 §5) ────────────────────────────────────

    use photonic_core::timeline::{
        CaptionCue, CaptionStyle, CaptionTrack, CaptionWord, KaraokeMode, KaraokeStyle,
    };

    /// Fetch the single `CaptionOverlay` node's resolved batch + content hash.
    fn caption_node(graph: &FrameGraph) -> (&CaptionBatch, ContentHash) {
        let n = graph
            .nodes
            .iter()
            .find(|n| matches!(n.op, IrOp::CaptionOverlay { .. }))
            .expect("a CaptionOverlay node is present");
        match &n.op {
            IrOp::CaptionOverlay { cue_batch } => (cue_batch, n.content_hash),
            // Unreachable: `find` above already matched on `IrOp::CaptionOverlay`,
            // so `n.op` is guaranteed to be that variant here.
            _ => unreachable!(),
        }
    }

    fn wordpop_track() -> CaptionTrack {
        let mut track = CaptionTrack::new("Captions");
        track.style = CaptionStyle {
            highlight: Some(KaraokeStyle {
                mode: KaraokeMode::WordPop,
                active_color: Color {
                    r: 1.0,
                    g: 1.0,
                    b: 0.0,
                    a: 1.0,
                }, // yellow
                inactive_color: Color {
                    r: 0.5,
                    g: 0.5,
                    b: 0.5,
                    a: 1.0,
                }, // grey
            }),
            ..CaptionStyle::default()
        };
        // "hello" [0,100), "world" [100,200); cue [0,200).
        let cue = CaptionCue::new(
            Tick(0),
            Tick(200),
            vec![
                CaptionWord::new("hello", Tick(0), Tick(100)),
                CaptionWord::new("world", Tick(100), Tick(200)),
            ],
        );
        track.cues.push(cue);
        track
    }

    /// A covering cue on an enabled caption track lowers to a `CaptionOverlay`
    /// carrying a populated batch (not the old empty default) — the un-stubbing.
    #[test]
    fn covering_cue_populates_caption_batch() {
        let (mut project, seq_id) = base_project();
        let tk = add_video_track(&mut project, seq_id);
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(solid_clip(Color::BLACK, 0, 400));
        project
            .sequences
            .get_mut(&seq_id)
            .unwrap()
            .caption_tracks
            .push(wordpop_track());

        let out = compile(&project, seq_id, 0, Tick(50), Quality::FULL, None);
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::CaptionOverlay { .. })),
            "graph has a CaptionOverlay node"
        );
        let (batch, _) = caption_node(&out.graph);
        assert_eq!(batch.cues.len(), 1, "one covering cue resolved");
        let cue = &batch.cues[0];
        assert_eq!(cue.words.len(), 2);
        assert_eq!(cue.words[0].text, "hello");
        assert_eq!(cue.words[1].text, "world");
        // Anchor is the track style's default caption position (01 §7).
        assert_eq!(cue.anchor, CaptionStyle::default().position);
    }

    /// No enabled caption track / no covering cue ⇒ no CaptionOverlay node.
    #[test]
    fn no_cue_emits_no_caption_overlay() {
        let (mut project, seq_id) = base_project();
        let tk = add_video_track(&mut project, seq_id);
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(solid_clip(Color::BLACK, 0, 400));
        project
            .sequences
            .get_mut(&seq_id)
            .unwrap()
            .caption_tracks
            .push(wordpop_track());
        // Tick 300 is past the cue's [0,200) span.
        let out = compile(&project, seq_id, 0, Tick(300), Quality::FULL, None);
        assert!(
            !out.graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::CaptionOverlay { .. })),
            "no covering cue ⇒ no CaptionOverlay"
        );
        // A disabled track never overlays even when a cue covers the tick.
        project.sequences.get_mut(&seq_id).unwrap().caption_tracks[0].enabled = false;
        let out = compile(&project, seq_id, 0, Tick(50), Quality::FULL, None);
        assert!(
            !out.graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::CaptionOverlay { .. })),
            "disabled caption track ⇒ no CaptionOverlay"
        );
    }

    /// WordPop karaoke (06 §5.1): the word whose window contains `t` renders in
    /// `active_color`, the others in `inactive_color`; and the resolved batch's
    /// content hash changes across ticks so the node-result cache re-renders the
    /// sweep (02 §5). Before/mid the second word must differ.
    #[test]
    fn wordpop_karaoke_recolors_and_rehashes() {
        let (mut project, seq_id) = base_project();
        let tk = add_video_track(&mut project, seq_id);
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(solid_clip(Color::BLACK, 0, 400));
        project
            .sequences
            .get_mut(&seq_id)
            .unwrap()
            .caption_tracks
            .push(wordpop_track());

        let active = [255, 255, 0, 255]; // yellow
        let inactive = [128, 128, 128, 255]; // grey (0.5 → 128)

        let at = |t: i64| compile(&project, seq_id, 0, Tick(t), Quality::FULL, None).graph;

        // t=50: word0 ("hello") active, word1 ("world") inactive.
        let g_before = at(50);
        let (b0, h0) = caption_node(&g_before);
        assert_eq!(b0.cues[0].words[0].color, active, "hello active at t=50");
        assert_eq!(
            b0.cues[0].words[1].color, inactive,
            "world inactive at t=50"
        );

        // t=150: swap — word1 ("world") active, word0 inactive.
        let g_mid = at(150);
        let (b1, h1) = caption_node(&g_mid);
        assert_eq!(
            b1.cues[0].words[0].color, inactive,
            "hello inactive at t=150"
        );
        assert_eq!(b1.cues[0].words[1].color, active, "world active at t=150");

        // The sweep must change the CaptionOverlay content hash (drives re-render).
        assert_ne!(
            h0, h1,
            "karaoke sweep changes the CaptionOverlay content hash"
        );
    }

    fn op_name(op: &IrOp) -> &'static str {
        match op {
            IrOp::DecodeVideo { .. } => "DecodeVideo",
            IrOp::NativeDecodeVideo { .. } => "NativeDecodeVideo",
            IrOp::NativeDecodeStill { .. } => "NativeDecodeStill",
            IrOp::NativeMediaVideo { .. } => "NativeMediaVideo",
            IrOp::NativeMediaStill { .. } => "NativeMediaStill",
            IrOp::DecodeStill { .. } => "DecodeStill",
            IrOp::RasterVector { .. } => "RasterVector",
            IrOp::NativeRasterVector { .. } => "NativeRasterVector",
            IrOp::SolidColor { .. } => "SolidColor",
            IrOp::Transform2D { .. } => "Transform2D",
            IrOp::Transform2DTransparent { .. } => "Transform2DTransparent",
            IrOp::StabilizeWarp { .. } => "StabilizeWarp",
            IrOp::Effect { .. } => "Effect",
            IrOp::Grade { .. } => "Grade",
            IrOp::NativeOutline { .. } => "NativeOutline",
            IrOp::NativeLinearHsl { .. } => "NativeLinearHsl",
            IrOp::NativeSurfaceBlur { .. } => "NativeSurfaceBlur",
            IrOp::NativeLensBlur { .. } => "NativeLensBlur",
            IrOp::NativeRankFilter { .. } => "NativeRankFilter",
            IrOp::NativeDeflicker { .. } => "NativeDeflicker",
            IrOp::NativeUnpremultiply => "NativeUnpremultiply",
            IrOp::NativeAlphaView { .. } => "NativeAlphaView",
            IrOp::NativeTransferLevels { .. } => "NativeTransferLevels",
            IrOp::NativeTransferCurves { .. } => "NativeTransferCurves",
            IrOp::NativeColorBalance { .. } => "NativeColorBalance",
            IrOp::NativePhotoFilter { .. } => "NativePhotoFilter",
            IrOp::NativeTransferMatrix { .. } => "NativeTransferMatrix",
            IrOp::NativeTransferArtistic { .. } => "NativeTransferArtistic",
            IrOp::NativeDropShadow { .. } => "NativeDropShadow",
            IrOp::NativePerspective { .. } => "NativePerspective",
            IrOp::NativeStylize { .. } => "NativeStylize",
            IrOp::NativeDetail { .. } => "NativeDetail",
            IrOp::NativeGeometricWarp { .. } => "NativeGeometricWarp",
            IrOp::NativeBoxBlur { .. } => "NativeBoxBlur",
            IrOp::NativeMosaic { .. } => "NativeMosaic",
            IrOp::NativeMotionBlur { .. } => "NativeMotionBlur",
            IrOp::NativeGaussianBlur { .. } => "NativeGaussianBlur",
            IrOp::NativeUnsharp { .. } => "NativeUnsharp",
            IrOp::NativeChannelExtract { .. } => "NativeChannelExtract",
            IrOp::NativeImageToMask => "NativeImageToMask",
            IrOp::NativeMaskToImage => "NativeMaskToImage",
            IrOp::NativeChannelCombine => "NativeChannelCombine",
            IrOp::NativeInvert => "NativeInvert",
            IrOp::NativeVignette { .. } => "NativeVignette",
            IrOp::NativeGrain { .. } => "NativeGrain",
            IrOp::NativeDenoise { .. } => "NativeDenoise",
            IrOp::NativeMaskShape { .. } => "NativeMaskShape",
            IrOp::NativePolygonMask { .. } => "NativePolygonMask",
            IrOp::NativeGlow { .. } => "NativeGlow",
            IrOp::NativeChromaKey { .. } => "NativeChromaKey",
            IrOp::NativeLumaKey { .. } => "NativeLumaKey",
            IrOp::NativeCanvas { .. } => "NativeCanvas",
            IrOp::NativeDissolve { .. } => "NativeDissolve",
            IrOp::NativeLumaWipe { .. } => "NativeLumaWipe",
            IrOp::NativeExposure { .. } => "NativeExposure",
            IrOp::NativeLinearOffset { .. } => "NativeLinearOffset",
            IrOp::NativePrinterLights { .. } => "NativePrinterLights",
            IrOp::NativeHighlightRolloff { .. } => "NativeHighlightRolloff",
            IrOp::NativeLogContrast { .. } => "NativeLogContrast",
            IrOp::NativeLogCdl { .. } => "NativeLogCdl",
            IrOp::WindowMatte { .. } => "WindowMatte",
            IrOp::BezierMatte { .. } => "BezierMatte",
            IrOp::QualifierMatte { .. } => "QualifierMatte",
            IrOp::GradeKeyMix { .. } => "GradeKeyMix",
            IrOp::GradeMatteRefine { .. } => "GradeMatteRefine",
            IrOp::GradeMatteApply => "GradeMatteApply",
            IrOp::NativeMatteCdl { .. } => "NativeMatteCdl",
            IrOp::NativeRgbMatrix { .. } => "NativeRgbMatrix",
            IrOp::NativeTonalExposure { .. } => "NativeTonalExposure",
            IrOp::GradeMatteConstant { .. } => "GradeMatteConstant",
            IrOp::GradeLayerMix { .. } => "GradeLayerMix",
            IrOp::NativeLogQualifier { .. } => "NativeLogQualifier",
            IrOp::NativeLogCurves { .. } => "NativeLogCurves",
            IrOp::NativeSaturationVibrance { .. } => "NativeSaturationVibrance",
            IrOp::NativeLut3d { .. } => "NativeLut3d",
            IrOp::NativeMaskMix { .. } => "NativeMaskMix",
            IrOp::NativeAcescct { .. } => "NativeAcescct",
            IrOp::NativeSdrOutput => "NativeSdrOutput",
            IrOp::NativeSdrVideoOutput => "NativeSdrVideoOutput",
            IrOp::NativeLuminanceMerge { .. } => "NativeLuminanceMerge",
            IrOp::NativeBoundedMerge { .. } => "NativeBoundedMerge",
            IrOp::NativeArithmeticMerge { .. } => "NativeArithmeticMerge",
            IrOp::Merge { .. } => "Merge",
            IrOp::WipeMix { .. } => "WipeMix",
            IrOp::PushMix { .. } => "PushMix",
            IrOp::LumaWipeMix { .. } => "LumaWipeMix",
            IrOp::CaptionOverlay { .. } => "CaptionOverlay",
            IrOp::Crop { .. } => "Crop",
            IrOp::Resize { .. } => "Resize",
            IrOp::MatteExtract { .. } => "MatteExtract",
            IrOp::NativeMatteExtract => "NativeMatteExtract",
            IrOp::TextGen { .. } => "TextGen",
            IrOp::NativeTextGen { .. } => "NativeTextGen",
            IrOp::NativeDecoratedTextGen { .. } => "NativeDecoratedTextGen",
            IrOp::NativeCaptionOutput { .. } => "NativeCaptionOutput",
            IrOp::ChannelSplit { .. } => "ChannelSplit",
            IrOp::ChannelCombine => "ChannelCombine",
            IrOp::Deinterlace { .. } => "Deinterlace",
            IrOp::Output { .. } => "Output",
        }
    }

    // ── 38 §1/§2/§3 sequence-semantics tests ──────────────────────────────────
    use photonic_core::timeline::{
        MediaProbe, ProbedColor, Ratio, SpeedMap, Transition, VideoStreamInfo,
    };

    /// One FPS_30 frame in ticks — the unit the transition-handle tests count in.
    fn tpf30() -> i64 {
        FrameRate::FPS_30.ticks_per_frame().0
    }
    /// `n` FPS_30 frames as a `Tick`.
    fn f30(n: i64) -> Tick {
        Tick(tpf30() * n)
    }

    /// Any `Merge` whose opacity is strictly between 0 and 1 — the signature of a
    /// live transition / fade mix (plain fully-opaque folds never emit one here,
    /// since the test clips all sit at track/clip opacity 1).
    fn has_fractional_merge(graph: &FrameGraph) -> bool {
        graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::Merge { opacity, .. } if opacity > 0.0 && opacity < 1.0))
    }

    /// Content hash of the graph's `Output` node — the whole render subtree's
    /// identity (equal hash ⇒ identical pixels ⇒ the node cache serves one entry).
    fn output_hash(graph: &FrameGraph) -> u128 {
        let out = graph.output.expect("graph has an output");
        graph.nodes[out.0 as usize].content_hash.0
    }

    fn count_code(out: &CompiledFrame, code: CompileCode) -> usize {
        out.diagnostics
            .iter()
            .filter(|d| d.code == Some(code))
            .count()
    }

    /// A video asset with a probe carrying just a `duration` (no video stream, so
    /// the per-clip conform check in §3.5 stays silent) — for handle math.
    fn video_asset_dur(project: &mut TimelineProject, duration: Tick) -> AssetId {
        let mut asset = MediaAsset::from_file(AssetKind::Video, "/tmp/handle.mp4");
        asset.probe = Some(MediaProbe {
            duration,
            video: None,
            audio: None,
            container: "mp4".into(),
            codec: "h264".into(),
            is_vfr: false,
            pixel_format: None,
            has_alpha: false,
        });
        let id = asset.id;
        project.media.insert(asset);
        id
    }

    /// A video asset whose probe reports `rate` as its video-stream frame rate —
    /// drives the §3.5 conform Info.
    fn video_asset_rate(project: &mut TimelineProject, rate: FrameRate) -> AssetId {
        let mut asset = MediaAsset::from_file(AssetKind::Video, "/tmp/rate.mp4");
        asset.probe = Some(MediaProbe {
            duration: Tick::from_seconds(10),
            video: Some(VideoStreamInfo {
                width: 1920,
                height: 1080,
                frame_rate: rate,
                pixel_aspect: 1.0,
                color: ProbedColor::default(),
                keyframe_index_cached: false,
                scan: Default::default(),
            }),
            audio: None,
            container: "mp4".into(),
            codec: "h264".into(),
            is_vfr: false,
            pixel_format: None,
            has_alpha: false,
        });
        let id = asset.id;
        project.media.insert(asset);
        id
    }

    // ---- Task 1: handle computation + duration clamp (38 §1.1/§1.2) ----

    /// A requested overlap longer than the outgoing clip's available source handle
    /// is clamped to the handle: the transition window genuinely shortens (mix live
    /// inside the clamped window, inert past it).
    #[test]
    fn transition_clamps_to_available_handle() {
        let (mut project, seq_id) = base_project();
        // Outgoing asset: 30-frame clip with 10 frames of handle past its out point.
        let out_asset = video_asset_dur(&mut project, f30(40));
        let in_asset = {
            let a = MediaAsset::from_file(AssetKind::Video, "/tmp/in.mp4");
            let id = a.id;
            project.media.insert(a);
            id
        };
        let tk = add_video_track(&mut project, seq_id);
        let a = Clip::new(ClipSource::Asset { asset: out_asset }, f30(0), f30(30));
        let mut b = Clip::new(ClipSource::Asset { asset: in_asset }, f30(30), f30(60));
        b.transition_in = Some(Transition::new(TransitionKind::CrossDissolve, f30(40)));
        let clips = &mut project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk].clips;
        clips.push(a);
        clips.push(b);

        // tick = B.start + 5f: inside the CLAMPED 10-frame window ⇒ a live mix.
        let inside = compile(&project, seq_id, 0, f30(35), Quality::FULL, None);
        assert!(
            has_fractional_merge(&inside.graph),
            "transition mixes inside the clamped window"
        );
        // The shortening is recorded as an Info (not a suppression Warning).
        assert_eq!(count_code(&inside, CompileCode::TransitionHandleClipped), 1);
        let clipped = inside
            .diagnostics
            .iter()
            .find(|d| d.code == Some(CompileCode::TransitionHandleClipped))
            .unwrap();
        assert_eq!(clipped.severity, DiagSeverity::Info);

        // tick = B.start + 20f: past the clamped window (would be inside the
        // authored 40f window) ⇒ plain covering-clip render, no mix.
        let outside = compile(&project, seq_id, 0, f30(50), Quality::FULL, None);
        assert!(
            !has_fractional_merge(&outside.graph),
            "no mix past the clamped window"
        );
    }

    /// A zero-length handle (probe ends exactly at the out point) suppresses the
    /// transition entirely: no mix, and a `TransitionHandleClipped` Warning.
    #[test]
    fn transition_with_zero_handle_does_not_render() {
        let (mut project, seq_id) = base_project();
        // probe.duration == out point ⇒ zero handle.
        let out_asset = video_asset_dur(&mut project, f30(30));
        let in_asset = {
            let a = MediaAsset::from_file(AssetKind::Video, "/tmp/in.mp4");
            let id = a.id;
            project.media.insert(a);
            id
        };
        let tk = add_video_track(&mut project, seq_id);
        let a = Clip::new(ClipSource::Asset { asset: out_asset }, f30(0), f30(30));
        let mut b = Clip::new(ClipSource::Asset { asset: in_asset }, f30(30), f30(60));
        b.transition_in = Some(Transition::new(TransitionKind::CrossDissolve, f30(40)));
        let clips = &mut project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk].clips;
        clips.push(a);
        clips.push(b);

        let out = compile(&project, seq_id, 0, f30(35), Quality::FULL, None);
        assert!(
            !has_fractional_merge(&out.graph),
            "zero handle ⇒ no transition mix"
        );
        assert_eq!(count_code(&out, CompileCode::TransitionHandleClipped), 1);
        let d = out
            .diagnostics
            .iter()
            .find(|d| d.code == Some(CompileCode::TransitionHandleClipped))
            .unwrap();
        assert_eq!(
            d.severity,
            DiagSeverity::Warning,
            "suppression is a Warning"
        );
    }

    /// An outgoing asset with no probe is of unknown length — never clamped, so the
    /// full authored window renders.
    #[test]
    fn transition_with_no_probe_is_not_clamped() {
        let (mut project, seq_id) = base_project();
        let out_asset = {
            let a = MediaAsset::from_file(AssetKind::Video, "/tmp/noprobe.mp4");
            let id = a.id;
            project.media.insert(a);
            id
        };
        let in_asset = {
            let a = MediaAsset::from_file(AssetKind::Video, "/tmp/in.mp4");
            let id = a.id;
            project.media.insert(a);
            id
        };
        let tk = add_video_track(&mut project, seq_id);
        let a = Clip::new(ClipSource::Asset { asset: out_asset }, f30(0), f30(30));
        let mut b = Clip::new(ClipSource::Asset { asset: in_asset }, f30(30), f30(60));
        b.transition_in = Some(Transition::new(TransitionKind::CrossDissolve, f30(40)));
        let clips = &mut project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk].clips;
        clips.push(a);
        clips.push(b);

        // tick = B.start + 20f: inside the FULL 40f window (no clamp) ⇒ a live mix.
        let out = compile(&project, seq_id, 0, f30(50), Quality::FULL, None);
        assert!(
            has_fractional_merge(&out.graph),
            "no probe ⇒ full window mixes"
        );
        assert_eq!(
            count_code(&out, CompileCode::TransitionHandleClipped),
            0,
            "unknown length is never clamped"
        );
    }

    /// The source→timeline handle conversion honours clip speed: 2× playback halves
    /// the timeline-domain handle for the same source material.
    #[test]
    fn available_handle_respects_constant_speed() {
        let mut project = TimelineProject::new();
        // 1s clip with 20 source-frames of material past its out point.
        let asset_1x = video_asset_dur(&mut project, f30(30) + f30(20));
        let clip_1x = Clip::new(ClipSource::Asset { asset: asset_1x }, f30(0), f30(30));
        assert_eq!(
            available_handle_ticks(&project, &clip_1x),
            Some(f30(20)),
            "1× speed: 20 source-frames = 20 timeline-frames"
        );

        // Same 20 source-frames of handle, but at 2× (out point consumes 2s of src).
        let asset_2x = video_asset_dur(&mut project, Tick::from_seconds(2) + f30(20));
        let mut clip_2x = Clip::new(ClipSource::Asset { asset: asset_2x }, f30(0), f30(30));
        clip_2x.speed = SpeedMap::Constant(Ratio::new(2, 1));
        assert_eq!(
            available_handle_ticks(&project, &clip_2x),
            Some(f30(10)),
            "2× speed halves the timeline-domain handle"
        );
    }

    // ---- Task 2: typed diagnostic channel defaults (38 §1.2 shared type) ----

    #[test]
    fn compile_diagnostic_defaults_are_info_and_uncoded() {
        let d = CompileDiagnostic::plain("x");
        assert_eq!(d.severity, DiagSeverity::Info);
        assert!(d.code.is_none());
        assert!(d.clip.is_none());
        // `at` keeps the same defaults for code/severity/clip.
        let d2 = CompileDiagnostic::at(GraphId::new(), GraphNodeId::new(), "y");
        assert_eq!(d2.severity, DiagSeverity::Info);
        assert!(d2.code.is_none());
        assert!(d2.clip.is_none());
    }

    // ---- Task 3: one transition per cut (38 §1.3), compiler side ----

    /// A `transition_out` at the sequence end (no following clip) is a fade-out:
    /// the clip is merged toward transparent over the window (a fractional merge),
    /// and is inert outside it.
    #[test]
    fn transition_out_at_sequence_end_fades_to_transparent() {
        let (mut project, seq_id) = base_project();
        let tk = add_video_track(&mut project, seq_id);
        let mut a = solid_clip(
            Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
            0,
            f30(100).0,
        );
        a.transition_out = Some(Transition::new(TransitionKind::CrossDissolve, f30(20)));
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(a);

        // Inside the fade window [end-20f, end) ⇒ merged toward transparent.
        let fading = compile(&project, seq_id, 0, f30(90), Quality::FULL, None);
        assert!(
            has_fractional_merge(&fading.graph),
            "fade-out merges toward transparent"
        );
        // Before the window ⇒ fully opaque, no fade merge.
        let solid = compile(&project, seq_id, 0, f30(50), Quality::FULL, None);
        assert!(
            !has_fractional_merge(&solid.graph),
            "no fade before the window"
        );
    }

    /// Both a `transition_out` on the outgoing clip AND a `transition_in` on the
    /// incoming clip set at the same cut (bypassing validation): only the incoming
    /// clip's `transition_in` window produces a mix — the `transition_out` is inert
    /// at a cut (38 §1.3), never a second transition.
    #[test]
    fn no_double_transition_at_a_cut() {
        let (mut project, seq_id) = base_project();
        let tk = add_video_track(&mut project, seq_id);
        let mut a = solid_clip(
            Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            0,
            f30(100).0,
        );
        // Illegal per Sequence::validate, but set directly here to prove the
        // compiler ignores a transition_out at a cut.
        a.transition_out = Some(Transition::new(TransitionKind::CrossDissolve, f30(20)));
        let mut b = solid_clip(
            Color {
                r: 0.0,
                g: 0.0,
                b: 1.0,
                a: 1.0,
            },
            f30(100).0,
            f30(100).0,
        );
        b.transition_in = Some(Transition::new(TransitionKind::CrossDissolve, f30(20)));
        let clips = &mut project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk].clips;
        clips.push(a);
        clips.push(b);

        // In A's transition_out window [80f, 100f) — before the cut, covering A.
        // transition_out at a cut is inert ⇒ no mix.
        let before_cut = compile(&project, seq_id, 0, f30(90), Quality::FULL, None);
        assert!(
            !has_fractional_merge(&before_cut.graph),
            "transition_out at a cut is inert"
        );
        // In B's transition_in window [100f, 120f) — after the cut, covering B.
        // The incoming clip's transition_in is the only mix path ⇒ a mix.
        let after_cut = compile(&project, seq_id, 0, f30(110), Quality::FULL, None);
        assert!(
            has_fractional_merge(&after_cut.graph),
            "transition_in owns the cut"
        );
    }

    // ---- Task 4: nest renders in the OUTER format (38 §2.3) ----

    /// A nest renders in the outer format, not the inner sequence's own format:
    /// the `Output` is the outer dimensions, and an inner clip's per-format reframe
    /// keyed by the OUTER format index is the transform that applies.
    #[test]
    fn nest_uses_outer_format_not_inner() {
        let mut project = TimelineProject::new();

        // Inner sequence is portrait 1080×1920 with a full-frame solid whose
        // reframe entry for OUTER format index 0 is a non-identity transform.
        let mut inner = Sequence::new("inner", FrameRate::FPS_30, 1080, 1920);
        let inner_id = inner.id;
        let mut it = Track::new(TrackKind::Video, "V1");
        let mut inner_clip = solid_clip(
            Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
            0,
            Tick::from_seconds(2).0,
        );
        let reframe = ClipTransform {
            x: 10.0,
            rotation: 0.3,
            ..ClipTransform::default()
        };
        inner_clip.reframe.insert(0, reframe);
        it.clips.push(inner_clip);
        inner.video_tracks.push(it);
        project.insert_sequence(inner);

        // Outer sequence is landscape 1920×1080.
        let mut outer = Sequence::new("outer", FrameRate::FPS_30, 1920, 1080);
        let outer_id = outer.id;
        let mut ot = Track::new(TrackKind::Video, "V1");
        ot.clips.push(Clip::new(
            ClipSource::NestedSequence { sequence: inner_id },
            Tick(0),
            Tick::from_seconds(2),
        ));
        outer.video_tracks.push(ot);
        project.insert_sequence(outer);

        let out = compile(&project, outer_id, 0, Tick(0), Quality::FULL, None);
        // Output is the OUTER format's dimensions.
        assert!(
            out.graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::Output { w: 1920, h: 1080 })),
            "nest outputs the outer format"
        );

        let outer_format = SequenceFormat::new("16:9", 1920, 1080);
        let inner_format = SequenceFormat::new("16:9", 1080, 1920);
        let want = clip_transform_matrix(&reframe, &outer_format);
        let unwanted = clip_transform_matrix(&reframe, &inner_format);
        assert_ne!(
            want, unwanted,
            "the two formats must disagree for a real test"
        );
        // The inner clip's reframe transform was resolved against the OUTER format.
        assert!(
            out.graph
                .nodes
                .iter()
                .any(|n| matches!(&n.op, IrOp::Transform2D { mat, .. } if *mat == want)),
            "inner reframe resolves against the outer format"
        );
    }

    // ---- Task 5: nested caption tracks render inside the nest (38 §2.3) ----

    fn inner_with_caption(name: &str) -> Sequence {
        let mut inner = Sequence::new(name, FrameRate::FPS_30, 4, 4);
        let mut v = Track::new(TrackKind::Video, "V1");
        v.clips
            .push(solid_clip(Color::BLACK, 0, Tick::from_seconds(2).0));
        inner.video_tracks.push(v);
        inner.caption_tracks.push(wordpop_track()); // cue [0, 200)
        inner
    }

    /// An inner sequence's enabled caption track renders inside the nest — the
    /// compiled graph carries a `CaptionOverlay` even though the outer sequence has
    /// no caption tracks.
    #[test]
    fn nested_sequence_captions_render() {
        let mut project = TimelineProject::new();
        let inner = inner_with_caption("inner");
        let inner_id = inner.id;
        project.insert_sequence(inner);

        let mut outer = Sequence::new("outer", FrameRate::FPS_30, 4, 4);
        let outer_id = outer.id;
        let mut ot = Track::new(TrackKind::Video, "V1");
        ot.clips.push(Clip::new(
            ClipSource::NestedSequence { sequence: inner_id },
            Tick(0),
            Tick::from_seconds(2),
        ));
        outer.video_tracks.push(ot);
        project.insert_sequence(outer);

        // src_time = 50 is inside the inner cue [0, 200).
        let out = compile(&project, outer_id, 0, Tick(50), Quality::FULL, None);
        assert!(
            out.graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::CaptionOverlay { .. })),
            "inner caption track overlays inside the nest"
        );
    }

    /// Nested captions resolve at the INNER timebase (`src_time`), not the outer
    /// tick: a cue covering the mapped `src_time` but not the outer tick still
    /// renders.
    #[test]
    fn nested_sequence_caption_uses_inner_timebase() {
        let mut project = TimelineProject::new();
        let inner = inner_with_caption("inner");
        let inner_id = inner.id;
        project.insert_sequence(inner);

        let mut outer = Sequence::new("outer", FrameRate::FPS_30, 4, 4);
        let outer_id = outer.id;
        let mut ot = Track::new(TrackKind::Video, "V1");
        // Nest starts at 5s, so at outer tick 5s+50 the mapped src_time is 50
        // (inside the cue) while the outer tick (~5s) is far past it.
        let start = Tick::from_seconds(5);
        ot.clips.push(Clip::new(
            ClipSource::NestedSequence { sequence: inner_id },
            start,
            Tick::from_seconds(5),
        ));
        outer.video_tracks.push(ot);
        project.insert_sequence(outer);

        let out = compile(&project, outer_id, 0, start + Tick(50), Quality::FULL, None);
        assert!(
            out.graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::CaptionOverlay { .. })),
            "caption resolves at the inner timebase, not the outer tick"
        );
    }

    // ---- Task 6: one Info per nest on inner/outer rate mismatch (38 §2.2) ----

    fn nest_project(host_rate: FrameRate, inner_rate: FrameRate) -> (TimelineProject, SequenceId) {
        let mut project = TimelineProject::new();
        let mut inner = Sequence::new("inner", inner_rate, 4, 4);
        let inner_id = inner.id;
        let mut v = Track::new(TrackKind::Video, "V1");
        v.clips.push(solid_clip(
            Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            0,
            Tick::from_seconds(5).0,
        ));
        inner.video_tracks.push(v);
        project.insert_sequence(inner);

        let mut outer = Sequence::new("outer", host_rate, 4, 4);
        let outer_id = outer.id;
        let mut ot = Track::new(TrackKind::Video, "V1");
        ot.clips.push(Clip::new(
            ClipSource::NestedSequence { sequence: inner_id },
            Tick(0),
            Tick::from_seconds(2),
        ));
        outer.video_tracks.push(ot);
        project.insert_sequence(outer);
        (project, outer_id)
    }

    #[test]
    fn nest_at_different_rate_emits_one_info() {
        let (project, outer_id) = nest_project(FrameRate::FPS_24, FrameRate::FPS_30);
        let out = compile(&project, outer_id, 0, Tick(0), Quality::FULL, None);
        let coded: Vec<_> = out
            .diagnostics
            .iter()
            .filter(|d| d.code == Some(CompileCode::FrameRateConformed))
            .collect();
        assert_eq!(coded.len(), 1, "exactly one rate-mismatch Info");
        assert_eq!(coded[0].severity, DiagSeverity::Info);
        assert!(coded[0].clip.is_some(), "the nest clip is the subject");

        // §2.2: sampling only — the rendered graph is unchanged vs a matching rate.
        let (matched, matched_id) = nest_project(FrameRate::FPS_24, FrameRate::FPS_24);
        let mout = compile(&matched, matched_id, 0, Tick(0), Quality::FULL, None);
        assert_eq!(
            out.graph.nodes.len(),
            mout.graph.nodes.len(),
            "rate mismatch changes diagnostics, not the render"
        );
    }

    #[test]
    fn nest_at_matching_rate_emits_nothing() {
        let (project, outer_id) = nest_project(FrameRate::FPS_30, FrameRate::FPS_30);
        let out = compile(&project, outer_id, 0, Tick(0), Quality::FULL, None);
        assert_eq!(count_code(&out, CompileCode::FrameRateConformed), 0);
    }

    #[test]
    fn two_nests_at_different_rates_emit_two() {
        // Two nest clips (distinct ids) referencing FPS_30 inners in a FPS_24 host.
        let mut project = TimelineProject::new();
        let mk_inner = |project: &mut TimelineProject| -> SequenceId {
            let mut inner = Sequence::new("inner", FrameRate::FPS_30, 4, 4);
            let id = inner.id;
            let mut v = Track::new(TrackKind::Video, "V1");
            v.clips.push(solid_clip(
                Color {
                    r: 0.0,
                    g: 1.0,
                    b: 0.0,
                    a: 1.0,
                },
                0,
                Tick::from_seconds(5).0,
            ));
            inner.video_tracks.push(v);
            project.insert_sequence(inner);
            id
        };
        let inner_a = mk_inner(&mut project);
        let inner_b = mk_inner(&mut project);

        let mut outer = Sequence::new("outer", FrameRate::FPS_24, 4, 4);
        let outer_id = outer.id;
        for inner in [inner_a, inner_b] {
            let mut t = Track::new(TrackKind::Video, "V");
            t.clips.push(Clip::new(
                ClipSource::NestedSequence { sequence: inner },
                Tick(0),
                Tick::from_seconds(2),
            ));
            outer.video_tracks.push(t);
        }
        project.insert_sequence(outer);

        let out = compile(&project, outer_id, 0, Tick(0), Quality::FULL, None);
        assert_eq!(
            count_code(&out, CompileCode::FrameRateConformed),
            2,
            "distinct nest clips each get their own Info"
        );
    }

    // ---- Task 7: shortened inner sequence holds the last frame + Warning (38 §2.4) ----

    /// Build outer+inner where the inner runs red [0,1s), green [1s,2s) and the
    /// nest clip references far past the inner's 2s content.
    fn shortened_nest() -> (TimelineProject, SequenceId, ClipId) {
        let mut project = TimelineProject::new();
        let mut inner = Sequence::new("inner", FrameRate::FPS_30, 4, 4);
        let inner_id = inner.id;
        let mut v = Track::new(TrackKind::Video, "V1");
        v.clips.push(solid_clip(
            Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            0,
            Tick::from_seconds(1).0,
        ));
        v.clips.push(solid_clip(
            Color {
                r: 0.0,
                g: 1.0,
                b: 0.0,
                a: 1.0,
            },
            Tick::from_seconds(1).0,
            Tick::from_seconds(1).0,
        ));
        inner.video_tracks.push(v);
        project.insert_sequence(inner);

        let mut outer = Sequence::new("outer", FrameRate::FPS_30, 4, 4);
        let outer_id = outer.id;
        let mut ot = Track::new(TrackKind::Video, "V1");
        let nest = Clip::new(
            ClipSource::NestedSequence { sequence: inner_id },
            Tick(0),
            Tick::from_seconds(10),
        );
        let nest_id = nest.id;
        ot.clips.push(nest);
        outer.video_tracks.push(ot);
        project.insert_sequence(outer);
        (project, outer_id, nest_id)
    }

    fn render4(graph: &FrameGraph) -> [f32; 4] {
        let img = crate::graph::eval_cpu::evaluate(
            graph,
            (4, 4),
            &mut crate::graph::eval_cpu::EmptyProvider,
        );
        img.pixels[0]
    }

    #[test]
    fn nest_past_inner_end_holds_last_frame() {
        let (project, outer_id, _nest) = shortened_nest();
        // Two ticks well past the inner's 2s content: both hold the same last frame.
        let a = compile(
            &project,
            outer_id,
            0,
            Tick::from_seconds(3),
            Quality::FULL,
            None,
        );
        let b = compile(
            &project,
            outer_id,
            0,
            Tick::from_seconds(4),
            Quality::FULL,
            None,
        );
        assert_eq!(
            output_hash(&a.graph),
            output_hash(&b.graph),
            "the held frame is content-hash-stable across the tail"
        );
        // The held frame is the inner's LAST rendered frame — green, not red or
        // transparent (which is what a raw past-the-end lookup would give).
        let held = render4(&a.graph);
        assert!(
            held[1] > 0.9 && held[0] < 0.1,
            "held last frame is green: {held:?}"
        );

        // A tick inside the inner content renders the earlier (red) frame — proving
        // the tail hold is not simply the whole nest reading one colour.
        let mid = compile(
            &project,
            outer_id,
            0,
            Tick(Tick::from_seconds(1).0 / 2),
            Quality::FULL,
            None,
        );
        let midpx = render4(&mid.graph);
        assert!(
            midpx[0] > 0.9 && midpx[1] < 0.1,
            "mid content is red: {midpx:?}"
        );
    }

    #[test]
    fn nest_past_inner_end_warns_once() {
        let (project, outer_id, nest_id) = shortened_nest();
        let out = compile(
            &project,
            outer_id,
            0,
            Tick::from_seconds(3),
            Quality::FULL,
            None,
        );
        let coded: Vec<_> = out
            .diagnostics
            .iter()
            .filter(|d| d.code == Some(CompileCode::NestedSequenceShortened))
            .collect();
        assert_eq!(coded.len(), 1, "exactly one shortened Warning");
        assert_eq!(coded[0].severity, DiagSeverity::Warning);
        assert_eq!(coded[0].clip, Some(nest_id), "warning names the nest clip");
    }

    #[test]
    fn nest_shortening_does_not_change_layout() {
        let (project, outer_id, nest_id) = shortened_nest();
        let before = project.sequences.get(&outer_id).unwrap().video_tracks[0]
            .clips
            .iter()
            .find(|c| c.id == nest_id)
            .map(|c| (c.start, c.duration))
            .unwrap();
        let _ = compile(
            &project,
            outer_id,
            0,
            Tick::from_seconds(3),
            Quality::FULL,
            None,
        );
        let after = project.sequences.get(&outer_id).unwrap().video_tracks[0]
            .clips
            .iter()
            .find(|c| c.id == nest_id)
            .map(|c| (c.start, c.duration))
            .unwrap();
        assert_eq!(before, after, "compile never mutates clip layout");
    }

    // ---- Task 8: Media::FrameRateConformed per mismatched-rate clip (38 §3.5) ----

    fn asset_clip_seq(seq_rate: FrameRate, asset: AssetId) -> (TimelineProject, SequenceId) {
        // The asset lives outside; caller inserts. Here we just wire the clip.
        let mut project = TimelineProject::new();
        let seq = Sequence::new("seq", seq_rate, 320, 180);
        let id = seq.id;
        project.insert_sequence(seq);
        let _ = asset;
        (project, id)
    }

    /// A 30fps source on a 24fps sequence emits exactly one conform Info naming the
    /// clip.
    #[test]
    fn conform_info_emitted_once_for_mismatched_source_rate() {
        let (mut project, seq_id) = asset_clip_seq(FrameRate::FPS_24, AssetId::new());
        let aid = video_asset_rate(&mut project, FrameRate::FPS_30);
        let tk = add_video_track(&mut project, seq_id);
        let clip = Clip::new(
            ClipSource::Asset { asset: aid },
            Tick(0),
            Tick::from_seconds(4),
        );
        let cid = clip.id;
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(clip);
        let out = compile(
            &project,
            seq_id,
            0,
            Tick::from_seconds(1),
            Quality::FULL,
            None,
        );
        let coded: Vec<_> = out
            .diagnostics
            .iter()
            .filter(|d| d.code == Some(CompileCode::FrameRateConformed))
            .collect();
        assert_eq!(coded.len(), 1);
        assert_eq!(coded[0].severity, DiagSeverity::Info);
        assert_eq!(coded[0].clip, Some(cid));
    }

    #[test]
    fn no_conform_info_for_matching_rate() {
        let (mut project, seq_id) = asset_clip_seq(FrameRate::FPS_24, AssetId::new());
        let aid = video_asset_rate(&mut project, FrameRate::FPS_24);
        let tk = add_video_track(&mut project, seq_id);
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(Clip::new(
                ClipSource::Asset { asset: aid },
                Tick(0),
                Tick::from_seconds(4),
            ));
        let out = compile(
            &project,
            seq_id,
            0,
            Tick::from_seconds(1),
            Quality::FULL,
            None,
        );
        assert_eq!(count_code(&out, CompileCode::FrameRateConformed), 0);
    }

    #[test]
    fn no_conform_info_for_equivalent_rational_rate() {
        // 60/2 is 30/1 as a rational — not a mismatch on a 30fps sequence.
        let (mut project, seq_id) = asset_clip_seq(FrameRate::FPS_30, AssetId::new());
        let aid = video_asset_rate(&mut project, FrameRate::new(60, 2));
        let tk = add_video_track(&mut project, seq_id);
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(Clip::new(
                ClipSource::Asset { asset: aid },
                Tick(0),
                Tick::from_seconds(4),
            ));
        let out = compile(
            &project,
            seq_id,
            0,
            Tick::from_seconds(1),
            Quality::FULL,
            None,
        );
        assert_eq!(count_code(&out, CompileCode::FrameRateConformed), 0);
    }

    #[test]
    fn no_conform_info_without_probe() {
        let (mut project, seq_id) = asset_clip_seq(FrameRate::FPS_24, AssetId::new());
        // Bare asset: no probe ⇒ unknown rate ⇒ no diagnostic.
        let asset = MediaAsset::from_file(AssetKind::Video, "/tmp/bare.mp4");
        let aid = asset.id;
        project.media.insert(asset);
        let tk = add_video_track(&mut project, seq_id);
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(Clip::new(
                ClipSource::Asset { asset: aid },
                Tick(0),
                Tick::from_seconds(4),
            ));
        let out = compile(
            &project,
            seq_id,
            0,
            Tick::from_seconds(1),
            Quality::FULL,
            None,
        );
        assert_eq!(count_code(&out, CompileCode::FrameRateConformed), 0);
    }

    /// Acceptance 10: conform is identical in preview and export — the
    /// `DecodeVideo` `src_time` is the same, only `proxy` differs.
    #[test]
    fn conform_src_time_identical_preview_vs_full() {
        let (mut project, seq_id) = asset_clip_seq(FrameRate::FPS_24, AssetId::new());
        let aid = video_asset_rate(&mut project, FrameRate::FPS_30);
        let tk = add_video_track(&mut project, seq_id);
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
            .clips
            .push(Clip::new(
                ClipSource::Asset { asset: aid },
                Tick(0),
                Tick::from_seconds(4),
            ));
        let decode = |q: Quality| -> (Tick, bool) {
            compile(&project, seq_id, 0, Tick::from_seconds(1), q, None)
                .graph
                .nodes
                .iter()
                .find_map(|n| match &n.op {
                    IrOp::DecodeVideo {
                        src_time, proxy, ..
                    } => Some((*src_time, *proxy)),
                    _ => None,
                })
                .expect("decode node present")
        };
        let (t_prev, p_prev) = decode(Quality::PREVIEW);
        let (t_full, p_full) = decode(Quality::FULL);
        assert_eq!(t_prev, t_full, "same src_time in preview and export");
        assert!(p_prev && !p_full, "they differ only in proxy");
    }

    // ---- Task 9: identical nest subtrees dedup to one node (38 §2.5) ----

    #[test]
    fn ten_identical_nests_share_one_subtree() {
        let mut project = TimelineProject::new();
        let mut inner = Sequence::new("inner", FrameRate::FPS_30, 4, 4);
        let inner_id = inner.id;
        let mut v = Track::new(TrackKind::Video, "V1");
        v.clips.push(solid_clip(
            Color {
                r: 0.3,
                g: 0.6,
                b: 0.9,
                a: 1.0,
            },
            0,
            Tick::from_seconds(5).0,
        ));
        inner.video_tracks.push(v);
        project.insert_sequence(inner);

        // Ten video tracks, each one identical nest clip (same source_in, start).
        let mut outer = Sequence::new("outer", FrameRate::FPS_30, 4, 4);
        let outer_id = outer.id;
        for _ in 0..10 {
            let mut t = Track::new(TrackKind::Video, "V");
            t.clips.push(Clip::new(
                ClipSource::NestedSequence { sequence: inner_id },
                Tick(0),
                Tick::from_seconds(2),
            ));
            outer.video_tracks.push(t);
        }
        project.insert_sequence(outer);

        let out = compile(
            &project,
            outer_id,
            0,
            Tick::from_seconds(1),
            Quality::FULL,
            None,
        );
        // The one inner source evaluates ONCE, not ten times.
        let sources = out
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.op, IrOp::SolidColor { .. } | IrOp::DecodeVideo { .. }))
            .count();
        assert_eq!(
            sources, 1,
            "the shared inner source is one node, got {sources}"
        );
        // Every fold Merge shares the same deduped nest subtree as its top input.
        let tops: Vec<IrNodeId> = out
            .graph
            .nodes
            .iter()
            .filter_map(|n| match &n.op {
                IrOp::Merge { .. } => Some(n.inputs[0].0),
                _ => None,
            })
            .collect();
        assert!(!tops.is_empty(), "the fold produced merges");
        assert!(
            tops.iter().all(|&t| t == tops[0]),
            "all fold merges share one nest subtree top"
        );
    }

    /// Companion negative: nests at different source times do NOT share — proving
    /// the dedup assertion above measures something real.
    #[test]
    fn nests_at_different_source_times_do_not_share() {
        let mut project = TimelineProject::new();
        // Inner is time-varying: red [0,1s), green [1s,2s).
        let mut inner = Sequence::new("inner", FrameRate::FPS_30, 4, 4);
        let inner_id = inner.id;
        let mut v = Track::new(TrackKind::Video, "V1");
        v.clips.push(solid_clip(
            Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            0,
            Tick::from_seconds(1).0,
        ));
        v.clips.push(solid_clip(
            Color {
                r: 0.0,
                g: 1.0,
                b: 0.0,
                a: 1.0,
            },
            Tick::from_seconds(1).0,
            Tick::from_seconds(1).0,
        ));
        inner.video_tracks.push(v);
        project.insert_sequence(inner);

        let mut outer = Sequence::new("outer", FrameRate::FPS_30, 4, 4);
        let outer_id = outer.id;
        // Nest A samples the red region (source_in 0); nest B samples green
        // (source_in 1s) — distinct inner frames.
        for source_in in [Tick(0), Tick::from_seconds(1)] {
            let mut t = Track::new(TrackKind::Video, "V");
            let mut c = Clip::new(
                ClipSource::NestedSequence { sequence: inner_id },
                Tick(0),
                Tick::from_seconds(1),
            );
            c.source_in = source_in;
            t.clips.push(c);
            outer.video_tracks.push(t);
        }
        project.insert_sequence(outer);

        let out = compile(&project, outer_id, 0, Tick(0), Quality::FULL, None);
        let solids = out
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.op, IrOp::SolidColor { .. }))
            .count();
        assert_eq!(
            solids, 2,
            "different source times ⇒ two distinct inner sources"
        );
    }

    // ── K-0.5: LUT provider threading ────────────────────────────────────────

    #[test]
    fn one_dimensional_shaper_changes_grade_cache_key() {
        use photonic_render::{
            grade::{ResolvedGradeOp, ResolvedGradePayload, ResolvedLut3d},
            Lut3d,
        };
        let digest = |shaper: Option<Vec<[f32; 3]>>| {
            let mut table = Lut3d::identity(2);
            table.shaper = shaper;
            let mut hash = xxhash_rust::xxh3::Xxh3::new();
            hash_resolved_grade_op(
                &mut hash,
                &ResolvedGradeOp {
                    payload: ResolvedGradePayload::Lut3d(ResolvedLut3d {
                        table: std::sync::Arc::new(table),
                        intensity: 1.0,
                        tetrahedral: false,
                    }),
                    mask: None,
                },
            );
            hash.digest()
        };
        assert_ne!(digest(None), digest(Some(vec![[0.0; 3], [1.0; 3]])));
        assert_ne!(
            digest(Some(vec![[0.0; 3], [1.0; 3]])),
            digest(Some(vec![[0.0; 3], [0.5; 3]]))
        );
    }

    /// A stub [`LutProvider`] returning one fixed table for any asset.
    struct StubLut(std::sync::Arc<photonic_render::Lut3d>);
    impl LutProvider for StubLut {
        fn lut(&self, _asset: AssetId) -> Option<std::sync::Arc<photonic_render::Lut3d>> {
            Some(self.0.clone())
        }
    }

    fn lut_grade_project() -> (TimelineProject, SequenceId) {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("seq", FrameRate::FPS_30, 4, 4);
        let seq_id = seq.id;
        let mut t = Track::new(TrackKind::Video, "V1");
        let mut clip = solid_clip(
            Color {
                r: 0.5,
                g: 0.5,
                b: 0.5,
                a: 1.0,
            },
            0,
            200,
        );
        // A single-op `Lut3d` grade referencing an asset the provider resolves.
        clip.grade = Some(single_lut_grade(AssetId::new()));
        t.clips.push(clip);
        seq.video_tracks.push(t);
        project.insert_sequence(seq);
        (project, seq_id)
    }

    /// K-0.5: a `Lut3d` grade resolves to a `Grade` node carrying the provider's
    /// table when a provider is threaded, and drops to identity (no `Grade` node)
    /// with `None` — never a black frame.
    #[test]
    fn lut_grade_resolves_with_provider_and_drops_without() {
        use photonic_render::grade::ResolvedGradePayload;
        let (project, seq_id) = lut_grade_project();

        let table = std::sync::Arc::new(photonic_render::Lut3d::identity(2));
        let stub = StubLut(table);
        let with = compile_with_luts(
            &project,
            seq_id,
            0,
            Tick(0),
            Quality::FULL,
            None,
            Some(&stub),
        );
        let grade = with
            .graph
            .nodes
            .iter()
            .find_map(|n| match &n.op {
                IrOp::Grade { ops } => Some(ops),
                _ => None,
            })
            .expect("a Grade node is present with a provider");
        assert_eq!(grade.len(), 1, "one resolved op");
        match &grade[0].payload {
            ResolvedGradePayload::Lut3d(l) => {
                assert_eq!(l.table.size, 2, "the Grade carries the provider's table")
            }
            other => panic!("expected a resolved Lut3d op, got {other:?}"),
        }

        assert!(with.diagnostics.is_empty());

        // No provider ⇒ the LUT op is inert ⇒ dropped to identity ⇒ no Grade node.
        let without = compile(&project, seq_id, 0, Tick(0), Quality::FULL, None);
        assert!(without
            .diagnostics
            .iter()
            .any(|d| d.code == Some(CompileCode::GradeUnresolved)
                && d.severity == DiagSeverity::Error
                && d.grade.is_some()));
        assert!(
            !without
                .graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::Grade { .. })),
            "the Lut3d op drops to identity with no provider"
        );
    }

    #[test]
    fn unresolved_grade_diagnostics_identify_clip_track_and_master() {
        let (mut project, seq_id) = lut_grade_project();
        let seq = project.sequences.get_mut(&seq_id).unwrap();
        let track_id = seq.video_tracks[0].id;
        let clip_id = seq.video_tracks[0].clips[0].id;
        let same_grade = seq.video_tracks[0].clips[0].grade.clone();
        seq.video_tracks[0].grade = same_grade.clone();
        seq.master_grade = same_grade;

        let compiled = compile(&project, seq_id, 0, Tick(0), Quality::FULL, None);
        let owners: Vec<_> = compiled
            .diagnostics
            .iter()
            .filter_map(|diagnostic| diagnostic.grade.as_ref().and_then(|grade| grade.owner))
            .collect();
        assert!(owners.contains(&VfxOwner::Clip(clip_id)));
        assert!(owners.contains(&VfxOwner::Track(track_id)));
        assert!(owners.contains(&VfxOwner::Master(seq_id)));
        assert_eq!(owners.len(), 3);
    }

    #[test]
    fn nested_grade_diagnostics_include_the_full_sequence_path() {
        let (mut project, inner_id) = lut_grade_project();
        let inner_clip = project.sequences[&inner_id].video_tracks[0].clips[0].id;
        let mut parent = inner_id;
        let mut path = Vec::new();
        for name in ["middle", "outer"] {
            let mut sequence = Sequence::new(name, FrameRate::FPS_30, 4, 4);
            let id = sequence.id;
            let mut track = Track::new(TrackKind::Video, "V1");
            track.clips.push(Clip::new(
                ClipSource::NestedSequence { sequence: parent },
                Tick::ZERO,
                Tick::from_seconds(2),
            ));
            sequence.video_tracks.push(track);
            project.insert_sequence(sequence);
            path.push(id);
            parent = id;
        }
        let compiled = compile(&project, parent, 0, Tick::ZERO, Quality::FULL, None);
        let grade = compiled
            .diagnostics
            .iter()
            .filter_map(|diagnostic| diagnostic.grade.as_ref())
            .find(|grade| grade.owner == Some(VfxOwner::Clip(inner_clip)))
            .expect("nested LUT failure is reported");
        assert_eq!(grade.sequence_path, [path[1], path[0], inner_id]);
        assert!(grade.to_string().contains("Nested sequence path"));
    }

    // ── K-0.4: directional Wipe / Push lowering ──────────────────────────────

    /// K-0.4: a `Wipe` transition lowers to a `WipeMix` node and emits NO
    /// diagnostic (the P3 cross-dissolve-fallback warning is gone).
    #[test]
    fn wipe_transition_lowers_without_diagnostic() {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("seq", FrameRate::FPS_30, 4, 4);
        let seq_id = seq.id;
        let mut t = Track::new(TrackKind::Video, "V1");
        t.clips.push(solid_clip(
            Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            0,
            100,
        ));
        let mut b = solid_clip(
            Color {
                r: 0.0,
                g: 0.0,
                b: 1.0,
                a: 1.0,
            },
            100,
            100,
        );
        b.transition_in = Some(Transition::new(TransitionKind::Wipe, Tick(40)));
        t.clips.push(b);
        seq.video_tracks.push(t);
        project.insert_sequence(seq);

        // Midpoint of the [100,140) overlap.
        let out = compile(&project, seq_id, 0, Tick(120), Quality::FULL, None);
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::WipeMix { .. })),
            "a WipeMix node lowers for a Wipe transition"
        );
    }

    /// K-0.4: a `Push` transition lowers to a `PushMix` node and emits no
    /// diagnostic.
    #[test]
    fn push_transition_lowers_without_diagnostic() {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("seq", FrameRate::FPS_30, 4, 4);
        let seq_id = seq.id;
        let mut t = Track::new(TrackKind::Video, "V1");
        t.clips.push(solid_clip(
            Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            0,
            100,
        ));
        let mut b = solid_clip(
            Color {
                r: 0.0,
                g: 0.0,
                b: 1.0,
                a: 1.0,
            },
            100,
            100,
        );
        b.transition_in = Some(Transition::new(TransitionKind::Push, Tick(40)));
        t.clips.push(b);
        seq.video_tracks.push(t);
        project.insert_sequence(seq);

        let out = compile(&project, seq_id, 0, Tick(120), Quality::FULL, None);
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::PushMix { .. })),
            "a PushMix node lowers for a Push transition"
        );
    }

    /// K-B7: a `LumaWipe` transition lowers to `LumaWipeMix` with no diagnostic.
    #[test]
    fn luma_wipe_transition_lowers_without_diagnostic() {
        use photonic_core::timeline::{LumaWipeMap, TransitionParams};
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("seq", FrameRate::FPS_30, 4, 4);
        let seq_id = seq.id;
        let mut t = Track::new(TrackKind::Video, "V1");
        t.clips.push(solid_clip(
            Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            0,
            100,
        ));
        let mut b = solid_clip(
            Color {
                r: 0.0,
                g: 0.0,
                b: 1.0,
                a: 1.0,
            },
            100,
            100,
        );
        let mut tr = Transition::new(TransitionKind::LumaWipe, Tick(40));
        tr.params = TransitionParams {
            luma_map: LumaWipeMap::Radial,
            softness: 0.05,
            invert: true,
            ..Default::default()
        };
        b.transition_in = Some(tr);
        t.clips.push(b);
        seq.video_tracks.push(t);
        project.insert_sequence(seq);

        let out = compile(&project, seq_id, 0, Tick(120), Quality::FULL, None);
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        let found = out.graph.nodes.iter().find_map(|n| match &n.op {
            IrOp::LumaWipeMix {
                kind,
                softness,
                invert,
                ..
            } => Some((*kind, *softness, *invert)),
            _ => None,
        });
        let (kind, soft, inv) = found.expect("a LumaWipeMix node lowers for LumaWipe");
        assert_eq!(kind, crate::graph::luma_wipe::LumaWipeKind::Radial);
        assert!((soft - 0.05).abs() < 1e-5);
        assert!(inv);
    }
}
