//! Effect registry discriminant and its parameter bag (01 §6.3, 08 §3).
//!
//! `EffectKind` is the canonical home for the effect-registry enum (relocated
//! here from `photonic-video`'s `contract.rs`, which now re-exports it). It is
//! shared by clip-level [`ClipEffect`](super::clip::ClipEffect) and the
//! filter-family `GraphOp` variants (08 §3). Arity (0-input generator vs. unary
//! filter) is a property of the registry entry, not this enum.

use super::anim::{PropPath, PropSet, PropValue};
use super::prop_registry::{self, PropTargetKind};
use super::unknown::UnknownTag;
use serde::{Deserialize, Serialize};

/// The v1 effect catalog (08 §3, normative; additive-only). `Transform2D`/`Crop`
/// are dedicated IR ops in 02, deliberately NOT effects.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EffectKind {
    Blur,
    Sharpen,
    Glow,
    ChromaKey,
    LumaKey,
    Invert,
    MaskShapeGen,
    /// Exposure-drift stabilization (deflicker). Unlike the other kinds this
    /// one reads a cached whole-range measurement rather than only its own
    /// frame — see [`photonic-video`'s `graph::deflicker`] for why that keeps
    /// the evaluator stateless.
    ///
    /// [`photonic-video`'s `graph::deflicker`]: https://docs.rs/photonic-video
    Deflicker,
    /// Forward-compat (39 §2.2): a variant this build does not know. The
    /// original serialized tag is preserved verbatim and re-emitted on save.
    /// Declared last so serde tries the known snake_case tags first.
    #[serde(untagged)]
    Unknown(UnknownTag),
}

impl EffectKind {
    /// The registry key for this effect's params.
    pub fn target_kind(self) -> PropTargetKind {
        PropTargetKind::Effect(self)
    }

    /// The stable [`EffectId`](super::effect_manifest::EffectId) this kind maps
    /// to (spec §10). Total over the known variants; an unknown variant maps
    /// to an owned id built from its preserved tag (which has no manifest, so it
    /// loads inert).
    pub fn effect_id(self) -> super::effect_manifest::EffectId {
        use super::effect_manifest::EffectId;
        match self {
            EffectKind::Blur => EffectId::new_static("blur.gaussian"),
            EffectKind::Sharpen => EffectId::new_static("sharpen.unsharp"),
            EffectKind::Glow => EffectId::new_static("stylize.glow"),
            EffectKind::ChromaKey => EffectId::new_static("key.chroma"),
            EffectKind::LumaKey => EffectId::new_static("key.luma"),
            EffectKind::Invert => EffectId::new_static("color.invert"),
            EffectKind::MaskShapeGen => EffectId::new_static("util.mask_shape"),
            EffectKind::Deflicker => EffectId::new_static("color.deflicker"),
            EffectKind::Unknown(tag) => EffectId::new(tag.as_str().to_string()),
        }
    }

    /// The preserved tag if this is an unknown (forward-compat) variant.
    pub fn unknown_tag(self) -> Option<UnknownTag> {
        match self {
            EffectKind::Unknown(t) => Some(t),
            _ => None,
        }
    }

    /// True if this is a forward-compat variant this build does not understand.
    pub fn is_unknown(self) -> bool {
        matches!(self, EffectKind::Unknown(_))
    }
}

/// Keyframe-authoring effect parameters: an ordered `PropPath → PropValue` map,
/// seeded from the kind's registry defaults, every entry animatable via the
/// surrounding [`AnimProps`](super::anim::AnimProps).
///
/// Backed by an ordered `Vec` of pairs (not a `HashMap`) so serialization is
/// deterministic — a hard requirement for the export-determinism tests (03 §6)
/// and for stable undo diffs.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EffectParams {
    pub entries: Vec<(PropPath, PropValue)>,
}

