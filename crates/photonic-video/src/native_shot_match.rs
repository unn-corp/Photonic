//! Bounded scene-linear AP1 measurement for explainable global shot matching.
//! Callers must supply a qualified scene tap, not a display-transformed frame.
//! This numerical foundation does not itself capture references or edit grades.

use photonic_core::timeline::native_balance::{
    NativeBalanceDomain, NativeBalanceSummary, NATIVE_BALANCE_HISTOGRAM_BINS,
    NATIVE_BALANCE_LOG_UNITS_PER_STOP, NATIVE_BALANCE_MAX_LOG2, NATIVE_BALANCE_MAX_PIXELS,
    NATIVE_BALANCE_MIN_LOG2,
};

use crate::shot_match::PrinterLightSuggestion;

struct Histogram {
    counts: Vec<u32>,
    log_sums: Vec<f64>,
    eligible: u32,
}

impl Histogram {
    fn new() -> Self {
        Self {
            counts: vec![0; NATIVE_BALANCE_HISTOGRAM_BINS],
            log_sums: vec![0.; NATIVE_BALANCE_HISTOGRAM_BINS],
            eligible: 0,
        }
    }

    fn add(&mut self, log: f64) {
        let bin = (((log - NATIVE_BALANCE_MIN_LOG2)
            / (NATIVE_BALANCE_MAX_LOG2 - NATIVE_BALANCE_MIN_LOG2)
            * NATIVE_BALANCE_HISTOGRAM_BINS as f64) as usize)
            .min(NATIVE_BALANCE_HISTOGRAM_BINS - 1);
        self.counts[bin] += 1;
        self.log_sums[bin] += log;
        self.eligible += 1;
    }

    fn trimmed_mean(&self) -> (f64, u32) {
        let trim = self.eligible / 10;
        let end = self.eligible - trim;
        let retained = end - trim;
        let mut rank = 0_u32;
        let mut sum = 0.;
        for (&count, &log_sum) in self.counts.iter().zip(&self.log_sums) {
            let kept = (rank + count).min(end).saturating_sub(rank.max(trim));
            if kept > 0 {
                // Exact sums for complete bins; a partially retained boundary
                // bin uses its mean. Its uncertainty is at most one bin width
                // (40 / 4096 stops), without per-pixel sorting/storage.
                sum += log_sum * (f64::from(kept) / f64::from(count));
            }
            rank += count;
        }
        (sum / f64::from(retained), retained)
    }
}

/// Analyze immutable premultiplied scene-linear AP1. Almost-opaque pixels are
/// unpremultiplied; per-channel nonpositive or out-of-range samples are excluded
/// from assistance, without modifying the rendered pixels. Nonfinite visible
/// color or invalid alpha refuses measurement. Histogram storage is fixed at
/// 144 KiB, independent of the bounded input image size.
pub fn measure_native_balance(
    pixels: &[[f32; 4]],
    width: u32,
    height: u32,
) -> Result<NativeBalanceSummary, &'static str> {
    let total = u64::from(width) * u64::from(height);
    if total == 0 || total > NATIVE_BALANCE_MAX_PIXELS || pixels.len() as u64 != total {
        return Err("native balance requires an exact nonempty frame of at most 16777216 pixels");
    }
    let mut histograms: [Histogram; 3] = std::array::from_fn(|_| Histogram::new());
    for rgba in pixels {
        let alpha = f64::from(rgba[3]);
        if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
            return Err("native balance frame has invalid alpha");
        }
        if alpha < 0.98 {
            continue;
        }
        for channel in 0..3 {
            let value = f64::from(rgba[channel]) / alpha;
            if !value.is_finite() {
                return Err("native balance frame has nonfinite eligible color");
            }
            if value <= 0. {
                continue;
            }
            let log = value.log2();
            if (NATIVE_BALANCE_MIN_LOG2..NATIVE_BALANCE_MAX_LOG2).contains(&log) {
                histograms[channel].add(log);
            }
        }
    }
    let minimum = total.div_ceil(20).max(16);
    if histograms.iter().any(|h| u64::from(h.eligible) < minimum) {
        return Err("native balance has too few opaque positive bounded AP1 samples");
    }
    let measured = histograms.each_ref().map(Histogram::trimmed_mean);
    let summary = NativeBalanceSummary {
        version: 1,
        domain: NativeBalanceDomain::SceneLinearAcescg,
        width,
        height,
        eligible_pixels: histograms.each_ref().map(|h| h.eligible),
        retained_pixels: measured.map(|(_, n)| n),
        trimmed_mean_log2_q20: measured.map(|(mean, _)| {
            (mean * f64::from(NATIVE_BALANCE_LOG_UNITS_PER_STOP))
                .round()
                .clamp(
                    f64::from(-20 * NATIVE_BALANCE_LOG_UNITS_PER_STOP),
                    f64::from(20 * NATIVE_BALANCE_LOG_UNITS_PER_STOP - 1),
                ) as i32
        }),
    };
    summary.validate()?;
    Ok(summary)
}

