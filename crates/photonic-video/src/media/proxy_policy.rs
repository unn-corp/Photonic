//! Adaptive preview proxy selection.
//!
//! This policy is intentionally pure and does not inspect codecs, files, or
//! hardware. The session supplies one [`PreviewPressure`] sample per completed
//! preview frame and whether the asset has a ready proxy. That keeps the
//! decision deterministic and makes an unavailable proxy a correctness-safe
//! fallback to the original media.
//!
//! A proxy is selected after three consecutive pressured samples: either a
//! missed frame or total decode-plus-evaluation work at/above the frame budget.
//! It is released only after 120 consecutive relaxed samples with no misses and
//! work at/below 70% of budget. The asymmetric thresholds prevent quality from
//! oscillating around the budget. At 60 fps, release needs roughly two seconds;
//! callers with irregular sampling should treat these as sample counts, not
//! wall-clock guarantees.
//!
//! The policy deliberately does not estimate decode time from proxy dimensions
//! or assume that a proxy is faster. Some devices decode a source more
//! efficiently than its proxy, so the selection is driven solely by observed
//! preview pressure. It also does not change export behavior: exports must
//! continue to explicitly choose their desired input.

use std::time::Duration;

/// Number of consecutive pressured preview samples required before using a
/// ready proxy.
pub const PRESSURE_SAMPLES_BEFORE_PROXY: u32 = 3;

/// Number of consecutive relaxed preview samples required before returning to
/// the original input.
pub const RELAXED_SAMPLES_BEFORE_ORIGINAL: u32 = 120;

/// The relaxed-work threshold as a fraction of the frame budget.
pub const RELAXED_BUDGET_NUMERATOR: u32 = 7;
/// The denominator for [`RELAXED_BUDGET_NUMERATOR`].
pub const RELAXED_BUDGET_DENOMINATOR: u32 = 10;

/// Observed work and presentation pressure for one completed preview frame.
///
/// `decode_time` and `evaluate_time` are intentionally explicit inputs rather
/// than inferred from wall-clock time. Their sum is a conservative estimate of
/// the preview work attributable to media decode and graph/GPU evaluation. A
/// zero `frame_budget` means timing is unavailable; in that case only
/// `missed_frames` can trigger proxy selection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PreviewPressure {
    /// Time spent obtaining the media frame for preview.
    pub decode_time: Duration,
    /// Time spent evaluating and preparing the preview frame.
    pub evaluate_time: Duration,
    /// Presentation budget for this frame, normally one frame interval.
    pub frame_budget: Duration,
    /// Number of missed or held presentations observed for this sample.
    pub missed_frames: u32,
}

impl PreviewPressure {
    /// Returns true when this sample should move preview toward a proxy.
    ///
    /// A missed frame is immediately pressured even when timing is absent. An
    /// overflowing duration sum is treated as over-budget rather than wrapping.
    pub fn is_pressured(self) -> bool {
        self.missed_frames > 0
            || (!self.frame_budget.is_zero()
                && self
                    .total_work()
                    .is_none_or(|work| work >= self.frame_budget))
    }

    /// Returns true only for a stable, comfortably under-budget sample.
    ///
    /// A zero budget is not relaxed: without a timing contract, returning to
    /// original media based on missing information can create an avoidable
    /// quality oscillation.
    pub fn is_relaxed(self) -> bool {
        if self.missed_frames > 0 || self.frame_budget.is_zero() {
            return false;
        }

        let Some(threshold) = self
            .frame_budget
            .checked_mul(RELAXED_BUDGET_NUMERATOR)
            .map(|value| value / RELAXED_BUDGET_DENOMINATOR)
        else {
            // An unrepresentably large budget cannot occur in normal preview
            // operation. Conservatively retain the proxy if it does.
            return false;
        };

        self.total_work().is_some_and(|work| work <= threshold)
    }

    fn total_work(self) -> Option<Duration> {
        self.decode_time.checked_add(self.evaluate_time)
    }
}

/// Input selected for a preview decode.
///
/// [`Original`](Self::Original) is both the normal high-quality choice and the
/// mandatory fallback when an adaptive proxy is not ready.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PreviewMediaChoice {
    /// Decode the original media.
    #[default]
    Original,
    /// Decode a ready proxy.
    Proxy,
}

