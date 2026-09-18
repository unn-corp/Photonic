//! Clips (01 §5): the atomic timeline elements.
//!
//! A clip positions a source in a sequence with trim, speed, an animatable
//! transform, an ordered effect stack, an optional grade and per-clip
//! composition, transitions, and audio. The composition (when set) substitutes
//! only the clip's *source* op; transform/effects/grade/reframe still apply on
//! top (02 §2 step 3).

use super::anim::{cubic_bezier_ease, AnimProps, Interp, PropSet};
use super::audio::ClipAudio;
use super::captions::CaptionStyle;
use super::effect_kind::{EffectKind, EffectParams};
use super::effect_manifest::EffectId;
use super::grade::Grade;
use super::ids::{AssetId, ClipId, GraphId, GroupId, SequenceId};
use super::prop_registry::PropTargetKind;
use super::sequence::Marker;
use super::stabilization::StabilizationSpec;
use super::time::Tick;
use super::unknown::UnknownTag;
use crate::Color;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// A clip on a track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    /// Defaults to the asset name.
    pub name: String,
    /// Position in the sequence.
    pub start: Tick,
    pub duration: Tick,
    pub source: ClipSource,
    /// Offset into the source media (trim); 0 for generators.
    #[serde(default)]
    pub source_in: Tick,
    #[serde(default)]
    pub speed: SpeedMap,
    /// pos/scale/rotation/anchor/opacity (01 §6, animatable).
    pub transform: AnimProps<ClipTransform>,
    /// Per-`SequenceFormat` static override, keyed by format index (CAP-012).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub reframe: HashMap<usize, ClipTransform>,
    /// Ordered effect stack; each param animatable (01 §6.3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<ClipEffect>,
    /// Color grade (07); stored here, evaluated as graph nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<Grade>,
    /// Per-clip node graph (D-06); substitutes the clip's SOURCE op only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition: Option<GraphId>,
    /// Overlaps the previous clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_in: Option<Transition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_out: Option<Transition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<ClipAudio>,
    #[serde(default = "super::grade::default_true")]
    pub enabled: bool,
    /// Organizational color label — index into the GUI's fixed swatch
    /// palette (the palette itself is a GUI concern, out of scope here).
    /// `None` = unlabeled (14 §M-1, gap #7's data half).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_label: Option<u8>,
    /// Clip-scoped markers (35 §1). `Marker.at` is clip-relative (0 = clip
    /// start); use [`Clip::marker_sequence_tick`] for the sequence position.
    /// Clip markers are always [`MarkerAnchor::Content`](super::sequence::MarkerAnchor).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
    /// The clip's immediate group (35 §3), or `None` if ungrouped. Resolves in
    /// [`Sequence::groups`](super::sequence::Sequence).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupId>,
    /// Groups this clip with its linked partner(s) (e.g. an A/V pair split
    /// from one media import) so an editor can move them as a unit. `None` =
    /// unlinked (14 §M-2, gap #8's data half — the GUI move-together wiring
    /// is a later story).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_group: Option<LinkGroupId>,
    /// Groups several camera angles behind this one clip (17 §G-20). When
    /// `Some`, the clip is a multicam clip: the engine renders
    /// `multicam.angles[multicam.active]`, and `source`/`source_in` mirror that
    /// active angle. `None` = an ordinary single-source clip. Serde-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multicam: Option<MulticamGroup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stabilization: Option<StabilizationSpec>,
}

impl Clip {
    /// A clip covering `[start, start+duration)` from `source`.
    pub fn new(source: ClipSource, start: Tick, duration: Tick) -> Self {
        Clip {
            id: ClipId::new(),
            name: String::new(),
            start,
            duration,
            source,
            source_in: Tick::ZERO,
            speed: SpeedMap::default(),
            transform: AnimProps::new(ClipTransform::default()),
            reframe: HashMap::new(),
            effects: Vec::new(),
            grade: None,
            composition: None,
            transition_in: None,
            transition_out: None,
            audio: None,
            enabled: true,
            color_label: None,
            markers: Vec::new(),
            group: None,
            link_group: None,
            multicam: None,
            stabilization: None,
        }
    }

    /// End position (exclusive) in the sequence.
    #[inline]
    pub fn end(&self) -> Tick {
        self.start + self.duration
    }

    /// The sequence-relative tick of a clip-scoped marker (35 §1): `clip.start`
    /// plus the marker's clip-relative `at`. Callers must not re-derive this.
    #[inline]
    pub fn marker_sequence_tick(&self, m: &Marker) -> Tick {
        self.start + m.at
    }

    /// Whether this clip overlaps `[start, end)` on the timeline.
    pub fn overlaps(&self, start: Tick, end: Tick) -> bool {
        self.start < end && start < self.end()
    }
}

/// Identifies a link group — clips carrying the same id (e.g. a split A/V
/// pair) are meant to move together (14 gap #8). Defined here rather than
/// alongside the rest of the id newtypes in `ids.rs` since this story's
/// territory is limited to `clip.rs`/`sequence.rs`/`ops.rs`/`commands.rs`;
/// same derive/shape as the `id_newtype!` family there.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LinkGroupId(pub Uuid);

impl LinkGroupId {
    /// A fresh random (v4) id.
    #[inline]
    pub fn new() -> Self {
        LinkGroupId(Uuid::new_v4())
    }
}

impl Default for LinkGroupId {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// What a clip plays.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ClipSource {
    /// Video/audio/image via the media pool.
    Asset {
        asset: AssetId,
    },
    /// `AssetKind::VectorDoc` — rasterized per frame (CAP-006/021).
    Vector {
        asset: AssetId,
    },
    /// Nested sequence (CAP-005); cycle-checked at edit time.
    NestedSequence {
        sequence: SequenceId,
    },
    SolidColor {
        color: Color,
    },
    /// Affects everything below (effects/grade apply to the composite).
    Adjustment,
    /// A title / text / graphics clip living on a video track (G-12). Carries
    /// styled text the engine renders through its text path (`TextGen`); no
    /// render logic lives here. Reuses the caption [`CaptionStyle`] cascade so
    /// titles and captions share one styling vocabulary.
    Text {
        content: TextClipContent,
    },
    /// Forward-compat (39 §2.2): a source kind this build does not know. The
    /// whole object — `source` tag and payload — is retained verbatim and
    /// re-emitted unchanged. Renders as a placeholder frame (same as a missing
    /// asset), never guessed. Declared last so serde tries the known tags first.
    #[serde(untagged)]
    Unknown(serde_json::Map<String, serde_json::Value>),
}

impl ClipSource {
    /// The asset this source references, if any (for relink/GC). An unknown
    /// source references no known asset, so it is never GC-relinked — the
    /// desired conservative behaviour.
    pub fn asset(&self) -> Option<AssetId> {
        match self {
            ClipSource::Asset { asset } | ClipSource::Vector { asset } => Some(*asset),
            _ => None,
        }
    }

