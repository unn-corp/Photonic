//! Exposure-drift stabilization ("deflicker") — measure-then-correct.
//!
//! This is the video analogue of loudness-on-export ([`analysis::analyze_loudness`]):
//! a **job, not a node** (32 §2's two-pass rule). One pass measures a per-frame
//! exposure curve over the whole clip and caches it by content hash; the
//! per-frame effect then reads the cached verdict and applies a scalar gain. No
//! temporal frame fetch is required, so the E-1 source-range contract
//! ([`super::source_range`]) stays at identity and the evaluator remains
//! stateless, order-independent and seek-correct.
//!
//! # Why the correction is solved in the perceptual domain
//!
//! [`Image`] pixels are premultiplied **linear**, but the correction is measured
//! and applied on sRGB-encoded values. That is deliberate, not a shortcut: the
//! objective is to minimise *perceptually visible* residual flicker, and a
//! difference that is negligible in linear light is not negligible on screen.
//! RE:Vision's DE:Flicker manual makes the same call for the same reason — their
//! OFX `Gamma Space` control defaults to *Delinearize*, i.e. it un-linearizes
//! before correcting. Correcting in linear light instead makes the residual
//! error non-uniform across the tone curve, which reads as the shadows still
//! breathing after the midtones have been flattened.
//!
//! # Why a median baseline rather than a moving average
//!
//! A long moving average smears a genuine brightness *step* — walk into a
//! sunlit room and the mean ramps up before you arrive and stays high after,
//! a visible halo on both sides of the cut-in. A running median holds the step
//! while still rejecting oscillation, which is what lets the window be long
//! enough to catch slow (0.05–0.2 Hz) auto-exposure drift.
//!
//! # Why the gain is clamped
//!
//! The correction must remove the camera's *hunting* around a correct exposure
//! without fighting genuine changes in scene brightness. The clamp is what
//! separates the two: oscillation lives well inside it, a real lighting change
//! saturates it and passes through largely untouched.

use std::collections::HashMap;

use photonic_core::timeline::{ClipId, Tick};

use super::ops::{linear_to_srgb, srgb_to_linear, Image};

/// Rec.709 luma weights, matching [`super::ops::luma709`].
const LUMA_R: f32 = 0.2126;
const LUMA_G: f32 = 0.7152;
const LUMA_B: f32 = 0.0722;

/// Encoded value above which the applied gain tapers back toward 1.0.
///
/// Brightening an already-clipped highlight cannot recover detail — it just
/// produces a flat, discoloured patch where the channels clip at different
/// points. Rolling the gain off near white keeps `gain > 1` from pushing
/// near-white pixels over the edge. Below the knee the gain is untouched, so
/// this cannot affect a correction that is not near clipping.
pub(crate) const HIGHLIGHT_KNEE: f32 = 0.75;

/// Per-frame, per-channel exposure measurement for one contiguous frame range.
///
/// Values are means of **sRGB-encoded, unpremultiplied** channels in `0..=1`, so
/// they are directly comparable to what a viewer perceives (see module docs).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ExposureCurve {
    /// One `[r, g, b]` mean per frame, in frame order.
    pub samples: Vec<[f32; 3]>,
}

impl ExposureCurve {
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// Tuning for [`solve_gains`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct DeflickerParams {
    /// Median-baseline window, in frames. Must be ≥ 1; even values are rounded
    /// up to odd so the window is symmetric about the sample.
    pub window_frames: usize,
    /// Post-median smoothing width, in frames, that removes the median's
    /// staircase. Must be ≥ 1.
    pub smooth_frames: usize,
    /// Maximum correction as a fraction, e.g. `0.25` allows ±25 %. Genuine
    /// lighting changes saturate this and survive; hunting does not reach it.
    pub max_change: f32,
    /// Iterative refinement passes. The baseline is recomputed from the
    /// corrected curve each pass, converging on a fixed point (the
    /// idempotency property `lapsify` gets right and most implementations
    /// do not). Two passes captures essentially all of the available gain.
    pub passes: u8,
    /// `0.0` applies one shared gain to all channels (exposure only); `1.0`
    /// gives each channel its own gain, which also stabilises white-balance
    /// drift. RE:Vision expose the same dial as *Max Chroma Change %*.
    pub chroma_amount: f32,
    /// Overall strength, `0.0..=1.0`. Blends the solved gain toward unity, the
    /// role van Roosmalen's forgetting factor γ plays (their recommended 0.85
    /// is this crate's default).
    pub amount: f32,
}

