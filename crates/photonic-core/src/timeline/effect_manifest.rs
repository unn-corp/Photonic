//! Data-driven effect catalogue (spec 30 §2). The effect registry is *data*: one
//! versioned [`EffectManifest`] per effect, compiled into a single `&'static`
//! table ([`MANIFESTS`]). Everything downstream — the runtime registry, the
//! [`prop_registry`](super::prop_registry) projection, inspector widgets, MCP
//! schemas and docs — is generated from this table rather than maintained in
//! parallel.
//!
//! # Deliberate deviation from spec §2.1/§3 (recorded here per the brief)
//!
//! The spec's `EffectManifest` carries a `KernelRef { wgsl, cpu: fn(&mut
//! ImageF32, &ResolvedParams) }`. That field is **not** present here. `ImageF32`
//! is `photonic_video::graph::ops::Image` and `ResolvedParams` lives in
//! `photonic_video::contract`; the dependency direction is `core ← render ←
//! video` (photonic-video depends on photonic-core, never the reverse). Core
//! therefore owns the *data* of an effect; the kernel-binding table (the
//! `{wgsl, cpu}` pair) lives in photonic-video and is joined to this table by
//! [`EffectId`]. An exhaustiveness test on the video side covers the join.
//!
//! # Deliberate deviation from spec §2.2 (`ParamKind::Path` projection)
//!
//! [`ParamKind::Path`] has no [`PropValueKind`](super::anim::PropValueKind)
//! counterpart, and adding one would require a `PropValue::Path(String)` variant
//! that breaks the crate-wide `Copy` derive on `PropValue` (and every exhaustive
//! `match` on `PropValueKind` in the GUI). None of the seven v1 manifests use a
//! `Path` (or `Enum`) param, so [`project`] maps `Path`/`Enum` to
//! `PropValueKind::Enum` as a total, currently-unreachable placeholder. Revisit
//! when the first effect actually needs a path param.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use super::anim::{PropValue, PropValueKind};
use super::effect_kind::{EffectKind, EffectParams};
use super::prop_registry::PropEntry;

// ── Identity ─────────────────────────────────────────────────────────────────

/// A stable, human-readable effect id, e.g. `EffectId("blur.gaussian")`.
///
/// Backed by a `Cow<'static, str>` (not `&'static str`) so an id authored by a
/// newer build — one this build has no manifest for — survives a load/save
/// round trip owned rather than being dropped (spec §2.6, inert-and-preserved).
/// The static table uses the borrowed form via [`EffectId::new_static`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EffectId(pub Cow<'static, str>);

impl EffectId {
    /// The sentinel "no id" value used as the serde default on
    /// [`ClipEffect`](super::clip::ClipEffect); an absent id is backfilled from
    /// the effect's legacy `kind` in `finalize_load`.
    pub const EMPTY: EffectId = EffectId(Cow::Borrowed(""));

    /// Construct a borrowed id for the `&'static` manifest table (const).
    pub const fn new_static(s: &'static str) -> EffectId {
        EffectId(Cow::Borrowed(s))
    }

    /// Construct an owned id (e.g. from deserialized text or an unknown tag).
    pub fn new(s: impl Into<Cow<'static, str>>) -> EffectId {
        EffectId(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The legacy [`EffectKind`] this id projects to, if it is one of the seven
    /// mapped v1 ids. `None` for any other (including future/unknown) id.
    pub fn legacy_kind(&self) -> Option<EffectKind> {
        LEGACY_IDS
            .iter()
            .find(|(s, _)| *s == self.as_str())
            .map(|(_, k)| *k)
    }
}

/// The bidirectional bridge between the mapped [`EffectKind`] variants and their
/// [`EffectId`]s (spec §10).
///
/// Every id with a real kernel **must** appear here: [`ClipEffect::from_manifest`]
/// resolves an id to its kind through this table, and an id that is missing
/// silently becomes [`EffectKind::Unknown`] — i.e. the effect is created inert
/// and its kernel never runs, with no error anywhere.
///
/// [`ClipEffect::from_manifest`]: super::clip::ClipEffect::from_manifest
const LEGACY_IDS: &[(&str, EffectKind)] = &[
    ("blur.gaussian", EffectKind::Blur),
    ("sharpen.unsharp", EffectKind::Sharpen),
    ("stylize.glow", EffectKind::Glow),
    ("key.chroma", EffectKind::ChromaKey),
    ("key.luma", EffectKind::LumaKey),
    ("color.invert", EffectKind::Invert),
    ("util.mask_shape", EffectKind::MaskShapeGen),
    ("color.deflicker", EffectKind::Deflicker),
];

// ── Schema ───────────────────────────────────────────────────────────────────

/// Coarse effect grouping for palettes and docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectCategory {
    Blur,
    Sharpen,
    Color,
    Stylize,
    Noise,
    Geo,
    Key,
    Util,
}

/// The value shape of a param. `Enum` carries its variant labels; `Path`
/// addresses an asset/file (see the module note on `Path` projection).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamKind {
    Float,
    Vec2,
    Color,
    Bool,
    Enum(&'static [&'static str]),
    Path,
}

