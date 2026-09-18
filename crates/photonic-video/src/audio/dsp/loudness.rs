//! EBU R128 / ITU-R BS.1770-4 loudness measurement (09 §6.6): K-weighting
//! pre-filter + 400ms gated block loudness -> integrated LUFS, plus a 4x
//! oversampled true-peak measurement.
//!
//! This operates on a full in-memory PCM buffer, not a per-block
//! [`super::DspUnit`] — 09 §6.6's export flow renders the whole work-range
//! master-bus PCM once and measures against that buffer in one pass; export
//! loudness normalization (`MasterBus::loudness_target`) is a static
//! post-render gain-offset step, not an `AudioFxKind`/`fx_chain` entry (09
//! §2 has no "loudness" fx kind). The live-meter use (09 §8's "rolling,
//! ungated real-time estimate") can call [`integrated_lufs`] on a rolling
//! buffer too — that's a P8-UI-layer choice, not this module's concern.

use crate::audio::CHANNELS;

use super::biquad::BiquadState;

// ── K-weighting pre-filter (BS.1770-4's own published design constants) ───
//
// Deliberately **not** built from `biquad::BiquadCoeffs::high_shelf`/
// `high_pass` (09 §6.1's RBJ-cookbook Q-parametrized shelf): the standard's
// own reference filter (reproduced by every conformant implementation,
// e.g. libebur128) derives the shelf stage via a bilinear transform with an
// extra `Vb` pre-warp term whose exponent (`0.4996667741545416`) is not
// quite the RBJ shelf's implicit `0.5` (`sqrt(A)`) — close enough to look
// right but off by ~0.4 LU at the standard's own 997Hz conformance
// frequency, which is exactly the tolerance §6.6's test hook (09 §11)
// checks against. Both stages below are still plain `{b0,b1,b2,a1,a2}`
// biquads processed through the shared [`BiquadState`] (09 §6.6: "same
// biquad machinery as §6.1"), just coefficient-derived differently.

/// High-shelf ("head" filter, approximating free-field-to-diffuse-field
/// response).
const SHELF_FREQ_HZ: f64 = 1_681.974_450_955_532;
const SHELF_GAIN_DB: f64 = 3.999_843_853_973_347;
const SHELF_Q: f64 = 0.707_175_236_955_419_6;
/// The standard's shelf pre-warp exponent (see module doc above) — applied
/// to the shelf's linear gain `Vh` to get the "bandwidth" term `Vb`.
const SHELF_VB_EXPONENT: f64 = 0.499_666_774_154_541_6;
/// High-pass ("RLB" filter).
const HPF_FREQ_HZ: f64 = 38.135_470_876_024_44;
const HPF_Q: f64 = 0.500_327_037_323_877_3;

/// Reuses [`biquad::BiquadCoeffs`](super::biquad::BiquadCoeffs)'s
/// `{b0,b1,b2,a1,a2}` shape without going through its RBJ constructors —
/// see the module doc above for why K-weighting needs its own derivation.
type KCoeffs = super::biquad::BiquadCoeffs;

fn k_weighting_shelf_coeffs(f0: f64, gain_db: f64, q: f64, fs: f64) -> KCoeffs {
    let k = (std::f64::consts::PI * f0 / fs).tan();
    let vh = 10f64.powf(gain_db / 20.0);
    let vb = vh.powf(SHELF_VB_EXPONENT);
    let a0 = 1.0 + k / q + k * k;
    KCoeffs {
        b0: (vh + vb * k / q + k * k) / a0,
        b1: 2.0 * (k * k - vh) / a0,
        b2: (vh - vb * k / q + k * k) / a0,
        a1: 2.0 * (k * k - 1.0) / a0,
        a2: (1.0 - k / q + k * k) / a0,
    }
}

fn k_weighting_hpf_coeffs(f0: f64, q: f64, fs: f64) -> KCoeffs {
    let k = (std::f64::consts::PI * f0 / fs).tan();
    let a0 = 1.0 + k / q + k * k;
    KCoeffs {
        b0: 1.0 / a0,
        b1: -2.0 / a0,
        b2: 1.0 / a0,
        a1: 2.0 * (k * k - 1.0) / a0,
        a2: (1.0 - k / q + k * k) / a0,
    }
}

