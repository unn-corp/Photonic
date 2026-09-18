//! Generic keyframe animation (01 §6).
//!
//! One system animates everything: clip transforms, effect params, grade-op
//! params, audio automation, and (eventually) vector-node properties. A target
//! struct `T: PropSet` carries a static `base` value plus a list of
//! [`PropertyTrack`]s, each keyframing one [`PropPath`] over clip-relative
//! [`Tick`]s. Evaluation ([`eval`]) is a pure, closed-form function with no I/O
//! — unit-tested against hand-computed expectations (CAP-007).

use super::time::Tick;
use crate::Color;
use serde::{Deserialize, Serialize};

/// A dotted property path addressing one animatable field of a target kind,
/// e.g. `"transform.x"`, `"params.blur_radius"`, `"params.slope[0]"`. Validated
/// against the static registry in [`super::prop_registry`]; an unknown path on
/// load keeps the track but flags it [`PropertyTrack::orphaned`] (01 §6.2).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PropPath(pub String);

impl PropPath {
    pub fn new(s: impl Into<String>) -> Self {
        PropPath(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for PropPath {
    fn from(s: &str) -> Self {
        PropPath(s.to_string())
    }
}

/// The kind of value a property holds. `Bool`/`Enum` do not interpolate — they
/// step (Hold) regardless of a keyframe's declared [`Interp`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PropValueKind {
    Float,
    Vec2,
    Color,
    Bool,
    Enum,
}

/// A concrete animatable value.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum PropValue {
    Float(f64),
    Vec2([f64; 2]),
    Color(Color),
    Bool(bool),
    Enum(u32),
}

impl PropValue {
    pub fn kind(&self) -> PropValueKind {
        match self {
            PropValue::Float(_) => PropValueKind::Float,
            PropValue::Vec2(_) => PropValueKind::Vec2,
            PropValue::Color(_) => PropValueKind::Color,
            PropValue::Bool(_) => PropValueKind::Bool,
            PropValue::Enum(_) => PropValueKind::Enum,
        }
    }
}

/// How a keyframe's value blends into the next keyframe over the segment that
/// starts at this keyframe.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Interp {
    /// Step: hold this keyframe's value until the next keyframe.
    Hold,
    /// Straight-line blend to the next keyframe.
    Linear,
    /// Normalized cubic-bezier ease (CSS-like): control handles in [0,1]² over
    /// the segment's unit `(time, value)` box, endpoints fixed at (0,0)/(1,1).
    Bezier {
        out_handle: [f64; 2],
        in_handle: [f64; 2],
    },
}

// ── Named easing presets (26 §10 K-B12) ─────────────────────────────────────

/// A named easing preset — a display name for one fixed [`Interp`].
///
/// **A preset is a name, not a stored form.** `EasePreset` is deliberately *not*
/// serializable and never reaches a document: [`Self::interp`] lowers a preset to
/// the plain `Hold`/`Linear`/`Bezier { out_handle, in_handle }` that already
/// round-trips, and [`Self::from_interp`] recovers the name from stored handles.
/// So picking "Ease In-Out" in the curve editor writes exactly what dragging the
/// handles there by hand would write — no new variant, no serde migration, and
/// an older build reads the document unchanged.
///
/// **26 §10 K-B12 asks for an explicit decision on the overshoot families.**
/// Back, elastic and bounce leave `[0,1]` on the value axis, which the normalized
/// handle form cannot express, so they are **omitted** rather than approximated
/// with a cubic (the item's own watch-out). Admitting them would need a real
/// `Interp` variant plus a migration; that is a separate, larger change. Every
/// preset here therefore stays inside the unit box — pinned by
/// `ease_presets_stay_inside_the_unit_box`.
///
/// The four eased curves are the CSS `transition-timing-function` keywords, the
/// same vocabulary [`cubic_bezier_ease`] already implements.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EasePreset {
    /// Step — hold the value until the next keyframe.
    Hold,
    /// Constant rate.
    Linear,
    /// CSS `ease` — `cubic-bezier(0.25, 0.1, 0.25, 1)`.
    Ease,
    /// CSS `ease-in` — `cubic-bezier(0.42, 0, 1, 1)`.
    EaseIn,
    /// CSS `ease-out` — `cubic-bezier(0, 0, 0.58, 1)`.
    EaseOut,
    /// CSS `ease-in-out` — `cubic-bezier(0.42, 0, 0.58, 1)`.
    EaseInOut,
}

