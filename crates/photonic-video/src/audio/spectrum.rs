//! Audio spectrum analysis for the scopes panel (26 K-E1).
//!
//! Pure, deterministic power spectrum over a mono or interleaved-stereo f32
//! window. No external FFT dependency — a small Cooley–Tukey radix-2 DFT is
//! enough for scope-grade magnitudes at BLOCK_FRAMES sizes (512).

/// Number of frequency bins returned by [`power_spectrum`] (N/2 for N-point DFT).
pub fn spectrum_bins(window_len: usize) -> usize {
    window_len / 2
}

/// Interleave stereo → mono (L+R)/2, or copy mono as-is. Length = frames.
pub fn to_mono(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    let frames = interleaved.len() / channels;
    let mut out = Vec::with_capacity(frames);
    for i in 0..frames {
        let base = i * channels;
        let mut s = 0.0f32;
        for c in 0..channels {
            s += interleaved[base + c];
        }
        out.push(s / channels as f32);
    }
    out
}

/// Magnitude spectrum (linear, non-negative) for a power-of-two mono window.
/// Returns `N/2` bins covering DC .. Nyquist-ε. Applies a Hann window.
///
/// Empty or non-power-of-two input returns an empty vec (caller pads/resizes).
pub fn power_spectrum(mono: &[f32]) -> Vec<f32> {
    let n = mono.len();
    if n < 2 || !n.is_power_of_two() {
        return Vec::new();
    }
    // Hann window + pack as complex (re, im interleaved).
    let mut re = vec![0.0f64; n];
    let mut im = vec![0.0f64; n];
    let n_f = n as f64;
    for (i, &s) in mono.iter().enumerate() {
        let w = 0.5 * (1.0 - (2.0 * std::f64::consts::PI * i as f64 / n_f).cos());
        re[i] = f64::from(s) * w;
    }
    fft_inplace(&mut re, &mut im);
    let half = n / 2;
    let mut mags = Vec::with_capacity(half);
    let scale = 2.0 / n_f;
    for i in 0..half {
        let mag = (re[i] * re[i] + im[i] * im[i]).sqrt() * scale;
        mags.push(mag as f32);
    }
    mags
}

/// Convert linear magnitudes to dB relative to `ref_level` (default 1.0 full-scale).
pub fn to_db(mags: &[f32], ref_level: f32) -> Vec<f32> {
    let r = ref_level.max(1e-12);
    mags.iter()
        .map(|&m| 20.0 * (m.max(1e-12) / r).log10())
        .collect()
}

/// In-place radix-2 Cooley–Tukey FFT (decimation in time).
fn fft_inplace(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    debug_assert_eq!(n, im.len());
    debug_assert!(n.is_power_of_two());
    // Bit-reverse permutation.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f64::consts::PI / len as f64;
        let wlen_re = ang.cos();
        let wlen_im = ang.sin();
        for i in (0..n).step_by(len) {
            let mut w_re = 1.0f64;
            let mut w_im = 0.0f64;
            for j in 0..len / 2 {
                let u_re = re[i + j];
                let u_im = im[i + j];
                let v_re = re[i + j + len / 2] * w_re - im[i + j + len / 2] * w_im;
                let v_im = re[i + j + len / 2] * w_im + im[i + j + len / 2] * w_re;
                re[i + j] = u_re + v_re;
                im[i + j] = u_im + v_im;
                re[i + j + len / 2] = u_re - v_re;
                im[i + j + len / 2] = u_im - v_im;
                let n_re = w_re * wlen_re - w_im * wlen_im;
                w_im = w_re * wlen_im + w_im * wlen_re;
                w_re = n_re;
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_tone_peaks_near_expected_bin() {
        // 48 kHz, 512 samples, 1 kHz sine → bin ≈ 1000 * 512 / 48000 ≈ 10.67
        let n = 512usize;
        let sr = 48_000.0f64;
        let f0 = 1000.0f64;
        let mut mono = vec![0.0f32; n];
        for i in 0..n {
            mono[i] = (2.0 * std::f64::consts::PI * f0 * i as f64 / sr).sin() as f32;
        }
        let mags = power_spectrum(&mono);
        assert_eq!(mags.len(), n / 2);
        let peak = mags
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert!(
            (peak as i32 - 11).abs() <= 2,
            "peak bin {peak} should be near 11 for 1 kHz @ 48k/512"
        );
        let db = to_db(&mags, 1.0);
        // Hann window reduces coherent gain (~6 dB); still a clear peak.
        assert!(
            db[peak] > -12.0,
            "peak should be near 0 dBFS after window, got {}",
            db[peak]
        );
    }

    #[test]
    fn to_mono_averages_stereo() {
        let stereo = [1.0f32, 3.0, 2.0, 4.0];
        let m = to_mono(&stereo, 2);
        assert_eq!(m, vec![2.0, 3.0]);
    }
}
