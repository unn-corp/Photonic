//! Versioned measurement contract for optional native shot-balance assistance.
//! These are scene-linear AP1 statistics, never statistics inferred from an
//! output-transformed reference PNG. Version 1 fixes the eligibility, histogram
//! range and trimming contract; changing those rules requires a new version.

use serde::{Deserialize, Serialize};

pub const NATIVE_BALANCE_MAX_PIXELS: u64 = 16_777_216;
pub const NATIVE_BALANCE_MIN_LOG2: f64 = -20.0;
pub const NATIVE_BALANCE_MAX_LOG2: f64 = 20.0;
pub const NATIVE_BALANCE_HISTOGRAM_BINS: usize = 4096;
/// Exact persisted units; avoids platform/JSON f64 roundtrip drift.
pub const NATIVE_BALANCE_LOG_UNITS_PER_STOP: i32 = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeBalanceDomain {
    SceneLinearAcescg,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NativeBalanceSummary {
    pub version: u32,
    pub domain: NativeBalanceDomain,
    pub width: u32,
    pub height: u32,
    /// Per-channel positive, bounded AP1 samples from alpha >= 0.98 pixels.
    pub eligible_pixels: [u32; 3],
    /// Samples left after removing floor(eligible / 10) at each end.
    pub retained_pixels: [u32; 3],
    /// Trimmed means of log2(straight AP1 channel), in 2^-20 stop units.
    /// Exact integer persistence; not nits or SDR codes.
    pub trimmed_mean_log2_q20: [i32; 3],
}

/// The scene measurement and display PNG were captured from the same frame.
/// The hash binds assistance metadata to that immutable gallery image. Older
/// display-only references deliberately have no native balance measurement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NativeReferenceBalance {
    pub summary: NativeBalanceSummary,
    pub image_hash: String,
}

impl NativeReferenceBalance {
    pub fn validate_for_image(
        &self,
        image_hash: &str,
        width: u32,
        height: u32,
    ) -> Result<(), &'static str> {
        self.summary.validate()?;
        if self.image_hash.len() != 32
            || !self.image_hash.bytes().all(|b| b.is_ascii_hexdigit())
            || self.image_hash != image_hash
            || (self.summary.width, self.summary.height) != (width, height)
        {
            return Err("native balance measurement does not match the reference image");
        }
        Ok(())
    }
}

