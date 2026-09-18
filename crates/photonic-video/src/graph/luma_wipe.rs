//! Analytical luma-map wipe generators (26 K-B7).
//!
//! Produces per-pixel switch times in `[0, 1]` (black first → white last) at
//! any resolution, full float precision — Photonic-authored maths, not
//! bundled GPL assets. Bound by `TransitionKind::LumaWipe` → `IrOp::LumaWipeMix`
//! (26 K-B7).

/// Built-in wipe families (MLT-style type 0/1/3 patterns restated in Photonic
/// vocabulary — clean-room).
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum LumaWipeKind {
    /// Left-to-right linear bar.
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

/// Switch time at normalised pixel `(u, v)` in `[0, 1]²` (origin top-left).
/// Result in `[0, 1]`; invert flips so white switches first.
pub fn luma_at(kind: LumaWipeKind, u: f32, v: f32, invert: bool) -> f32 {
    let u = u.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let t = match kind {
        LumaWipeKind::LinearH => u,
        LumaWipeKind::LinearV => v,
        LumaWipeKind::Radial => {
            let dx = u - 0.5;
            let dy = v - 0.5;
            // Max distance to a corner is √0.5; normalise so corner → 1.
            let r = (dx * dx + dy * dy).sqrt();
            (r / std::f32::consts::FRAC_1_SQRT_2).clamp(0.0, 1.0)
            // FRAC_1_SQRT_2 == √0.5
        }
        LumaWipeKind::BarnDoorH => (2.0 * (u - 0.5).abs()).clamp(0.0, 1.0),
        LumaWipeKind::Clock => {
            let dx = u - 0.5;
            let dy = v - 0.5;
            // atan2: (-π, π] → [0, 1) clockwise from +x.
            let a = dy.atan2(dx); // -π..π from +x, CCW
            let mut t = (-a + std::f32::consts::PI) / (2.0 * std::f32::consts::PI);
            if t >= 1.0 {
                t = 0.0;
            }
            t
        }
    };
    let t = t.clamp(0.0, 1.0);
    if invert {
        1.0 - t
    } else {
        t
    }
}

/// Fill a row-major greyscale buffer (`w * h` values in `[0, 1]`).
pub fn fill_map(kind: LumaWipeKind, w: u32, h: u32, invert: bool, out: &mut [f32]) {
    assert_eq!(out.len(), (w * h) as usize);
    let wf = w.max(1) as f32;
    let hf = h.max(1) as f32;
    for y in 0..h {
        for x in 0..w {
            let u = (x as f32 + 0.5) / wf;
            let v = (y as f32 + 0.5) / hf;
            out[(y * w + x) as usize] = luma_at(kind, u, v, invert);
        }
    }
}

/// Soft wipe mix factor at progress `t` ∈ [0,1] given map value `m` and
/// `softness` ∈ [0, 0.5]. Black (`m=0`) switches first.
pub fn soft_mix(t: f32, m: f32, softness: f32) -> f32 {
    let soft = softness.clamp(0.0, 0.5);
    if soft < 1e-6 {
        return if t >= m { 1.0 } else { 0.0 };
    }
    // smoothstep(m - soft, m + soft, t)
    let lo = m - soft;
    let hi = m + soft;
    if t <= lo {
        0.0
    } else if t >= hi {
        1.0
    } else {
        let x = ((t - lo) / (hi - lo)).clamp(0.0, 1.0);
        x * x * (3.0 - 2.0 * x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_h_monotone_left_to_right() {
        let left = luma_at(LumaWipeKind::LinearH, 0.0, 0.5, false);
        let mid = luma_at(LumaWipeKind::LinearH, 0.5, 0.5, false);
        let right = luma_at(LumaWipeKind::LinearH, 1.0, 0.5, false);
        assert!(left < mid && mid < right);
    }

    #[test]
    fn invert_flips() {
        let a = luma_at(LumaWipeKind::LinearV, 0.5, 0.25, false);
        let b = luma_at(LumaWipeKind::LinearV, 0.5, 0.25, true);
        assert!((a + b - 1.0).abs() < 1e-5);
    }

    #[test]
    fn soft_mix_edges() {
        assert_eq!(soft_mix(0.0, 0.5, 0.1), 0.0);
        assert_eq!(soft_mix(1.0, 0.5, 0.1), 1.0);
        let mid = soft_mix(0.5, 0.5, 0.1);
        assert!((mid - 0.5).abs() < 0.05);
    }

    #[test]
    fn fill_map_size() {
        let mut buf = vec![0.0f32; 8 * 8];
        fill_map(LumaWipeKind::Radial, 8, 8, false, &mut buf);
        assert!(buf.iter().all(|v| (0.0..=1.0).contains(v)));
        // Centre darker (switches first) than corner for radial.
        assert!(buf[4 * 8 + 4] < buf[0]);
    }
}