/// Stateful, deterministic hysteresis policy for adaptive preview proxies.
///
/// The type is deliberately not synchronized: its owner should be the preview
/// session/engine thread, which avoids locks on the presentation path. Call
/// [`observe`](Self::observe) once per completed preview frame. If a proxy
/// disappears, passing `false` for `proxy_ready` resets history and immediately
/// returns [`PreviewMediaChoice::Original`].
#[derive(Clone, Debug, Default)]
pub struct AdaptiveProxyPolicy {
    choice: PreviewMediaChoice,
    pressured_samples: u32,
    relaxed_samples: u32,
}

impl AdaptiveProxyPolicy {
    /// Construct a policy that starts on the original media.
    pub const fn new() -> Self {
        Self {
            choice: PreviewMediaChoice::Original,
            pressured_samples: 0,
            relaxed_samples: 0,
        }
    }

    /// Incorporate a completed-frame sample and return the input to use next.
    ///
    /// Three consecutive pressured samples select a ready proxy. Once on a
    /// proxy, only 120 consecutive relaxed samples return to the original. A
    /// neutral sample resets the applicable run, so isolated stalls and brief
    /// recoveries cannot flip quality back and forth.
    pub fn observe(&mut self, sample: PreviewPressure, proxy_ready: bool) -> PreviewMediaChoice {
        if !proxy_ready {
            self.reset();
            return PreviewMediaChoice::Original;
        }

        match self.choice {
            PreviewMediaChoice::Original => {
                self.relaxed_samples = 0;
                if sample.is_pressured() {
                    self.pressured_samples = self.pressured_samples.saturating_add(1);
                    if self.pressured_samples >= PRESSURE_SAMPLES_BEFORE_PROXY {
                        self.choice = PreviewMediaChoice::Proxy;
                        self.pressured_samples = 0;
                    }
                } else {
                    self.pressured_samples = 0;
                }
            }
            PreviewMediaChoice::Proxy => {
                self.pressured_samples = 0;
                if sample.is_relaxed() {
                    self.relaxed_samples = self.relaxed_samples.saturating_add(1);
                    if self.relaxed_samples >= RELAXED_SAMPLES_BEFORE_ORIGINAL {
                        self.choice = PreviewMediaChoice::Original;
                        self.relaxed_samples = 0;
                    }
                } else {
                    self.relaxed_samples = 0;
                }
            }
        }

        self.choice
    }

    /// Reset to original media and discard accumulated hysteresis history.
    pub fn reset(&mut self) {
        self.choice = PreviewMediaChoice::Original;
        self.pressured_samples = 0;
        self.relaxed_samples = 0;
    }

