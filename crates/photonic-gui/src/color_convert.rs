//! Color-space conversions backing the industry-grade color picker.
//!
//! Photonic's color store is **gamma-encoded sRGB** `[f32; 4]` (each channel is
//! `u8 / 255.0`, no gamma decode — see `photonic_core::color`). Everything here
//! takes/returns that convention unless a function name says otherwise:
//!
//! * HSV / HSL operate directly on gamma sRGB (matching every classic picker).
//! * OKLab / OKLCH require **linear** sRGB, so those paths gamma-decode first.
//! * WCAG relative luminance and color-blindness simulation also work in linear.
//!
//! All hues are degrees in `[0, 360)`. `s`, `v`, `l`, alpha are `[0, 1]`.

/// sRGB gamma decode (one channel, `0..=1`).
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB gamma encode (one channel, `0..=1`).
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

// ── HSV ─────────────────────────────────────────────────────────────────────

/// Gamma sRGB → HSV. `h ∈ [0,360)`, `s,v ∈ [0,1]`.
pub fn rgb_to_hsv(rgb: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = rgb;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let v = max;
    let s = if max <= 0.0 { 0.0 } else { d / max };
    let h = hue_from(r, g, b, max, d);
    [h, s, v]
}

/// HSV → gamma sRGB.
pub fn hsv_to_rgb(hsv: [f32; 3]) -> [f32; 3] {
    let [h, s, v] = hsv;
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}

// ── HSL ─────────────────────────────────────────────────────────────────────

/// Gamma sRGB → HSL. `h ∈ [0,360)`, `s,l ∈ [0,1]`.
pub fn rgb_to_hsl(rgb: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = rgb;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let l = (max + min) * 0.5;
    let s = if d <= 0.0 {
        0.0
    } else {
        d / (1.0 - (2.0 * l - 1.0).abs())
    };
    let h = hue_from(r, g, b, max, d);
    [h, s, l]
}

/// HSL → gamma sRGB.
pub fn hsl_to_rgb(hsl: [f32; 3]) -> [f32; 3] {
    let [h, s, l] = hsl;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let m = l - c * 0.5;
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}

/// Shared hue computation (degrees) from max channel + delta.
fn hue_from(r: f32, g: f32, b: f32, max: f32, d: f32) -> f32 {
    if d <= 0.0 {
        return 0.0;
    }
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0).rem_euclid(360.0)
}

// ── OKLab / OKLCH (Björn Ottosson) ───────────────────────────────────────────

/// Gamma sRGB → OKLab `[L, a, b]` (`L ∈ [0,1]`, a/b unbounded, ~±0.4).
pub fn rgb_to_oklab(rgb: [f32; 3]) -> [f32; 3] {
    let r = srgb_to_linear(rgb[0]);
    let g = srgb_to_linear(rgb[1]);
    let b = srgb_to_linear(rgb[2]);

    let l = 0.412_221_47 * r + 0.536_332_54 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_84 * g + 0.629_978_7 * b;

    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();

    [
        0.210_454_26 * l_ + 0.793_617_8 * m_ - 0.004_072_047 * s_,
        1.977_998_5 * l_ - 2.428_592_2 * m_ + 0.450_593_7 * s_,
        0.025_904_037 * l_ + 0.782_771_77 * m_ - 0.808_675_77 * s_,
    ]
}

/// OKLab `[L, a, b]` → gamma sRGB (channels clamped to `0..=1`).
pub fn oklab_to_rgb(lab: [f32; 3]) -> [f32; 3] {
    let [l, a, b] = lab;
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;

    let l3 = l_ * l_ * l_;
    let m3 = m_ * m_ * m_;
    let s3 = s_ * s_ * s_;

    let r = 4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3;
    let g = -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3;
    let b = -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3;

    [
        linear_to_srgb(r.clamp(0.0, 1.0)),
        linear_to_srgb(g.clamp(0.0, 1.0)),
        linear_to_srgb(b.clamp(0.0, 1.0)),
    ]
}