/// A UI presentation hint for a param widget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiHint {
    Slider,
    Dial,
    Angle,
    ColorSwatch,
    Enum,
    Point,
    Rect,
}

/// Display mapping for a param: the shown value is `backend * factor + offset`;
/// the stored (canonical) value is unaffected (spec §2, display-vs-canonical).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Display {
    pub factor: f64,
    pub offset: f64,
    pub suffix: &'static str,
    pub decimals: u8,
}

impl Display {
    pub const IDENTITY: Display = Display {
        factor: 1.0,
        offset: 0.0,
        suffix: "",
        decimals: 3,
    };
}

/// One parameter of an effect. `path` is `params.`-prefixed and unique within a
/// manifest; `default` is the canonical seed value and its discriminant must
/// match `kind`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamSpec {
    pub path: &'static str,
    pub kind: ParamKind,
    pub default: PropValue,
    pub range: Option<(f64, f64)>,
    pub animatable: bool,
    pub display: Display,
    pub ui: UiHint,
    pub group: Option<&'static str>,
}

/// How an effect treats the alpha channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaBehaviour {
    Preserves,
    Modifies,
    Requires,
}

/// Bit-depth requirement of an effect's kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitDepth {
    Any,
    RequiresFloat,
}

/// GPU support level for an effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuSupport {
    Native,
    CpuFallback,
    CpuOnly,
}

/// Capabilities an effect declares to the scheduler/UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    pub alpha: AlphaBehaviour,
    pub bit_depth: BitDepth,
    pub linear_light: bool,
    pub gpu: GpuSupport,
}

impl Caps {
    pub const DEFAULT: Caps = Caps {
        alpha: AlphaBehaviour::Preserves,
        bit_depth: BitDepth::Any,
        linear_light: true,
        gpu: GpuSupport::Native,
    };
}

/// The scopes an effect may be attached to (spec §2). `reverse_safe` marks
/// effects safe to apply under a reversed/speed-mapped clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Applicability {
    pub clip: bool,
    pub track: bool,
    pub master: bool,
    pub asset: bool,
    pub reverse_safe: bool,
}

impl Applicability {
    pub const CLIP_ONLY: Applicability = Applicability {
        clip: true,
        track: false,
        master: false,
        asset: false,
        reverse_safe: true,
    };
    /// All four video effect scopes (K-B1 residual until per-id curated).
    pub const ALL_SCOPES: Applicability = Applicability {
        clip: true,
        track: true,
        master: true,
        asset: true,
        reverse_safe: true,
    };

    /// Whether `owner` is an allowed attach site for this applicability.
    pub fn allows(self, owner: super::commands::VfxOwner) -> bool {
        match owner {
            super::commands::VfxOwner::Clip(_) => self.clip,
            super::commands::VfxOwner::Track(_) => self.track,
            super::commands::VfxOwner::Master(_) => self.master,
            super::commands::VfxOwner::Asset(_) => self.asset,
        }
    }
}

/// The colour space a kernel operates in (spec §4.2). Spatial ops run in linear
/// light; code-value ops run in the transfer (encoded) domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperandSpace {
    LinearStraight,
    TransferStraight,
}

/// A single versioned effect definition (spec §2.1). The kernel binding
/// (`{wgsl, cpu}`) is deliberately absent — see the module doc.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectManifest {
    pub id: EffectId,
    pub version: u16,
    pub name: &'static str,
    pub category: EffectCategory,
    pub params: &'static [ParamSpec],
    pub caps: Caps,
    pub applies: Applicability,
    pub space: OperandSpace,
    pub arity: u8,
}

// ── ParamSpec constructors (const) ───────────────────────────────────────────

/// A float param. The default is `0.0`, which matches
/// [`EffectParams::seed`](super::effect_kind::EffectParams::seed)'s neutral rule
/// for every v1 effect (all their float ranges contain 0).
const fn pf(path: &'static str, range: Option<(f64, f64)>, ui: UiHint) -> ParamSpec {
    pf_def(path, 0.0, range, ui)
}

/// A float param with an explicit default (e.g. levels in_white = 1.0).
const fn pf_def(
    path: &'static str,
    default: f64,
    range: Option<(f64, f64)>,
    ui: UiHint,
) -> ParamSpec {
    ParamSpec {
        path,
        kind: ParamKind::Float,
        default: PropValue::Float(default),
        range,
        animatable: true,
        display: Display::IDENTITY,
        ui,
        group: None,
    }
}

/// A colour param, defaulting to transparent black (seed's neutral colour).
const fn pcol(path: &'static str) -> ParamSpec {
    ParamSpec {
        path,
        kind: ParamKind::Color,
        default: PropValue::Color(crate::Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        }),
        range: None,
        animatable: true,
        display: Display::IDENTITY,
        ui: UiHint::ColorSwatch,
        group: None,
    }
}