impl Default for DeflickerParams {
    fn default() -> Self {
        Self {
            window_frames: 121,
            smooth_frames: 15,
            max_change: 0.25,
            passes: 2,
            chroma_amount: 0.0,
            amount: 0.85,
        }
    }
}

impl DeflickerParams {
    /// Clamp every field into its supported range. Callers hand us animatable
    /// user params, so this is the single place that decides what a nonsense
    /// value means rather than letting it reach the arithmetic.
    pub fn sanitized(mut self) -> Self {
        self.window_frames = self.window_frames.max(1) | 1;
        self.smooth_frames = self.smooth_frames.max(1) | 1;
        self.max_change = self.max_change.clamp(0.0, 0.9);
        self.passes = self.passes.clamp(1, 8);
        self.chroma_amount = self.chroma_amount.clamp(0.0, 1.0);
        self.amount = self.amount.clamp(0.0, 1.0);
        self
    }
}

/// Mean sRGB-encoded, unpremultiplied `[r, g, b]` of `img` — one sample of an
/// [`ExposureCurve`].
///
/// Fully-transparent pixels carry no colour and are excluded; a frame that is
/// entirely transparent measures as mid-grey, which yields a unit gain rather
/// than a division by zero.
pub fn measure_frame(img: &Image) -> [f32; 3] {
    let mut sum = [0.0f64; 3];
    let mut n = 0u64;
    for px in &img.pixels {
        let a = px[3];
        if a <= 1e-6 {
            continue;
        }
        let inv = 1.0 / a;
        sum[0] += linear_to_srgb((px[0] * inv).clamp(0.0, 1.0)) as f64;
        sum[1] += linear_to_srgb((px[1] * inv).clamp(0.0, 1.0)) as f64;
        sum[2] += linear_to_srgb((px[2] * inv).clamp(0.0, 1.0)) as f64;
        n += 1;
    }
    if n == 0 {
        return [0.5; 3];
    }
    let inv = 1.0 / n as f64;
    [
        (sum[0] * inv) as f32,
        (sum[1] * inv) as f32,
        (sum[2] * inv) as f32,
    ]
}

/// Running median of `v` over a centred, edge-clamped window of `window`
/// samples.
fn running_median(v: &[f32], window: usize) -> Vec<f32> {
    let n = v.len();
    if n == 0 {
        return Vec::new();
    }
    let half = window / 2;
    let mut out = Vec::with_capacity(n);
    let mut scratch: Vec<f32> = Vec::with_capacity(window);
    for i in 0..n {
        scratch.clear();
        for k in 0..window {
            // Edge-clamp rather than shrink the window: a shrinking window makes
            // the correction strength visibly change over the first and last
            // `half` frames of every clip.
            let idx = (i + k).saturating_sub(half).min(n - 1);
            scratch.push(v[idx]);
        }
        scratch.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        out.push(scratch[scratch.len() / 2]);
    }
    out
}

/// Centred, edge-clamped moving average of `v` over `window` samples.
fn moving_average(v: &[f32], window: usize) -> Vec<f32> {
    let n = v.len();
    if n == 0 {
        return Vec::new();
    }
    let half = window / 2;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let mut sum = 0.0f64;
        for k in 0..window {
            let idx = (i + k).saturating_sub(half).min(n - 1);
            sum += v[idx] as f64;
        }
        out.push((sum / window as f64) as f32);
    }
    out
}

/// Median baseline: a running median (step-preserving) followed by a short mean
/// that removes the median's staircase without reintroducing step-smearing.
fn baseline(v: &[f32], window: usize, smooth: usize) -> Vec<f32> {
    moving_average(&running_median(v, window), smooth)
}

