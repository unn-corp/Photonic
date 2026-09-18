//! Frame-comparison metric (11-testing-phasing.md §1.2), promoted out of the
//! `golden_frames.rs` test binary so it is a single source of truth shared by
//! that corpus harness AND the out-of-crate acceptance-story harness
//! (29 §3 / CAP-019). Copy-pasting the metric between the two guarantees they
//! drift; living here as an always-compiled `pub` module (integration tests in
//! OTHER crates cannot see a `cfg(test)` module) prevents that.
//!
//! The metric has two layers and BOTH must pass (11 §1.2):
//!
//! | Layer       | Metric              | Threshold (CPU-vs-CPU)  |
//! |-------------|---------------------|-------------------------|
//! | Per-channel | max abs diff        | ≤ 0.02 (linear-light)   |
//! | Aggregate   | PSNR                | ≥ 40 dB                 |

use crate::graph::ops::Image;
use image::{Rgba, RgbaImage};

/// 11 §1.2 per-channel bound: max abs diff, linear-light RGBA, CPU-vs-CPU.
pub const MAX_ABS_TOL: f32 = 0.02;
/// 11 §1.2 aggregate bound: PSNR ≥ 40 dB, CPU-vs-CPU reference.
pub const MIN_PSNR_DB: f64 = 40.0;

// ── sRGB transfer (self-consistent PNG round-trip) ───────────────────────────

/// Linear-light channel → sRGB-encoded channel (both in `[0, 1]`).
pub fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// sRGB-encoded channel → linear-light channel — the exact inverse of
/// [`linear_to_srgb`].
pub fn srgb_to_linear(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Working image (premultiplied linear) → straight-alpha sRGB8 PNG buffer.
/// Un-premultiply, sRGB-encode RGB, keep alpha linear — the natural, eyeball-
/// able display encoding.
pub fn encode_png(img: &Image) -> RgbaImage {
    let mut buf = RgbaImage::new(img.width, img.height);
    let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    for y in 0..img.height {
        for x in 0..img.width {
            let p = img.pixel(x, y);
            let a = p[3].clamp(0.0, 1.0);
            let (r, g, b) = if a > 1e-6 {
                (p[0] / a, p[1] / a, p[2] / a)
            } else {
                (0.0, 0.0, 0.0)
            };
            buf.put_pixel(
                x,
                y,
                Rgba([
                    to8(linear_to_srgb(r)),
                    to8(linear_to_srgb(g)),
                    to8(linear_to_srgb(b)),
                    to8(a),
                ]),
            );
        }
    }
    buf
}

/// PNG buffer → working image (premultiplied linear) — the exact inverse of
/// [`encode_png`].
pub fn decode_png(buf: &RgbaImage) -> Image {
    let (w, h) = buf.dimensions();
    let mut pixels = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let Rgba([r, g, b, a]) = *buf.get_pixel(x, y);
            let af = a as f32 / 255.0;
            pixels.push([
                srgb_to_linear(r as f32 / 255.0) * af,
                srgb_to_linear(g as f32 / 255.0) * af,
                srgb_to_linear(b as f32 / 255.0) * af,
                af,
            ]);
        }
    }
    Image {
        width: w,
        height: h,
        pixels,
    }
}

// ── comparison metric (11 §1.2, both layers required) ────────────────────────

/// The two-layer measurement produced by [`measure`].
#[derive(Clone, Copy, Debug)]
pub struct Metric {
    /// Layer 1: worst per-channel absolute difference (linear-light).
    pub max_abs: f32,
    /// Layer 2: aggregate PSNR in dB (`f64::INFINITY` when identical).
    pub psnr_db: f64,
}

impl Metric {
    /// True iff BOTH layers pass their 11 §1.2 threshold.
    pub fn within_tolerance(&self) -> bool {
        self.max_abs <= MAX_ABS_TOL && self.psnr_db >= MIN_PSNR_DB
    }
}

/// Compares two premultiplied-linear working images channel-by-channel.
/// `Err` only on a dimension mismatch — a passing/failing *verdict* is the
/// caller's to draw from the returned [`Metric`] via [`Metric::within_tolerance`].
pub fn measure(reference: &Image, actual: &Image) -> Result<Metric, String> {
    if reference.width != actual.width || reference.height != actual.height {
        return Err(format!(
            "dimension mismatch: reference {}x{} vs actual {}x{}",
            reference.width, reference.height, actual.width, actual.height
        ));
    }
    let n = reference.pixels.len();
    let mut max_abs = 0.0f32;
    let mut sse = 0.0f64;
    for (rp, ap) in reference.pixels.iter().zip(actual.pixels.iter()) {
        for c in 0..4 {
            let d = (rp[c] - ap[c]).abs();
            max_abs = max_abs.max(d);
            sse += (d as f64) * (d as f64);
        }
    }
    let mse = sse / (n as f64 * 4.0);
    // Linear-light working values are in [0, 1], so PSNR uses MAX = 1.0.
    let psnr_db = if mse <= 0.0 {
        f64::INFINITY
    } else {
        -10.0 * mse.log10()
    };
    Ok(Metric { max_abs, psnr_db })
}