/// A boolean param, defaulting to `false` (seed's neutral boolean).
const fn pbool(path: &'static str) -> ParamSpec {
    ParamSpec {
        path,
        kind: ParamKind::Bool,
        default: PropValue::Bool(false),
        range: None,
        animatable: true,
        display: Display::IDENTITY,
        ui: UiHint::Slider,
        group: None,
    }
}

// ── Param blocks (reproduce the legacy prop_registry blocks byte-for-byte) ────

const BLUR_PARAMS: &[ParamSpec] = &[pf("params.radius", Some((0.0, 500.0)), UiHint::Slider)];

const SHARPEN_PARAMS: &[ParamSpec] = &[
    pf("params.amount", Some((0.0, 10.0)), UiHint::Slider),
    pf("params.radius", Some((0.0, 100.0)), UiHint::Slider),
];

const GLOW_PARAMS: &[ParamSpec] = &[
    pf("params.radius", Some((0.0, 500.0)), UiHint::Slider),
    pf("params.threshold", Some((0.0, 1.0)), UiHint::Slider),
    pf("params.intensity", Some((0.0, 10.0)), UiHint::Slider),
    pcol("params.tint"),
];

const CHROMAKEY_PARAMS: &[ParamSpec] = &[
    pcol("params.key_color"),
    pf("params.tolerance", Some((0.0, 1.0)), UiHint::Slider),
    pf("params.edge_softness", Some((0.0, 1.0)), UiHint::Slider),
    pf("params.spill_suppress", Some((0.0, 1.0)), UiHint::Slider),
];

const LUMAKEY_PARAMS: &[ParamSpec] = &[
    pf("params.threshold", Some((0.0, 1.0)), UiHint::Slider),
    pf("params.softness", Some((0.0, 1.0)), UiHint::Slider),
    pbool("params.invert"),
];

const INVERT_PARAMS: &[ParamSpec] = &[];