impl NativeBalanceSummary {
    pub fn trimmed_mean_log2(&self) -> [f64; 3] {
        self.trimmed_mean_log2_q20
            .map(|n| f64::from(n) / f64::from(NATIVE_BALANCE_LOG_UNITS_PER_STOP))
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1 {
            return Err("unsupported native balance summary version");
        }
        let total = u64::from(self.width) * u64::from(self.height);
        if total == 0 || total > NATIVE_BALANCE_MAX_PIXELS {
            return Err("native balance summary has invalid or excessive dimensions");
        }
        let minimum = total.div_ceil(20).max(16);
        for channel in 0..3 {
            let eligible = u64::from(self.eligible_pixels[channel]);
            let retained = u64::from(self.retained_pixels[channel]);
            if eligible < minimum || eligible > total || retained != eligible - 2 * (eligible / 10)
            {
                return Err("native balance summary has inconsistent sample coverage");
            }
            let mean = self.trimmed_mean_log2_q20[channel];
            if !(-20 * NATIVE_BALANCE_LOG_UNITS_PER_STOP..20 * NATIVE_BALANCE_LOG_UNITS_PER_STOP)
                .contains(&mean)
            {
                return Err("native balance summary has invalid log means");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> NativeBalanceSummary {
        NativeBalanceSummary {
            version: 1,
            domain: NativeBalanceDomain::SceneLinearAcescg,
            width: 10,
            height: 10,
            eligible_pixels: [100; 3],
            retained_pixels: [80; 3],
            trimmed_mean_log2_q20: [
                -2 * NATIVE_BALANCE_LOG_UNITS_PER_STOP,
                NATIVE_BALANCE_LOG_UNITS_PER_STOP,
                10 * NATIVE_BALANCE_LOG_UNITS_PER_STOP,
            ],
        }
    }

    #[test]
    fn native_balance_summary_roundtrip_and_contract_validation() {
        let summary = valid();
        assert!(summary.validate().is_ok());
        assert_eq!(
            serde_json::from_str::<NativeBalanceSummary>(&serde_json::to_string(&summary).unwrap())
                .unwrap(),
            summary
        );
        for mutate in [
            |s: &mut NativeBalanceSummary| s.version = 2,
            |s: &mut NativeBalanceSummary| s.width = 0,
            |s: &mut NativeBalanceSummary| s.width = u32::MAX,
            |s: &mut NativeBalanceSummary| s.eligible_pixels[0] = 101,
            |s: &mut NativeBalanceSummary| s.eligible_pixels[1] = 15,
            |s: &mut NativeBalanceSummary| s.retained_pixels[2] = 79,
            |s: &mut NativeBalanceSummary| s.trimmed_mean_log2_q20[0] = i32::MIN,
            |s: &mut NativeBalanceSummary| s.trimmed_mean_log2_q20[1] = i32::MAX,
            |s: &mut NativeBalanceSummary| {
                s.trimmed_mean_log2_q20[2] = -20 * NATIVE_BALANCE_LOG_UNITS_PER_STOP - 1
            },
            |s: &mut NativeBalanceSummary| {
                s.trimmed_mean_log2_q20[0] = 20 * NATIVE_BALANCE_LOG_UNITS_PER_STOP
            },
        ] {
            let mut broken = summary.clone();
            mutate(&mut broken);
            assert!(broken.validate().is_err(), "{broken:?}");
        }
        let mut wrong_domain = serde_json::to_value(&summary).unwrap();
        wrong_domain["domain"] = serde_json::json!("srgb_display");
        assert!(serde_json::from_value::<NativeBalanceSummary>(wrong_domain).is_err());
    }

    #[test]
    fn native_balance_coverage_floor_and_pixel_limit_are_exact() {
        let mut summary = valid();
        summary.width = 4096;
        summary.height = 4096;
        let minimum = (NATIVE_BALANCE_MAX_PIXELS.div_ceil(20)) as u32;
        summary.eligible_pixels = [minimum; 3];
        summary.retained_pixels = [minimum - 2 * (minimum / 10); 3];
        assert!(summary.validate().is_ok());
        summary.eligible_pixels[0] -= 1;
        summary.retained_pixels[0] =
            summary.eligible_pixels[0] - 2 * (summary.eligible_pixels[0] / 10);
        assert!(summary.validate().is_err());
        summary = valid();
        summary.width = 4;
        summary.height = 4;
        summary.eligible_pixels = [16; 3];
        summary.retained_pixels = [14; 3];
        assert!(summary.validate().is_ok());
        summary.width = 3;
        assert!(summary.validate().is_err());
    }

    #[test]
    fn native_balance_reference_is_bound_to_exact_image_and_dimensions() {
        let reference = NativeReferenceBalance {
            summary: valid(),
            image_hash: "a".repeat(32),
        };
        assert!(reference
            .validate_for_image(&"a".repeat(32), 10, 10)
            .is_ok());
        assert!(reference
            .validate_for_image(&"b".repeat(32), 10, 10)
            .is_err());
        assert!(reference
            .validate_for_image(&"a".repeat(32), 20, 5)
            .is_err());
        let mut broken = reference.clone();
        broken.image_hash = "z".repeat(32);
        assert!(broken.validate_for_image(&"z".repeat(32), 10, 10).is_err());
        let mut broken = reference;
        broken.summary.version = 2;
        assert!(broken.validate_for_image(&"a".repeat(32), 10, 10).is_err());
    }
}