    /// The preserved `source` tag if this is an unknown (forward-compat) variant.
    pub fn unknown_tag(&self) -> Option<&str> {
        match self {
            ClipSource::Unknown(map) => map.get("source").and_then(|v| v.as_str()),
            _ => None,
        }
    }

    /// True if this is a forward-compat variant this build does not understand.
    pub fn is_unknown(&self) -> bool {
        matches!(self, ClipSource::Unknown(_))
    }
}

/// Styled text carried by a [`ClipSource::Text`] title / graphics clip (G-12).
/// Reuses the caption [`CaptionStyle`] type for font / size / fill / stroke /
/// background / position so titles and captions share one styling vocabulary;
/// the engine renders it (no render logic here). Serde-additive: `style`
/// defaults so older/hand-written text clips omitting it still load.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextClipContent {
    pub text: String,
    #[serde(default)]
    pub style: CaptionStyle,
}

impl TextClipContent {
    /// Text with the default caption style.
    pub fn new(text: impl Into<String>) -> Self {
        TextClipContent {
            text: text.into(),
            style: CaptionStyle::default(),
        }
    }
}

/// One camera angle in a [`MulticamGroup`] (17 §G-20): a named source with its
/// own trim. Grouping several angles behind one clip lets an editor cut between
/// cameras while keeping a single clip on the timeline; the engine renders the
/// group's active angle (no render logic here).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MulticamAngle {
    /// Label for the angle picker (defaults to the folded clip's name).
    #[serde(default)]
    pub name: String,
    pub source: ClipSource,
    /// Trim into this angle's source media.
    #[serde(default)]
    pub source_in: Tick,
}

impl MulticamAngle {
    pub fn new(name: impl Into<String>, source: ClipSource, source_in: Tick) -> Self {
        MulticamAngle {
            name: name.into(),
            source,
            source_in,
        }
    }
}

/// A grouped set of camera angles with one live/active angle (17 §G-20). Held
/// on a [`Clip`] (`Clip::multicam`); the engine renders `angles[active]`. The
/// owning clip's `source`/`source_in` mirror the active angle so a
/// multicam-unaware consumer (timeline thumbnail, older loader) still shows the
/// live camera. Serde-additive: absent on every pre-multicam clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MulticamGroup {
    pub angles: Vec<MulticamAngle>,
    /// Index into `angles` of the live camera. Clamped by ops to a valid angle.
    #[serde(default)]
    pub active: usize,
}

impl MulticamGroup {
    /// The live angle, if `active` is in range.
    pub fn active_angle(&self) -> Option<&MulticamAngle> {
        self.angles.get(self.active)
    }
}

/// An exact rational speed factor. `num/den`; negative `num` = reverse.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Ratio {
    pub num: i32,
    pub den: u32,
}

impl Ratio {
    pub const ONE: Ratio = Ratio { num: 1, den: 1 };

    pub fn new(num: i32, den: u32) -> Ratio {
        Ratio {
            num,
            den: den.max(1),
        }
    }

    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }
}

/// One control point in a keyframed speed ramp (G-11): at clip-relative
/// timeline tick `at`, the playback speed `ratio` takes effect, and `interp`
/// governs how speed transitions from this key to the *next* key (the segment
/// leaving this key) — mirroring [`Interp`] on animation keyframes.
///
/// - [`Interp::Hold`] (the default) keeps the classic piecewise-constant ramp:
///   `ratio` holds until the next key, integrating to exact integer source
///   ticks.
/// - [`Interp::Linear`] / [`Interp::Bezier`] ramp the speed continuously from
///   this key's `ratio` to the next key's `ratio`, so a slow-mo→fast-mo ramp
///   eases smoothly. Because the bezier handles are floats, `SpeedKey`/
///   `SpeedMap` are no longer `Eq`/`Hash` (nothing keys a map on them).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpeedKey {
    pub at: Tick,
    pub ratio: Ratio,
    /// Easing of the segment leaving this key. Serde-defaulted to
    /// [`Interp::Hold`] and omitted when `Hold`, so pre-existing keyframed
    /// documents (which carried no easing) stay byte-shape-identical and
    /// integrate exactly as before.
    #[serde(default = "hold_interp", skip_serializing_if = "is_hold")]
    pub interp: Interp,
}

fn hold_interp() -> Interp {
    Interp::Hold
}
fn is_hold(i: &Interp) -> bool {
    matches!(i, Interp::Hold)
}

impl SpeedKey {
    /// A key whose leaving segment *holds* `ratio` (piecewise-constant — the
    /// classic exact-integer ramp).
    #[inline]
    pub fn new(at: Tick, ratio: Ratio) -> Self {
        SpeedKey {
            at,
            ratio,
            interp: Interp::Hold,
        }
    }

    /// A key whose leaving segment *eases* from `ratio` toward the next key's
    /// ratio with `interp` (`Linear`/`Bezier` for a smooth speed ramp).
    #[inline]
    pub fn eased(at: Tick, ratio: Ratio, interp: Interp) -> Self {
        SpeedKey { at, ratio, interp }
    }
}

/// Clip speed. `Constant` is the default and common case; `Keyframed` is a
/// variable-speed ramp (G-11). Serde-additive: the internal `speed` tag keeps
/// existing `constant` documents byte-identical, and `keyframed` is a new tag.
///
/// Ramp interpolation is per-[`SpeedKey`]: a key's segment either *holds* its
/// ratio (piecewise-constant, integrating to exact integer source ticks — the
/// default and the classic behavior) or *eases* (linear/bezier) continuously
/// toward the next key's ratio for a smooth slow-mo→fast-mo ramp. The
/// all-holds case stays exact (i128 rational arithmetic); an eased ramp
/// integrates each segment and rounds to the nearest tick.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "speed", rename_all = "snake_case")]
pub enum SpeedMap {
    Constant(Ratio),
    Keyframed { keys: Vec<SpeedKey> },
}

/// Why a speed map cannot be applied to a clip.
///
/// This is deliberately separate from the editor-facing [`super::ops::EditError`]
/// so importers and non-timeline callers can validate a map before constructing
/// a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeedMapError {
    ZeroDenominator,
    KeyOutsideClip,
    DuplicateKeyTick,
    InvalidBezierHandle,
}

impl Default for SpeedMap {
    fn default() -> Self {
        SpeedMap::Constant(Ratio::ONE)
    }
}

impl SpeedMap {
    /// Sort keyed control points into the canonical serialized order.
    pub fn normalize(&mut self) {
        if let SpeedMap::Keyframed { keys } = self {
            keys.sort_by_key(|key| key.at.0);
        }
    }

