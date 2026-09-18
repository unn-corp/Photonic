//! Colour-space constants and CPU reference math for the video path
//! (03-render-color-pipeline.md §3.2/§4.1/§4.4).
//!
//! This is the **single source of truth** (§4.4 rule 1) for the YUV→linear
//! conversion constants: the WGSL shader (`crate::video::YUV_CONVERT_SHADER`)
//! and this module's `eval_cpu`-style reference implementation share these exact
//! literals, and `wgsl_yuv_constants_match_rust` (a `#[test]` in `video.rs`)
//! asserts the shader source contains each one, so an edit in one place without
//! the other fails CI. The transfer functions are the exact ITU-R BT.709 and
//! sRGB curves (§4.1) — the BT.709 curve owns the video-signal domain, the sRGB
//! curve owns the asset/display domain; they are never interchanged (§4.1).
//!
//! (03 §3.3 recommends eventually consolidating all transfer-function /
//! premultiply logic into `photonic_core::color`; that refactor is deferred —
//! the sRGB curve here duplicates `raster/adjust.rs` for now.)

// ── YUV→RGB matrix coefficients (§3.2 step 2) ──────────────────────────────────
// R = y' + CR_R·cr,  G = y' − CB_G·cb − CR_G·cr,  B = y' + CB_B·cb.
// The two G coefficients are stored as positive magnitudes (subtracted in the
// formula) so the WGSL literals match these constants byte-for-byte for the
// §4.4-rule-1 string-match test.
pub const BT709_CR_R: f32 = 1.5748;
pub const BT709_CB_G: f32 = 0.1873;
pub const BT709_CR_G: f32 = 0.4681;
pub const BT709_CB_B: f32 = 1.8556;

pub const BT601_CR_R: f32 = 1.402;
pub const BT601_CB_G: f32 = 0.344136;
pub const BT601_CR_G: f32 = 0.714136;
pub const BT601_CB_B: f32 = 1.772;

// ── Limited-range expansion (§3.2 step 1), in the 0..255 code domain ───────────
// Full range is the identity (luma) / centre-0.5 (chroma).
pub const LIMITED_LUMA_MIN: f32 = 16.0;
pub const LIMITED_LUMA_SCALE: f32 = 219.0;
pub const LIMITED_CHROMA_MIN: f32 = 16.0;
pub const LIMITED_CHROMA_SCALE: f32 = 224.0;
pub const CODE_MAX: f32 = 255.0;
pub const CHROMA_CENTRE: f32 = 0.5;

// ── ITU-R BT.709 transfer function (§3.2 step 3 / §4.1) ────────────────────────
pub const BT709_EOTF_THRESHOLD: f32 = 0.081; // signal domain: E' < 0.081 is linear
pub const BT709_OETF_THRESHOLD: f32 = 0.018; // scene domain:  E  < 0.018 is linear
pub const BT709_SLOPE: f32 = 4.5;
pub const BT709_ALPHA: f32 = 1.099;
pub const BT709_BETA: f32 = 0.099;
pub const BT709_GAMMA: f32 = 0.45; // OETF exponent; EOTF uses 1/0.45

// ── sRGB transfer function (§4.1, present pass §5) ─────────────────────────────
pub const SRGB_OETF_THRESHOLD: f32 = 0.0031308; // linear → gamma breakpoint
pub const SRGB_SLOPE: f32 = 12.92;
pub const SRGB_ALPHA: f32 = 1.055;
pub const SRGB_BETA: f32 = 0.055;
pub const SRGB_GAMMA_INV: f32 = 2.4; // OETF uses 1/2.4

/// YUV→RGB matrix selection.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Matrix {
    /// BT.709 — the HD+ default (03 §3.1).
    Bt709,
    /// BT.601 — the SD default.
    Bt601,
}

/// Video signal range.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Range {
    /// 16–235 luma / 16–240 chroma (broadcast).
    Limited,
    /// 0–255 (full-swing).
    Full,
}

/// Matrix + range selected from `MediaProbe.video.color` at import (03 §3.1).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Colorimetry {
    pub matrix: Matrix,
    pub range: Range,
}

impl Colorimetry {
    /// The broadcast-HD default when probe data is absent/ambiguous (03 §3.1).
    pub const BT709_LIMITED: Colorimetry = Colorimetry {
        matrix: Matrix::Bt709,
        range: Range::Limited,
    };
}

/// BT.709 EOTF: video signal (gamma) → scene-linear (§3.2 step 3). Exact inverse
/// of [`bt709_oetf`].
pub fn bt709_eotf(e_prime: f32) -> f32 {
    if e_prime < BT709_EOTF_THRESHOLD {
        e_prime / BT709_SLOPE
    } else {
        ((e_prime + BT709_BETA) / BT709_ALPHA).powf(1.0 / BT709_GAMMA)
    }
}

/// BT.709 OETF: scene-linear → video signal (gamma) (§4.1, export encode §3.5).
pub fn bt709_oetf(e: f32) -> f32 {
    if e < BT709_OETF_THRESHOLD {
        BT709_SLOPE * e
    } else {
        BT709_ALPHA * e.powf(BT709_GAMMA) - BT709_BETA
    }
}

