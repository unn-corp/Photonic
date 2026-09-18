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
    EffectParams, FrameRate, Grade, GradeOp, GradeOpKind, GradeOpParams, GraphId, GraphNode,
    GraphNodeId, GraphNodeParams, GraphOp, InPort, KaraokeMode, LutInterp, NodeGraph, PropPath,
    PropSet, PropTargetKind, PropValue, Ratio, ScanType, Sequence, SequenceFormat, SequenceId,
    SpeedMap, TextClipContent, TimeSource, TimelineProject, TransitionKind,
};
use photonic_core::Color;
use photonic_render::caption::CaptionWordRun;

use crate::contract::{
    AssetId, CaptionBatch, CaptionCueRun, MatteModel, ResolvedParams, ResolvedTextBlock, Tick,
    VectorRef, VectorStateKey, TICKS_PER_SECOND,
};
use crate::graph::ir::{
    Channel, ContentHash, DeinterlaceMethod, FieldOrder, FitMode, FrameGraph, IrNode, IrNodeId,
    IrOp, LinearColor, OutPort, Sampling, TextureDesc, WipeDirection,
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
pub trait LutProvider {
    fn lut(&self, asset: AssetId) -> Option<std::sync::Arc<photonic_render::Lut3d>>;
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
    /// measures burnt-in caption pixels the colourist is not grading.
    #[default]
    Program,
    /// The named clip's texture after its own `Grade`, before the track fold
    /// (07 §5). Falls back to [`ScopeTapPoint::Program`] when the clip is not in
    /// this frame.
    Clip(ClipId),
}

impl CompiledFrame {
    /// Resolve `point` to an IR node, or `None` when this frame has no such tap
    /// (empty program, or a clip the playhead is not over). Callers own the
    /// fallback policy; [`CompiledFrame::resolve_tap`] applies the 13 §10.2 one.
    pub fn tap(&self, point: ScopeTapPoint) -> Option<IrNodeId> {
        match point {
            ScopeTapPoint::Program => self.program_tap,
            ScopeTapPoint::Clip(id) => self
                .clip_taps
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
/// [`compile`] (LUT ops inert → identity).
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
    let format_index = format_index.min(seq.formats.len().saturating_sub(1));
    let Some(format) = seq.formats.get(format_index) else {
        b.diag(CompileDiagnostic::plain(format!(
            "compile: sequence {sequence} has no formats"
        )));
        return b.finish(None);
    };

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
    nodes: Vec<IrNode>,
    /// content hash → node id, so an identical (op, inputs) subgraph is emitted
    /// once (TimeOffset dedup, 02 §2 step 7).
    dedup: HashMap<u128, IrNodeId>,
    diagnostics: Vec<CompileDiagnostic>,
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
            nodes: Vec::new(),
            dedup: HashMap::new(),
            diagnostics: Vec::new(),
            view_index: HashMap::new(),
            luts: None,
            deflicker: None,
            stabilization: None,
            clip_taps: Vec::new(),
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

    fn finish(self, output: Option<IrNodeId>) -> CompiledFrame {
        CompiledFrame {
            graph: FrameGraph {
                nodes: self.nodes,
                output,
            },
            diagnostics: self.diagnostics,
            program_tap: self.program_tap,
            clip_taps: self.clip_taps,
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
                    let node =
                        apply_stack(b, &track.effects, track.grade.as_ref(), node, tick, None);
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
        let image = apply_stack(b, &track.effects, track.grade.as_ref(), image, tick, None);
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
        Some(asset) => apply_stack(b, &asset.effects, asset.grade.as_ref(), source, dt, None),
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
            clip.grade.as_ref(),
            cur,
            dt,
            Some(clip.id),
        );
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
/// enabled effect in author order, then the grade on top. This is the SINGLE place
/// the "effects beneath grade" rule lives — it is called at all four effect scopes
/// (asset, clip, track, master, 35 §2.4) so the ordering can never drift between
/// them.
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
        cur = apply_grade(b, grade, cur, dt);
    }
    cur
}

/// Resolve `grade` at `tick` and emit a `Grade` IR op carrying the resolved stack
/// (07 §2/§3), or return `input` unchanged when the grade is bypassed / empty /
/// fully inert. Shared by clip grades (step 2) and graph `Grade`/`Lut` nodes.
fn apply_grade(b: &mut Builder<'_>, grade: &Grade, input: IrNodeId, tick: Tick) -> IrNodeId {
    let ops = resolve_grade(b.luts, grade, tick);
    if ops.is_empty() {
        input
    } else {
        b.push(IrOp::Grade { ops }, vec![(input, OutPort::default())])
    }
}

/// Resolve an authoring [`Grade`] at `tick` into the resolved op stack (07 §2)
/// via `photonic_render::grade::resolve`.
///
/// `Lut3d` asset ops resolve against `luts` (K-0.5): a table is looked up per
/// referenced [`AssetId`]; a `None` result (no provider / offline / failed asset)
/// drops the op to identity (07 §1 — never a black frame). The provider's `lut`
/// is a lock-free read of a pre-warmed cache, so no `.cube` parsing happens on
/// this per-frame path (see [`LutProvider`]).
fn resolve_grade(
    luts: Option<&dyn LutProvider>,
    grade: &Grade,
    tick: Tick,
) -> Vec<crate::contract::ResolvedGradeOp> {
    photonic_render::grade::resolve(grade, tick, |asset: AssetId| {
        luts.and_then(|p| p.lut(asset))
    })
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
                AssetKind::Image => b.push(IrOp::DecodeStill { asset: *asset }, vec![]),
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
            apply_grade(b, grade, input, tick)
        }
        GraphOp::Lut { asset } => {
            // 08 §2: `Lut` lowers to a single-op `Grade{Lut3d}` — one mechanism,
            // not a parallel LUT path. In P3 the LUT asset resolves inert (no pool
            // at compile), so this is a passthrough until the lut_provider reaches
            // compile; the chain shape is correct now.
            let input = lower_primary_or_default(b, lc, primary(), tick, cycle);
            apply_grade(b, &single_lut_grade(*asset), input, tick)
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
            h.update(&[1, m.rectangle as u8, m.invert as u8]);
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
            for opt in [&c.hue_vs_hue, &c.hue_vs_sat] {
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
    use photonic_core::timeline::{
        Clip, FrameRate, GraphEdge, GraphNode, GraphOp, InPort, MediaAsset, NodeGraph,
        OutPort as GOutPort, Sequence, Track, TrackKind,
    };
    use photonic_core::Color;

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
            IrOp::DecodeStill { .. } => "DecodeStill",
            IrOp::RasterVector { .. } => "RasterVector",
            IrOp::SolidColor { .. } => "SolidColor",
            IrOp::Transform2D { .. } => "Transform2D",
            IrOp::StabilizeWarp { .. } => "StabilizeWarp",
            IrOp::Effect { .. } => "Effect",
            IrOp::Grade { .. } => "Grade",
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

        // No provider ⇒ the LUT op is inert ⇒ dropped to identity ⇒ no Grade node.
        let without = compile(&project, seq_id, 0, Tick(0), Quality::FULL, None);
        assert!(
            !without
                .graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::Grade { .. })),
            "the Lut3d op drops to identity with no provider"
        );
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