/// Tolerance used by [`EasePreset::from_interp`] when matching stored handles
/// against a preset. Handles round-trip exactly through serde, so this only
/// absorbs a lossy re-writer; the presets are separated by ≥ 0.1 in handle
/// space, so no two can match the same `Interp`
/// (`ease_preset_handles_are_far_apart`).
const PRESET_EPS: f64 = 1e-9;

impl EasePreset {
    /// Every preset, in picker order (gentlest vocabulary first).
    pub const ALL: &'static [EasePreset] = &[
        EasePreset::Hold,
        EasePreset::Linear,
        EasePreset::Ease,
        EasePreset::EaseIn,
        EasePreset::EaseOut,
        EasePreset::EaseInOut,
    ];

    /// Stable machine identifier (snake_case). Safe to put in a tool argument or
    /// a preference — unlike [`Self::label`], it is not display text.
    pub const fn id(self) -> &'static str {
        match self {
            EasePreset::Hold => "hold",
            EasePreset::Linear => "linear",
            EasePreset::Ease => "ease",
            EasePreset::EaseIn => "ease_in",
            EasePreset::EaseOut => "ease_out",
            EasePreset::EaseInOut => "ease_in_out",
        }
    }

    /// Human-readable name for a picker row.
    pub const fn label(self) -> &'static str {
        match self {
            EasePreset::Hold => "Hold",
            EasePreset::Linear => "Linear",
            EasePreset::Ease => "Ease",
            EasePreset::EaseIn => "Ease In",
            EasePreset::EaseOut => "Ease Out",
            EasePreset::EaseInOut => "Ease In-Out",
        }
    }

    /// The `(out_handle, in_handle)` pair, or `None` for the two non-Bezier
    /// presets.
    pub const fn handles(self) -> Option<([f64; 2], [f64; 2])> {
        match self {
            EasePreset::Hold | EasePreset::Linear => None,
            EasePreset::Ease => Some(([0.25, 0.1], [0.25, 1.0])),
            EasePreset::EaseIn => Some(([0.42, 0.0], [1.0, 1.0])),
            EasePreset::EaseOut => Some(([0.0, 0.0], [0.58, 1.0])),
            EasePreset::EaseInOut => Some(([0.42, 0.0], [0.58, 1.0])),
        }
    }

    /// Lower the preset to the stored interpolation — what actually lands in the
    /// document.
    pub const fn interp(self) -> Interp {
        match self.handles() {
            None => match self {
                EasePreset::Hold => Interp::Hold,
                _ => Interp::Linear,
            },
            Some((out_handle, in_handle)) => Interp::Bezier {
                out_handle,
                in_handle,
            },
        }
    }

    /// The CSS spelling, for a tooltip or a readout.
    pub fn css(self) -> String {
        match self.handles() {
            None => match self {
                EasePreset::Hold => "step-end".to_string(),
                _ => "linear".to_string(),
            },
            Some(([x1, y1], [x2, y2])) => format!("cubic-bezier({x1}, {y1}, {x2}, {y2})"),
        }
    }

    /// Look a preset up by [`id`](Self::id).
    pub fn from_id(id: &str) -> Option<EasePreset> {
        EasePreset::ALL.iter().copied().find(|p| p.id() == id)
    }

    /// Recover the preset name behind a stored [`Interp`], or `None` when the
    /// curve is a hand-authored Bezier that matches no preset ("Custom").
    ///
    /// Matching on the `Interp` *variant* alone is not enough: all four eased
    /// presets are `Interp::Bezier`, so a variant-only comparison reports every
    /// one of them as the current preset at once
    /// (`preset_identity_separates_curves_variant_matching_conflates`).
    pub fn from_interp(interp: &Interp) -> Option<EasePreset> {
        match interp {
            Interp::Hold => Some(EasePreset::Hold),
            Interp::Linear => Some(EasePreset::Linear),
            Interp::Bezier {
                out_handle,
                in_handle,
            } => EasePreset::ALL.iter().copied().find(|p| match p.handles() {
                Some((o, i)) => close2(o, *out_handle) && close2(i, *in_handle),
                None => false,
            }),
        }
    }
}