/// Solve the per-frame, per-channel gain that flattens `curve`'s hunting while
/// leaving genuine brightness changes intact.
///
/// Returned gains multiply **sRGB-encoded** channel values (see module docs).
/// A gain of `1.0` is a no-op, and an empty curve yields an empty result.
pub fn solve_gains(curve: &ExposureCurve, params: DeflickerParams) -> Vec<[f32; 3]> {
    let params = params.sanitized();
    let n = curve.len();
    if n == 0 {
        return Vec::new();
    }

    // Work in log space so the correction is multiplicative and symmetric:
    // a 10 % brightening and a 10 % darkening are then equal and opposite,
    // which a linear difference would not be.
    let lim = (1.0 + params.max_change).ln();
    let mut log_gain = vec![[0.0f32; 3]; n];

    for _ in 0..params.passes {
        for c in 0..3 {
            let corrected: Vec<f32> = curve
                .samples
                .iter()
                .zip(&log_gain)
                .map(|(sample, gain)| sample[c].max(1e-6).ln() + gain[c])
                .collect();
            let base = baseline(&corrected, params.window_frames, params.smooth_frames);
            for ((gain, b), corr) in log_gain.iter_mut().zip(&base).zip(&corrected) {
                gain[c] = (gain[c] + (b - corr)).clamp(-lim, lim);
            }
        }
    }

    // Blend per-channel gains toward the shared (exposure-only) gain. At
    // `chroma_amount == 0` every channel moves together, so saturation is
    // untouched; at 1.0 each channel is free, which is what stabilises
    // white-balance drift.
    let a = params.chroma_amount;
    let strength = params.amount;
    log_gain
        .into_iter()
        .map(|g| {
            let shared = LUMA_R * g[0] + LUMA_G * g[1] + LUMA_B * g[2];
            let mut out = [0.0f32; 3];
            for c in 0..3 {
                let blended = shared + (g[c] - shared) * a;
                out[c] = (blended * strength).exp();
            }
            out
        })
        .collect()
}

/// Gain actually applied to an encoded channel value, after highlight rolloff.
#[inline]
fn rolled_off(gain: f32, encoded: f32) -> f32 {
    if gain <= 1.0 || encoded <= HIGHLIGHT_KNEE {
        return gain;
    }
    let t = ((encoded - HIGHLIGHT_KNEE) / (1.0 - HIGHLIGHT_KNEE)).clamp(0.0, 1.0);
    // Quadratic taper: full gain at the knee, exactly 1.0 at white.
    1.0 + (gain - 1.0) * (1.0 - t * t)
}

/// Apply a per-channel exposure gain to `img`.
///
/// The gain multiplies sRGB-encoded values, so the pipeline is
/// unpremultiply → encode → scale → decode → repremultiply. Alpha is carried
/// through untouched: this changes exposure, not coverage.
pub fn apply_gain(img: &Image, gain: [f32; 3]) -> Image {
    let mut out = img.clone();
    if gain == [1.0; 3] {
        return out;
    }
    for px in &mut out.pixels {
        let a = px[3];
        if a <= 1e-6 {
            continue;
        }
        let inv = 1.0 / a;
        for c in 0..3 {
            let encoded = linear_to_srgb((px[c] * inv).clamp(0.0, 1.0));
            let g = rolled_off(gain[c], encoded);
            let corrected = (encoded * g).clamp(0.0, 1.0);
            px[c] = srgb_to_linear(corrected) * a;
        }
    }
    out
}

// ── The analysis job and its result store ────────────────────────────────────

/// Solved gains for one clip, sampled on a regular tick grid.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipGains {
    /// Clip-relative tick of `gains[0]`.
    pub first: Tick,
    /// Ticks between consecutive samples. Always ≥ 1.
    pub step: i64,
    pub gains: Vec<[f32; 3]>,
    /// Rolling-band track for the whole clip, if one was detected.
    ///
    /// One track, not one fit per sample: band phase advances every frame, so a
    /// value sampled on `step` and held would be wrong for every frame in
    /// between. The track carries the advance and is evaluated per frame.
    pub band: Option<crate::graph::rolling_bands::BandTrack>,
    /// Ticks per source frame — how `band` converts a tick into a frame index.
    pub frame_ticks: i64,
}