/// Propose editable per-channel exposure from two independently measured AP1
/// summaries. The two-stop safety cap is assistance policy, not a rendering
/// clamp. Differences in subject/framing or later nonlinear grades can still
/// defeat this global estimate; the user must inspect the proposed correction.
pub fn suggest_native_printer_lights(
    reference: &NativeBalanceSummary,
    current: &NativeBalanceSummary,
) -> Result<PrinterLightSuggestion, &'static str> {
    reference.validate()?;
    current.validate()?;
    if (reference.width, reference.height) != (current.width, current.height) {
        return Err("native balance reference dimensions differ from the current frame");
    }
    let raw_points = std::array::from_fn(|c| {
        (12.0 * (reference.trimmed_mean_log2()[c] - current.trimmed_mean_log2()[c])) as f32
    });
    Ok(PrinterLightSuggestion {
        operator: "printer_lights",
        points: raw_points.map(|p| p.clamp(-24., 24.)),
        raw_points,
        eligible_pixels_reference: reference.eligible_pixels.map(|n| n as usize),
        eligible_pixels_current: current.eligible_pixels.map(|n| n as usize),
        trimmed_pixels_reference: std::array::from_fn(|c| (reference.eligible_pixels[c] - reference.retained_pixels[c]) as usize),
        trimmed_pixels_current: std::array::from_fn(|c| (current.eligible_pixels[c] - current.retained_pixels[c]) as usize),
        method: "per_channel_10_percent_trimmed_log2_opaque_positive_scene_linear_ap1_v1",
        note: "Global scene-linear AP1 estimate, capped at two stops. Negative and extreme channels are excluded from measurement, never clipped in rendering. Different subjects/framing and later nonlinear grades can mislead it; inspect the editable correction.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_balance_extended_channels_and_premultiplication_have_exact_stops() {
        for alpha in [0.99_f32, 1.] {
            let reference = [[0.125 * alpha, 2. * alpha, 32. * alpha, alpha]; 100];
            let current = [[0.25 * alpha, 1. * alpha, 128. * alpha, alpha]; 100];
            let a = measure_native_balance(&reference, 10, 10).unwrap();
            let b = measure_native_balance(&current, 10, 10).unwrap();
            let suggestion = suggest_native_printer_lights(&a, &b).unwrap();
            for (actual, expected) in suggestion.points.into_iter().zip([-12., 12., -24.]) {
                assert!((actual - expected).abs() < 0.0001, "{suggestion:?}");
            }
            assert_eq!(suggestion.eligible_pixels_reference, [100; 3]);
            assert_eq!(suggestion.trimmed_pixels_reference, [20; 3]);
        }
    }

    #[test]
    fn native_balance_excludes_signed_hidden_and_extreme_samples_without_mutation() {
        let mut pixels = vec![[0.18, 0.18, 0.18, 1.]; 100];
        pixels[0] = [-1., 0., 1_048_576., 1.];
        pixels[1] = [f32::NAN, f32::INFINITY, -100., 0.];
        pixels[2] = [0.1, 0.2, 0.3, 0.5];
        let before: Vec<_> = pixels.iter().map(|p| p.map(f32::to_bits)).collect();
        let summary = measure_native_balance(&pixels, 10, 10).unwrap();
        assert_eq!(summary.eligible_pixels, [97; 3]);
        assert_eq!(summary.retained_pixels, [79; 3]);
        assert!(summary
            .trimmed_mean_log2()
            .iter()
            .all(|m| (*m - f64::from(0.18_f32).log2()).abs()
                <= 0.5 / f64::from(NATIVE_BALANCE_LOG_UNITS_PER_STOP) + 1e-12));
        assert_eq!(
            before,
            pixels
                .iter()
                .map(|p| p.map(f32::to_bits))
                .collect::<Vec<_>>()
        );
        pixels[0] = [f32::NAN, 0.2, 0.3, 1.];
        assert!(measure_native_balance(&pixels, 10, 10).is_err());
        pixels[0] = [0.1, 0.2, 0.3, f32::INFINITY];
        assert!(measure_native_balance(&pixels, 10, 10).is_err());
    }

    #[test]
    fn native_balance_histogram_matches_independent_exact_trimmed_sort() {
        let pixels: Vec<_> = (0..4096)
            .map(|i| {
                let a = ((i * 1597 + 17) % 4096) as f64 / 4096.;
                let b = ((i * 811 + 3) % 4096) as f64 / 4096.;
                let c = ((i * 4051 + 1) % 4096) as f64 / 4096.;
                [
                    2_f64.powf(-19.9 + a * 39.8) as f32,
                    2_f64.powf(-4. + b * 0.02) as f32,
                    2_f64.powf(-10. + c * 25.) as f32,
                    1.,
                ]
            })
            .collect();
        let summary = measure_native_balance(&pixels, 64, 64).unwrap();
        for channel in 0..3 {
            let mut exact: Vec<_> = pixels
                .iter()
                .map(|p| f64::from(p[channel]).log2())
                .collect();
            exact.sort_by(f64::total_cmp);
            let trim = exact.len() / 10;
            let kept = &exact[trim..exact.len() - trim];
            let mean = kept.iter().sum::<f64>() / kept.len() as f64;
            assert!(
                (mean - summary.trimmed_mean_log2()[channel]).abs()
                    <= 40. / 4096. + 1. / f64::from(NATIVE_BALANCE_LOG_UNITS_PER_STOP),
                "channel{channel}: {mean} vs {summary:?}"
            );
        }
        let mut reordered = pixels;
        reordered.reverse();
        let reversed = measure_native_balance(&reordered, 64, 64).unwrap();
        for (a, b) in summary
            .trimmed_mean_log2()
            .into_iter()
            .zip(reversed.trimmed_mean_log2())
        {
            assert!((a - b).abs() < 1e-10);
        }
    }

    #[test]
    fn native_balance_rejects_bad_dimensions_coverage_and_forged_summaries() {
        assert!(measure_native_balance(&[], 0, 0).is_err());
        assert!(measure_native_balance(&[[0.2; 4]; 16], 4096, 4097).is_err());
        assert!(measure_native_balance(&[[0.2, 0.2, 0.2, 1.]; 15], 4, 4).is_err());
        assert!(measure_native_balance(&[[0., 0., 0., 1.]; 16], 4, 4).is_err());
        let valid = measure_native_balance(&[[0.2, 0.2, 0.2, 1.]; 16], 4, 4).unwrap();
        let mut forged = valid.clone();
        forged.retained_pixels[0] = u32::MAX;
        assert!(suggest_native_printer_lights(&forged, &valid).is_err());
        let mut other_size = valid.clone();
        other_size.width = 8;
        other_size.height = 2;
        assert!(suggest_native_printer_lights(&other_size, &valid).is_err());
        let mut bright = valid.clone();
        bright.trimmed_mean_log2_q20 = [10 * NATIVE_BALANCE_LOG_UNITS_PER_STOP; 3];
        let suggestion = suggest_native_printer_lights(&bright, &valid).unwrap();
        assert_eq!(suggestion.points, [24.; 3]);
        assert!(suggestion.raw_points.iter().all(|p| *p > 24.));
    }
}