fn close2(a: [f64; 2], b: [f64; 2]) -> bool {
    (a[0] - b[0]).abs() <= PRESET_EPS && (a[1] - b[1]).abs() <= PRESET_EPS
}

/// Serializable multi-track keyframe payload for interchange (26 §10 K-B11).
///
/// Copy from one effect/clip lane set, paste onto another with path mapping and
/// a time offset. Not document state — clipboard / MCP wire only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct KeyframeClipboard {
    /// Tracks exactly as copied (source property paths preserved for mapping).
    pub tracks: Vec<PropertyTrack>,
    /// Earliest keyframe time across all tracks (or zero if empty). Paste UIs
    /// re-anchor by mapping this tick onto a destination time.
    #[serde(default)]
    pub anchor: Tick,
}

impl KeyframeClipboard {
    /// Build from tracks, computing [`Self::anchor`] as the min keyframe time.
    pub fn from_tracks(tracks: Vec<PropertyTrack>) -> Self {
        let anchor = tracks
            .iter()
            .flat_map(|t| t.keyframes.iter().map(|k| k.at))
            .min()
            .unwrap_or(Tick::ZERO);
        Self { tracks, anchor }
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.iter().all(|t| t.keyframes.is_empty())
    }
}

/// One keyframe: a clip-relative time, a value, and the interpolation that
/// governs the segment leaving it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    pub at: Tick,
    pub value: PropValue,
    pub interp: Interp,
}

impl Keyframe {
    pub fn new(at: Tick, value: PropValue, interp: Interp) -> Self {
        Keyframe { at, value, interp }
    }
}

/// A keyframe lane for one property. Keyframes are kept sorted by `at` with
/// unique times (enforced by the edit ops / [`Self::insert_keyframe`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PropertyTrack {
    pub property: PropPath,
    pub keyframes: Vec<Keyframe>,
    /// Set when [`property`](Self::property) does not resolve in the registry
    /// for the owning target kind (01 §6.2). The track is retained (an asset or
    /// plugin may re-register the path later) and skipped by evaluation, never
    /// dropped silently.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub orphaned: bool,
}

impl PropertyTrack {
    pub fn new(property: impl Into<PropPath>) -> Self {
        PropertyTrack {
            property: property.into(),
            keyframes: Vec::new(),
            orphaned: false,
        }
    }

    /// Insert or replace a keyframe, keeping the lane sorted with unique `at`.
    pub fn insert_keyframe(&mut self, kf: Keyframe) {
        match self.keyframes.binary_search_by(|k| k.at.cmp(&kf.at)) {
            Ok(i) => self.keyframes[i] = kf,
            Err(i) => self.keyframes.insert(i, kf),
        }
    }

    /// Remove the keyframe at exactly `at`, returning it if present.
    pub fn remove_keyframe(&mut self, at: Tick) -> Option<Keyframe> {
        match self.keyframes.binary_search_by(|k| k.at.cmp(&at)) {
            Ok(i) => Some(self.keyframes.remove(i)),
            Err(_) => None,
        }
    }
}

/// A target of the animation system: a plain struct of animatable fields plus
/// its keyframe lanes. `T` supplies the static base values; the tracks override
/// per-property across time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnimProps<T: PropSet> {
    pub base: T,
    #[serde(default = "Vec::new", skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<PropertyTrack>,
}

impl<T: PropSet> AnimProps<T> {
    pub fn new(base: T) -> Self {
        AnimProps {
            base,
            tracks: Vec::new(),
        }
    }

    /// The lane for `path`, if one exists.
    pub fn track(&self, path: &PropPath) -> Option<&PropertyTrack> {
        self.tracks.iter().find(|t| &t.property == path)
    }

