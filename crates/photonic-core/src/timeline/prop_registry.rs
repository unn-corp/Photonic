//! Static property registry (01 §6.2).
//!
//! Every animatable target kind publishes a block of `(path, kind, range)`
//! entries so the animation system can validate a [`PropPath`](super::anim::PropPath),
//! seed defaults, and drive generic UI. A path that does not resolve here on
//! load is not an error: the owning [`PropertyTrack`](super::anim::PropertyTrack)
//! is kept and flagged `orphaned` (an asset/plugin may register the path later),
//! never dropped — mirroring the `#[serde(other)]` forward-compat rule for
//! unknown grade-op kinds (07 §1).
//!
//! Path convention (matches 01 §6.2's two literal examples `transform.x` and
//! `params.blur_radius`): the [`ClipTransform`](super::clip::ClipTransform) block
//! uses a `transform.` prefix; every effect/grade/audio-fx/audio param block
//! uses a `params.` prefix. Array components use a `[i]` suffix
//! (`params.slope[0]`).

use super::anim::PropValueKind;
use super::audio::AudioFxKind;
use super::effect_kind::EffectKind;
use super::grade::GradeOpKind;

/// One animatable property: its path, value kind, and optional numeric range
/// (min, max) used for UI clamping and default seeding.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PropEntry {
    pub path: &'static str,
    pub kind: PropValueKind,
    /// Inclusive `(min, max)` for `Float`/`Vec2` props; `None` for unbounded or
    /// non-numeric props.
    pub range: Option<(f64, f64)>,
}

const fn f(path: &'static str, range: Option<(f64, f64)>) -> PropEntry {
    PropEntry {
        path,
        kind: PropValueKind::Float,
        range,
    }
}
const fn col(path: &'static str) -> PropEntry {
    PropEntry {
        path,
        kind: PropValueKind::Color,
        range: None,
    }
}
const fn boolean(path: &'static str) -> PropEntry {
    PropEntry {
        path,
        kind: PropValueKind::Bool,
        range: None,
    }
}

/// The kind of target an [`AnimProps`](super::anim::AnimProps) animates. Used as
/// the registry lookup key ([`PropSet::TARGET_KIND`](super::anim::PropSet::TARGET_KIND)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PropTargetKind {
    ClipTransform,
    Effect(EffectKind),
    GradeOp(GradeOpKind),
    TrackAudioParams,
    ClipAudioParams,
    MasterBusParams,
    AudioFx(AudioFxKind),
    /// Generic graph-node params — validated leniently (any path accepted) since
    /// the node op's param surface is open (08 §6.4).
    GraphNode,
}

// ── Registry blocks ────────────────────────────────────────────────────────

const CLIP_TRANSFORM: &[PropEntry] = &[
    f("transform.x", None),
    f("transform.y", None),
    f("transform.scale_x", Some((0.0, 1000.0))),
    f("transform.scale_y", Some((0.0, 1000.0))),
    f("transform.rotation", None),
    f("transform.anchor_x", None),
    f("transform.anchor_y", None),
    f("transform.opacity", Some((0.0, 1.0))),
];

const EFFECT_BLUR: &[PropEntry] = &[f("params.radius", Some((0.0, 500.0)))];
const EFFECT_SHARPEN: &[PropEntry] = &[
    f("params.amount", Some((0.0, 10.0))),
    f("params.radius", Some((0.0, 100.0))),
];
const EFFECT_GLOW: &[PropEntry] = &[
    f("params.radius", Some((0.0, 500.0))),
    f("params.threshold", Some((0.0, 1.0))),
    f("params.intensity", Some((0.0, 10.0))),
    col("params.tint"),
];
const EFFECT_CHROMAKEY: &[PropEntry] = &[
    col("params.key_color"),
    f("params.tolerance", Some((0.0, 1.0))),
    f("params.edge_softness", Some((0.0, 1.0))),
    f("params.spill_suppress", Some((0.0, 1.0))),
];
const EFFECT_LUMAKEY: &[PropEntry] = &[
    f("params.threshold", Some((0.0, 1.0))),
    f("params.softness", Some((0.0, 1.0))),
    boolean("params.invert"),
];
const EFFECT_INVERT: &[PropEntry] = &[];
/// Ranges mirror `DEFLICKER_PARAMS` in `effect_manifest`; `params.window` is in
/// seconds so a setting means the same thing at any frame rate.
const EFFECT_DEFLICKER: &[PropEntry] = &[
    f("params.amount", Some((0.0, 1.0))),
    f("params.window", Some((0.2, 30.0))),
    f("params.max_change", Some((0.0, 0.9))),
    f("params.chroma_amount", Some((0.0, 1.0))),
];
const EFFECT_MASKSHAPE: &[PropEntry] = &[
    f("params.center_x", Some((0.0, 1.0))),
    f("params.center_y", Some((0.0, 1.0))),
    f("params.size_x", Some((0.0, 2.0))),
    f("params.size_y", Some((0.0, 2.0))),
    f("params.rotation", None),
    f("params.feather", Some((0.0, 1.0))),
];