impl ClipGains {
    /// Gain at clip-relative `dt`, linearly interpolated between samples.
    ///
    /// The analysis job samples on a stride (a long clip is measured at a few
    /// hundred points, not tens of thousands), so consecutive output frames
    /// frequently fall between two samples. Holding the nearest one would step
    /// the gain at the sample rate, which reads as its own low-frequency
    /// flicker — precisely the artefact this effect exists to remove. Beyond the
    /// ends the edge sample is held, so the first and last frames stay
    /// corrected rather than fading to unity.
    pub fn at(&self, dt: Tick) -> Option<[f32; 3]> {
        if self.gains.is_empty() {
            return None;
        }
        let last = self.gains.len() - 1;
        let step = self.step.max(1);
        let rel = dt.0.saturating_sub(self.first.0).max(0);
        let idx = (rel / step) as usize;
        if idx >= last {
            return Some(self.gains[last]);
        }
        let frac = (rel % step) as f32 / step as f32;
        let (a, b) = (self.gains[idx], self.gains[idx + 1]);
        Some([
            a[0] + (b[0] - a[0]) * frac,
            a[1] + (b[1] - a[1]) * frac,
            a[2] + (b[2] - a[2]) * frac,
        ])
    }

    /// The band as it stands at clip-relative `dt`, if one was detected.
    ///
    /// Evaluated per frame from the track's deterministic phase advance rather
    /// than looked up, which is the only way a sparsely-measured clip can carry
    /// a correct band phase on every frame.
    pub fn band_at(&self, dt: Tick) -> Option<crate::graph::rolling_bands::BandModel> {
        let track = self.band?;
        let frame = dt.0.saturating_sub(self.first.0).max(0) / self.frame_ticks.max(1);
        Some(track.model_at_frame(frame))
    }

    /// Attach a measured band track. `frame_ticks` is the source frame duration.
    pub fn with_band(
        mut self,
        band: Option<crate::graph::rolling_bands::BandTrack>,
        frame_ticks: i64,
    ) -> Self {
        self.band = band;
        self.frame_ticks = frame_ticks.max(1);
        self
    }
}

/// Measured gains for every analysed clip — the store [`compile_full`] consults.
///
/// [`compile_full`]: crate::graph::compile::compile_full
#[derive(Clone, Debug, Default)]
pub struct GainTable {
    clips: HashMap<ClipId, ClipGains>,
}

impl GainTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, clip: ClipId, gains: ClipGains) {
        self.clips.insert(clip, gains);
    }

    pub fn remove(&mut self, clip: &ClipId) {
        self.clips.remove(clip);
    }

    pub fn contains(&self, clip: &ClipId) -> bool {
        self.clips.contains_key(clip)
    }

    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }

    pub fn len(&self) -> usize {
        self.clips.len()
    }

    pub fn clear(&mut self) {
        self.clips.clear();
    }
}

impl crate::graph::compile::DeflickerGains for GainTable {
    fn gain(&self, clip: ClipId, dt: Tick) -> Option<[f32; 3]> {
        self.clips.get(&clip)?.at(dt)
    }

    fn band(&self, clip: ClipId, dt: Tick) -> Option<crate::graph::rolling_bands::BandModel> {
        self.clips.get(&clip)?.band_at(dt)
    }
}

/// Translate the user-facing manifest params into solver params.
///
/// `window_secs` is converted with the clip's frame rate so a setting means the
/// same thing at 24 and 60 fps — the reason the manifest exposes seconds rather
/// than frames.
pub fn params_for(
    amount: f32,
    window_secs: f32,
    max_change: f32,
    chroma_amount: f32,
    fps: f32,
) -> DeflickerParams {
    let fps = if fps.is_finite() && fps > 0.0 {
        fps
    } else {
        30.0
    };
    DeflickerParams {
        window_frames: (window_secs.max(0.0) * fps).round().max(1.0) as usize,
        // A ~0.25 s post-median smooth removes the staircase without being long
        // enough to reintroduce step-smearing.
        smooth_frames: (fps * 0.25).round().max(1.0) as usize,
        max_change,
        passes: 2,
        chroma_amount,
        amount,
    }
    .sanitized()
}