    /// Validate the map against a clip-relative duration.
    ///
    /// Key order is normalized by callers before storage, but duplicate ticks
    /// are rejected because a speed at one instant must be unambiguous.
    pub fn validate_for_duration(&self, duration: Tick) -> Result<(), SpeedMapError> {
        let SpeedMap::Keyframed { keys } = self else {
            return match self {
                SpeedMap::Constant(ratio) if ratio.den == 0 => Err(SpeedMapError::ZeroDenominator),
                SpeedMap::Constant(_) => Ok(()),
                SpeedMap::Keyframed { .. } => unreachable!("keyframed handled above"),
            };
        };
        let mut sorted = keys.clone();
        sorted.sort_by_key(|key| key.at.0);
        let mut last = None;
        for key in &sorted {
            if key.ratio.den == 0 {
                return Err(SpeedMapError::ZeroDenominator);
            }
            if key.at.0 < 0 || key.at > duration {
                return Err(SpeedMapError::KeyOutsideClip);
            }
            if last == Some(key.at) {
                return Err(SpeedMapError::DuplicateKeyTick);
            }
            if let Interp::Bezier {
                out_handle,
                in_handle,
            } = key.interp
            {
                if !out_handle
                    .iter()
                    .chain(in_handle.iter())
                    .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
                {
                    return Err(SpeedMapError::InvalidBezierHandle);
                }
            }
            last = Some(key.at);
        }
        Ok(())
    }

    /// Source-time delta consumed over the clip-relative interval `[0, dt]`
    /// (01 §5.1: exact rational arithmetic; `source = source_in + dt * speed`).
    /// Returns the source-time delta only (the caller adds `source_in`).
    ///
    /// - `Constant`: exact rational scale of `dt`.
    /// - `Keyframed`: the speed integrated segment-by-segment (∫₀ᵈᵗ speed).
    ///   Handles negative `dt` (reverse trim) by symmetry and an empty ramp as
    ///   identity (1×). All-holds ramps use exact integer arithmetic (i128
    ///   accumulate); an eased (linear/bezier) ramp integrates in `f64` and
    ///   rounds to the nearest tick.
    pub fn source_delta(&self, dt: Tick) -> Tick {
        match self {
            SpeedMap::Constant(r) => Tick(scale_ticks(dt.0, *r)),
            SpeedMap::Keyframed { keys } => Tick(integrate_ramp(keys, dt.0)),
        }
    }

    /// Source-time delta without tick rounding.
    ///
    /// The frame renderer intentionally uses [`Self::source_delta`]; audio
    /// retiming uses this value to address individual PCM samples without
    /// accumulating a tick-rounding error per sample.
    pub fn source_delta_f64(&self, dt: Tick) -> f64 {
        match self {
            SpeedMap::Constant(r) => dt.0 as f64 * r.as_f64(),
            SpeedMap::Keyframed { keys } => integrate_ramp_f64(keys, dt.0),
        }
    }

    /// Instantaneous signed playback ratio at a clip-relative time.
    ///
    /// This complements [`Self::source_delta_f64`] for audio: a zero-rate
    /// interval deliberately emits silence rather than repeatedly sampling a
    /// DC value.
    pub fn ratio_at_f64(&self, at: Tick) -> f64 {
        match self {
            SpeedMap::Constant(ratio) => ratio.as_f64(),
            SpeedMap::Keyframed { keys } if keys.is_empty() => 1.0,
            SpeedMap::Keyframed { keys } => {
                let mut sorted = keys.clone();
                sorted.sort_by_key(|key| key.at.0);
                let index = sorted.partition_point(|key| key.at <= at);
                match index {
                    0 => sorted[0].ratio.as_f64(),
                    i if i == sorted.len() => sorted[i - 1].ratio.as_f64(),
                    i => segment_ratio(sorted[i - 1], sorted[i], at.0 as f64),
                }
            }
        }
    }

    /// Inclusive extrema of the source-time path over `0..=duration`.
    ///
    /// Reverse and zero-crossing ramps can visit source positions that are not
    /// visible from only the start and end deltas. The returned values are
    /// unrounded source ticks for prefetch and source-bound diagnostics.
    pub fn source_delta_range(&self, duration: Tick) -> (f64, f64) {
        let mut samples = vec![0_i64, duration.0.max(0)];
        if let SpeedMap::Keyframed { keys } = self {
            let mut sorted = keys.clone();
            sorted.sort_by_key(|k| k.at.0);
            for key in &sorted {
                if (0..=duration.0).contains(&key.at.0) {
                    samples.push(key.at.0);
                }
            }
            for pair in sorted.windows(2) {
                let lo = pair[0].at.0.max(0);
                let hi = pair[1].at.0.min(duration.0);
                if lo < hi && pair[0].ratio.num.signum() != pair[1].ratio.num.signum() {
                    samples.push(zero_crossing(pair[0], pair[1]));
                }
            }
        }
        samples.sort_unstable();
        samples.dedup();
        samples
            .into_iter()
            .map(|t| self.source_delta_f64(Tick(t)))
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), value| {
                (lo.min(value), hi.max(value))
            })
    }
}