impl EffectParams {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a param bag for a target kind.
    ///
    /// When the kind is an effect with a manifest, the manifest's declared
    /// defaults win — it is the single source of truth for an effect's data
    /// (30 §2), and seeding from anywhere else lets the two tables drift. The
    /// registry's neutral rule (0 / range-min for floats, transparent for
    /// colours) remains the fallback for non-effect targets and for any kind
    /// with no manifest.
    ///
    /// This is behaviour-preserving for the v1 kinds, whose manifest defaults
    /// are all the neutral values already; it exists so an effect whose sensible
    /// default is *not* neutral (deflicker's strength, for instance — seeding it
    /// to 0 would add an effect that does nothing) can say so in one place.
    pub fn seed(kind: PropTargetKind) -> Self {
        if let PropTargetKind::Effect(e) = kind {
            if let Some(m) = super::effect_manifest::manifest(e.effect_id()) {
                return EffectParams {
                    entries: m
                        .params
                        .iter()
                        .map(|spec| (PropPath::new(spec.path), spec.default))
                        .collect(),
                };
            }
        }
        let mut entries = Vec::new();
        for e in prop_registry::entries(kind) {
            let value = match e.kind {
                super::anim::PropValueKind::Float => {
                    // Neutral default: 0 if in range, else the range min.
                    let v = match e.range {
                        Some((lo, hi)) if (lo..=hi).contains(&0.0) => 0.0,
                        Some((lo, _)) => lo,
                        None => 0.0,
                    };
                    PropValue::Float(v)
                }
                super::anim::PropValueKind::Vec2 => PropValue::Vec2([0.0, 0.0]),
                super::anim::PropValueKind::Color => PropValue::Color(crate::Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.0,
                }),
                super::anim::PropValueKind::Bool => PropValue::Bool(false),
                super::anim::PropValueKind::Enum => PropValue::Enum(0),
            };
            entries.push((PropPath::new(e.path), value));
        }
        EffectParams { entries }
    }

    pub fn get(&self, path: &str) -> Option<&PropValue> {
        self.entries
            .iter()
            .find(|(p, _)| p.as_str() == path)
            .map(|(_, v)| v)
    }

    /// Set (or insert) a param, preserving insertion order.
    pub fn set(&mut self, path: impl Into<PropPath>, value: PropValue) {
        let path = path.into();
        if let Some(slot) = self.entries.iter_mut().find(|(p, _)| *p == path) {
            slot.1 = value;
        } else {
            self.entries.push((path, value));
        }
    }
}

impl PropSet for EffectParams {
    // Effect params are an open map shared across effect kinds, graph nodes, and
    // audio-fx units; per-kind path validation happens through `prop_registry`
    // with the concrete kind. The generic bound uses the lenient `GraphNode`
    // target so the trait has a single const.
    const TARGET_KIND: PropTargetKind = PropTargetKind::GraphNode;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_populates_from_registry() {
        let p = EffectParams::seed(PropTargetKind::Effect(EffectKind::Glow));
        assert!(p.get("params.radius").is_some());
        assert!(matches!(p.get("params.tint"), Some(PropValue::Color(_))));
    }

    #[test]
    fn set_preserves_order_and_updates() {
        let mut p = EffectParams::seed(PropTargetKind::Effect(EffectKind::Blur));
        p.set("params.radius", PropValue::Float(12.0));
        assert_eq!(p.get("params.radius"), Some(&PropValue::Float(12.0)));
        let n = p.entries.len();
        p.set("params.radius", PropValue::Float(20.0));
        assert_eq!(p.entries.len(), n, "existing key must not duplicate");
    }

    #[test]
    fn effect_params_roundtrip_serde() {
        let p = EffectParams::seed(PropTargetKind::Effect(EffectKind::ChromaKey));
        let json = serde_json::to_string(&p).unwrap();
        let back: EffectParams = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn unknown_variant_preserves_tag_verbatim() {
        let k: EffectKind = serde_json::from_str("\"film_look\"").unwrap();
        assert!(k.is_unknown());
        assert_eq!(k.unknown_tag().unwrap().as_str(), "film_look");
        // Re-emits the original tag byte-for-byte (39 §2.2 rule 1).
        assert_eq!(serde_json::to_string(&k).unwrap(), "\"film_look\"");
    }

    #[test]
    fn known_variants_are_not_shadowed_by_unknown_fallback() {
        for (k, tag) in [
            (EffectKind::Blur, "\"blur\""),
            (EffectKind::Sharpen, "\"sharpen\""),
            (EffectKind::Glow, "\"glow\""),
            (EffectKind::ChromaKey, "\"chroma_key\""),
            (EffectKind::LumaKey, "\"luma_key\""),
            (EffectKind::Invert, "\"invert\""),
            (EffectKind::MaskShapeGen, "\"mask_shape_gen\""),
        ] {
            assert_eq!(serde_json::to_string(&k).unwrap(), tag);
            let back: EffectKind = serde_json::from_str(tag).unwrap();
            assert_eq!(back, k, "known tag {tag} must not degrade to Unknown");
            assert!(!back.is_unknown());
        }
    }
}