/// Amplified abs-diff heatmap (11 §1.2 "diff PNG"): the 0.02 tolerance maps to
/// full red.
pub fn diff_heatmap(reference: &Image, actual: &Image) -> RgbaImage {
    let mut buf = RgbaImage::new(reference.width, reference.height);
    for y in 0..reference.height {
        for x in 0..reference.width {
            let rp = reference.pixel(x, y);
            let ap = actual.pixel(x, y);
            let mut d = 0.0f32;
            for c in 0..4 {
                d = d.max((rp[c] - ap[c]).abs());
            }
            // Scale so the 0.02 tolerance maps to full white.
            let v = ((d / MAX_ABS_TOL).clamp(0.0, 1.0) * 255.0) as u8;
            buf.put_pixel(x, y, Rgba([v, 0, 0, 255]));
        }
    }
    buf
}

/// Panics with the measured metric values (and the label) unless both 11 §1.2
/// layers pass. On failure it also writes the abs-diff heatmap to a
/// `label`-derived path under the OS temp dir so the reviewer has the diff PNG
/// the spec calls for, then names that path in the panic message.
pub fn assert_within_tolerance(reference: &Image, actual: &Image, label: &str) {
    let metric = match measure(reference, actual) {
        Ok(m) => m,
        Err(e) => panic!("frame_compare[{label}]: {e}"),
    };
    if metric.within_tolerance() {
        return;
    }
    let safe: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let path = std::env::temp_dir().join(format!("photonic_frame_diff_{safe}.png"));
    let heatmap_note = match diff_heatmap(reference, actual).save(&path) {
        Ok(()) => format!("diff heatmap: {}", path.display()),
        Err(e) => format!("diff heatmap unwritten ({e})"),
    };
    panic!(
        "frame_compare[{label}] out of tolerance: max_abs={:.5} (≤ {MAX_ABS_TOL}), \
         psnr_db={:.2} (≥ {MIN_PSNR_DB}); {heatmap_note}",
        metric.max_abs, metric.psnr_db
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, rgba: [f32; 4]) -> Image {
        Image {
            width,
            height,
            pixels: vec![rgba; (width * height) as usize],
        }
    }

    #[test]
    fn identical_images_are_lossless() {
        let a = solid(4, 4, [0.25, 0.5, 0.75, 1.0]);
        let m = measure(&a, &a).expect("same dims");
        assert_eq!(m.max_abs, 0.0);
        assert_eq!(m.psnr_db, f64::INFINITY);
        assert!(m.within_tolerance());
    }

    #[test]
    fn single_channel_perturbation_rejects_on_max_abs_layer() {
        let reference = solid(4, 4, [0.25, 0.5, 0.75, 1.0]);
        // +0.03 on the green channel of one pixel — above the 0.02 layer-1 bound.
        let mut actual = reference.clone();
        actual.pixels[0][1] += 0.03;
        let m = measure(&reference, &actual).expect("same dims");
        assert!(
            m.max_abs > MAX_ABS_TOL,
            "max_abs {} should exceed the {MAX_ABS_TOL} per-channel bound",
            m.max_abs
        );
        assert!(!m.within_tolerance());
    }

    #[test]
    #[should_panic(expected = "out of tolerance")]
    fn assert_within_tolerance_panics_on_perturbation() {
        let reference = solid(2, 2, [0.1, 0.2, 0.3, 1.0]);
        let mut actual = reference.clone();
        actual.pixels[0][0] += 0.03;
        assert_within_tolerance(&reference, &actual, "unit-perturbation");
    }

    #[test]
    fn png_round_trip_is_within_tolerance() {
        let img = solid(3, 3, [0.2, 0.4, 0.6, 1.0]);
        let restored = decode_png(&encode_png(&img));
        let m = measure(&img, &restored).expect("same dims");
        assert!(
            m.within_tolerance(),
            "8-bit PNG round-trip should stay inside tolerance: {m:?}"
        );
    }

    #[test]
    fn dimension_mismatch_is_an_error() {
        let a = solid(2, 2, [0.0; 4]);
        let b = solid(3, 3, [0.0; 4]);
        assert!(measure(&a, &b).is_err());
    }
}