fn zero_crossing(a: SpeedKey, b: SpeedKey) -> i64 {
    let mut lo = a.at.0 as f64;
    let mut hi = b.at.0 as f64;
    let target = 0.0;
    for _ in 0..48 {
        let mid = (lo + hi) * 0.5;
        let ratio = segment_ratio(a, b, mid);
        if (ratio - target).abs() < 1e-12 {
            return mid.round() as i64;
        }
        if ratio.signum() == a.ratio.as_f64().signum() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    ((lo + hi) * 0.5).round() as i64
}

fn segment_ratio(a: SpeedKey, b: SpeedKey, at: f64) -> f64 {
    let span = (b.at.0 - a.at.0) as f64;
    if span <= 0.0 || matches!(a.interp, Interp::Hold) {
        return a.ratio.as_f64();
    }
    let u = ((at - a.at.0 as f64) / span).clamp(0.0, 1.0);
    let ease = match a.interp {
        Interp::Hold => 0.0,
        Interp::Linear => u,
        Interp::Bezier {
            out_handle,
            in_handle,
        } => cubic_bezier_ease(out_handle, in_handle, u),
    };
    a.ratio.as_f64() + (b.ratio.as_f64() - a.ratio.as_f64()) * ease
}

/// `len * ratio`, exact (multiply-before-divide in i128, saturating back to i64).
#[inline]
fn scale_ticks(len: i64, r: Ratio) -> i64 {
    ((len as i128 * r.num as i128) / r.den.max(1) as i128) as i64
}

/// Integrate a speed ramp over `[0, target]` (or `[target, 0]` when
/// `target < 0`, negating the result). Each key's segment either holds its
/// ratio until the next key or eases toward the next key's ratio; the
/// first/last key's ratio holds before/after the ramp. An empty ramp is
/// identity (1×). Keys need not be pre-sorted. The all-holds ramp keeps the
/// classic exact i128 path; any eased segment routes to the `f64` path.
fn integrate_ramp(keys: &[SpeedKey], target: i64) -> i64 {
    if keys.is_empty() {
        return target;
    }
    if target == 0 {
        return 0;
    }
    let mut sorted = keys.to_vec();
    sorted.sort_by_key(|k| k.at.0);
    let n = sorted.len();
    // An eased segment needs both a non-hold easing AND a next key to ramp to.
    let has_ease = sorted
        .iter()
        .enumerate()
        .any(|(i, k)| i + 1 < n && !matches!(k.interp, Interp::Hold));
    if has_ease {
        integrate_eased(&sorted, target)
    } else {
        integrate_hold(&sorted, target)
    }
}

fn integrate_ramp_f64(keys: &[SpeedKey], target: i64) -> f64 {
    if keys.is_empty() {
        return target as f64;
    }
    if target == 0 {
        return 0.0;
    }
    let mut sorted = keys.to_vec();
    sorted.sort_by_key(|key| key.at.0);
    integrate_eased_f64(&sorted, target)
}

/// Exact piecewise-constant integration (the classic path): each key's ratio
/// holds until the next; the first/last holds before/after. `sorted` is sorted
/// by `at`. Exact integer arithmetic (i128 accumulate).
fn integrate_hold(sorted: &[SpeedKey], target: i64) -> i64 {
    let (lo, hi) = if target >= 0 {
        (0, target)
    } else {
        (target, 0)
    };
    let n = sorted.len();
    let mut acc: i128 = 0;
    for i in 0..n {
        // Segment `i` spans `[seg_start, seg_end)` at `sorted[i].ratio`.
        let seg_start = if i == 0 { i64::MIN } else { sorted[i].at.0 };
        let seg_end = if i + 1 < n {
            sorted[i + 1].at.0
        } else {
            i64::MAX
        };
        let a = seg_start.max(lo);
        let b = seg_end.min(hi);
        if b > a {
            let r = sorted[i].ratio;
            acc += (b - a) as i128 * r.num as i128 / r.den.max(1) as i128;
        }
    }
    if target < 0 {
        acc = -acc;
    }
    acc as i64
}

/// `f64` integration for a ramp with at least one eased segment. The speed
/// before the first key holds at the first ratio, after the last key holds at
/// the last ratio, and each inter-key segment either holds `r0` or ramps
/// `r0→r1` following its easing `e(u)` (`speed(t) = r0 + (r1−r0)·e(u)`), so
/// `∫ speed dt = (b−a)·r0 + (r1−r0)·w·∫ e du` over the clamped sub-interval.
/// Rounded to the nearest tick.
fn integrate_eased(sorted: &[SpeedKey], target: i64) -> i64 {
    integrate_eased_f64(sorted, target).round() as i64
}

fn integrate_eased_f64(sorted: &[SpeedKey], target: i64) -> f64 {
    let (lo, hi) = if target >= 0 {
        (0, target)
    } else {
        (target, 0)
    };
    let n = sorted.len();
    let mut acc = 0.0_f64;

    // Pre-first-key hold: the first key's ratio for `t < sorted[0].at`.
    let a = lo;
    let b = sorted[0].at.0.min(hi);
    if b > a {
        acc += (b - a) as f64 * sorted[0].ratio.as_f64();
    }

    // Inter-key segments `[sorted[i].at, sorted[i+1].at)`.
    for i in 0..n.saturating_sub(1) {
        let s0 = sorted[i].at.0;
        let s1 = sorted[i + 1].at.0;
        let a = s0.max(lo);
        let b = s1.min(hi);
        if b <= a {
            continue;
        }
        let r0 = sorted[i].ratio.as_f64();
        let w = (s1 - s0) as f64;
        // Hold segments (and degenerate coincident keys) stay constant at `r0`.
        if matches!(sorted[i].interp, Interp::Hold) || w <= 0.0 {
            acc += (b - a) as f64 * r0;
        } else {
            let r1 = sorted[i + 1].ratio.as_f64();
            let ua = (a - s0) as f64 / w;
            let ub = (b - s0) as f64 / w;
            let ie = integ_ease(&sorted[i].interp, ua, ub);
            acc += (b - a) as f64 * r0 + (r1 - r0) * w * ie;
        }
    }

    // Post-last-key hold.
    let a = sorted[n - 1].at.0.max(lo);
    if hi > a {
        acc += (hi - a) as f64 * sorted[n - 1].ratio.as_f64();
    }

    if target < 0 {
        -acc
    } else {
        acc
    }
}

/// Definite integral `∫_{ua}^{ub} e(u) du` of the normalized easing progress
/// `e(u)` for a ramp segment, with `0 ≤ ua ≤ ub ≤ 1`. `Linear` is closed-form;
/// `Bezier` uses deterministic composite Simpson sampling of
/// [`cubic_bezier_ease`] (the ease is smooth and monotone, and the caller
/// tick-rounds the result). `Hold` never reaches here (its segment is handled
/// as a constant by [`integrate_eased`]).
fn integ_ease(interp: &Interp, ua: f64, ub: f64) -> f64 {
    match interp {
        Interp::Hold => 0.0,
        Interp::Linear => (ub * ub - ua * ua) * 0.5,
        Interp::Bezier {
            out_handle,
            in_handle,
        } => {
            const N: usize = 64; // even → composite Simpson
            let h = (ub - ua) / N as f64;
            if h == 0.0 {
                return 0.0;
            }
            let mut sum = cubic_bezier_ease(*out_handle, *in_handle, ua)
                + cubic_bezier_ease(*out_handle, *in_handle, ub);
            for k in 1..N {
                let u = ua + k as f64 * h;
                let weight = if k % 2 == 1 { 4.0 } else { 2.0 };
                sum += weight * cubic_bezier_ease(*out_handle, *in_handle, u);
            }
            sum * h / 3.0
        }
    }
}

/// Animatable clip transform (01 §5/§6). Field names match the `prop_registry`
/// `transform.*` paths.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorSpace {
    /// Anchor coordinates are absolute output-frame pixels (legacy v3 files).
    Absolute,
    /// Anchor coordinates are offsets from the output-frame center.
    #[default]
    CenterOffset,
}