    /// The lane for `path`, creating an empty one if absent.
    pub fn track_mut(&mut self, path: &PropPath) -> &mut PropertyTrack {
        if let Some(i) = self.tracks.iter().position(|t| &t.property == path) {
            &mut self.tracks[i]
        } else {
            self.tracks.push(PropertyTrack::new(path.clone()));
            self.tracks.last_mut().unwrap()
        }
    }

    /// True when no property is keyframed — the common case, letting callers
    /// skip animation eval entirely.
    pub fn is_static(&self) -> bool {
        self.tracks.iter().all(|t| t.keyframes.is_empty())
    }
}

/// Marker trait for structs that back an [`AnimProps`]. Ties the target to its
/// registry block ([`super::prop_registry`]) via [`TARGET_KIND`](Self::TARGET_KIND).
pub trait PropSet: Clone + std::fmt::Debug + PartialEq {
    /// The registry key under which this set's `(path, kind, range)` entries
    /// are published (01 §6.2).
    const TARGET_KIND: super::prop_registry::PropTargetKind;
}

/// Evaluate one property lane at clip-relative time `t` (pure, closed-form).
///
/// - Empty lane → `base`.
/// - `t` at/before the first keyframe → the first keyframe's value.
/// - `t` at/after the last keyframe → the last keyframe's value.
/// - Otherwise interpolate the bracketing segment per the *starting* keyframe's
///   [`Interp`]. `Bool`/`Enum` values always Hold, regardless of `interp`.
pub fn eval(track: &PropertyTrack, base: &PropValue, t: Tick) -> PropValue {
    let kfs = &track.keyframes;
    if track.orphaned || kfs.is_empty() {
        return *base;
    }
    if t <= kfs[0].at {
        return kfs[0].value;
    }
    let last = kfs.last().unwrap();
    if t >= last.at {
        return last.value;
    }
    // Find the segment [k0, k1] with k0.at <= t < k1.at.
    let i = match kfs.binary_search_by(|k| k.at.cmp(&t)) {
        Ok(exact) => return kfs[exact].value,
        Err(next) => next - 1, // `next` is the first kf with at > t
    };
    let k0 = &kfs[i];
    let k1 = &kfs[i + 1];

    // Non-interpolable value kinds step regardless of declared interp.
    if matches!(k0.value, PropValue::Bool(_) | PropValue::Enum(_)) {
        return k0.value;
    }

    let span = (k1.at.0 - k0.at.0) as f64;
    let u = if span <= 0.0 {
        0.0
    } else {
        (t.0 - k0.at.0) as f64 / span
    };

    match k0.interp {
        Interp::Hold => k0.value,
        Interp::Linear => lerp_value(&k0.value, &k1.value, u),
        Interp::Bezier {
            out_handle,
            in_handle,
        } => {
            let eased = cubic_bezier_ease(out_handle, in_handle, u);
            lerp_value(&k0.value, &k1.value, eased)
        }
    }
}

/// Linear blend between two values by `f` in [0,1]. Mismatched variants or
/// non-interpolable kinds hold `a`.
fn lerp_value(a: &PropValue, b: &PropValue, f: f64) -> PropValue {
    let l = |x: f64, y: f64| x + (y - x) * f;
    match (a, b) {
        (PropValue::Float(x), PropValue::Float(y)) => PropValue::Float(l(*x, *y)),
        (PropValue::Vec2(x), PropValue::Vec2(y)) => PropValue::Vec2([l(x[0], y[0]), l(x[1], y[1])]),
        (PropValue::Color(x), PropValue::Color(y)) => PropValue::Color(Color {
            r: l(x.r as f64, y.r as f64) as f32,
            g: l(x.g as f64, y.g as f64) as f32,
            b: l(x.b as f64, y.b as f64) as f32,
            a: l(x.a as f64, y.a as f64) as f32,
        }),
        // Bool/Enum or mismatched variants: hold the start value.
        _ => *a,
    }
}

