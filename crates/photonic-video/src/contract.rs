//! Timeline-contract types referenced by the frame-graph IR (`graph::ir`).
//!
//! P2 relocated the canonical data-model types (`Tick`, `FrameRate`, `AssetId`,
//! `VectorRef`, `VectorStateKey`, `EffectKind`, and `TICKS_PER_SECOND`) into
//! `photonic_core::timeline`; this module now **re-exports** them so existing IR
//! code keeps compiling against `crate::contract::*` unchanged. The engine-side
//! *resolved* types below (keyframe-evaluated IR payloads) are not data model —
//! they stay here and are finalized in their respective phases.

// ── Relocated to photonic_core::timeline (canonical home) ───────────────────
pub use photonic_core::timeline::{
    AssetId, EffectKind, FrameRate, PropPath, PropValue, Tick, VectorRef, VectorStateKey,
    TICKS_PER_SECOND,
};
use photonic_core::Color;

/// Keyframe-resolved effect parameters (02 §2: "the IR carries resolved params;
/// the evaluator is time-ignorant"). Every animatable knob of an `Effect` op is
/// evaluated at compile time (`graph::compile`) into this ordered bag; the
/// evaluator reads it as a static payload.
///
/// Backed by an ordered `Vec` of `(path, value)` pairs — never a `HashMap` —
/// mirroring the authoring [`EffectParams`](photonic_core::timeline::EffectParams)
/// rule (`effect_kind.rs`): a stable order is load-bearing for the content hash
/// (`graph::compile::hash_op`) and hence for `NodeCache` correctness (SS-3). The
/// compiler emits entries in `prop_registry` order, so two resolves of the same
/// effect are byte-identical.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ResolvedParams {
    pub entries: Vec<(PropPath, PropValue)>,
}

impl ResolvedParams {
    /// The resolved value for `path`, if present.
    pub fn get(&self, path: &str) -> Option<&PropValue> {
        self.entries
            .iter()
            .find(|(p, _)| p.as_str() == path)
            .map(|(_, v)| v)
    }

    /// The resolved `f32` at `path` (a `Float` value narrowed to `f32`), or
    /// `default` when the path is absent / not a float.
    pub fn f32_or(&self, path: &str, default: f32) -> f32 {
        match self.get(path) {
            Some(PropValue::Float(v)) => *v as f32,
            _ => default,
        }
    }

    /// The resolved [`Color`] at `path` (authoring sRGB straight-alpha), or
    /// `default` when the path is absent / not a color.
    pub fn color_or(&self, path: &str, default: Color) -> Color {
        match self.get(path) {
            Some(PropValue::Color(c)) => *c,
            _ => default,
        }
    }

    /// The resolved `bool` at `path`, or `default` when absent / not a bool.
    pub fn bool_or(&self, path: &str, default: bool) -> bool {
        match self.get(path) {
            Some(PropValue::Bool(b)) => *b,
            _ => default,
        }
    }
}

/// Keyframe-resolved grade operator (07 §2's `ResolvedGradeOp`, the IR-side
/// sibling of the authoring `GradeOp`). Finalized in P7 as the resolved grade
/// stack payload — re-exported from `photonic-render` (GPU/CPU color math).
pub use photonic_render::grade::ResolvedGradeOp;

/// One caption cue resolved to positioned, styled, karaoke-resolved word runs
/// for the render text pipeline (06 §5.3) — re-exported from `photonic-render`
/// (owns the glyphon color/compositing math), the same pattern as
/// [`ResolvedGradeOp`].
pub use photonic_render::caption::CaptionCueRun;

/// Batch of caption cues covering the compiled tick (06 §4/§5.3), each word's
/// style fully cascade-resolved and its karaoke/animation state baked at compile
/// time so the evaluator stays time-ignorant (02 §2). Empty by default (no cue
/// covers the tick); populated by `graph::compile::splice_captions`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CaptionBatch {
    pub cues: Vec<CaptionCueRun>,
}

/// Matte-extraction model selector (08 §3 `MaskFromMatte`; wraps
/// photonic-matte's U²-Net). Finalized in P8.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MatteModel {
    U2NetP,
}

/// Resolved styled-text block for `TextGen` nodes (08 §3) — the title/text clip
/// (`ClipSource::Text`, G-12) and the node-graph `Text` op both lower to it. The
/// payload is a single fully style-resolved [`CaptionCueRun`], the very same
/// positioned/styled glyph run the `CaptionOverlay` compositor consumes (06 §5.3),
/// so titles render through one text-raster mechanism rather than a parallel
/// path. `None` renders transparent (an empty string, or the node-graph `Text`
/// placeholder until its authoring payload lands). Keyframe evaluation is the
/// compiler's job (02 §2): the cue is baked at the compiled tick.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ResolvedTextBlock {
    pub cue: Option<CaptionCueRun>,
}