impl AnchorSpace {
    fn is_center_offset(&self) -> bool {
        *self == Self::CenterOffset
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipTransform {
    pub x: f64,
    pub y: f64,
    pub scale_x: f64,
    pub scale_y: f64,
    pub rotation: f64,
    #[serde(default, skip_serializing_if = "AnchorSpace::is_center_offset")]
    pub anchor_space: AnchorSpace,
    pub anchor_x: f64,
    pub anchor_y: f64,
    pub opacity: f64,
}

impl Default for ClipTransform {
    fn default() -> Self {
        ClipTransform {
            x: 0.0,
            y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation: 0.0,
            anchor_space: AnchorSpace::CenterOffset,
            anchor_x: 0.0,
            anchor_y: 0.0,
            opacity: 1.0,
        }
    }
}

impl PropSet for ClipTransform {
    const TARGET_KIND: PropTargetKind = PropTargetKind::ClipTransform;
}

/// One effect in a clip's ordered stack.
///
/// `id`/`version` are the data-driven manifest identity (spec §10). They are
/// additive to the v4 shape: absent in old files (defaulting to the empty
/// sentinel / 0), they are backfilled from `kind` in
/// [`finalize_load`](super::load::finalize_load). `kind` is retained as the
/// projection of `id` for the legacy dispatch paths; it is removed after one
/// format version.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipEffect {
    pub kind: EffectKind,
    /// Stable manifest id. Empty (`EffectId::EMPTY`) in v4 files; backfilled from
    /// `kind` on load. An id with no manifest loads inert-and-preserved (§2.6).
    #[serde(
        default = "ClipEffect::default_id",
        skip_serializing_if = "EffectId::is_empty"
    )]
    pub id: EffectId,
    /// Manifest schema version this effect's params conform to. 0 in v4 files;
    /// backfilled to the manifest's current version on load.
    #[serde(
        default = "ClipEffect::default_version",
        skip_serializing_if = "is_zero_u16"
    )]
    pub version: u16,
    #[serde(default = "super::grade::default_true")]
    pub enabled: bool,
    /// Set on load for an effect whose `id` this build has no manifest for: its
    /// `params` are preserved untouched and it is skipped by the compiler, the
    /// same way a disabled effect is (§2.6).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inert: bool,
    /// K-B3 effect zone: half-open range `[start, end)` in the **same domain
    /// as keyframe evaluation** for the stack this effect sits on — clip-
    /// relative ticks for clip/asset stacks (`dt = tick − clip.start`),
    /// sequence-relative for track/master. `None` = whole span (default).
    /// Additive; older files load without a zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<(Tick, Tick)>,
    pub params: AnimProps<EffectParams>,
}

impl ClipEffect {
    /// An effect seeded with its kind's default params (legacy neutral seed).
    pub fn new(kind: EffectKind) -> Self {
        ClipEffect {
            kind,
            id: EffectId::EMPTY,
            version: 0,
            enabled: true,
            inert: false,
            zone: None,
            params: AnimProps::new(EffectParams::seed(kind.target_kind())),
        }
    }

    /// An effect seeded from a manifest's explicit [`ParamSpec::default`]s.
    /// `None` if this build has no manifest for `id`. Unlike [`Self::new`]
    /// (neutral seed), this uses the manifest's declared defaults — which agree
    /// with the neutral seed for the seven v1 kinds (proven by test).
    pub fn from_manifest(id: EffectId) -> Option<Self> {
        use super::anim::PropPath;
        let m = super::effect_manifest::manifest(id.clone())?;
        let mut params = EffectParams::new();
        for spec in m.params {
            params.set(PropPath::new(spec.path), spec.default);
        }
        let kind = id
            .legacy_kind()
            .unwrap_or_else(|| EffectKind::Unknown(UnknownTag::intern(id.as_str())));
        Some(ClipEffect {
            kind,
            id: m.id.clone(),
            version: m.version,
            enabled: true,
            inert: false,
            zone: None,
            params: AnimProps::new(params),
        })
    }

    /// Whether this effect is active at evaluation domain tick `dt` (K-B3).
    /// Whole-span (`zone: None`) and disabled checks are the caller's job for
    /// `enabled`/`inert`; this only answers the zone half-open range.
    #[inline]
    pub fn active_at(&self, dt: Tick) -> bool {
        match self.zone {
            None => true,
            Some((a, b)) => dt >= a && dt < b,
        }
    }

    fn default_id() -> EffectId {
        EffectId::EMPTY
    }

    fn default_version() -> u16 {
        0
    }
}

fn is_zero_u16(v: &u16) -> bool {
    *v == 0
}

/// A clip-level transition (01 §5, catalog in 08 §2.0b).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub kind: TransitionKind,
    pub duration: Tick,
    #[serde(default)]
    pub params: TransitionParams,
}

impl Transition {
    pub fn new(kind: TransitionKind, duration: Tick) -> Self {
        Transition {
            kind,
            duration,
            params: TransitionParams::default(),
        }
    }
}

/// v1 transition catalog (08 §2.0b). Additive-only.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TransitionKind {
    CrossDissolve,
    DipToBlack,
    DipToColor,
    Wipe,
    Push,
    /// Analytical luma-map wipe (26 K-B7): Photonic-authored switch maps, not
    /// bundled image assets. Map family / invert / softness live on
    /// [`TransitionParams`].
    LumaWipe,
    /// Forward-compat (39 §2.2): a variant this build does not know. The
    /// original serialized tag is preserved verbatim and re-emitted on save.
    /// An unknown transition renders as a hard cut (never a guessed dissolve).
    /// Declared last so serde tries the known snake_case tags first.
    #[serde(untagged)]
    Unknown(UnknownTag),
}

/// Built-in luma wipe map families (26 K-B7). Clean-room analytical generators
/// — runtime synthesis, no bundled GPL maps. Softness / invert ride
/// [`TransitionParams`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LumaWipeMap {
    /// Left-to-right linear bar (black first).
    #[default]
    LinearH,
    /// Top-to-bottom linear bar.
    LinearV,
    /// Radial iris from centre.
    Radial,
    /// Barn-door: opens from the centre outward horizontally.
    BarnDoorH,
    /// Clock sweep (angle from +x, clockwise).
    Clock,
}

impl TransitionKind {
    /// The preserved tag if this is an unknown (forward-compat) variant.
    pub fn unknown_tag(self) -> Option<UnknownTag> {
        match self {
            TransitionKind::Unknown(t) => Some(t),
            _ => None,
        }
    }

    /// True if this is a forward-compat variant this build does not understand.
    pub fn is_unknown(self) -> bool {
        matches!(self, TransitionKind::Unknown(_))
    }
}

/// Transition parameters (union across the catalog; only the fields relevant to
/// a given `kind` are meaningful).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransitionParams {
    /// Easing over the overlap window `t∈0..1`.
    #[serde(default)]
    pub curve: EaseCurve,
    /// Through-color for `DipToColor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
    /// Direction for `Wipe`/`Push`.
    #[serde(default)]
    pub direction: WipeDirection,
    /// Edge softness for `Wipe` / `LumaWipe` (map units, 0..0.5).
    #[serde(default)]
    pub softness: f32,
    /// Built-in map family for `LumaWipe` (26 K-B7).
    #[serde(default)]
    pub luma_map: LumaWipeMap,
    /// Invert the luma map so white switches first.
    #[serde(default)]
    pub invert: bool,
}