const GRADE_EXPOSURE: &[PropEntry] = &[f("params.stops", Some((-10.0, 10.0)))];
const GRADE_CONTRAST: &[PropEntry] = &[
    f("params.pivot", Some((0.0, 1.0))),
    f("params.amount", Some((-1.0, 1.0))),
];
const GRADE_WHITEBALANCE: &[PropEntry] = &[
    f("params.temp", Some((-1.0, 1.0))),
    f("params.tint", Some((-1.0, 1.0))),
];
const GRADE_CDL: &[PropEntry] = &[
    f("params.slope[0]", Some((0.0, 4.0))),
    f("params.slope[1]", Some((0.0, 4.0))),
    f("params.slope[2]", Some((0.0, 4.0))),
    f("params.offset[0]", Some((-1.0, 1.0))),
    f("params.offset[1]", Some((-1.0, 1.0))),
    f("params.offset[2]", Some((-1.0, 1.0))),
    f("params.power[0]", Some((0.0, 4.0))),
    f("params.power[1]", Some((0.0, 4.0))),
    f("params.power[2]", Some((0.0, 4.0))),
    f("params.sat", Some((0.0, 4.0))),
];
const GRADE_WHEELS: &[PropEntry] = &[
    f("params.lift[0]", Some((-1.0, 1.0))),
    f("params.lift[1]", Some((-1.0, 1.0))),
    f("params.lift[2]", Some((-1.0, 1.0))),
    f("params.gamma[0]", Some((0.1, 4.0))),
    f("params.gamma[1]", Some((0.1, 4.0))),
    f("params.gamma[2]", Some((0.1, 4.0))),
    f("params.gain[0]", Some((0.0, 4.0))),
    f("params.gain[1]", Some((0.0, 4.0))),
    f("params.gain[2]", Some((0.0, 4.0))),
    f("params.sat", Some((0.0, 4.0))),
];
// Curves/HslQualifier/Lut3d expose structural params (vectors, asset refs) that
// are not per-scalar animatable; their animatable knobs register here.
const GRADE_HSLQUAL: &[PropEntry] = &[
    f("params.softness", Some((0.0, 1.0))),
    f("params.correction.sat", Some((0.0, 4.0))),
];
const GRADE_LUT3D: &[PropEntry] = &[f("params.intensity", Some((0.0, 1.0)))];
const GRADE_CURVES: &[PropEntry] = &[];

const TRACK_AUDIO: &[PropEntry] = &[
    f("params.volume_db", Some((-96.0, 12.0))),
    f("params.pan", Some((-1.0, 1.0))),
];
const CLIP_AUDIO: &[PropEntry] = &[f("params.gain_db", Some((-96.0, 12.0)))];
const MASTER_BUS: &[PropEntry] = &[f("params.volume_db", Some((-96.0, 12.0)))];