/// Run the deflicker analysis over a clip's frames and solve its gains.
///
/// `frames` must be in presentation order, sampled every `step` ticks starting
/// at clip-relative `first`. Decoding is the caller's job — this keeps the
/// analysis a pure function of the pixels it is handed, which is what makes it
/// cacheable by content hash.
/// Measure a clip's exposure curve and solve its gains.
///
/// Band analysis is separate ([`rolling_bands::track_from_burst`]) because it
/// needs *consecutive* frames, which this strided pass cannot supply; attach the
/// result with [`ClipGains::with_band`].
///
/// [`rolling_bands::track_from_burst`]: crate::graph::rolling_bands::track_from_burst
pub fn measure_clip<'a>(
    frames: impl IntoIterator<Item = &'a Image>,
    first: Tick,
    step: i64,
    params: DeflickerParams,
) -> ClipGains {
    let curve = ExposureCurve {
        samples: frames.into_iter().map(measure_frame).collect(),
    };
    ClipGains {
        first,
        step: step.max(1),
        gains: solve_gains(&curve, params),
        band: None,
        frame_ticks: step.max(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::ir::LinearColor;

    fn gray(v: f32) -> Image {
        let lin = srgb_to_linear(v);
        Image::filled(
            4,
            4,
            LinearColor {
                r: lin,
                g: lin,
                b: lin,
                a: 1.0,
            },
        )
    }

    /// A curve that oscillates around a constant level — pure "hunting".
    fn hunting(n: usize, level: f32, amplitude: f32, period: f32) -> ExposureCurve {
        let samples = (0..n)
            .map(|i| {
                let v =
                    level * (1.0 + amplitude * (i as f32 / period * std::f32::consts::TAU).sin());
                [v; 3]
            })
            .collect();
        ExposureCurve { samples }
    }

    #[test]
    fn measure_frame_reads_encoded_value_not_linear() {
        // 0.5 encoded is ~0.214 linear; measuring in linear would report that.
        let m = measure_frame(&gray(0.5));
        assert!((m[0] - 0.5).abs() < 1e-3, "measured {} want 0.5", m[0]);
    }

    #[test]
    fn transparent_frame_measures_neutral_and_yields_unit_gain() {
        let img = Image::new(4, 4); // fully transparent
        assert_eq!(measure_frame(&img), [0.5; 3]);
    }

    #[test]
    fn flat_curve_yields_unit_gain() {
        let curve = ExposureCurve {
            samples: vec![[0.5; 3]; 60],
        };
        for g in solve_gains(&curve, DeflickerParams::default()) {
            for c in 0..3 {
                assert!((g[c] - 1.0).abs() < 1e-4, "gain {} should be unity", g[c]);
            }
        }
    }

    #[test]
    fn empty_curve_is_handled() {
        assert!(solve_gains(&ExposureCurve::default(), DeflickerParams::default()).is_empty());
    }

    /// The core claim: applying the solved gains flattens the oscillation.
    #[test]
    fn hunting_is_substantially_removed() {
        let n = 300;
        let curve = hunting(n, 0.5, 0.10, 40.0);
        let params = DeflickerParams {
            amount: 1.0,
            ..DeflickerParams::default()
        };
        let gains = solve_gains(&curve, params);
        let corrected: Vec<f32> = (0..n).map(|i| curve.samples[i][0] * gains[i][0]).collect();

        let dev = |v: &[f32]| {
            let mean = v.iter().sum::<f32>() / v.len() as f32;
            (v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / v.len() as f32).sqrt() / mean
        };
        let before = dev(&curve.samples.iter().map(|s| s[0]).collect::<Vec<_>>());
        let after = dev(&corrected);
        assert!(
            after < before * 0.2,
            "residual {after:.5} should be well under 20% of {before:.5}"
        );
    }

    /// A genuine sustained brightness change must survive: the clamp is what
    /// distinguishes "the light really changed" from "the camera is hunting".
    #[test]
    fn genuine_step_survives_the_correction() {
        let n = 400;
        let samples: Vec<[f32; 3]> = (0..n)
            .map(|i| if i < n / 2 { [0.25; 3] } else { [0.60; 3] })
            .collect();
        let curve = ExposureCurve { samples };
        let params = DeflickerParams {
            amount: 1.0,
            ..DeflickerParams::default()
        };
        let gains = solve_gains(&curve, params);
        let before_ratio = 0.60 / 0.25;
        let lo = curve.samples[10][0] * gains[10][0];
        let hi = curve.samples[n - 10][0] * gains[n - 10][0];
        let after_ratio = hi / lo;
        // The step must remain clearly present; the clamp bounds how much of it
        // the correction can eat.
        assert!(
            after_ratio > before_ratio * 0.5,
            "step ratio collapsed from {before_ratio:.2} to {after_ratio:.2}"
        );
    }

    #[test]
    fn max_change_bounds_the_gain() {
        let curve = hunting(200, 0.5, 0.9, 30.0);
        let params = DeflickerParams {
            max_change: 0.10,
            amount: 1.0,
            ..DeflickerParams::default()
        };
        for g in solve_gains(&curve, params) {
            for c in 0..3 {
                assert!(
                    g[c] >= 1.0 / 1.1001 && g[c] <= 1.1001,
                    "gain {} escaped the ±10% clamp",
                    g[c]
                );
            }
        }
    }

    #[test]
    fn amount_zero_disables_the_effect() {
        let curve = hunting(120, 0.5, 0.2, 25.0);
        let params = DeflickerParams {
            amount: 0.0,
            ..DeflickerParams::default()
        };
        for g in solve_gains(&curve, params) {
            for c in 0..3 {
                assert!((g[c] - 1.0).abs() < 1e-6);
            }
        }
    }

    /// `chroma_amount = 0` must move all channels together, so a colour cast is
    /// preserved rather than neutralised.
    #[test]
    fn chroma_amount_zero_preserves_colour_balance() {
        let n = 200;
        // Blue drifts independently of red/green — an auto-white-balance wobble.
        let samples: Vec<[f32; 3]> = (0..n)
            .map(|i| {
                let t = i as f32 / 20.0 * std::f32::consts::TAU;
                [0.5, 0.5, 0.5 * (1.0 + 0.12 * t.sin())]
            })
            .collect();
        let curve = ExposureCurve { samples };

        let shared = solve_gains(
            &curve,
            DeflickerParams {
                chroma_amount: 0.0,
                amount: 1.0,
                ..DeflickerParams::default()
            },
        );
        for g in &shared {
            assert!(
                (g[0] - g[2]).abs() < 1e-4,
                "channels must move together at chroma_amount=0 ({} vs {})",
                g[0],
                g[2]
            );
        }

        // At full chroma the blue channel gets its own, different correction.
        let split = solve_gains(
            &curve,
            DeflickerParams {
                chroma_amount: 1.0,
                amount: 1.0,
                ..DeflickerParams::default()
            },
        );
        let max_split = split
            .iter()
            .map(|g| (g[0] - g[2]).abs())
            .fold(0.0f32, f32::max);
        assert!(
            max_split > 1e-3,
            "chroma_amount=1 should let channels diverge (max {max_split})"
        );
    }

    #[test]
    fn apply_gain_scales_in_encoded_space() {
        let img = gray(0.4);
        let out = apply_gain(&img, [1.25; 3]);
        let got = linear_to_srgb(out.pixels[0][0]);
        assert!(
            (got - 0.5).abs() < 1e-3,
            "0.4 encoded × 1.25 should be 0.5, got {got}"
        );
    }

    #[test]
    fn unit_gain_is_bit_identical() {
        let img = gray(0.37);
        let out = apply_gain(&img, [1.0; 3]);
        assert_eq!(out.pixels, img.pixels);
    }

    #[test]
    fn alpha_is_never_modified() {
        let mut img = gray(0.5);
        for (i, px) in img.pixels.iter_mut().enumerate() {
            let a = 0.25 + 0.05 * i as f32;
            px[3] = a.min(1.0);
        }
        let before: Vec<f32> = img.pixels.iter().map(|p| p[3]).collect();
        let out = apply_gain(&img, [1.2, 0.9, 1.05]);
        let after: Vec<f32> = out.pixels.iter().map(|p| p[3]).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn highlight_rolloff_cannot_push_white_over() {
        // Near-white with a brightening gain must stay at or under white.
        let img = gray(0.98);
        let out = apply_gain(&img, [1.5; 3]);
        let got = linear_to_srgb(out.pixels[0][0]);
        assert!(got <= 1.0 + 1e-6, "clipped past white: {got}");
        // And the knee must leave midtones alone.
        assert_eq!(rolled_off(1.5, 0.5), 1.5);
    }

    #[test]
    fn darkening_gain_is_not_rolled_off() {
        // The knee exists to protect highlights from *brightening* only.
        assert_eq!(rolled_off(0.8, 0.95), 0.8);
    }

    #[test]
    fn sanitized_rejects_nonsense_params() {
        let p = DeflickerParams {
            window_frames: 0,
            smooth_frames: 0,
            max_change: 9.0,
            passes: 0,
            chroma_amount: -1.0,
            amount: 5.0,
        }
        .sanitized();
        assert_eq!(p.window_frames, 1);
        assert_eq!(p.smooth_frames, 1);
        assert_eq!(p.max_change, 0.9);
        assert_eq!(p.passes, 1);
        assert_eq!(p.chroma_amount, 0.0);
        assert_eq!(p.amount, 1.0);
        // Even windows become odd so the median stays centred.
        assert_eq!(
            DeflickerParams {
                window_frames: 8,
                ..DeflickerParams::default()
            }
            .sanitized()
            .window_frames,
            9
        );
    }

    #[test]
    fn running_median_preserves_a_step() {
        let v: Vec<f32> = (0..100).map(|i| if i < 50 { 0.0 } else { 1.0 }).collect();
        let med = running_median(&v, 11);
        // Well away from the step the median is exact on both sides.
        assert_eq!(med[10], 0.0);
        assert_eq!(med[90], 1.0);
        // A moving average of the same width would have smeared the step over
        // 11 samples; the median transitions within one.
        let transitions = med.windows(2).filter(|w| w[0] != w[1]).count();
        assert_eq!(transitions, 1, "median should step exactly once");
    }

    #[test]
    fn iteration_converges() {
        let curve = hunting(240, 0.5, 0.15, 35.0);
        let base = DeflickerParams {
            amount: 1.0,
            ..DeflickerParams::default()
        };
        let residual = |passes: u8| {
            let g = solve_gains(&curve, DeflickerParams { passes, ..base });
            let v: Vec<f32> = (0..curve.len())
                .map(|i| curve.samples[i][0] * g[i][0])
                .collect();
            let mean = v.iter().sum::<f32>() / v.len() as f32;
            (v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / v.len() as f32).sqrt()
        };
        let (one, two, four) = (residual(1), residual(2), residual(4));
        assert!(
            two <= one,
            "pass 2 ({two}) should not be worse than 1 ({one})"
        );
        // Converged: the fourth pass buys essentially nothing over the second.
        assert!(
            (four - two).abs() < two * 0.25 + 1e-6,
            "expected convergence by pass 2 (2={two}, 4={four})"
        );
    }

    /// Strided sampling means most frames land between samples; holding the
    /// nearest would step the gain at the sample rate, which is its own flicker.
    #[test]
    fn gains_interpolate_between_samples() {
        let g = ClipGains {
            first: Tick(0),
            step: 100,
            gains: vec![[1.0; 3], [2.0; 3]],
            band: None,
            frame_ticks: 1,
        };
        assert_eq!(g.at(Tick(0)).unwrap()[0], 1.0);
        assert!((g.at(Tick(50)).unwrap()[0] - 1.5).abs() < 1e-6, "midpoint");
        assert!((g.at(Tick(25)).unwrap()[0] - 1.25).abs() < 1e-6, "quarter");
        // Past the end the last sample is held, not faded to unity.
        assert_eq!(g.at(Tick(100)).unwrap()[0], 2.0);
        assert_eq!(g.at(Tick(9999)).unwrap()[0], 2.0);
        // Before `first` the first sample is held.
        assert_eq!(g.at(Tick(-50)).unwrap()[0], 1.0);
    }

    /// The job as the session runs it: a strided exposure pass, plus a band
    /// track from a consecutive burst, evaluated per frame.
    #[test]
    fn clip_gains_carry_a_per_frame_band() {
        use crate::graph::ir::LinearColor;
        use crate::graph::rolling_bands;

        let flat = |level: f32| {
            Image::filled(
                24,
                200,
                LinearColor {
                    r: level,
                    g: level,
                    b: level,
                    a: 1.0,
                },
            )
        };
        let with_band = |phase: f32| {
            let mut img = flat(0.4);
            let (h, w) = (img.height as usize, img.width as usize);
            let k = std::f32::consts::TAU * 14.0 / h as f32;
            for y in 0..h {
                let m = 1.0 + 0.06 * (k * y as f32 + phase).cos();
                for x in 0..w {
                    let px = &mut img.pixels[y * w + x];
                    px[0] *= m;
                    px[1] *= m;
                    px[2] *= m;
                }
            }
            img
        };

        // 100 Hz mains at 30 fps — the advance the snap will recognise.
        let drift = std::f32::consts::TAU / 3.0;
        let frames: Vec<Image> = (0..12).map(|i| with_band(drift * i as f32)).collect();

        // Exposure is measured on a stride; bands come from the consecutive burst.
        let strided: Vec<&Image> = frames.iter().step_by(3).collect();
        let burst: Vec<Vec<f32>> = frames.iter().map(rolling_bands::row_profile).collect();
        // 30 fps + a quarter-turn drift snaps cleanly to a mains candidate.
        let track = rolling_bands::track_from_burst(&burst, 30.0, 0).expect("band track");

        let g = measure_clip(strided, Tick(0), 300, DeflickerParams::default())
            .with_band(Some(track), 100);
        assert_eq!(g.band.map(|b| b.cycles), Some(14));

        // The phase must ADVANCE frame to frame — the whole point of a track.
        let p0 = g.band_at(Tick(0)).unwrap().phase;
        let p1 = g.band_at(Tick(100)).unwrap().phase;
        let p2 = g.band_at(Tick(200)).unwrap().phase;
        assert!(
            (p1 - p0).abs() > 0.1 && ((p2 - p1) - (p1 - p0)).abs() < 1e-3,
            "phase should advance uniformly per frame: {p0} {p1} {p2}"
        );
        // Two ticks inside the same frame share a phase.
        assert_eq!(g.band_at(Tick(10)).unwrap().phase, p0);

        // No track => no band correction, whatever the ticks.
        let plain = measure_clip(frames.iter(), Tick(0), 100, DeflickerParams::default());
        assert!(plain.band_at(Tick(0)).is_none());
        assert!(plain.band_at(Tick(5000)).is_none());
    }

    /// The whole loop on real pixels: measure a flickering sequence, solve, apply,
    /// re-measure. This is the claim the effect actually makes to a user.
    #[test]
    fn round_trip_on_images_reduces_measured_flicker() {
        let n = 240;
        // A sequence that hunts ±8 % around mid-grey, plus a genuine slow ramp
        // the correction should leave alone.
        let frames: Vec<Image> = (0..n)
            .map(|i| {
                let t = i as f32;
                let hunt = 1.0 + 0.08 * (t / 18.0 * std::f32::consts::TAU).sin();
                let ramp = 0.40 + 0.20 * (t / n as f32);
                gray((ramp * hunt).clamp(0.02, 0.98))
            })
            .collect();

        let curve = ExposureCurve {
            samples: frames.iter().map(measure_frame).collect(),
        };
        let params = DeflickerParams {
            amount: 1.0,
            ..DeflickerParams::default()
        };
        let gains = solve_gains(&curve, params);
        assert_eq!(gains.len(), n);

        let after: Vec<f32> = frames
            .iter()
            .zip(&gains)
            .map(|(f, g)| measure_frame(&apply_gain(f, *g))[0])
            .collect();
        let before: Vec<f32> = curve.samples.iter().map(|s| s[0]).collect();

        // High-pass both against a short window: that isolates the hunting from
        // the ramp, which must survive.
        let hp = |v: &[f32]| -> f32 {
            let base = moving_average(v, 9);
            let d: Vec<f32> = v.iter().zip(&base).map(|(x, b)| (x - b) / b).collect();
            (d.iter().map(|x| x * x).sum::<f32>() / d.len() as f32).sqrt()
        };
        let (fb, fa) = (hp(&before), hp(&after));
        assert!(
            fa < fb * 0.25,
            "flicker {fb:.5} -> {fa:.5}; expected at least a 4x reduction"
        );

        // The genuine ramp survives: the sequence still gets brighter overall.
        assert!(
            after[n - 1] > after[0] * 1.2,
            "the real brightness ramp was flattened ({} -> {})",
            after[0],
            after[n - 1]
        );
    }
}
