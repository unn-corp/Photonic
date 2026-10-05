//! Explainable Legacy SDR global shot-balance proposal shared by GUI and MCP.
//! It proposes an editable correction; it never mutates the document.

use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct PrinterLightSuggestion {
    pub operator: &'static str,
    pub points: [f32; 3],
    pub raw_points: [f32; 3],
    pub eligible_pixels_reference: [usize; 3],
    pub eligible_pixels_current: [usize; 3],
    pub trimmed_pixels_reference: [usize; 3],
    pub trimmed_pixels_current: [usize; 3],
    pub method: &'static str,
    pub note: &'static str,
}

/// Compare per-channel trimmed mean log2 light over opaque, non-clipped pixels. The
/// frames need equal dimensions but need not be pixel-aligned. This estimates
/// a global RGB balance, not scene-aware or subject-aware matching.
pub fn suggest_printer_lights(reference: &[u8], current: &[u8]) -> Option<PrinterLightSuggestion> {
    if reference.len() != current.len()
        || reference.is_empty()
        || !reference.len().is_multiple_of(4)
    {
        return None;
    }
    let pixels = reference.len() / 4;
    let mut histograms = [[[0usize; 256]; 3]; 2];
    let mut counts = [[0usize; 3]; 2];
    for (which, image) in [reference, current].into_iter().enumerate() {
        for rgba in image.chunks_exact(4) {
            if rgba[3] < 250 {
                continue;
            }
            for channel in 0..3 {
                let code = rgba[channel];
                if !(8..=245).contains(&code) {
                    continue;
                }
                histograms[which][channel][usize::from(code)] += 1;
                counts[which][channel] += 1;
            }
        }
    }
    let minimum = pixels.div_ceil(20).max(16);
    if counts.iter().flatten().any(|count| *count < minimum) {
        return None;
    }
    let mut points = [0.0f32; 3];
    let mut raw_points = [0.0f32; 3];
    let mut trimmed = [[0usize; 3]; 2];
    for channel in 0..3 {
        let (reference_mean, reference_trimmed) =
            trimmed_log_mean(&histograms[0][channel], counts[0][channel]);
        let (current_mean, current_trimmed) =
            trimmed_log_mean(&histograms[1][channel], counts[1][channel]);
        trimmed[0][channel] = reference_trimmed;
        trimmed[1][channel] = current_trimmed;
        raw_points[channel] = (12.0 * (reference_mean - current_mean)) as f32;
        points[channel] = raw_points[channel].clamp(-24.0, 24.0);
    }
    Some(PrinterLightSuggestion {
        operator: "printer_lights",
        points,
        raw_points,
        eligible_pixels_reference: counts[0],
        eligible_pixels_current: counts[1],
        trimmed_pixels_reference: trimmed[0],
        trimmed_pixels_current: trimmed[1],
        method: "per_channel_10_percent_trimmed_mean_log2_of_opaque_nonclipped_legacy_sdr_pixels",
        note: "Global balance estimate; differing subjects or framing can still mislead it. Inspect the proposed look before keeping it.",
    })
}

/// Keep the central 80% of each channel's valid code-value distribution. Fixed
/// histograms bound memory even for 4K frames and make the estimate independent
/// of pixel order; trimming each image separately resists small specular/shadow
/// areas without assuming that the shots are spatially aligned.
fn trimmed_log_mean(histogram: &[usize; 256], count: usize) -> (f64, usize) {
    let trim = count / 10;
    let end = count - trim;
    let mut rank = 0usize;
    let mut sum = 0.0f64;
    for (code, &bucket) in histogram.iter().enumerate() {
        let kept = (rank + bucket).min(end).saturating_sub(rank.max(trim));
        if kept > 0 {
            let linear = crate::graph::ops::srgb_to_linear(code as f32 / 255.0);
            sum += f64::from(linear).log2() * kept as f64;
        }
        rank += bucket;
    }
    let kept = end - trim;
    (sum / kept as f64, count - kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_stop_difference_proposes_twelve_negative_points() {
        let encode = |linear: f32| {
            (crate::graph::ops::linear_to_srgb(linear) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        let reference = [encode(0.1), encode(0.2), encode(0.3), 255].repeat(64);
        let current = [encode(0.2), encode(0.4), encode(0.6), 255].repeat(64);
        let suggestion = suggest_printer_lights(&reference, &current).unwrap();
        for point in suggestion.points {
            assert!((point + 12.0).abs() < 0.5, "{suggestion:?}");
        }
        let transparent = [128, 128, 128, 0].repeat(64);
        assert!(suggest_printer_lights(&transparent, &current).is_none());
        let clipped = [255, 255, 255, 255].repeat(64);
        assert!(suggest_printer_lights(&clipped, &current).is_none());
    }

    #[test]
    fn small_dark_outlier_does_not_skew_the_match() {
        let mut reference = [128, 128, 128, 255].repeat(100);
        let current = reference.clone();
        for rgba in reference.chunks_exact_mut(4).take(8) {
            rgba[..3].fill(9);
        }
        let suggestion = suggest_printer_lights(&reference, &current).unwrap();
        assert!(suggestion.points.iter().all(|point| point.abs() < 0.01));
        assert_eq!(suggestion.trimmed_pixels_reference, [20; 3]);
        assert_eq!(suggestion.trimmed_pixels_current, [20; 3]);
    }
}