const AUDIOFX_EQ: &[PropEntry] = &[
    f("params.low_shelf.freq_hz", Some((20.0, 20000.0))),
    f("params.low_shelf.gain_db", Some((-24.0, 24.0))),
    f("params.band1.freq_hz", Some((20.0, 20000.0))),
    f("params.band1.gain_db", Some((-24.0, 24.0))),
    f("params.band1.q", Some((0.1, 10.0))),
    f("params.band2.freq_hz", Some((20.0, 20000.0))),
    f("params.band2.gain_db", Some((-24.0, 24.0))),
    f("params.band2.q", Some((0.1, 10.0))),
    f("params.band3.freq_hz", Some((20.0, 20000.0))),
    f("params.band3.gain_db", Some((-24.0, 24.0))),
    f("params.band3.q", Some((0.1, 10.0))),
    f("params.high_shelf.freq_hz", Some((20.0, 20000.0))),
    f("params.high_shelf.gain_db", Some((-24.0, 24.0))),
];
const AUDIOFX_COMPRESSOR: &[PropEntry] = &[
    f("params.threshold_db", Some((-60.0, 0.0))),
    f("params.ratio", Some((1.0, 20.0))),
    f("params.attack_ms", Some((0.1, 200.0))),
    f("params.release_ms", Some((1.0, 2000.0))),
    f("params.makeup_db", Some((0.0, 24.0))),
];
const AUDIOFX_GATE: &[PropEntry] = &[
    f("params.threshold_db", Some((-96.0, 0.0))),
    f("params.attack_ms", Some((0.1, 200.0))),
    f("params.hold_ms", Some((0.0, 2000.0))),
    f("params.release_ms", Some((1.0, 2000.0))),
    f("params.range_db", Some((0.0, 96.0))),
];
const AUDIOFX_LIMITER: &[PropEntry] = &[
    f("params.ceiling_db", Some((-24.0, 0.0))),
    f("params.release_ms", Some((1.0, 2000.0))),
];

/// The registry block for a target kind.
pub fn entries(kind: PropTargetKind) -> &'static [PropEntry] {
    match kind {
        PropTargetKind::ClipTransform => CLIP_TRANSFORM,
        PropTargetKind::Effect(e) => match e {
            EffectKind::Blur => EFFECT_BLUR,
            EffectKind::Sharpen => EFFECT_SHARPEN,
            EffectKind::Glow => EFFECT_GLOW,
            EffectKind::ChromaKey => EFFECT_CHROMAKEY,
            EffectKind::LumaKey => EFFECT_LUMAKEY,
            EffectKind::Invert => EFFECT_INVERT,
            EffectKind::MaskShapeGen => EFFECT_MASKSHAPE,
            EffectKind::Deflicker => EFFECT_DEFLICKER,
            // Catalog ids are resolved from their manifest in `resolve`.
            // Truly unknown ids retain orphaned tracks without guessing paths.
            EffectKind::Unknown(_) => &[],
        },
        PropTargetKind::GradeOp(g) => match g {
            GradeOpKind::Exposure => GRADE_EXPOSURE,
            GradeOpKind::Contrast => GRADE_CONTRAST,
            GradeOpKind::WhiteBalance => GRADE_WHITEBALANCE,
            GradeOpKind::Cdl => GRADE_CDL,
            GradeOpKind::Wheels => GRADE_WHEELS,
            GradeOpKind::Curves => GRADE_CURVES,
            GradeOpKind::HslQualifier => GRADE_HSLQUAL,
            GradeOpKind::Lut3d => GRADE_LUT3D,
            // Forward-compat (39 §2.2): see the EffectKind::Unknown note above.
            GradeOpKind::Unknown(_) => &[],
        },
        PropTargetKind::TrackAudioParams => TRACK_AUDIO,
        PropTargetKind::ClipAudioParams => CLIP_AUDIO,
        PropTargetKind::MasterBusParams => MASTER_BUS,
        PropTargetKind::AudioFx(a) => match a {
            AudioFxKind::Eq => AUDIOFX_EQ,
            AudioFxKind::Compressor => AUDIOFX_COMPRESSOR,
            AudioFxKind::Gate => AUDIOFX_GATE,
            AudioFxKind::Limiter => AUDIOFX_LIMITER,
            // Forward-compat (39 §2.2): see the EffectKind::Unknown note above.
            AudioFxKind::Unknown(_) => &[],
        },
        PropTargetKind::GraphNode => &[],
    }
}

/// Resolve a path against a target kind's registry block.
///
/// `GraphNode` accepts any path (open param surface). For every other kind an
/// unknown path returns `None`, which the loader treats as an orphaned track
/// (kept + flagged, never dropped — 01 §6.2).
pub fn resolve(kind: PropTargetKind, path: &str) -> Option<PropEntry> {
    if matches!(kind, PropTargetKind::GraphNode) {
        return Some(PropEntry {
            path: "*",
            kind: PropValueKind::Float,
            range: None,
        });
    }
    entries(kind)
        .iter()
        .copied()
        .find(|e| e.path == path)
        .or_else(|| {
            // Catalog effects use stable ids carried by Unknown; their manifest is
            // authoritative even when the legacy static registry has no block.
            let PropTargetKind::Effect(effect) = kind else {
                return None;
            };
            super::effect_manifest::manifest(effect.effect_id())?
                .params
                .iter()
                .find(|spec| spec.path == path)
                .map(super::effect_manifest::project)
        })
}