impl Default for TransitionParams {
    fn default() -> Self {
        TransitionParams {
            curve: EaseCurve::default(),
            color: None,
            direction: WipeDirection::default(),
            softness: 0.0,
            luma_map: LumaWipeMap::default(),
            invert: false,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EaseCurve {
    Linear,
    EaseIn,
    EaseOut,
    #[default]
    EaseInOut,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WipeDirection {
    #[default]
    Left,
    Right,
    Up,
    Down,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_transform_defaults_to_center_offset_and_round_trips() {
        let transform = ClipTransform::default();
        assert_eq!(transform.anchor_space, AnchorSpace::CenterOffset);

        let json = serde_json::to_string(&transform).unwrap();
        assert!(!json.contains("anchor_space"));
        assert_eq!(
            serde_json::from_str::<ClipTransform>(&json).unwrap(),
            transform
        );

        let legacy = ClipTransform {
            anchor_space: AnchorSpace::Absolute,
            anchor_x: 12.0,
            ..transform
        };
        let legacy_json = serde_json::to_string(&legacy).unwrap();
        assert!(legacy_json.contains("\"anchor_space\":\"absolute\""));
        assert_eq!(
            serde_json::from_str::<ClipTransform>(&legacy_json).unwrap(),
            legacy
        );
    }

    #[test]
    fn clip_end_and_overlap() {
        let c = Clip::new(ClipSource::Adjustment, Tick(100), Tick(50));
        assert_eq!(c.end(), Tick(150));
        assert!(c.overlaps(Tick(120), Tick(130)));
        assert!(!c.overlaps(Tick(150), Tick(200)));
        assert!(!c.overlaps(Tick(0), Tick(100)));
    }

    #[test]
    fn speed_source_delta_is_exact() {
        // 2× speed: 100 ticks of timeline maps to 200 ticks of source.
        let s = SpeedMap::Constant(Ratio::new(2, 1));
        assert_eq!(s.source_delta(Tick(100)), Tick(200));
        // Half speed.
        let h = SpeedMap::Constant(Ratio::new(1, 2));
        assert_eq!(h.source_delta(Tick(100)), Tick(50));
        // Reverse.
        let r = SpeedMap::Constant(Ratio::new(-1, 1));
        assert_eq!(r.source_delta(Tick(100)), Tick(-100));
    }

    // ── Speed ramps (G-11) ───────────────────────────────────────────────

    #[test]
    fn speed_map_default_is_constant_one() {
        assert_eq!(SpeedMap::default(), SpeedMap::Constant(Ratio::ONE));
    }

    #[test]
    fn speed_map_constant_serde_tag_unchanged() {
        // Additive discipline: `Constant` still serializes under the `constant`
        // tag with the flattened ratio and no ramp field, so pre-existing saved
        // clips load byte-shape-identically.
        let j = serde_json::to_string(&SpeedMap::Constant(Ratio::new(2, 1))).unwrap();
        assert!(j.contains("\"speed\":\"constant\""));
        assert!(!j.contains("keys"));
        let back: SpeedMap = serde_json::from_str(&j).unwrap();
        assert_eq!(back, SpeedMap::Constant(Ratio::new(2, 1)));
    }

    #[test]
    fn speed_map_keyframed_serde_roundtrip() {
        let ramp = SpeedMap::Keyframed {
            keys: vec![
                SpeedKey::new(Tick(0), Ratio::new(1, 2)),
                SpeedKey::new(Tick(50), Ratio::new(2, 1)),
                SpeedKey::new(Tick(120), Ratio::new(-1, 1)),
            ],
        };
        let j = serde_json::to_string(&ramp).unwrap();
        assert!(j.contains("\"speed\":\"keyframed\""));
        let back: SpeedMap = serde_json::from_str(&j).unwrap();
        assert_eq!(ramp, back);
    }

    #[test]
    fn speed_ramp_source_time_mapping() {
        // A 2× ramp doubles the source advance (100 timeline → 200 source),
        // matching the constant case; a ½× ramp halves it (100 → 50).
        let fast = SpeedMap::Keyframed {
            keys: vec![SpeedKey::new(Tick(0), Ratio::new(2, 1))],
        };
        assert_eq!(fast.source_delta(Tick(100)), Tick(200));
        let slow = SpeedMap::Keyframed {
            keys: vec![SpeedKey::new(Tick(0), Ratio::new(1, 2))],
        };
        assert_eq!(slow.source_delta(Tick(100)), Tick(50));

        // A single-key ramp is exactly equivalent to the matching constant.
        for dt in [Tick(0), Tick(37), Tick(100), Tick(-100)] {
            assert_eq!(
                fast.source_delta(dt),
                SpeedMap::Constant(Ratio::new(2, 1)).source_delta(dt)
            );
        }

        // A two-segment ramp integrates piecewise: 1× over [0,50) = 50 source,
        // then 2× over [50,100) = 100 source ⇒ 150 total; 50 at the split.
        let ramp = SpeedMap::Keyframed {
            keys: vec![
                SpeedKey::new(Tick(0), Ratio::new(1, 1)),
                SpeedKey::new(Tick(50), Ratio::new(2, 1)),
            ],
        };
        assert_eq!(ramp.source_delta(Tick(50)), Tick(50));
        assert_eq!(ramp.source_delta(Tick(100)), Tick(150));

        // Before the first key the first key's ratio holds (reverse scrub).
        assert_eq!(ramp.source_delta(Tick(-20)), Tick(-20));
    }

    #[test]
    fn speed_ramp_empty_is_identity() {
        let empty = SpeedMap::Keyframed { keys: vec![] };
        assert_eq!(empty.source_delta(Tick(100)), Tick(100));
    }

    #[test]
    fn speed_ramp_integrates_regardless_of_key_order() {
        let sorted = SpeedMap::Keyframed {
            keys: vec![
                SpeedKey::new(Tick(0), Ratio::new(1, 1)),
                SpeedKey::new(Tick(50), Ratio::new(2, 1)),
            ],
        };
        let shuffled = SpeedMap::Keyframed {
            keys: vec![
                SpeedKey::new(Tick(50), Ratio::new(2, 1)),
                SpeedKey::new(Tick(0), Ratio::new(1, 1)),
            ],
        };
        assert_eq!(
            sorted.source_delta(Tick(100)),
            shuffled.source_delta(Tick(100))
        );
    }

    // ── Eased speed ramps (G-11 bezier) ──────────────────────────────────

    #[test]
    fn speed_key_hold_serde_omits_interp() {
        // A Hold key (the default) must not emit an `interp` field, so existing
        // keyframed documents stay byte-shape-identical and load unchanged.
        let j = serde_json::to_string(&SpeedKey::new(Tick(0), Ratio::new(1, 2))).unwrap();
        assert!(!j.contains("interp"));
        let back: SpeedKey = serde_json::from_str(&j).unwrap();
        assert_eq!(back.interp, Interp::Hold);
    }

    #[test]
    fn speed_key_eased_serde_roundtrip() {
        let key = SpeedKey::eased(
            Tick(10),
            Ratio::new(3, 1),
            Interp::Bezier {
                out_handle: [0.42, 0.0],
                in_handle: [0.58, 1.0],
            },
        );
        let j = serde_json::to_string(&key).unwrap();
        assert!(j.contains("interp"));
        assert!(j.contains("bezier"));
        let back: SpeedKey = serde_json::from_str(&j).unwrap();
        assert_eq!(back, key);
    }

    #[test]
    fn speed_ramp_linear_ease_averages_endpoints() {
        // A linear speed ramp from 1× to 3× over [0,100): average speed is 2×,
        // so 100 timeline ticks map to 200 source ticks; the first half [0,50]
        // ramps 1×→2× (avg 1.5×) → 75 source ticks.
        let ramp = SpeedMap::Keyframed {
            keys: vec![
                SpeedKey::eased(Tick(0), Ratio::new(1, 1), Interp::Linear),
                SpeedKey::new(Tick(100), Ratio::new(3, 1)),
            ],
        };
        assert_eq!(ramp.source_delta(Tick(100)), Tick(200));
        assert_eq!(ramp.source_delta(Tick(50)), Tick(75));
        // Before the first key the speed holds at 1×; after the last, at 3×.
        assert_eq!(ramp.source_delta(Tick(-10)), Tick(-10));
        assert_eq!(ramp.source_delta(Tick(120)), Tick(260));
    }

    #[test]
    fn speed_ramp_bezier_identity_matches_linear() {
        // cubic-bezier(0,0,1,1) is the identity easing → equals a Linear ramp
        // exactly (Simpson integrates the linear integrand exactly).
        let ident = Interp::Bezier {
            out_handle: [0.0, 0.0],
            in_handle: [1.0, 1.0],
        };
        let bez = SpeedMap::Keyframed {
            keys: vec![
                SpeedKey::eased(Tick(0), Ratio::new(1, 1), ident),
                SpeedKey::new(Tick(100), Ratio::new(3, 1)),
            ],
        };
        assert_eq!(bez.source_delta(Tick(100)), Tick(200));
        assert_eq!(bez.source_delta(Tick(50)), Tick(75));
    }

    #[test]
    fn speed_ramp_bezier_symmetric_ease_preserves_total() {
        // A symmetric ease-in-out (cubic-bezier(0.42,0,0.58,1)) integrates to
        // 0.5 over [0,1] by symmetry, so the total source advance over the full
        // ramp equals the linear/average case (200) even though the
        // instantaneous speed eases in and out.
        let ease = Interp::Bezier {
            out_handle: [0.42, 0.0],
            in_handle: [0.58, 1.0],
        };
        let ramp = SpeedMap::Keyframed {
            keys: vec![
                SpeedKey::eased(Tick(0), Ratio::new(1, 1), ease),
                SpeedKey::new(Tick(100), Ratio::new(3, 1)),
            ],
        };
        let d = ramp.source_delta(Tick(100)).0;
        assert!((d - 200).abs() <= 1, "expected ~200 source ticks, got {d}");
    }

    #[test]
    fn speed_ramp_all_hold_stays_on_exact_path() {
        // A ramp whose keys are all Hold must integrate identically to the
        // classic exact path (no float rounding) — flipping an eased key back
        // to Hold restores byte-exact behavior.
        let exact = SpeedMap::Keyframed {
            keys: vec![
                SpeedKey::new(Tick(0), Ratio::new(1, 1)),
                SpeedKey::new(Tick(50), Ratio::new(2, 1)),
            ],
        };
        assert_eq!(exact.source_delta(Tick(100)), Tick(150));
    }

    // ── Text clips (G-12) ────────────────────────────────────────────────

    #[test]
    fn text_clip_content_new_uses_default_style() {
        let c = TextClipContent::new("Title");
        assert_eq!(c.text, "Title");
        assert_eq!(c.style, CaptionStyle::default());
    }

    #[test]
    fn clip_source_text_serde_roundtrip() {
        let mut content = TextClipContent::new("Chapter One");
        content.style.font_size = 96.0;
        let clip = Clip::new(
            ClipSource::Text {
                content: content.clone(),
            },
            Tick(0),
            Tick(300),
        );
        let j = serde_json::to_string(&clip).unwrap();
        assert!(j.contains("\"source\":\"text\""));
        let back: Clip = serde_json::from_str(&j).unwrap();
        assert_eq!(clip, back);
        assert!(
            matches!(back.source, ClipSource::Text { content } if content.text == "Chapter One")
        );
    }

    #[test]
    fn clip_roundtrip_serde() {
        let mut c = Clip::new(
            ClipSource::Asset {
                asset: AssetId::new(),
            },
            Tick(0),
            Tick(100),
        );
        c.effects.push(ClipEffect::new(EffectKind::Blur));
        c.transition_in = Some(Transition::new(TransitionKind::CrossDissolve, Tick(20)));
        c.color_label = Some(3);
        c.link_group = Some(LinkGroupId::new());
        c.reframe.insert(1, ClipTransform::default());
        let j = serde_json::to_string(&c).unwrap();
        let back: Clip = serde_json::from_str(&j).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn clip_color_label_and_link_group_default_to_none() {
        let c = Clip::new(ClipSource::Adjustment, Tick(0), Tick(10));
        assert_eq!(c.color_label, None);
        assert_eq!(c.link_group, None);
    }

    #[test]
    fn clip_color_label_and_link_group_absent_from_json_when_unset() {
        // Additive-field discipline: an unset optional field must not appear
        // in the serialized form, so pre-existing saved documents that never
        // had these fields still round-trip byte-for-byte-equivalent shape.
        let c = Clip::new(ClipSource::Adjustment, Tick(0), Tick(10));
        let j = serde_json::to_string(&c).unwrap();
        assert!(!j.contains("color_label"));
        assert!(!j.contains("link_group"));
    }

    #[test]
    fn clip_markers_absent_from_json_when_empty() {
        // Additive discipline: a clip with no markers omits the key, so
        // pre-migration clips stay shape-identical.
        let c = Clip::new(ClipSource::Adjustment, Tick(0), Tick(10));
        assert!(c.markers.is_empty());
        assert_eq!(c.group, None);
        let j = serde_json::to_string(&c).unwrap();
        assert!(!j.contains("markers"));
        assert!(!j.contains("\"group\""));
    }

    #[test]
    fn clip_marker_is_clip_relative() {
        // A clip marker's `at` is clip-relative; the sequence position is
        // `clip.start + m.at` (35 §1).
        let c = Clip::new(ClipSource::Adjustment, Tick(500), Tick(100));
        let m = Marker::clip_scoped(Tick(10), "cm");
        assert_eq!(c.marker_sequence_tick(&m), Tick(510));
    }

    #[test]
    fn link_group_id_serde_is_transparent_uuid() {
        let id = LinkGroupId::new();
        let j = serde_json::to_string(&id).unwrap();
        let as_uuid: Uuid = serde_json::from_str(&j).unwrap();
        assert_eq!(as_uuid, id.0);
        let back: LinkGroupId = serde_json::from_str(&j).unwrap();
        assert_eq!(back, id);
    }

    // ── Multicam (G-20) ──────────────────────────────────────────────────

    #[test]
    fn multicam_group_serde_roundtrip() {
        let group = MulticamGroup {
            angles: vec![
                MulticamAngle::new(
                    "Cam A",
                    ClipSource::Asset {
                        asset: AssetId::new(),
                    },
                    Tick(0),
                ),
                MulticamAngle::new(
                    "Cam B",
                    ClipSource::Asset {
                        asset: AssetId::new(),
                    },
                    Tick(30),
                ),
            ],
            active: 1,
        };
        let j = serde_json::to_string(&group).unwrap();
        let back: MulticamGroup = serde_json::from_str(&j).unwrap();
        assert_eq!(group, back);
        assert_eq!(back.active_angle().unwrap().name, "Cam B");
    }

    #[test]
    fn clip_multicam_defaults_none_and_absent_from_json() {
        let c = Clip::new(ClipSource::Adjustment, Tick(0), Tick(10));
        assert_eq!(c.multicam, None);
        let j = serde_json::to_string(&c).unwrap();
        assert!(!j.contains("multicam"));
    }

    #[test]
    fn clip_with_multicam_serde_roundtrip() {
        let mut c = Clip::new(
            ClipSource::Asset {
                asset: AssetId::new(),
            },
            Tick(0),
            Tick(100),
        );
        c.multicam = Some(MulticamGroup {
            angles: vec![MulticamAngle::new(
                "A",
                ClipSource::Asset {
                    asset: AssetId::new(),
                },
                Tick(0),
            )],
            active: 0,
        });
        let j = serde_json::to_string(&c).unwrap();
        let back: Clip = serde_json::from_str(&j).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn transition_kind_unknown_preserves_tag() {
        let k: TransitionKind = serde_json::from_str("\"iris_wipe\"").unwrap();
        assert!(k.is_unknown());
        assert_eq!(k.unknown_tag().unwrap().as_str(), "iris_wipe");
        assert_eq!(serde_json::to_string(&k).unwrap(), "\"iris_wipe\"");
        // Known variants still resolve, not shadowed by the untagged fallback.
        for (k, tag) in [
            (TransitionKind::CrossDissolve, "\"cross_dissolve\""),
            (TransitionKind::DipToBlack, "\"dip_to_black\""),
            (TransitionKind::DipToColor, "\"dip_to_color\""),
            (TransitionKind::Wipe, "\"wipe\""),
            (TransitionKind::Push, "\"push\""),
            (TransitionKind::LumaWipe, "\"luma_wipe\""),
        ] {
            assert_eq!(serde_json::to_string(&k).unwrap(), tag);
            let back: TransitionKind = serde_json::from_str(tag).unwrap();
            assert_eq!(back, k);
            assert!(!back.is_unknown());
        }
    }

    #[test]
    fn clip_source_unknown_preserves_payload() {
        let raw = r#"{"source":"holo_gen","seed":7,"nested":{"a":[1,2]}}"#;
        let src: ClipSource = serde_json::from_str(raw).unwrap();
        assert!(src.is_unknown());
        assert_eq!(src.unknown_tag(), Some("holo_gen"));
        assert_eq!(
            src.asset(),
            None,
            "unknown source references no known asset"
        );
        // The whole object round-trips value-equal to the input.
        let back: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&src).unwrap()).unwrap();
        let orig: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(back, orig);

        // Known source tags still resolve to their concrete variant.
        let asset: ClipSource = serde_json::from_str(r#"{"source":"adjustment"}"#).unwrap();
        assert!(!asset.is_unknown());
        assert!(matches!(asset, ClipSource::Adjustment));
    }

    #[test]
    fn clip_source_malformed_known_falls_to_unknown_with_known_tag() {
        // serde's per-variant untagged fallback is greedy: a KNOWN tag with a
        // malformed field degrades to Unknown at the serde layer (verified
        // empirically). The document-level integrity guard therefore lives in
        // `load::finalize_load`, which rejects a retained Unknown whose tag is
        // a KNOWN catalog tag (see `load::KNOWN_CLIP_SOURCE_TAGS`). Here we only
        // pin the serde-layer behaviour so the load guard has a defined input.
        let bad = r#"{"source":"solid_color","color":"not-a-color"}"#;
        let src: ClipSource = serde_json::from_str(bad).unwrap();
        assert!(src.is_unknown());
        assert_eq!(src.unknown_tag(), Some("solid_color"));
    }

    #[test]
    fn effect_zone_active_at_is_half_open() {
        let mut fx = ClipEffect::new(EffectKind::Blur);
        assert!(fx.active_at(Tick(0)));
        assert!(fx.active_at(Tick(9999)));
        fx.zone = Some((Tick(10), Tick(20)));
        assert!(!fx.active_at(Tick(9)));
        assert!(fx.active_at(Tick(10)));
        assert!(fx.active_at(Tick(19)));
        assert!(!fx.active_at(Tick(20)));
    }

    #[test]
    fn effect_zone_serde_omits_none_and_roundtrips_some() {
        let plain = ClipEffect::new(EffectKind::Blur);
        let j = serde_json::to_string(&plain).unwrap();
        assert!(!j.contains("zone"), "default None must not serialize: {j}");
        let mut zoned = plain.clone();
        zoned.zone = Some((Tick(0), Tick(50)));
        let j2 = serde_json::to_string(&zoned).unwrap();
        assert!(j2.contains("zone"), "Some zone must serialize: {j2}");
        let back: ClipEffect = serde_json::from_str(&j2).unwrap();
        assert_eq!(back.zone, Some((Tick(0), Tick(50))));
        // Re-load a serialized plain effect (no zone key) → None.
        let legacy: ClipEffect = serde_json::from_str(&j).unwrap();
        assert_eq!(legacy.zone, None);
    }

    #[test]
    fn speed_map_validation_rejects_duplicate_key_ticks() {
        let map = SpeedMap::Keyframed {
            keys: vec![
                SpeedKey::new(Tick(10), Ratio::ONE),
                SpeedKey::new(Tick(10), Ratio::new(2, 1)),
            ],
        };
        assert_eq!(
            map.validate_for_duration(Tick(100)),
            Err(SpeedMapError::DuplicateKeyTick)
        );
    }

    #[test]
    fn speed_map_validation_rejects_constant_zero_denominator() {
        let map = SpeedMap::Constant(Ratio { num: 1, den: 0 });
        assert_eq!(
            map.validate_for_duration(Tick(100)),
            Err(SpeedMapError::ZeroDenominator)
        );
    }

    #[test]
    fn speed_map_range_includes_reverse_turnaround() {
        let map = SpeedMap::Keyframed {
            keys: vec![
                SpeedKey::eased(Tick::ZERO, Ratio::new(1, 1), Interp::Linear),
                SpeedKey::new(Tick(100), Ratio::new(-1, 1)),
            ],
        };
        let (_, high) = map.source_delta_range(Tick(100));
        assert!(
            (high - 25.0).abs() < 1.0,
            "turnaround should be near 25 ticks: {high}"
        );
    }
}