/// Deflicker (`color.deflicker`). `window` is in **seconds** rather than frames
/// so the same setting behaves identically at 24 and 60 fps; the kernel converts
/// using the clip's rate. The default 4 s is long enough to catch slow
/// auto-exposure drift (0.05–0.2 Hz) without the baseline chasing the hunting
/// it is meant to measure against.
const DEFLICKER_PARAMS: &[ParamSpec] = &[
    pf_def("params.amount", 0.85, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.window", 4.0, Some((0.2, 30.0)), UiHint::Slider),
    pf_def("params.max_change", 0.25, Some((0.0, 0.9)), UiHint::Slider),
    pf_def(
        "params.chroma_amount",
        0.0,
        Some((0.0, 1.0)),
        UiHint::Slider,
    ),
];

const MASKSHAPE_PARAMS: &[ParamSpec] = &[
    pf("params.center_x", Some((0.0, 1.0)), UiHint::Slider),
    pf("params.center_y", Some((0.0, 1.0)), UiHint::Slider),
    pf("params.size_x", Some((0.0, 2.0)), UiHint::Slider),
    pf("params.size_y", Some((0.0, 2.0)), UiHint::Slider),
    pf("params.rotation", None, UiHint::Angle),
    pf("params.feather", Some((0.0, 1.0)), UiHint::Slider),
];

// K-B16 Tier-1 catalogue params
const MOTION_BLUR_PARAMS: &[ParamSpec] = &[
    pf("params.angle", Some((0.0, 360.0)), UiHint::Angle),
    pf_def("params.distance", 8.0, Some((0.0, 500.0)), UiHint::Slider),
];
const LEVELS_PARAMS: &[ParamSpec] = &[
    pf("params.in_black", Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.in_white", 1.0, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.gamma", 1.0, Some((0.01, 9.99)), UiHint::Slider),
    pf("params.out_black", Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.out_white", 1.0, Some((0.0, 1.0)), UiHint::Slider),
];
const POSTERIZE_PARAMS: &[ParamSpec] = &[pf_def(
    "params.levels",
    4.0,
    Some((2.0, 255.0)),
    UiHint::Slider,
)];
const THRESHOLD_PARAMS: &[ParamSpec] = &[pf_def(
    "params.level",
    0.5,
    Some((0.0, 1.0)),
    UiHint::Slider,
)];
const HUE_SAT_PARAMS: &[ParamSpec] = &[
    pf("params.hue", Some((-180.0, 180.0)), UiHint::Angle),
    pf("params.saturation", Some((-1.0, 1.0)), UiHint::Slider),
    pf("params.lightness", Some((-1.0, 1.0)), UiHint::Slider),
];
const VIBRANCE_PARAMS: &[ParamSpec] = &[pf("params.amount", Some((-1.0, 1.0)), UiHint::Slider)];
const CHANNEL_MIXER_PARAMS: &[ParamSpec] = &[
    pf_def("params.rr", 1.0, Some((-2.0, 2.0)), UiHint::Slider),
    pf("params.rg", Some((-2.0, 2.0)), UiHint::Slider),
    pf("params.rb", Some((-2.0, 2.0)), UiHint::Slider),
    pf("params.gr", Some((-2.0, 2.0)), UiHint::Slider),
    pf_def("params.gg", 1.0, Some((-2.0, 2.0)), UiHint::Slider),
    pf("params.gb", Some((-2.0, 2.0)), UiHint::Slider),
    pf("params.br", Some((-2.0, 2.0)), UiHint::Slider),
    pf("params.bg", Some((-2.0, 2.0)), UiHint::Slider),
    pf_def("params.bb", 1.0, Some((-2.0, 2.0)), UiHint::Slider),
];
const BW_PARAMS: &[ParamSpec] = &[
    pf_def("params.wr", 0.299, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.wg", 0.587, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.wb", 0.114, Some((0.0, 1.0)), UiHint::Slider),
];
const MOSAIC_PARAMS: &[ParamSpec] = &[pf_def(
    "params.block",
    8.0,
    Some((1.0, 256.0)),
    UiHint::Slider,
)];
const GRAIN_PARAMS: &[ParamSpec] = &[
    pf_def("params.amount", 0.1, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.monochrome", 1.0, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.seed", 1.0, Some((0.0, 1.0e9)), UiHint::Slider),
];
const VIGNETTE_PARAMS: &[ParamSpec] = &[
    pf_def("params.amount", -0.5, Some((-1.0, 1.0)), UiHint::Slider),
    pf_def("params.feather", 0.5, Some((0.0, 1.0)), UiHint::Slider),
];
const CA_PARAMS: &[ParamSpec] = &[pf_def(
    "params.amount",
    2.0,
    Some((-64.0, 64.0)),
    UiHint::Slider,
)];
const CLARITY_PARAMS: &[ParamSpec] = &[pf("params.amount", Some((-1.0, 1.0)), UiHint::Slider)];
/// Multi-point RGB curve: up to 5 knots (x,y in 0..1) plus optional contrast
/// pivot. Identity defaults; non-zero contrast overrides the middle knot.
const CURVES_PARAMS: &[ParamSpec] = &[
    pf("params.contrast", Some((-1.0, 1.0)), UiHint::Slider),
    pf("params.p0x", Some((0.0, 1.0)), UiHint::Slider),
    pf("params.p0y", Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.p1x", 0.25, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.p1y", 0.25, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.p2x", 0.5, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.p2y", 0.5, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.p3x", 0.75, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.p3y", 0.75, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.p4x", 1.0, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.p4y", 1.0, Some((0.0, 1.0)), UiHint::Slider),
];
const PHOTO_FILTER_PARAMS: &[ParamSpec] = &[
    pf_def("params.r", 1.0, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.g", 0.5, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.b", 0.2, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.density", 0.25, Some((0.0, 1.0)), UiHint::Slider),
    pf_def(
        "params.preserve_luminosity",
        1.0,
        Some((0.0, 1.0)),
        UiHint::Slider,
    ),
];
const COLOR_BALANCE_PARAMS: &[ParamSpec] = &[
    pf("params.shadows_r", Some((-1.0, 1.0)), UiHint::Slider),
    pf("params.shadows_g", Some((-1.0, 1.0)), UiHint::Slider),
    pf("params.shadows_b", Some((-1.0, 1.0)), UiHint::Slider),
    pf("params.midtones_r", Some((-1.0, 1.0)), UiHint::Slider),
    pf("params.midtones_g", Some((-1.0, 1.0)), UiHint::Slider),
    pf("params.midtones_b", Some((-1.0, 1.0)), UiHint::Slider),
    pf("params.highlights_r", Some((-1.0, 1.0)), UiHint::Slider),
    pf("params.highlights_g", Some((-1.0, 1.0)), UiHint::Slider),
    pf("params.highlights_b", Some((-1.0, 1.0)), UiHint::Slider),
    pf_def(
        "params.preserve_luminosity",
        1.0,
        Some((0.0, 1.0)),
        UiHint::Slider,
    ),
];
const SURFACE_BLUR_PARAMS: &[ParamSpec] = &[
    pf_def("params.radius", 2.0, Some((0.0, 64.0)), UiHint::Slider),
    pf_def("params.threshold", 0.25, Some((0.0, 1.0)), UiHint::Slider),
];
const SMART_SHARPEN_PARAMS: &[ParamSpec] = &[
    pf_def("params.amount", 1.0, Some((0.0, 10.0)), UiHint::Slider),
    pf_def("params.radius", 1.0, Some((0.0, 100.0)), UiHint::Slider),
    pf("params.threshold", Some((0.0, 255.0)), UiHint::Slider),
];
const REDUCE_NOISE_PARAMS: &[ParamSpec] = &[pf_def(
    "params.strength",
    0.5,
    Some((0.0, 1.0)),
    UiHint::Slider,
)];
const DUST_PARAMS: &[ParamSpec] = &[
    pf_def("params.radius", 1.0, Some((0.0, 16.0)), UiHint::Slider),
    pf_def("params.threshold", 16.0, Some((0.0, 255.0)), UiHint::Slider),
];
const PINCH_PARAMS: &[ParamSpec] = &[pf("params.amount", Some((-1.0, 1.0)), UiHint::Slider)];
const RIPPLE_PARAMS: &[ParamSpec] = &[
    pf_def("params.amplitude", 4.0, Some((-64.0, 64.0)), UiHint::Slider),
    pf_def(
        "params.wavelength",
        16.0,
        Some((1.0, 512.0)),
        UiHint::Slider,
    ),
];
const PERSPECTIVE_PARAMS: &[ParamSpec] = &[
    pf("params.tl_x", Some((-1.0, 2.0)), UiHint::Slider),
    pf("params.tl_y", Some((-1.0, 2.0)), UiHint::Slider),
    pf_def("params.tr_x", 1.0, Some((-1.0, 2.0)), UiHint::Slider),
    pf("params.tr_y", Some((-1.0, 2.0)), UiHint::Slider),
    pf_def("params.br_x", 1.0, Some((-1.0, 2.0)), UiHint::Slider),
    pf_def("params.br_y", 1.0, Some((-1.0, 2.0)), UiHint::Slider),
    pf("params.bl_x", Some((-1.0, 2.0)), UiHint::Slider),
    pf_def("params.bl_y", 1.0, Some((-1.0, 2.0)), UiHint::Slider),
];
const ALPHA_VIEW_PARAMS: &[ParamSpec] = &[pf("params.mode", Some((0.0, 2.0)), UiHint::Slider)]; // 0=alpha,1=premul,2=straight
const DROP_SHADOW_PARAMS: &[ParamSpec] = &[
    pf_def("params.x", 4.0, Some((-200.0, 200.0)), UiHint::Slider),
    pf_def("params.y", 4.0, Some((-200.0, 200.0)), UiHint::Slider),
    pf_def("params.radius", 3.0, Some((0.0, 64.0)), UiHint::Slider),
    pf("params.r", Some((0.0, 1.0)), UiHint::Slider),
    pf("params.g", Some((0.0, 1.0)), UiHint::Slider),
    pf("params.b", Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.opacity", 0.5, Some((0.0, 1.0)), UiHint::Slider),
];
const OUTLINE_PARAMS: &[ParamSpec] = &[
    pf_def("params.thickness", 2.0, Some((0.0, 64.0)), UiHint::Slider),
    pf_def("params.r", 1.0, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.g", 1.0, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.b", 1.0, Some((0.0, 1.0)), UiHint::Slider),
    pf_def("params.opacity", 1.0, Some((0.0, 1.0)), UiHint::Slider),
];

const CAPS_TRANSFER: Caps = Caps {
    alpha: AlphaBehaviour::Preserves,
    bit_depth: BitDepth::Any,
    linear_light: false,
    gpu: GpuSupport::Native,
};

// ── The table ────────────────────────────────────────────────────────────────

/// The effect catalogue, sorted by [`EffectId`] (invariant checked by test)
/// so lookups and generated docs are stable. Includes the seven v1 kinds plus
/// the K-B16 Tier-1 raster bridge expansion (30 §5.1).
pub static MANIFESTS: &[EffectManifest] = &[
    EffectManifest {
        id: EffectId::new_static("blur.box"),
        version: 1,
        name: "Box Blur",
        category: EffectCategory::Blur,
        params: BLUR_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("blur.gaussian"),
        version: 1,
        name: "Gaussian Blur",
        category: EffectCategory::Blur,
        params: BLUR_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("blur.lens"),
        version: 1,
        name: "Lens Blur",
        category: EffectCategory::Blur,
        params: BLUR_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("blur.motion"),
        version: 1,
        name: "Motion Blur",
        category: EffectCategory::Blur,
        params: MOTION_BLUR_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("blur.surface"),
        version: 1,
        name: "Surface Blur",
        category: EffectCategory::Blur,
        params: SURFACE_BLUR_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("color.black_and_white"),
        version: 1,
        name: "Black & White",
        category: EffectCategory::Color,
        params: BW_PARAMS,
        caps: CAPS_TRANSFER,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::TransferStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("color.channel_mixer"),
        version: 1,
        name: "Channel Mixer",
        category: EffectCategory::Color,
        params: CHANNEL_MIXER_PARAMS,
        caps: CAPS_TRANSFER,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::TransferStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("color.clarity"),
        version: 1,
        name: "Clarity",
        category: EffectCategory::Color,
        params: CLARITY_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("color.color_balance"),
        version: 1,
        name: "Color Balance",
        category: EffectCategory::Color,
        params: COLOR_BALANCE_PARAMS,
        caps: CAPS_TRANSFER,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::TransferStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("color.curves"),
        version: 1,
        name: "Curves",
        category: EffectCategory::Color,
        params: CURVES_PARAMS,
        caps: CAPS_TRANSFER,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::TransferStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("color.deflicker"),
        version: 1,
        name: "Deflicker",
        category: EffectCategory::Color,
        params: DEFLICKER_PARAMS,
        caps: CAPS_TRANSFER,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::TransferStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("color.desaturate"),
        version: 1,
        name: "Desaturate",
        category: EffectCategory::Color,
        params: INVERT_PARAMS,
        caps: CAPS_TRANSFER,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::TransferStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("color.hue_saturation"),
        version: 1,
        name: "Hue/Saturation",
        category: EffectCategory::Color,
        params: HUE_SAT_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("color.invert"),
        version: 1,
        name: "Invert",
        category: EffectCategory::Color,
        params: INVERT_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("color.invert_raster"),
        version: 1,
        name: "Invert (Raster)",
        category: EffectCategory::Color,
        params: INVERT_PARAMS,
        caps: CAPS_TRANSFER,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::TransferStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("color.levels"),
        version: 1,
        name: "Levels",
        category: EffectCategory::Color,
        params: LEVELS_PARAMS,
        caps: CAPS_TRANSFER,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::TransferStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("color.photo_filter"),
        version: 1,
        name: "Photo Filter",
        category: EffectCategory::Color,
        params: PHOTO_FILTER_PARAMS,
        caps: CAPS_TRANSFER,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::TransferStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("color.posterize"),
        version: 1,
        name: "Posterize",
        category: EffectCategory::Color,
        params: POSTERIZE_PARAMS,
        caps: CAPS_TRANSFER,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::TransferStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("color.threshold"),
        version: 1,
        name: "Threshold",
        category: EffectCategory::Color,
        params: THRESHOLD_PARAMS,
        caps: CAPS_TRANSFER,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::TransferStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("color.vibrance"),
        version: 1,
        name: "Vibrance",
        category: EffectCategory::Color,
        params: VIBRANCE_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("filter.high_pass"),
        version: 1,
        name: "High Pass",
        category: EffectCategory::Color,
        params: BLUR_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("filter.median"),
        version: 1,
        name: "Median",
        category: EffectCategory::Blur,
        params: BLUR_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("geo.perspective"),
        version: 1,
        name: "Perspective",
        category: EffectCategory::Geo,
        params: PERSPECTIVE_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("geo.pinch"),
        version: 1,
        name: "Pinch",
        category: EffectCategory::Geo,
        params: PINCH_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("geo.ripple"),
        version: 1,
        name: "Ripple",
        category: EffectCategory::Geo,
        params: RIPPLE_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("geo.spherize"),
        version: 1,
        name: "Spherize",
        category: EffectCategory::Geo,
        params: PINCH_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("key.chroma"),
        version: 1,
        name: "Chroma Key",
        category: EffectCategory::Key,
        params: CHROMAKEY_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("key.luma"),
        version: 1,
        name: "Luma Key",
        category: EffectCategory::Key,
        params: LUMAKEY_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("noise.reduce"),
        version: 1,
        name: "Reduce Noise",
        category: EffectCategory::Noise,
        params: REDUCE_NOISE_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("repair.dust_and_scratches"),
        version: 1,
        name: "Dust & Scratches",
        category: EffectCategory::Util,
        params: DUST_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("sharpen.smart"),
        version: 1,
        name: "Smart Sharpen",
        category: EffectCategory::Sharpen,
        params: SMART_SHARPEN_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("sharpen.unsharp"),
        version: 1,
        name: "Unsharp Mask",
        category: EffectCategory::Sharpen,
        params: SHARPEN_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("sharpen.unsharp_raster"),
        version: 1,
        name: "Unsharp Mask (Raster)",
        category: EffectCategory::Sharpen,
        params: SHARPEN_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("stylize.chromatic_aberration"),
        version: 1,
        name: "Chromatic Aberration",
        category: EffectCategory::Stylize,
        params: CA_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("stylize.emboss"),
        version: 1,
        name: "Emboss",
        category: EffectCategory::Stylize,
        params: INVERT_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("stylize.find_edges"),
        version: 1,
        name: "Find Edges",
        category: EffectCategory::Stylize,
        params: INVERT_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("stylize.glow"),
        version: 1,
        name: "Glow",
        category: EffectCategory::Stylize,
        params: GLOW_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("stylize.grain"),
        version: 1,
        name: "Grain",
        category: EffectCategory::Stylize,
        params: GRAIN_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("stylize.mosaic"),
        version: 1,
        name: "Mosaic",
        category: EffectCategory::Stylize,
        params: MOSAIC_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("stylize.vignette"),
        version: 1,
        name: "Vignette",
        category: EffectCategory::Stylize,
        params: VIGNETTE_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("util.alpha_view"),
        version: 1,
        name: "Alpha View",
        category: EffectCategory::Util,
        params: ALPHA_VIEW_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("util.drop_shadow"),
        version: 1,
        name: "Drop Shadow",
        category: EffectCategory::Util,
        params: DROP_SHADOW_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("util.mask_shape"),
        version: 1,
        name: "Mask Shape",
        category: EffectCategory::Util,
        params: MASKSHAPE_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        // Generator: zero image inputs.
        arity: 0,
    },
    EffectManifest {
        id: EffectId::new_static("util.outline"),
        version: 1,
        name: "Outline",
        category: EffectCategory::Util,
        params: OUTLINE_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
    EffectManifest {
        id: EffectId::new_static("util.unpremultiply"),
        version: 1,
        name: "Unpremultiply",
        category: EffectCategory::Util,
        params: INVERT_PARAMS,
        caps: Caps::DEFAULT,
        applies: Applicability::ALL_SCOPES,
        space: OperandSpace::LinearStraight,
        arity: 1,
    },
];

/// The full catalogue.
pub fn manifests() -> &'static [EffectManifest] {
    MANIFESTS
}

/// The manifest for `id`, if this build knows it.
pub fn manifest(id: EffectId) -> Option<&'static EffectManifest> {
    MANIFESTS.iter().find(|m| m.id == id)
}

// ── prop_registry projection ─────────────────────────────────────────────────

/// Project one [`ParamSpec`] to the [`PropEntry`] shape `prop_registry` exposes.
/// `PropEntry` is a strict subset of `ParamSpec` (path/kind/range), so this is
/// lossless for the fields it carries. See the module note on `Path`/`Enum`.
pub const fn project(spec: &ParamSpec) -> PropEntry {
    let kind = match spec.kind {
        ParamKind::Float => PropValueKind::Float,
        ParamKind::Vec2 => PropValueKind::Vec2,
        ParamKind::Color => PropValueKind::Color,
        ParamKind::Bool => PropValueKind::Bool,
        // No PropValueKind::Path exists; unused by every v1 effect.
        ParamKind::Enum(_) | ParamKind::Path => PropValueKind::Enum,
    };
    PropEntry {
        path: spec.path,
        kind,
        range: spec.range,
    }
}

/// The prop_registry block for an effect kind, projected from its manifest.
/// Empty for an unknown kind (no manifest → no registered paths).
pub fn entries_for_effect(kind: EffectKind) -> Vec<PropEntry> {
    match manifest(kind.effect_id()) {
        Some(m) => m.params.iter().map(project).collect(),
        None => Vec::new(),
    }
}

// ── Migration framework (spec §2.6) ──────────────────────────────────────────

/// A pure, total per-version param migration. The signature takes only `&mut
/// EffectParams`, so no I/O or document access is representable.
#[derive(Clone)]
pub struct EffectMigration {
    pub id: EffectId,
    pub from: u16,
    pub to: u16,
    pub forward: fn(&mut EffectParams),
    /// `None` ⇒ the forward migration is lossy; downgrades are refused.
    pub backward: Option<fn(&mut EffectParams)>,
}

/// The migration table. Empty at v1 (no effect has bumped its version yet).
pub static MIGRATIONS: &[EffectMigration] = &[];

/// A migration failure: no registered step advances the chain past `from`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MigrationError {
    /// The chain `from → … → to` has a gap: no migration leaves version `from`.
    NoPath { id: String, from: u16, to: u16 },
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrationError::NoPath { id, from, to } => write!(
                f,
                "no migration path for effect `{id}` from version {from} to {to}"
            ),
        }
    }
}