/// Whether a path is registered for a target kind (i.e. not orphaned on load).
pub fn is_known(kind: PropTargetKind, path: &str) -> bool {
    resolve(kind, path).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_transform_paths_resolve() {
        assert!(is_known(PropTargetKind::ClipTransform, "transform.x"));
        assert!(is_known(PropTargetKind::ClipTransform, "transform.opacity"));
        assert!(!is_known(PropTargetKind::ClipTransform, "transform.bogus"));
    }

    #[test]
    fn effect_blocks_resolve_per_kind() {
        assert!(is_known(
            PropTargetKind::Effect(EffectKind::Blur),
            "params.radius"
        ));
        // Blur has no `threshold`; Glow does.
        assert!(!is_known(
            PropTargetKind::Effect(EffectKind::Blur),
            "params.threshold"
        ));
        assert!(is_known(
            PropTargetKind::Effect(EffectKind::Glow),
            "params.threshold"
        ));
    }

    #[test]
    fn track_audio_params_registered() {
        assert!(is_known(
            PropTargetKind::TrackAudioParams,
            "params.volume_db"
        ));
        assert!(is_known(PropTargetKind::TrackAudioParams, "params.pan"));
    }

    #[test]
    fn cdl_array_component_paths_resolve() {
        assert!(is_known(
            PropTargetKind::GradeOp(GradeOpKind::Cdl),
            "params.slope[0]"
        ));
        assert!(is_known(
            PropTargetKind::GradeOp(GradeOpKind::Cdl),
            "params.sat"
        ));
    }

    #[test]
    fn graph_node_accepts_any_path() {
        assert!(is_known(PropTargetKind::GraphNode, "anything.at.all"));
    }

    #[test]
    fn unknown_path_is_orphaned_not_resolved() {
        assert!(resolve(PropTargetKind::Effect(EffectKind::Blur), "params.nope").is_none());
    }

    /// Spec §2/§9 step 1: the effect blocks are a *projection* of the effect
    /// manifest table, not a parallel structure. `entries()` still returns the
    /// `&'static` blocks above (30+ callers depend on that), so the projection
    /// relationship is enforced here rather than by restructuring callers: for
    /// every legacy effect kind, `entries(Effect(k))` must equal the manifest's
    /// projected params in path/kind/range and order.
    #[test]
    fn projection_matches_legacy_blocks() {
        use super::super::effect_manifest;
        for kind in [
            EffectKind::Blur,
            EffectKind::Sharpen,
            EffectKind::Glow,
            EffectKind::ChromaKey,
            EffectKind::LumaKey,
            EffectKind::Invert,
            EffectKind::MaskShapeGen,
        ] {
            let legacy = entries(PropTargetKind::Effect(kind));
            let projected = effect_manifest::entries_for_effect(kind);
            assert_eq!(
                legacy,
                projected.as_slice(),
                "manifest projection must reproduce the legacy block for {kind:?}"
            );
        }
    }

    #[test]
    fn unknown_variant_kinds_have_no_registered_paths() {
        // Forward-compat (39 §2.2): an unknown effect/grade/audio kind registers
        // zero paths, so every PropertyTrack targeting it is flagged orphaned
        // (retained, not dropped) rather than resolving to a guessed kind.
        let tag = super::super::unknown::UnknownTag::intern("film_look");
        assert!(entries(PropTargetKind::Effect(EffectKind::Unknown(tag))).is_empty());
        assert!(entries(PropTargetKind::GradeOp(GradeOpKind::Unknown(tag))).is_empty());
        assert!(entries(PropTargetKind::AudioFx(AudioFxKind::Unknown(tag))).is_empty());
        // And such a path never resolves (→ orphaned at load, never guessed).
        assert!(resolve(
            PropTargetKind::Effect(EffectKind::Unknown(tag)),
            "params.radius"
        )
        .is_none());
    }
}