#[derive(Clone, Copy, Debug)]
struct KWeightingCoeffs {
    shelf: KCoeffs,
    hpf: KCoeffs,
}

impl KWeightingCoeffs {
    fn new(fs: f64) -> Self {
        KWeightingCoeffs {
            shelf: k_weighting_shelf_coeffs(SHELF_FREQ_HZ, SHELF_GAIN_DB, SHELF_Q, fs),
            hpf: k_weighting_hpf_coeffs(HPF_FREQ_HZ, HPF_Q, fs),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct KWeighting {
    shelf: BiquadState,
    hpf: BiquadState,
}

impl KWeighting {
    fn process(&mut self, coeffs: &KWeightingCoeffs, x: f64) -> f64 {
        let y = self.shelf.process(&coeffs.shelf, x);
        self.hpf.process(&coeffs.hpf, y)
    }
}

// ── Gated block loudness ───────────────────────────────────────────────────

/// 400ms gating blocks (09 §6.6), stepped every 100ms (BS.1770-4 Annex 1's
/// 75%-overlap block stepping — needed for a rolling live meter to match the
/// standard's own ballistics; doesn't change the integrated value for a
/// constant-loudness test signal).
const BLOCK_MS: f64 = 400.0;
const HOP_MS: f64 = 100.0;
const ABSOLUTE_GATE_LUFS: f64 = -70.0;
const RELATIVE_GATE_LU: f64 = -10.0;
/// v1 output is stereo-only (09 §4); front L/R channel weight is 1.0 in
/// BS.1770-4's channel table.
const CHANNEL_WEIGHT: f64 = 1.0;

fn loudness_db(mean_square_weighted_sum: f64) -> f64 {
    -0.691 + 10.0 * mean_square_weighted_sum.max(1e-12).log10()
}

/// Integrated loudness (LUFS) of an interleaved-stereo buffer at
/// `sample_rate` (09 §6.6). `pcm.len()` MUST be a multiple of [`CHANNELS`].
/// Returns [`f64::NEG_INFINITY`] if there isn't enough signal for even one
/// gating block, or if every block is gated out (silence).
pub fn integrated_lufs(pcm: &[f32], sample_rate: u32) -> f64 {
    let fs = sample_rate as f64;
    let frames = pcm.len() / CHANNELS;
    let coeffs = KWeightingCoeffs::new(fs);
    let mut kw = [KWeighting::default(); CHANNELS];

    let mut weighted = vec![0.0f64; pcm.len()];
    for f in 0..frames {
        for c in 0..CHANNELS {
            let x = pcm[f * CHANNELS + c] as f64;
            weighted[f * CHANNELS + c] = kw[c].process(&coeffs, x);
        }
    }

    let block_frames = (BLOCK_MS * 0.001 * fs).round() as usize;
    let hop_frames = ((HOP_MS * 0.001 * fs).round() as usize).max(1);
    if block_frames == 0 || frames < block_frames {
        return f64::NEG_INFINITY;
    }

    let mut block_z = Vec::new();
    let mut start = 0;
    while start + block_frames <= frames {
        let mut sum_sq = [0f64; CHANNELS];
        for f in start..start + block_frames {
            for (c, sum) in sum_sq.iter_mut().enumerate() {
                let s = weighted[f * CHANNELS + c];
                *sum += s * s;
            }
        }
        let z: f64 = sum_sq
            .iter()
            .map(|&s| CHANNEL_WEIGHT * s / block_frames as f64)
            .sum();
        block_z.push(z);
        start += hop_frames;
    }

    // Absolute gate (-70 LUFS).
    let gated1: Vec<f64> = block_z
        .iter()
        .copied()
        .filter(|&z| loudness_db(z) > ABSOLUTE_GATE_LUFS)
        .collect();
    if gated1.is_empty() {
        return f64::NEG_INFINITY;
    }
    let mean1 = gated1.iter().sum::<f64>() / gated1.len() as f64;
    let relative_threshold = loudness_db(mean1) + RELATIVE_GATE_LU;

    // Relative gate (-10 LU below the absolute-gated mean).
    let gated2: Vec<f64> = gated1
        .iter()
        .copied()
        .filter(|&z| loudness_db(z) > relative_threshold)
        .collect();
    if gated2.is_empty() {
        return f64::NEG_INFINITY;
    }
    let mean2 = gated2.iter().sum::<f64>() / gated2.len() as f64;
    loudness_db(mean2)
}

// ── True peak (4x oversampled) ─────────────────────────────────────────────

const OVERSAMPLE_FACTOR: usize = 4;
/// Windowed-sinc kernel half-width, in *original-rate* samples (support is
/// `2*TAPS_HALF` original samples either side of each interpolated point).
/// This is an offline analysis pass (not a real-time budget), so tap count
/// is chosen for clean stopband behavior, not CPU cost.
const TAPS_HALF: usize = 12;

/// Windowed sinc at offset `u` (in original-sample units) from the kernel
/// center, tapered to zero at `+/- half_width` by a Blackman-like window —
/// closed-form bandlimited interpolation, no external resampler dependency
/// (`rubato` is reserved for decode-time source-rate conversion, 09 §3, not
/// this analysis-only path).
fn windowed_sinc(u: f64, half_width: f64) -> f64 {
    if u.abs() >= half_width {
        return 0.0;
    }
    let sinc = if u.abs() < 1e-9 {
        1.0
    } else {
        (std::f64::consts::PI * u).sin() / (std::f64::consts::PI * u)
    };
    let p = u / half_width;
    let window = 0.42
        + 0.5 * (std::f64::consts::PI * p).cos()
        + 0.08 * (2.0 * std::f64::consts::PI * p).cos();
    sinc * window
}

fn oversample_4x(x: &[f32]) -> Vec<f32> {
    if x.is_empty() {
        return Vec::new();
    }
    let n = x.len();
    let half_width = TAPS_HALF as f64;
    let up_len = n * OVERSAMPLE_FACTOR;
    let mut out = vec![0f32; up_len];
    for (i, out_i) in out.iter_mut().enumerate() {
        let t = i as f64 / OVERSAMPLE_FACTOR as f64;
        let lo = (t - half_width).ceil().max(0.0) as usize;
        let hi = (t + half_width).floor().min((n - 1) as f64) as usize;
        let mut acc = 0f64;
        for (k, &xk) in x.iter().enumerate().take(hi + 1).skip(lo) {
            acc += xk as f64 * windowed_sinc(t - k as f64, half_width);
        }
        *out_i = acc as f32;
    }
    out
}

/// 4x-oversampled true-peak (dBTP) of an interleaved-stereo buffer (09
/// §6.6). `sample_rate` isn't needed by the oversampling math itself (it
/// operates purely in sample-index units) but is accepted for API symmetry
/// with [`integrated_lufs`] and to leave room for a future rate-dependent
/// tap budget.
pub fn true_peak_dbtp(pcm: &[f32], _sample_rate: u32) -> f32 {
    let frames = pcm.len() / CHANNELS;
    let mut peak = 0f32;
    for c in 0..CHANNELS {
        let channel: Vec<f32> = (0..frames).map(|f| pcm[f * CHANNELS + c]).collect();
        let up = oversample_4x(&channel);
        let p = up.iter().fold(0f32, |m, &s| m.max(s.abs()));
        peak = peak.max(p);
    }
    20.0 * peak.max(1e-9).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: u32 = 48_000;

    fn stereo_sine(freq: f64, amp: f64, seconds: f64, fs: u32) -> Vec<f32> {
        let frames = (fs as f64 * seconds) as usize;
        let mut out = vec![0f32; frames * CHANNELS];
        for f in 0..frames {
            let t = f as f64 / fs as f64;
            let s = (amp * (2.0 * std::f64::consts::PI * freq * t).sin()) as f32;
            out[f * CHANNELS] = s;
            out[f * CHANNELS + 1] = s;
        }
        out
    }

    /// A steady full-bandwidth tone at 997Hz (BS.1770-4's own conformance
    /// test frequency) is amplitude-solved against the K-weighting filter's
    /// own analytic magnitude at 997Hz (`g`, computed independently via
    /// [`super::biquad::BiquadCoeffs::magnitude_at`] on the exact same
    /// coefficients [`KWeightingCoeffs::new`] builds — *not* assumed to be
    /// unity: the standard's shelf stage has a real ~+0.65dB gain there, not
    /// 0dB) so the closed-form loudness formula
    /// (`-0.691 + 10*log10(sum of per-channel mean-square)`) lands exactly
    /// on -23 LUFS. A steady signal is never gated (constant block loudness
    /// -> the relative gate's -10 LU-below-mean threshold never excludes
    /// anything), so this exercises the full K-weighting + gating pipeline,
    /// not just the closed-form algebra with the filter response guessed
    /// away.
    #[test]
    fn steady_997hz_sine_measures_minus_23_lufs() {
        let freq = 997.0;
        let coeffs = KWeightingCoeffs::new(FS as f64);
        let g =
            coeffs.shelf.magnitude_at(freq, FS as f64) * coeffs.hpf.magnitude_at(freq, FS as f64);

        // target_lufs = -0.691 + 20*log10(amp * g)  =>  amp = 10^((target+0.691)/20) / g
        let target_lufs = -23.0;
        let amp = 10f64.powf((target_lufs + 0.691) / 20.0) / g;
        let pcm = stereo_sine(freq, amp, 2.0, FS);
        let lufs = integrated_lufs(&pcm, FS);
        assert!(
            (lufs - target_lufs).abs() < 0.1,
            "expected ~{target_lufs} LUFS, got {lufs}"
        );
    }

    #[test]
    fn silence_is_gated_to_negative_infinity() {
        let pcm = vec![0f32; FS as usize * CHANNELS];
        let lufs = integrated_lufs(&pcm, FS);
        assert_eq!(lufs, f64::NEG_INFINITY);
    }

    #[test]
    fn louder_signal_measures_higher_lufs() {
        let quiet = stereo_sine(997.0, 0.05, 1.0, FS);
        let loud = stereo_sine(997.0, 0.3, 1.0, FS);
        assert!(integrated_lufs(&loud, FS) > integrated_lufs(&quiet, FS));
    }

    #[test]
    fn sample_domain_true_peak_of_a_mid_freq_sine_matches_naive_peak() {
        // A slow-moving mid-frequency sine has negligible inter-sample
        // energy between adjacent samples — true peak should track the
        // naive sample peak closely.
        let amp = 0.7;
        let pcm = stereo_sine(300.0, amp, 0.05, FS);
        let naive_peak_db = 20.0 * amp.log10();
        let tp = true_peak_dbtp(&pcm, FS) as f64;
        assert!(
            (tp - naive_peak_db).abs() < 0.3,
            "true_peak={tp} naive={naive_peak_db}"
        );
    }

    /// Inter-sample-peak case (09 §11): a Nyquist-rate alternating signal
    /// (`+A, -A, +A, -A, ...`) has every *sample* at exactly `+/-A`, but its
    /// band-limited reconstruction overshoots between samples (the boundary
    /// case of bandlimited reconstruction rings, classic true-peak-meter
    /// stress signal) — a correct true-peak measurement must read higher
    /// than the naive sample-peak reading.
    #[test]
    fn nyquist_alternating_signal_shows_intersample_overshoot() {
        let amp = 0.9f32;
        let frames = 2000;
        let mut pcm = vec![0f32; frames * CHANNELS];
        for f in 0..frames {
            let s = if f % 2 == 0 { amp } else { -amp };
            pcm[f * CHANNELS] = s;
            pcm[f * CHANNELS + 1] = s;
        }
        let sample_peak_db = 20.0 * amp.log10();
        let tp = true_peak_dbtp(&pcm, FS);
        assert!(
            tp > sample_peak_db + 0.05,
            "true peak {tp}dBTP should exceed naive sample peak {sample_peak_db}dBTP"
        );
    }
}