impl std::error::Error for MigrationError {}

/// Migrate `params` for `id` from version `from` up to the manifest's current
/// version, chaining `from → from+1 → …`. A gap in the chain is a
/// [`MigrationError::NoPath`], never a silent skip. An id with no manifest is a
/// no-op (returns `from` unchanged — the caller marks it inert).
pub fn migrate(id: &EffectId, from: u16, params: &mut EffectParams) -> Result<u16, MigrationError> {
    let target = match manifest(id.clone()) {
        Some(m) => m.version,
        None => return Ok(from),
    };
    let mut cur = from;
    while cur < target {
        match MIGRATIONS.iter().find(|m| &m.id == id && m.from == cur) {
            Some(step) => {
                (step.forward)(params);
                cur = step.to;
            }
            None => {
                return Err(MigrationError::NoPath {
                    id: id.as_str().to_string(),
                    from: cur,
                    to: target,
                })
            }
        }
    }
    Ok(cur)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::PropTargetKind;
    use std::collections::HashSet;

    /// Every known [`EffectKind`] must round-trip through its [`EffectId`].
    ///
    /// A kind whose id is missing from [`LEGACY_IDS`] still *compiles* and still
    /// appears in the palette, but [`ClipEffect::from_manifest`] resolves it to
    /// `Unknown` — so the effect is created inert and its kernel silently never
    /// runs. This test is the tripwire for that, since nothing else fails.
    ///
    /// [`ClipEffect::from_manifest`]: super::super::clip::ClipEffect::from_manifest
    #[test]
    fn every_known_kind_round_trips_through_its_id() {
        let kinds = [
            EffectKind::Blur,
            EffectKind::Sharpen,
            EffectKind::Glow,
            EffectKind::ChromaKey,
            EffectKind::LumaKey,
            EffectKind::Invert,
            EffectKind::MaskShapeGen,
            EffectKind::Deflicker,
        ];
        for kind in kinds {
            let id = kind.effect_id();
            assert!(
                manifest(id.clone()).is_some(),
                "{id:?} has no manifest entry"
            );
            assert_eq!(
                id.legacy_kind(),
                Some(kind),
                "{id:?} does not map back to {kind:?} — from_manifest would build it inert"
            );
        }
    }

    #[test]
    fn ids_are_unique_and_sorted() {
        let ids: Vec<&str> = MANIFESTS.iter().map(|m| m.id.as_str()).collect();
        let unique: HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "manifest ids must be unique");
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(
            ids, sorted,
            "manifest ids must be stored sorted (spec §2.1)"
        );
    }

    #[test]
    fn param_paths_unique_and_prefixed() {
        for m in MANIFESTS {
            let mut seen = HashSet::new();
            for p in m.params {
                assert!(
                    p.path.starts_with("params."),
                    "{}: param path {:?} must be `params.`-prefixed",
                    m.id.as_str(),
                    p.path
                );
                assert!(
                    seen.insert(p.path),
                    "{}: duplicate param path {:?}",
                    m.id.as_str(),
                    p.path
                );
            }
        }
    }

    #[test]
    fn defaults_match_kind_and_range() {
        for m in MANIFESTS {
            for p in m.params {
                // Default discriminant matches the declared kind.
                let ok = matches!(
                    (p.kind, p.default),
                    (ParamKind::Float, PropValue::Float(_))
                        | (ParamKind::Vec2, PropValue::Vec2(_))
                        | (ParamKind::Color, PropValue::Color(_))
                        | (ParamKind::Bool, PropValue::Bool(_))
                        | (ParamKind::Enum(_), PropValue::Enum(_))
                );
                assert!(
                    ok,
                    "{} {}: default {:?} does not match kind {:?}",
                    m.id.as_str(),
                    p.path,
                    p.default,
                    p.kind
                );
                // Default inside range when both are present.
                if let (Some((lo, hi)), PropValue::Float(v)) = (p.range, p.default) {
                    assert!(
                        (lo..=hi).contains(&v),
                        "{} {}: default {v} outside range {lo}..={hi}",
                        m.id.as_str(),
                        p.path
                    );
                }
                // Display::IDENTITY unless a manifest opts out (all v1 do not).
                assert_eq!(
                    p.display,
                    Display::IDENTITY,
                    "{} uses non-identity display",
                    p.path
                );
            }
        }
    }

    /// Every v1 manifest's [`ParamSpec::default`] must equal what
    /// [`EffectParams::seed`] produces for the same legacy kind — the
    /// zero-behaviour-change proof for the two seed paths (spec §10).
    #[test]
    fn manifest_default_matches_seed_for_legacy_kinds() {
        for (id, kind) in LEGACY_IDS {
            let m = manifest(EffectId::new_static(id)).expect("legacy id has a manifest");
            let seeded = EffectParams::seed(PropTargetKind::Effect(*kind));
            assert_eq!(
                m.params.len(),
                seeded.entries.len(),
                "{id}: manifest and seed disagree on param count"
            );
            for spec in m.params {
                let got = seeded
                    .get(spec.path)
                    .unwrap_or_else(|| panic!("{id}: seed missing manifest param {}", spec.path));
                assert_eq!(
                    *got, spec.default,
                    "{id} {}: seed value != manifest default",
                    spec.path
                );
            }
        }
    }

    #[test]
    fn migration_chain_reaches_current_version() {
        for m in MANIFESTS {
            let mut params = EffectParams::new();
            let reached = migrate(&m.id, 1, &mut params).expect("v1 chain is a no-op");
            assert_eq!(
                reached,
                m.version,
                "{}: chain must reach current version",
                m.id.as_str()
            );
        }
    }

    /// §8 item 7: every reversible migration must round-trip. A loop over the
    /// table (empty at v1) rather than per-migration tests, so new migrations are
    /// covered automatically.
    #[test]
    fn migration_round_trips() {
        for mig in MIGRATIONS {
            if let Some(backward) = mig.backward {
                let mut p = EffectParams::new();
                let original = p.clone();
                (mig.forward)(&mut p);
                backward(&mut p);
                assert_eq!(
                    p,
                    original,
                    "backward∘forward must be identity for {} {}→{}",
                    mig.id.as_str(),
                    mig.from,
                    mig.to
                );
            }
        }
    }

    #[test]
    fn unknown_id_has_no_manifest_and_migrate_is_noop() {
        let id = EffectId::new("future.thing".to_string());
        assert!(manifest(id.clone()).is_none());
        let mut p = EffectParams::new();
        assert_eq!(
            migrate(&id, 3, &mut p),
            Ok(3),
            "unknown id migrate is a no-op"
        );
    }
}
