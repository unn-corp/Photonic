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
use photonic_core::Color;
use photonic_render::caption::CaptionWordRun;

use crate::contract::{
    AssetId, CaptionBatch, CaptionCueRun, MatteModel, ResolvedParams, ResolvedTextBlock, Tick,
    VectorRef, VectorStateKey, TICKS_PER_SECOND,
};
use crate::graph::ir::{
    Channel, ContentHash, DeinterlaceMethod, FieldOrder, FitMode, FrameGraph, IrNode, IrNodeId,
    IrOp, LinearColor, OutPort, Sampling, TextureDesc, WipeDirection, WorkingColorDomain,
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
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClipGradeInspection {
    pub clip: ClipId,
    pub ops: Vec<(GradeOpId, photonic_render::grade::ResolvedGradeOp)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NativeQualifierInspection {
    pub clip: ClipId,
    pub op: GradeOpId,
    pub input: IrNodeId,
    pub qualifier: Box<photonic_render::grade::ResolvedHslQualifier>,
    pub mask: Option<photonic_render::grade::ResolvedMask>,
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
    /// qualified Native Managed path has no captions; its program tap is after
    /// the display/output transform and carries that explicit encoding.
    #[default]
    Program,
    /// The named clip's texture after its own `Grade`, before the track fold
    /// (07 §5). Falls back to [`ScopeTapPoint::Program`] when the clip is not in
    /// this frame.
    Clip(ClipId),
    /// The named clip immediately before its own grade. Intended for grade
    /// inspection; falls back to Program when the clip is not in this frame.
    ClipPreGrade(ClipId),
    /// Native qualifier's scene input after earlier correctors, before its key/CDL.
    NativeQualifierInput { clip: ClipId, op: GradeOpId },
    /// Native curve scene input before this corrector.
    NativeCurveInput { clip: ClipId, op: GradeOpId },
    NativeGraphQualifierInput {
        clip: ClipId,
        node: u32,
        op: GradeOpId,
    },
    GradeGraphMatteOutput { clip: ClipId, node: u32 },
    NativeGraphCurveInput {
        clip: ClipId,
        node: u32,
        op: GradeOpId,
    },
}

impl CompiledFrame {
    /// Resolve `point` to an IR node, or `None` when this frame has no such tap
    /// (empty program, or a clip the playhead is not over). Callers own the
    /// fallback policy; [`CompiledFrame::resolve_tap`] applies the 13 §10.2 one.
    pub fn tap(&self, point: ScopeTapPoint) -> Option<IrNodeId> {
        match point {
            ScopeTapPoint::Program => self.program_tap,
            ScopeTapPoint::GradeGraphMatteOutput { clip, node } => self.native_graph_mattes.iter().find(|(c, id, _)| *c == clip && *id == node).map(|(_, _, output)| *output),
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
        &[],
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
    ancestry: &[SequenceId],
) -> CompiledFrame {
    let native_luts = luts.map(|provider| NativeLutView { provider, project });
    let mut b = Builder::new();
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
    if project.project_graph.is_some()
        || !seq.caption_tracks.is_empty()
        || !seq.master_effects.is_empty()
    {
        return blank(b, "native preview does not yet support project graphs, captions, or master effects and grades", None);
    }
    let active: Vec<_> = seq
        .video_tracks
        .iter()
        .filter(|track| track.enabled && track.kind.is_visual())
        .filter_map(|track| {
            covering_clip_index(&track.clips, tick).map(|index| (track, &track.clips[index]))
        })
        .filter(|(_, clip)| clip.enabled)
        .collect();
    if active.is_empty() {
        b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
        let source = b.transparent(format);
        let output = b.push(
            if delivery {
                IrOp::NativeSdrVideoOutput
            } else {
                IrOp::NativeSdrOutput
            },
            vec![(source, OutPort::default())],
        );
        return b.finish(Some(output));
    }
    b.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
    b.sequence_path = sequence_path.clone();
    let mut composite = None;
    for (track, clip) in active {
        if matches!(clip.source, ClipSource::Adjustment) {
            if !track.effects.is_empty()
                || track.grade.is_some()
                || track.blend != BlendMode::Normal
                || track.opacity != 1.0
                || !clip.effects.is_empty()
                || clip.group.is_some()
                || clip.composition.is_some()
                || clip.transition_in.is_some()
                || clip.transition_out.is_some()
                || clip.multicam.is_some()
                || clip.stabilization.is_some()
                || clip.transform != AnimProps::new(ClipTransform::default())
                || !clip.reframe.is_empty()
            {
                return blank(Builder::new(), "native adjustment preview supports only an untransformed grade over the composite", Some(clip.id));
            }
            if let Some(below) = composite {
                let adjusted = clip.grade.as_ref().map_or(below, |grade| {
                    apply_grade(
                        &mut b,
                        grade,
                        below,
                        tick - clip.start,
                        Some(VfxOwner::Clip(clip.id)),
                    )
                });
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
        if !track.effects.is_empty()
            || track.blend != BlendMode::Normal
            || !track.opacity.is_finite()
            || !(0.0..=1.0).contains(&track.opacity)
            || clip.composition.is_some()
            || clip.transition_in.is_some()
            || clip.transition_out.is_some()
            || clip.multicam.is_some()
            || clip.stabilization.is_some()
            || !clip.effects.is_empty()
        {
            return blank(
                Builder::new(),
                "native preview cannot reproduce an authored effect or composite stage",
                Some(clip.id),
            );
        }
        // Do not truncate malformed ancestry and silently omit an authored grade.
        let mut group_chain = Vec::new();
        let mut group = clip.group;
        let mut visited = HashSet::new();
        while let Some(id) = group {
            let Some(node) = seq.groups.get(&id) else {
                return blank(
                    Builder::new(),
                    "native preview has a missing group reference",
                    Some(clip.id),
                );
            };
            if !visited.insert(id) {
                return blank(
                    Builder::new(),
                    "native preview has a cyclic group reference",
                    Some(clip.id),
                );
            }
            if !matches!(
                node.kind,
                timeline::GroupKind::Normal | timeline::GroupKind::AvLink
            ) {
                return blank(
                    Builder::new(),
                    "native preview has an unsupported group kind",
                    Some(clip.id),
                );
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
            return blank(
                Builder::new(),
                "native preview requires a finite, invertible clip transform and opacity within 0..=1",
                Some(clip.id),
            );
        }
        let asset_graded = match clip.source {
            ClipSource::Asset { asset } => {
                if project.media.assets.get(&asset).is_none_or(|media| {
                    !matches!(media.kind, AssetKind::Video | AssetKind::Image)
                        || !media.effects.is_empty()
                }) {
                    return blank(
                Builder::new(),
                "native preview requires a video or sRGB still asset without asset-level effects",
                Some(clip.id),
            );
                }
                if check_sources_online {
                    if let Some(error) = source_errors.and_then(|errors| errors.get(&asset)) {
                        return blank(Builder::new(), error, Some(clip.id));
                    }
                    let Some(timeline::AssetSource::File { path, .. }) =
                        project.media.assets.get(&asset).map(|media| &media.source)
                    else {
                        return blank(
                            Builder::new(),
                            "native preview source is not a file-backed asset",
                            Some(clip.id),
                        );
                    };
                    if !path.is_file() {
                        return blank(
                            Builder::new(),
                            &format!(
                                "native preview source {asset} is offline: {}",
                                path.display()
                            ),
                            Some(clip.id),
                        );
                    }
                    if let Some(diagnostic) = crate::color::inspect_input(project, seq, clip)
                        .and_then(|finding| finding.diagnostic)
                    {
                        return blank(Builder::new(), &diagnostic, Some(clip.id));
                    }
                }
                let source = build_clip_source(
                    &mut b,
                    project,
                    seq,
                    format_index,
                    format,
                    clip,
                    tick,
                    quality,
                    &mut HashSet::new(),
                );
                project
                    .media
                    .assets
                    .get(&asset)
                    .and_then(|media| media.grade.as_ref())
                    .map_or(source, |grade| {
                        apply_grade(
                            &mut b,
                            grade,
                            source,
                            tick - clip.start,
                            Some(VfxOwner::Asset(asset)),
                        )
                    })
            }
            ClipSource::NestedSequence {
                sequence: nested_id,
            } => {
                let Some(nested) = project.sequences.get(&nested_id) else {
                    return blank(b, "native nested sequence is missing", Some(clip.id));
                };
                if nested.color != seq.color
                    || !rates_equal(nested.frame_rate, seq.frame_rate)
                    || nested.formats.get(format_index).is_none_or(|inner| {
                        inner.width != format.width || inner.height != format.height
                    })
                {
                    return blank(b, "native nest requires matching color configuration, frame rate and selected canvas", Some(clip.id));
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
                    &sequence_path,
                );
                if inner
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == DiagSeverity::Error)
                {
                    let mut failed = blank(
                        b,
                        "native nested source cannot render accurately",
                        Some(clip.id),
                    );
                    failed.diagnostics.extend(inner.diagnostics);
                    return failed;
                }
                let Some(output) = inner.graph.output else {
                    return blank(b, "native nested sequence has no output", Some(clip.id));
                };
                let output_node = &inner.graph.nodes[output.0 as usize];
                if !matches!(output_node.op, IrOp::NativeSdrOutput) || output_node.inputs.len() != 1
                {
                    return blank(
                        b,
                        "native nested sequence has an invalid output boundary",
                        Some(clip.id),
                    );
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
                for diagnostic in inner.diagnostics {
                    b.diag(diagnostic);
                }
                imported[scene_output.0 as usize]
            }
            _ => {
                return blank(
                    b,
                    "native preview requires a qualified asset or nested sequence source",
                    Some(clip.id),
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
        let mut pre_graded = transformed;
        for id in group_chain.iter().rev() {
            if let Some(grade) = seq.groups[id].pre_grade.as_ref() {
                pre_graded = apply_grade(
                    &mut b,
                    grade,
                    pre_graded,
                    tick - clip.start,
                    Some(VfxOwner::GroupPre(*id)),
                );
            }
        }
        b.clip_pre_grade_taps.push((clip.id, pre_graded));
        let mut graded = clip.grade.as_ref().map_or(pre_graded, |grade| {
            apply_grade(
                &mut b,
                grade,
                pre_graded,
                tick - clip.start,
                Some(VfxOwner::Clip(clip.id)),
            )
        });
        graded = apply_clip_look(&mut b, project, clip, graded, tick - clip.start);
        for id in group_chain {
            if let Some(grade) = seq.groups[&id].post_grade.as_ref() {
                graded = apply_grade(
                    &mut b,
                    grade,
                    graded,
                    tick - clip.start,
                    Some(VfxOwner::GroupPost(id)),
                );
            }
        }
        let track_graded = track.grade.as_ref().map_or(graded, |grade| {
            apply_grade(&mut b, grade, graded, tick, Some(VfxOwner::Track(track.id)))
        });
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
        b.clip_taps.push((clip.id, graded));
        composite = Some(fold_over(
            &mut b,
            composite,
            track_graded,
            (transform.opacity as f32) * track.opacity,
            BlendMode::Normal,
        ));
    }
    let Some(composite) = composite else {
        let source = b.transparent(format);
        let output = b.push(
            if delivery {
                IrOp::NativeSdrVideoOutput
            } else {
                IrOp::NativeSdrOutput
            },
            vec![(source, OutPort::default())],
        );
        return b.finish(Some(output));
    };
    let program = seq.master_grade.as_ref().map_or(composite, |grade| {
        apply_grade(
            &mut b,
            grade,
            composite,
            tick,
            Some(VfxOwner::Master(seq.id)),
        )
    });
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
    let output_op = if delivery {
        IrOp::NativeSdrVideoOutput
    } else {
        IrOp::NativeSdrOutput
    };
    let output = b.push(output_op, vec![(program, OutPort::default())]);
    // Native program scopes measure the display/delivery signal. Clip taps
    // remain scene-linear and are gated until a scene-referred scale exists.
    b.program_tap = Some(output);
    let compiled = b.finish(Some(output));
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
                cur = apply_grade(b, grade, cur, dt, Some(VfxOwner::GroupPre(*id)));
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
                cur = apply_grade(b, grade, cur, dt, Some(VfxOwner::GroupPost(id)));
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
    for fx in effects {
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
            cur = apply_grade(b, grade, cur, dt, Some(VfxOwner::Clip(clip.id)));
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
                        photonic_render::grade::resolve_with_ids_and_diagnostics(
                            &single,
                            tick,
                            |asset| b.luts.and_then(|provider| provider.lut(asset)),
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
                                    if let Some(VfxOwner::Clip(clip)) = owner {
                                        b.native_graph_qualifier_inspections.push((
                                            node,
                                            NativeQualifierInspection {
                                                clip,
                                                op: *op,
                                                input: upstream,
                                                qualifier: qualifier.clone(),
                                                mask: resolved.mask,
                                            },
                                        ));
                                    }
                                }
                                let upstream = if native {
                                    b.push(IrOp::NativeAcescct { direction: photonic_render::native_transfer::AcescctDirection::Encode }, vec![(upstream, OutPort::default())])
                                } else {
                                    upstream
                                };
                                b.push(
                                    IrOp::QualifierMatte {
                                        qualifier,
                                        mask: resolved.mask,
                                        native,
                                    },
                                    vec![(upstream, OutPort::default())],
                                )
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
                && graph.nodes[&node].output_type() == photonic_core::timeline::GradeGraphPortType::Matte
            {
                if let Some(VfxOwner::Clip(clip)) = owner {
                    b.native_graph_mattes.push((clip, node, result));
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
    let (ops, diagnostics) =
        photonic_render::grade::resolve_with_ids_and_diagnostics(grade, tick, |asset| {
            b.luts.and_then(|provider| provider.lut(asset))
        });
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
                        if let Some(VfxOwner::Clip(clip)) = owner {
                            b.native_curve_inputs.push((clip, op_id, current));
                            if let Some(node) = graph_node {
                                b.native_graph_curve_inputs
                                    .push((clip, node, op_id, current));
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
                    if b.sequence_path.len() == 1 {
                        if let Some(VfxOwner::Clip(clip)) = owner {
                            let inspection = NativeQualifierInspection {
                                clip,
                                op: op_id,
                                input: current,
                                qualifier: qualifier.clone(),
                                mask,
                            };
                            if let Some(node) = graph_node {
                                b.native_graph_qualifier_inspections
                                    .push((node, inspection.clone()));
                            }
                            b.native_qualifier_inspections.push(inspection);
                        }
                    }
                    let cdl = qualifier.correction;
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
                            IrOp::NativeLogQualifier {
                                qualifier: qualifier.clone(),
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
        // Glyph keeps its fill; the underline decoration is a render follow-up.
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
    let start = w.start.0 - lead;
    ((tick.0 - start) as f32 / lead as f32).clamp(0.0, 1.0)
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
        IrOp::NativeDecodeStill { asset } => {
            h.update(&[34]);
            h.update(&asset.0.as_u128().to_le_bytes());
        }
        IrOp::DecodeStill { asset } => {
            h.update(&[1]);
            h.update(&asset.0.as_u128().to_le_bytes());
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
        IrOp::MatteExtract { model } => {
            h.update(&[11]);
            h.update(&[*model as u8]);
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
    use photonic_core::timeline::{
        Clip, FrameRate, GraphEdge, GraphNode, GraphOp, InPort, MediaAsset, NodeGraph,
        OutPort as GOutPort, Sequence, Track, TrackKind,
    };
    use photonic_core::Color;

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
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[1].blend = BlendMode::Multiply;
        let unsupported_blend =
            compile_native_preview(&project, seq_id, 0, Tick::ZERO, Quality::PREVIEW);
        assert!(unsupported_blend
            .diagnostics
            .iter()
            .any(|d| d.code == Some(CompileCode::ColorPipelineUnavailable)));
        assert!(!unsupported_blend
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, IrOp::NativeDecodeVideo { .. })));
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
            IrOp::DecodeStill { .. } => "DecodeStill",
            IrOp::RasterVector { .. } => "RasterVector",
            IrOp::SolidColor { .. } => "SolidColor",
            IrOp::Transform2D { .. } => "Transform2D",
            IrOp::Transform2DTransparent { .. } => "Transform2DTransparent",
            IrOp::StabilizeWarp { .. } => "StabilizeWarp",
            IrOp::Effect { .. } => "Effect",
            IrOp::Grade { .. } => "Grade",
            IrOp::NativeExposure { .. } => "NativeExposure",
            IrOp::NativeLinearOffset { .. } => "NativeLinearOffset",
            IrOp::NativePrinterLights { .. } => "NativePrinterLights",
            IrOp::NativeHighlightRolloff { .. } => "NativeHighlightRolloff",
            IrOp::NativeLogContrast { .. } => "NativeLogContrast",
            IrOp::NativeLogCdl { .. } => "NativeLogCdl",
            IrOp::QualifierMatte { .. } => "QualifierMatte",
            IrOp::GradeKeyMix { .. } => "GradeKeyMix",
            IrOp::GradeMatteRefine { .. } => "GradeMatteRefine",
            IrOp::GradeMatteApply => "GradeMatteApply",
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
            IrOp::Merge { .. } => "Merge",
            IrOp::WipeMix { .. } => "WipeMix",
            IrOp::PushMix { .. } => "PushMix",
            IrOp::LumaWipeMix { .. } => "LumaWipeMix",
            IrOp::CaptionOverlay { .. } => "CaptionOverlay",
            IrOp::Crop { .. } => "Crop",
            IrOp::Resize { .. } => "Resize",
            IrOp::MatteExtract { .. } => "MatteExtract",
            IrOp::TextGen { .. } => "TextGen",
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