/// Evaluate a CSS-style cubic-bezier easing `(p1, p2)` at normalized time `u`,
/// returning the eased value in [0,1]. Endpoints are fixed at (0,0)/(1,1); the
/// curve is solved for the parameter `s` where `bezier_x(s) == u`, then
/// `bezier_y(s)` is returned. Uses Newton's method with a bisection fallback,
/// the standard approach browsers use for `transition-timing-function`.
pub fn cubic_bezier_ease(p1: [f64; 2], p2: [f64; 2], u: f64) -> f64 {
    let u = u.clamp(0.0, 1.0);
    let (x1, y1, x2, y2) = (p1[0], p1[1], p2[0], p2[1]);
    // Bezier basis on one axis with control points (0, c1, c2, 1).
    let bez = |c1: f64, c2: f64, s: f64| {
        let mt = 1.0 - s;
        3.0 * mt * mt * s * c1 + 3.0 * mt * s * s * c2 + s * s * s
    };
    let dbez = |c1: f64, c2: f64, s: f64| {
        let mt = 1.0 - s;
        3.0 * mt * mt * c1 + 6.0 * mt * s * (c2 - c1) + 3.0 * s * s * (1.0 - c2)
    };
    // Solve bezier_x(s) = u for s.
    let mut s = u; // good initial guess
    for _ in 0..8 {
        let x = bez(x1, x2, s) - u;
        if x.abs() < 1e-9 {
            break;
        }
        let dx = dbez(x1, x2, s);
        if dx.abs() < 1e-9 {
            break;
        }
        s -= x / dx;
    }
    // Bisection fallback if Newton wandered out of range or stalled.
    if !(0.0..=1.0).contains(&s) || (bez(x1, x2, s) - u).abs() > 1e-6 {
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        s = u;
        for _ in 0..32 {
            s = 0.5 * (lo + hi);
            let x = bez(x1, x2, s);
            if (x - u).abs() < 1e-9 {
                break;
            }
            if x < u {
                lo = s;
            } else {
                hi = s;
            }
        }
    }
    bez(y1, y2, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(kfs: Vec<Keyframe>) -> PropertyTrack {
        PropertyTrack {
            property: PropPath::new("params.x"),
            keyframes: kfs,
            orphaned: false,
        }
    }

    #[test]
    fn empty_track_returns_base() {
        let t = track(vec![]);
        let base = PropValue::Float(3.0);
        assert_eq!(eval(&t, &base, Tick(100)), base);
    }

    #[test]
    fn before_first_and_after_last_clamp() {
        let t = track(vec![
            Keyframe::new(Tick(100), PropValue::Float(0.0), Interp::Linear),
            Keyframe::new(Tick(200), PropValue::Float(10.0), Interp::Linear),
        ]);
        let base = PropValue::Float(-1.0);
        assert_eq!(eval(&t, &base, Tick(0)), PropValue::Float(0.0));
        assert_eq!(eval(&t, &base, Tick(500)), PropValue::Float(10.0));
    }

    #[test]
    fn linear_midpoint_is_closed_form() {
        let t = track(vec![
            Keyframe::new(Tick(0), PropValue::Float(0.0), Interp::Linear),
            Keyframe::new(Tick(100), PropValue::Float(10.0), Interp::Linear),
        ]);
        let base = PropValue::Float(0.0);
        assert_eq!(eval(&t, &base, Tick(50)), PropValue::Float(5.0));
        assert_eq!(eval(&t, &base, Tick(25)), PropValue::Float(2.5));
    }

    #[test]
    fn hold_steps_to_next() {
        let t = track(vec![
            Keyframe::new(Tick(0), PropValue::Float(0.0), Interp::Hold),
            Keyframe::new(Tick(100), PropValue::Float(10.0), Interp::Linear),
        ]);
        let base = PropValue::Float(0.0);
        assert_eq!(eval(&t, &base, Tick(99)), PropValue::Float(0.0));
        assert_eq!(eval(&t, &base, Tick(100)), PropValue::Float(10.0));
    }

    #[test]
    fn vec2_and_color_interpolate_componentwise() {
        let t = track(vec![
            Keyframe::new(Tick(0), PropValue::Vec2([0.0, 10.0]), Interp::Linear),
            Keyframe::new(Tick(100), PropValue::Vec2([10.0, 0.0]), Interp::Linear),
        ]);
        let base = PropValue::Vec2([0.0, 0.0]);
        assert_eq!(eval(&t, &base, Tick(50)), PropValue::Vec2([5.0, 5.0]));
    }

    #[test]
    fn bool_and_enum_hold_regardless_of_interp() {
        let t = track(vec![
            Keyframe::new(Tick(0), PropValue::Bool(false), Interp::Linear),
            Keyframe::new(Tick(100), PropValue::Bool(true), Interp::Linear),
        ]);
        let base = PropValue::Bool(false);
        assert_eq!(eval(&t, &base, Tick(50)), PropValue::Bool(false));
        assert_eq!(eval(&t, &base, Tick(100)), PropValue::Bool(true));
    }

    #[test]
    fn linear_bezier_matches_linear() {
        // cubic-bezier(0,0,1,1) is the identity easing → equals Linear exactly.
        let bez = Interp::Bezier {
            out_handle: [0.0, 0.0],
            in_handle: [1.0, 1.0],
        };
        let t = track(vec![
            Keyframe::new(Tick(0), PropValue::Float(0.0), bez),
            Keyframe::new(Tick(100), PropValue::Float(100.0), Interp::Linear),
        ]);
        let base = PropValue::Float(0.0);
        // ease(0.5) == 0.5 for the identity curve.
        match eval(&t, &base, Tick(50)) {
            PropValue::Float(v) => assert!((v - 50.0).abs() < 1e-6, "got {v}"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn ease_in_out_is_symmetric_and_monotone() {
        // Classic ease-in-out cubic-bezier(0.42, 0, 0.58, 1).
        let p1 = [0.42, 0.0];
        let p2 = [0.58, 1.0];
        assert!((cubic_bezier_ease(p1, p2, 0.0) - 0.0).abs() < 1e-6);
        assert!((cubic_bezier_ease(p1, p2, 1.0) - 1.0).abs() < 1e-6);
        // Symmetric about the midpoint: ease(0.5) == 0.5.
        assert!((cubic_bezier_ease(p1, p2, 0.5) - 0.5).abs() < 1e-6);
        // Monotonically increasing.
        let mut prev = -1.0;
        for i in 0..=20 {
            let v = cubic_bezier_ease(p1, p2, i as f64 / 20.0);
            assert!(v >= prev - 1e-9, "not monotone at {i}: {v} < {prev}");
            prev = v;
        }
    }

    // ── Named easing presets (26 §10 K-B12) ─────────────────────────────────

    /// The handle values themselves are the thing under test here — these are
    /// the published CSS `transition-timing-function` curves, and a silent drift
    /// in one would change every animation authored with it.
    #[test]
    fn ease_presets_pin_the_css_handle_values() {
        assert_eq!(EasePreset::Hold.interp(), Interp::Hold);
        assert_eq!(EasePreset::Linear.interp(), Interp::Linear);
        assert_eq!(EasePreset::Ease.handles(), Some(([0.25, 0.1], [0.25, 1.0])));
        assert_eq!(
            EasePreset::EaseIn.handles(),
            Some(([0.42, 0.0], [1.0, 1.0]))
        );
        assert_eq!(
            EasePreset::EaseOut.handles(),
            Some(([0.0, 0.0], [0.58, 1.0]))
        );
        assert_eq!(
            EasePreset::EaseInOut.handles(),
            Some(([0.42, 0.0], [0.58, 1.0]))
        );
        // The CSS spelling a picker shows must agree with the handles it applies.
        assert_eq!(EasePreset::EaseIn.css(), "cubic-bezier(0.42, 0, 1, 1)");
        assert_eq!(EasePreset::Hold.css(), "step-end");
        assert_eq!(EasePreset::Linear.css(), "linear");
    }

    #[test]
    fn ease_preset_ids_and_labels_are_unique() {
        let n = EasePreset::ALL.len();
        let mut ids: Vec<&str> = EasePreset::ALL.iter().map(|p| p.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate preset id");
        let mut labels: Vec<&str> = EasePreset::ALL.iter().map(|p| p.label()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), n, "duplicate preset label");
        for p in EasePreset::ALL {
            assert_eq!(EasePreset::from_id(p.id()), Some(*p));
        }
        assert_eq!(EasePreset::from_id("bounce_in"), None);
    }

    #[test]
    fn from_interp_round_trips_every_preset_and_rejects_custom() {
        for p in EasePreset::ALL {
            assert_eq!(
                EasePreset::from_interp(&p.interp()),
                Some(*p),
                "{} did not round-trip",
                p.id()
            );
        }
        // A hand-dragged curve that is no preset reads as "Custom" (None).
        let custom = Interp::Bezier {
            out_handle: [0.3, 0.7],
            in_handle: [0.9, 0.2],
        };
        assert_eq!(EasePreset::from_interp(&custom), None);
    }

    /// Sensitivity guard for [`EasePreset::from_interp`]: prove the *pre-fix*
    /// path — comparing only the `Interp` variant, which is what a picker doing
    /// `matches!(a, Bezier{..}) == matches!(b, Bezier{..})` does — really cannot
    /// tell two different eased presets apart, so this test can fail if
    /// `from_interp` ever degrades into that.
    #[test]
    fn preset_identity_separates_curves_variant_matching_conflates() {
        let eased: Vec<EasePreset> = EasePreset::ALL
            .iter()
            .copied()
            .filter(|p| p.handles().is_some())
            .collect();
        assert!(eased.len() >= 2, "need two Bezier presets to compare");
        let variant = |i: &Interp| std::mem::discriminant(i);
        let mut pairs = 0usize;
        for a in &eased {
            for b in &eased {
                if a == b {
                    continue;
                }
                pairs += 1;
                // The pre-fix comparison sees these as the same easing…
                assert_eq!(
                    variant(&a.interp()),
                    variant(&b.interp()),
                    "variant matching should conflate {} and {}",
                    a.id(),
                    b.id()
                );
                // …while preset identity keeps them distinct.
                assert_ne!(
                    EasePreset::from_interp(&a.interp()),
                    EasePreset::from_interp(&b.interp()),
                );
            }
        }
        assert!(pairs > 0, "no preset pairs compared");
    }

    /// No two presets may sit within the match tolerance of each other, or
    /// [`EasePreset::from_interp`]'s "first match wins" would be ambiguous.
    #[test]
    fn ease_preset_handles_are_far_apart() {
        let with_handles: Vec<([f64; 2], [f64; 2])> =
            EasePreset::ALL.iter().filter_map(|p| p.handles()).collect();
        for (i, a) in with_handles.iter().enumerate() {
            for b in with_handles.iter().skip(i + 1) {
                let d = (a.0[0] - b.0[0])
                    .abs()
                    .max((a.0[1] - b.0[1]).abs())
                    .max((a.1[0] - b.1[0]).abs())
                    .max((a.1[1] - b.1[1]).abs());
                assert!(d > 1e-3, "presets {a:?} and {b:?} are only {d} apart");
            }
        }
    }

    /// 26 §10 K-B12's watch-out: never approximate an overshooting family with a
    /// cubic. Every offered preset must stay inside the unit box — and the check
    /// must be able to fail, so it also asserts that a classic "back" curve
    /// (which is what a silent approximation would try to smuggle in) *does*
    /// leave it.
    #[test]
    fn ease_presets_stay_inside_the_unit_box() {
        for p in EasePreset::ALL {
            let Some((o, i)) = p.handles() else { continue };
            for s in 0..=64 {
                let v = cubic_bezier_ease(o, i, s as f64 / 64.0);
                assert!(
                    (-1e-9..=1.0 + 1e-9).contains(&v),
                    "{} overshoots: ease({})={v}",
                    p.id(),
                    s as f64 / 64.0
                );
            }
        }
        // The excluded family really is excluded *because* it overshoots.
        let back = ([0.68, -0.55], [0.265, 1.55]);
        let overshoots = (0..=64)
            .map(|s| cubic_bezier_ease(back.0, back.1, s as f64 / 64.0))
            .any(|v| !(-1e-9..=1.0 + 1e-9).contains(&v));
        assert!(
            overshoots,
            "the unit-box check cannot fail — it would pass a back/elastic curve"
        );
    }

    /// Presets are names over the *existing* stored form: applying one writes
    /// plain Bezier handles, the JSON carries no preset name, and reading it
    /// back recovers the name. No new persisted variant, so no migration.
    #[test]
    fn presets_round_trip_through_serde_as_plain_handles() {
        for p in EasePreset::ALL {
            let kf = Keyframe::new(Tick(120), PropValue::Float(1.5), p.interp());
            let json = serde_json::to_string(&kf).expect("serialize");
            if p.handles().is_some() {
                // An eased preset must store as a bare Bezier: handles only, no
                // trace of the name it was picked by. (`Hold`/`Linear` are
                // exempt — their ids *are* the pre-existing variant tags.)
                assert!(json.contains("bezier"), "not stored as Bezier: {json}");
                for name in [p.id(), p.label()] {
                    assert!(
                        !json.contains(name),
                        "{} leaked its preset name into the document: {json}",
                        p.id()
                    );
                }
            } else {
                assert!(
                    json.contains(&format!(r#""kind":"{}""#, p.id())),
                    "{} should store as the pre-existing variant tag: {json}",
                    p.id()
                );
            }
            let back: Keyframe = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, kf);
            assert_eq!(EasePreset::from_interp(&back.interp), Some(*p));
        }
    }

    /// The names mean what they say when evaluated: ease-in starts slow (below
    /// linear early), ease-out starts fast (above linear early), ease-in-out is
    /// symmetric, and Hold does not move at all.
    #[test]
    fn preset_names_match_their_evaluated_shape() {
        let ease = |p: EasePreset, u: f64| {
            let (o, i) = p.handles().expect("bezier preset");
            cubic_bezier_ease(o, i, u)
        };
        assert!(ease(EasePreset::EaseIn, 0.25) < 0.25);
        assert!(ease(EasePreset::EaseOut, 0.25) > 0.25);
        assert!((ease(EasePreset::EaseInOut, 0.5) - 0.5).abs() < 1e-6);
        assert!(ease(EasePreset::EaseInOut, 0.25) < 0.25);
        assert!(ease(EasePreset::EaseInOut, 0.75) > 0.75);
        assert!(ease(EasePreset::Ease, 0.25) > 0.25, "CSS ease leads early");

        // Through `eval`, on a real lane: Hold steps, Linear is the diagonal.
        let lane = |p: EasePreset| {
            track(vec![
                Keyframe::new(Tick(0), PropValue::Float(0.0), p.interp()),
                Keyframe::new(Tick(100), PropValue::Float(100.0), Interp::Linear),
            ])
        };
        let base = PropValue::Float(0.0);
        assert_eq!(
            eval(&lane(EasePreset::Hold), &base, Tick(50)),
            PropValue::Float(0.0)
        );
        assert_eq!(
            eval(&lane(EasePreset::Linear), &base, Tick(50)),
            PropValue::Float(50.0)
        );
        match eval(&lane(EasePreset::EaseIn), &base, Tick(25)) {
            PropValue::Float(v) => assert!(v < 25.0, "ease-in should lag at t=0.25, got {v}"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn insert_and_remove_keep_sorted_unique() {
        let mut tr = PropertyTrack::new("params.x");
        tr.insert_keyframe(Keyframe::new(
            Tick(100),
            PropValue::Float(1.0),
            Interp::Hold,
        ));
        tr.insert_keyframe(Keyframe::new(Tick(0), PropValue::Float(0.0), Interp::Hold));
        tr.insert_keyframe(Keyframe::new(Tick(50), PropValue::Float(0.5), Interp::Hold));
        // Replace at 50.
        tr.insert_keyframe(Keyframe::new(Tick(50), PropValue::Float(9.0), Interp::Hold));
        let ats: Vec<i64> = tr.keyframes.iter().map(|k| k.at.0).collect();
        assert_eq!(ats, vec![0, 50, 100]);
        assert_eq!(tr.keyframes[1].value, PropValue::Float(9.0));
        assert!(tr.remove_keyframe(Tick(50)).is_some());
        assert!(tr.remove_keyframe(Tick(50)).is_none());
        assert_eq!(tr.keyframes.len(), 2);
    }
}