/// Gamma sRGB → OKLCH `[L, C, h]` (`L ∈ [0,1]`, `C ≥ 0`, `h ∈ [0,360)`).
pub fn rgb_to_oklch(rgb: [f32; 3]) -> [f32; 3] {
    let [l, a, b] = rgb_to_oklab(rgb);
    let c = (a * a + b * b).sqrt();
    let h = b.atan2(a).to_degrees().rem_euclid(360.0);
    [l, c, h]
}

/// OKLCH `[L, C, h]` → gamma sRGB.
pub fn oklch_to_rgb(lch: [f32; 3]) -> [f32; 3] {
    let [l, c, h] = lch;
    let hr = h.to_radians();
    oklab_to_rgb([l, c * hr.cos(), c * hr.sin()])
}

// ── Hex ───────────────────────────────────────────────────────────────────────

/// Parse `#RGB`, `#RGBA`, `#RRGGBB`, or `#RRGGBBAA` (with or without `#`) into
/// gamma sRGB `[f32; 4]`. Alpha defaults to 1.0 when absent.
pub fn parse_hex(s: &str) -> Option<[f32; 4]> {
    let h = s.trim().trim_start_matches('#');
    let bytes = |slice: &str| u8::from_str_radix(slice, 16).ok();
    let dup = |c: char| c.to_digit(16).map(|d| (d * 17) as u8);

    match h.len() {
        3 => {
            let mut it = h.chars();
            let r = dup(it.next()?)?;
            let g = dup(it.next()?)?;
            let b = dup(it.next()?)?;
            Some(bytes_to_rgba([r, g, b, 255]))
        }
        4 => {
            let mut it = h.chars();
            let r = dup(it.next()?)?;
            let g = dup(it.next()?)?;
            let b = dup(it.next()?)?;
            let a = dup(it.next()?)?;
            Some(bytes_to_rgba([r, g, b, a]))
        }
        6 => Some(bytes_to_rgba([
            bytes(&h[0..2])?,
            bytes(&h[2..4])?,
            bytes(&h[4..6])?,
            255,
        ])),
        8 => Some(bytes_to_rgba([
            bytes(&h[0..2])?,
            bytes(&h[2..4])?,
            bytes(&h[4..6])?,
            bytes(&h[6..8])?,
        ])),
        _ => None,
    }
}

/// Format gamma sRGB as `#RRGGBB` (or `#RRGGBBAA` when `alpha`), uppercase.
pub fn format_hex(rgba: [f32; 4], alpha: bool) -> String {
    let b = rgba_to_bytes(rgba);
    if alpha {
        format!("#{:02X}{:02X}{:02X}{:02X}", b[0], b[1], b[2], b[3])
    } else {
        format!("#{:02X}{:02X}{:02X}", b[0], b[1], b[2])
    }
}

fn bytes_to_rgba(b: [u8; 4]) -> [f32; 4] {
    [
        b[0] as f32 / 255.0,
        b[1] as f32 / 255.0,
        b[2] as f32 / 255.0,
        b[3] as f32 / 255.0,
    ]
}