/// sRGB OETF: scene-linear → sRGB gamma (present pass §5; mirrors
/// `raster/adjust.rs:83`).
pub fn srgb_oetf(c: f32) -> f32 {
    if c <= SRGB_OETF_THRESHOLD {
        SRGB_SLOPE * c
    } else {
        SRGB_ALPHA * c.powf(1.0 / SRGB_GAMMA_INV) - SRGB_BETA
    }
}

/// Range-expand a raw 0..1 luma/chroma sample into the video-signal domain
/// (§3.2 step 1): returns `(y', cb, cr)` with chroma centred on 0.
pub fn range_expand(y: f32, cb: f32, cr: f32, range: Range) -> (f32, f32, f32) {
    match range {
        Range::Full => (y, cb - CHROMA_CENTRE, cr - CHROMA_CENTRE),
        Range::Limited => {
            let yp = (y * CODE_MAX - LIMITED_LUMA_MIN) / LIMITED_LUMA_SCALE;
            let cbp = (cb * CODE_MAX - LIMITED_CHROMA_MIN) / LIMITED_CHROMA_SCALE - CHROMA_CENTRE;
            let crp = (cr * CODE_MAX - LIMITED_CHROMA_MIN) / LIMITED_CHROMA_SCALE - CHROMA_CENTRE;
            (yp, cbp, crp)
        }
    }
}

/// Full CPU reference for one YUV sample → **premultiplied linear** Rec.709
/// `[r, g, b, a]` — the exact operation order the WGSL shader runs (§3.2/§3.3,
/// §4.4 rule 2): range-expand → matrix → BT.709 EOTF → premultiply. `a_raw` is
/// straight (no transfer function, §3.2 step 4).
pub fn yuv_to_working(y: f32, cb: f32, cr: f32, a_raw: f32, cm: Colorimetry) -> [f32; 4] {
    let (yp, cbv, crv) = range_expand(y, cb, cr, cm.range);
    let (cr_r, cb_g, cr_g, cb_b) = match cm.matrix {
        Matrix::Bt709 => (BT709_CR_R, BT709_CB_G, BT709_CR_G, BT709_CB_B),
        Matrix::Bt601 => (BT601_CR_R, BT601_CB_G, BT601_CR_G, BT601_CB_B),
    };
    // Video-signal-domain RGB (still gamma-encoded per the OETF convention).
    // The two G coefficients are stored positive and subtracted (see the const
    // block) so this matches the WGSL exactly.
    let r = yp + cr_r * crv;
    let g = yp - cb_g * cbv - cr_g * crv;
    let b = yp + cb_b * cbv;
    // Decode to scene-linear, then premultiply by straight alpha.
    let lin = [bt709_eotf(r), bt709_eotf(g), bt709_eotf(b)];
    [lin[0] * a_raw, lin[1] * a_raw, lin[2] * a_raw, a_raw]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 03 §6 risk hook: BT.709 EOTF/OETF round-trip at reference values within
    /// 1e-4 (18% grey, 100% white, plus the linear-toe region and black).
    #[test]
    fn bt709_transfer_round_trips_at_reference_values() {
        for &scene in &[0.0_f32, 0.005, 0.018, 0.18, 0.5, 1.0] {
            let round = bt709_eotf(bt709_oetf(scene));
            assert!(
                (round - scene).abs() < 1e-4,
                "BT.709 round-trip at {scene}: got {round}"
            );
        }
        // 18% grey and 100% white encode to their published BT.709 signal
        // levels: OETF(0.18) ≈ 0.409, OETF(1.0) == 1.0.
        assert!((bt709_oetf(0.18) - 0.409).abs() < 1e-3);
        assert!((bt709_oetf(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn srgb_oetf_reference_points() {
        assert!((srgb_oetf(0.0) - 0.0).abs() < 1e-6);
        assert!((srgb_oetf(1.0) - 1.0).abs() < 1e-6);
        // Just above the linear toe: continuity holds within a hair.
        let lo = srgb_oetf(SRGB_OETF_THRESHOLD);
        assert!((lo - SRGB_SLOPE * SRGB_OETF_THRESHOLD).abs() < 1e-6);
    }

    #[test]
    fn limited_range_black_and_white() {
        // 16/255 luma (limited black) → ~0 signal; 235/255 → ~1 signal.
        let (yb, _, _) = range_expand(16.0 / 255.0, 0.5, 0.5, Range::Limited);
        let (yw, _, _) = range_expand(235.0 / 255.0, 0.5, 0.5, Range::Limited);
        assert!(yb.abs() < 1e-4, "limited black y'={yb}");
        assert!((yw - 1.0).abs() < 1e-4, "limited white y'={yw}");
    }

    #[test]
    fn premultiply_scales_rgb_by_alpha() {
        // Neutral grey at half alpha → premultiplied rgb is halved, alpha kept.
        let px = yuv_to_working(0.5, 0.5, 0.5, 0.5, Colorimetry::BT709_LIMITED);
        assert!((px[3] - 0.5).abs() < 1e-6);
        let straight = yuv_to_working(0.5, 0.5, 0.5, 1.0, Colorimetry::BT709_LIMITED);
        for i in 0..3 {
            assert!((px[i] - straight[i] * 0.5).abs() < 1e-4);
        }
    }
}