    /// Current choice before another sample is observed.
    pub const fn choice(&self) -> PreviewMediaChoice {
        self.choice
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pressure() -> PreviewPressure {
        PreviewPressure {
            decode_time: Duration::from_millis(20),
            evaluate_time: Duration::from_millis(14),
            frame_budget: Duration::from_millis(33),
            missed_frames: 0,
        }
    }

    fn relaxed() -> PreviewPressure {
        PreviewPressure {
            decode_time: Duration::from_millis(10),
            evaluate_time: Duration::from_millis(10),
            frame_budget: Duration::from_millis(33),
            missed_frames: 0,
        }
    }

    #[test]
    fn switches_to_a_ready_proxy_only_after_sustained_pressure() {
        let mut policy = AdaptiveProxyPolicy::new();
        for _ in 0..PRESSURE_SAMPLES_BEFORE_PROXY - 1 {
            assert_eq!(
                policy.observe(pressure(), true),
                PreviewMediaChoice::Original
            );
        }

        assert_eq!(policy.observe(pressure(), true), PreviewMediaChoice::Proxy);
    }

    #[test]
    fn returns_to_original_only_after_sustained_relief() {
        let mut policy = AdaptiveProxyPolicy::new();
        for _ in 0..PRESSURE_SAMPLES_BEFORE_PROXY {
            policy.observe(pressure(), true);
        }
        assert_eq!(policy.choice(), PreviewMediaChoice::Proxy);

        for _ in 0..RELAXED_SAMPLES_BEFORE_ORIGINAL - 1 {
            assert_eq!(policy.observe(relaxed(), true), PreviewMediaChoice::Proxy);
        }
        assert_eq!(
            policy.observe(relaxed(), true),
            PreviewMediaChoice::Original
        );
    }

    #[test]
    fn neutral_and_pressured_samples_break_a_relief_run() {
        let mut policy = AdaptiveProxyPolicy::new();
        for _ in 0..PRESSURE_SAMPLES_BEFORE_PROXY {
            policy.observe(pressure(), true);
        }

        for _ in 0..RELAXED_SAMPLES_BEFORE_ORIGINAL - 1 {
            policy.observe(relaxed(), true);
        }
        let neutral = PreviewPressure {
            decode_time: Duration::from_millis(24),
            evaluate_time: Duration::from_millis(3),
            frame_budget: Duration::from_millis(33),
            missed_frames: 0,
        };
        assert!(!neutral.is_pressured());
        assert!(!neutral.is_relaxed());
        assert_eq!(policy.observe(neutral, true), PreviewMediaChoice::Proxy);
        assert_eq!(policy.observe(relaxed(), true), PreviewMediaChoice::Proxy);
    }

    #[test]
    fn unavailable_proxy_immediately_falls_back_and_resets_history() {
        let mut policy = AdaptiveProxyPolicy::new();
        for _ in 0..PRESSURE_SAMPLES_BEFORE_PROXY {
            policy.observe(pressure(), true);
        }
        assert_eq!(policy.choice(), PreviewMediaChoice::Proxy);

        assert_eq!(
            policy.observe(pressure(), false),
            PreviewMediaChoice::Original
        );
        for _ in 0..PRESSURE_SAMPLES_BEFORE_PROXY - 1 {
            assert_eq!(
                policy.observe(pressure(), true),
                PreviewMediaChoice::Original
            );
        }
    }

    #[test]
    fn missed_frame_is_pressure_without_a_timing_budget() {
        let missed = PreviewPressure {
            frame_budget: Duration::ZERO,
            missed_frames: 1,
            ..PreviewPressure::default()
        };
        assert!(missed.is_pressured());
        assert!(!missed.is_relaxed());

        let mut policy = AdaptiveProxyPolicy::new();
        for _ in 0..PRESSURE_SAMPLES_BEFORE_PROXY {
            policy.observe(missed, true);
        }
        assert_eq!(policy.choice(), PreviewMediaChoice::Proxy);
    }

    #[test]
    fn overflowing_work_is_conservatively_pressured() {
        let sample = PreviewPressure {
            decode_time: Duration::MAX,
            evaluate_time: Duration::from_nanos(1),
            frame_budget: Duration::from_millis(33),
            missed_frames: 0,
        };
        assert!(sample.is_pressured());
        assert!(!sample.is_relaxed());
    }

    #[test]
    fn exact_budget_boundaries_have_no_dead_zone() {
        let budget = Duration::from_millis(100);
        let at_budget = PreviewPressure {
            decode_time: Duration::from_millis(70),
            evaluate_time: Duration::from_millis(30),
            frame_budget: budget,
            missed_frames: 0,
        };
        let at_relaxed_boundary = PreviewPressure {
            decode_time: Duration::from_millis(40),
            evaluate_time: Duration::from_millis(30),
            frame_budget: budget,
            missed_frames: 0,
        };

        assert!(at_budget.is_pressured());
        assert!(!at_budget.is_relaxed());
        assert!(!at_relaxed_boundary.is_pressured());
        assert!(at_relaxed_boundary.is_relaxed());
    }

    #[test]
    fn pressure_without_a_ready_proxy_never_accumulates() {
        let mut policy = AdaptiveProxyPolicy::new();
        for _ in 0..PRESSURE_SAMPLES_BEFORE_PROXY + 1 {
            assert_eq!(
                policy.observe(pressure(), false),
                PreviewMediaChoice::Original
            );
        }
        for _ in 0..PRESSURE_SAMPLES_BEFORE_PROXY - 1 {
            policy.observe(pressure(), true);
        }
        assert_eq!(policy.choice(), PreviewMediaChoice::Original);
    }
}