/// Gamma sRGB `[f32; 4]` → `[u8; 4]` (rounded).
pub fn rgba_to_bytes(rgba: [f32; 4]) -> [u8; 4] {
    [
        (rgba[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgba[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgba[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgba[3].clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

// ── WCAG contrast ─────────────────────────────────────────────────────────────

/// WCAG relative luminance of a gamma sRGB color (ignores alpha).
pub fn relative_luminance(rgb: [f32; 3]) -> f32 {
    let r = srgb_to_linear(rgb[0]);
    let g = srgb_to_linear(rgb[1]);
    let b = srgb_to_linear(rgb[2]);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// WCAG contrast ratio between two gamma sRGB colors, `1.0..=21.0`.
pub fn contrast_ratio(a: [f32; 3], b: [f32; 3]) -> f32 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

// ── Color-blindness simulation ────────────────────────────────────────────────

/// The three common dichromacies for simulation previews.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ColorVisionDeficiency {
    /// Red-blind.
    Protanopia,
    /// Green-blind.
    Deuteranopia,
    /// Blue-blind.
    Tritanopia,
}

/// Simulate how a gamma sRGB color appears under a dichromacy. Uses the
/// widely-published Machado et al. (2009) severity-1.0 matrices applied in
/// linear sRGB.
pub fn simulate_cvd(rgb: [f32; 3], kind: ColorVisionDeficiency) -> [f32; 3] {
    let m = match kind {
        ColorVisionDeficiency::Protanopia => [
            [0.152_286, 1.052_583, -0.204_868],
            [0.114_503, 0.786_281, 0.099_216],
            [-0.003_882, -0.048_116, 1.051_998],
        ],
        ColorVisionDeficiency::Deuteranopia => [
            [0.367_322, 0.860_646, -0.227_968],
            [0.280_085, 0.672_501, 0.047_413],
            [-0.011_820, 0.042_940, 0.968_881],
        ],
        ColorVisionDeficiency::Tritanopia => [
            [1.255_528, -0.076_749, -0.178_779],
            [-0.078_411, 0.930_809, 0.147_602],
            [0.004_733, 0.691_367, 0.303_900],
        ],
    };
    let lin = [
        srgb_to_linear(rgb[0]),
        srgb_to_linear(rgb[1]),
        srgb_to_linear(rgb[2]),
    ];
    let out = [
        m[0][0] * lin[0] + m[0][1] * lin[1] + m[0][2] * lin[2],
        m[1][0] * lin[0] + m[1][1] * lin[1] + m[1][2] * lin[2],
        m[2][0] * lin[0] + m[2][1] * lin[1] + m[2][2] * lin[2],
    ];
    [
        linear_to_srgb(out[0].clamp(0.0, 1.0)),
        linear_to_srgb(out[1].clamp(0.0, 1.0)),
        linear_to_srgb(out[2].clamp(0.0, 1.0)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3], eps: f32) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < eps)
    }

    #[test]
    fn hsv_roundtrips() {
        for rgb in [[0.2, 0.7, 0.4], [1.0, 0.0, 0.0], [0.5, 0.5, 0.5], [0.0; 3]] {
            assert!(close(hsv_to_rgb(rgb_to_hsv(rgb)), rgb, 1e-4), "{rgb:?}");
        }
    }

    #[test]
    fn hsl_roundtrips() {
        for rgb in [[0.2, 0.7, 0.4], [0.9, 0.1, 0.6], [0.3, 0.3, 0.3]] {
            assert!(close(hsl_to_rgb(rgb_to_hsl(rgb)), rgb, 1e-4), "{rgb:?}");
        }
    }

    #[test]
    fn oklab_and_oklch_roundtrip() {
        for rgb in [[0.2, 0.7, 0.4], [0.95, 0.4, 0.1], [0.05, 0.05, 0.2]] {
            assert!(
                close(oklab_to_rgb(rgb_to_oklab(rgb)), rgb, 2e-3),
                "lab {rgb:?}"
            );
            assert!(
                close(oklch_to_rgb(rgb_to_oklch(rgb)), rgb, 2e-3),
                "lch {rgb:?}"
            );
        }
    }

    #[test]
    fn oklab_white_is_lightness_one() {
        let [l, a, b] = rgb_to_oklab([1.0, 1.0, 1.0]);
        assert!((l - 1.0).abs() < 1e-3);
        assert!(a.abs() < 1e-3 && b.abs() < 1e-3);
    }

    #[test]
    fn hex_parse_and_format() {
        assert_eq!(parse_hex("#ff0000"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_hex("00ff00"), Some([0.0, 1.0, 0.0, 1.0]));
        assert_eq!(parse_hex("#fff"), Some([1.0, 1.0, 1.0, 1.0]));
        assert_eq!(
            parse_hex("#12345678").map(rgba_to_bytes),
            Some([0x12, 0x34, 0x56, 0x78])
        );
        assert_eq!(parse_hex("nope"), None);
        assert_eq!(format_hex([1.0, 0.0, 0.0, 1.0], false), "#FF0000");
        assert_eq!(format_hex([1.0, 0.0, 0.0, 0.5], true), "#FF000080");
    }

    #[test]
    fn contrast_extremes() {
        let cr = contrast_ratio([0.0; 3], [1.0; 3]);
        assert!((cr - 21.0).abs() < 0.1, "black/white ≈ 21:1, got {cr}");
        assert!((contrast_ratio([0.5; 3], [0.5; 3]) - 1.0).abs() < 1e-3);
    }
}
