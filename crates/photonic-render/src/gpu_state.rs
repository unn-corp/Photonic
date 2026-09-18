//! GPU health state machine and device-loss bookkeeping (37 §1.3).
//!
//! Device loss (a TDR, a driver reset, an eGPU unplug) invalidates the whole
//! `wgpu::Device` and every resource created from it. Recovery is: pause,
//! drop every GPU-resident cache (all reconstructible from the document plus a
//! content hash), re-request an adapter/device, rebuild, resume — with a small
//! bounded number of retries before we give up. This module owns only the
//! *state* of that episode: which phase we are in and how many attempts we
//! have spent. The engine-side cache teardown and rebuild live elsewhere and
//! observe this state through a cheaply-cloned [`GpuHealth`] handle.
//!
//! The state transitions are monotone within one loss episode:
//! `Healthy -> Lost -> Recovering -> {Healthy | Lost | Unrecoverable}`.
//! [`GpuState::Unrecoverable`] is absorbing — the only way out is a fresh
//! [`GpuHealth::new`].

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The phase of the GPU-health machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuState {
    /// Device is live; the normal steady state.
    Healthy,
    /// Device loss was detected at `at`; playback is paused, caches pending drop.
    Lost {
        /// When the loss was observed.
        at: Instant,
    },
    /// A recovery attempt is in flight (re-requesting adapter/device, rebuild).
    Recovering,
    /// Recovery failed `MAX_RECOVERY_ATTEMPTS` times; the session is fatal.
    Unrecoverable,
}

/// Maximum number of recovery attempts before declaring the GPU unrecoverable.
pub const MAX_RECOVERY_ATTEMPTS: u32 = 3;

/// Exponential backoff between recovery attempts: 100 ms, 400 ms, 1600 ms, …
///
/// Pure function of the attempt index (0-based); monotone increasing.
pub fn backoff_delay(attempt: u32) -> Duration {
    // 100ms * 4^attempt — 100, 400, 1600, ...
    let millis = 100u64.saturating_mul(4u64.saturating_pow(attempt));
    Duration::from_millis(millis)
}

#[derive(Debug)]
struct GpuHealthInner {
    state: Mutex<GpuState>,
    attempts: AtomicU32,
}

/// A cheaply-cloned handle to shared GPU-health state.
///
/// `Send + Sync`; cloning shares the same underlying state (an `Arc`), so the
/// renderer, the engine, and the app shell all observe one machine.
#[derive(Clone, Debug)]
pub struct GpuHealth {
    inner: Arc<GpuHealthInner>,
}

impl Default for GpuHealth {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuHealth {
    /// A fresh handle in the [`GpuState::Healthy`] state with zero attempts.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(GpuHealthInner {
                state: Mutex::new(GpuState::Healthy),
                attempts: AtomicU32::new(0),
            }),
        }
    }

    /// The current state.
    pub fn state(&self) -> GpuState {
        *self.inner.state.lock().expect("gpu state mutex poisoned")
    }

    /// The number of recovery attempts spent in the current episode.
    pub fn attempts(&self) -> u32 {
        self.inner.attempts.load(Ordering::SeqCst)
    }

    /// Record device loss. `Healthy` or `Recovering` transition to
    /// `Lost { at: now }`; a no-op once `Unrecoverable` (absorbing) and while
    /// already `Lost` (the timestamp of the first observation is kept).
    pub fn mark_lost(&self) {
        let mut state = self.inner.state.lock().expect("gpu state mutex poisoned");
        match *state {
            GpuState::Healthy | GpuState::Recovering => {
                *state = GpuState::Lost { at: Instant::now() };
            }
            GpuState::Lost { .. } | GpuState::Unrecoverable => {}
        }
    }

    /// Begin a recovery attempt: `Lost -> Recovering`, incrementing the attempt
    /// counter. Returns `false` (and flips to `Unrecoverable`) when the attempt
    /// budget `MAX_RECOVERY_ATTEMPTS` is already exhausted, or when called from
    /// a state other than `Lost`.
    pub fn begin_recovery(&self) -> bool {
        let mut state = self.inner.state.lock().expect("gpu state mutex poisoned");
        match *state {
            GpuState::Lost { .. } => {
                if self.inner.attempts.load(Ordering::SeqCst) >= MAX_RECOVERY_ATTEMPTS {
                    *state = GpuState::Unrecoverable;
                    false
                } else {
                    self.inner.attempts.fetch_add(1, Ordering::SeqCst);
                    *state = GpuState::Recovering;
                    true
                }
            }
            _ => false,
        }
    }

    /// Recovery succeeded: `Recovering -> Healthy`, attempts reset to 0. A no-op
    /// from any other state.
    pub fn mark_recovered(&self) {
        let mut state = self.inner.state.lock().expect("gpu state mutex poisoned");
        if matches!(*state, GpuState::Recovering) {
            *state = GpuState::Healthy;
            self.inner.attempts.store(0, Ordering::SeqCst);
        }
    }

    /// Force the absorbing `Unrecoverable` state.
    pub fn mark_unrecoverable(&self) {
        let mut state = self.inner.state.lock().expect("gpu state mutex poisoned");
        *state = GpuState::Unrecoverable;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_lost(s: GpuState) -> bool {
        matches!(s, GpuState::Lost { .. })
    }

    #[test]
    fn fresh_handle_is_healthy() {
        let h = GpuHealth::new();
        assert_eq!(h.state(), GpuState::Healthy);
        assert_eq!(h.attempts(), 0);
    }

    #[test]
    fn healthy_to_lost_to_recovering_to_healthy() {
        let h = GpuHealth::new();
        h.mark_lost();
        assert!(is_lost(h.state()));
        assert!(h.begin_recovery());
        assert_eq!(h.state(), GpuState::Recovering);
        assert_eq!(h.attempts(), 1);
        h.mark_recovered();
        assert_eq!(h.state(), GpuState::Healthy);
        assert_eq!(h.attempts(), 0);
    }

    #[test]
    fn recovering_can_relapse_to_lost() {
        let h = GpuHealth::new();
        h.mark_lost();
        assert!(h.begin_recovery());
        // A second loss during recovery is legal.
        h.mark_lost();
        assert!(is_lost(h.state()));
    }

    #[test]
    fn begin_recovery_only_from_lost() {
        let h = GpuHealth::new();
        // From Healthy: illegal, no-op false.
        assert!(!h.begin_recovery());
        assert_eq!(h.state(), GpuState::Healthy);
        assert_eq!(h.attempts(), 0);
    }

    #[test]
    fn fourth_attempt_flips_to_unrecoverable() {
        let h = GpuHealth::new();
        // Three legal attempts, each relapsing to Lost.
        for expected in 1..=MAX_RECOVERY_ATTEMPTS {
            h.mark_lost();
            assert!(h.begin_recovery());
            assert_eq!(h.attempts(), expected);
        }
        // Fourth: budget exhausted -> false and Unrecoverable.
        h.mark_lost();
        assert!(!h.begin_recovery());
        assert_eq!(h.state(), GpuState::Unrecoverable);
    }

    #[test]
    fn unrecoverable_absorbs_mark_lost() {
        let h = GpuHealth::new();
        h.mark_unrecoverable();
        assert_eq!(h.state(), GpuState::Unrecoverable);
        h.mark_lost();
        assert_eq!(h.state(), GpuState::Unrecoverable);
    }

    #[test]
    fn mark_recovered_is_noop_unless_recovering() {
        let h = GpuHealth::new();
        h.mark_lost();
        let before = h.state();
        h.mark_recovered(); // from Lost: no-op
        assert_eq!(h.state(), before);
    }

    #[test]
    fn backoff_is_monotone_increasing() {
        let d0 = backoff_delay(0);
        let d1 = backoff_delay(1);
        let d2 = backoff_delay(2);
        assert_eq!(d0, Duration::from_millis(100));
        assert_eq!(d1, Duration::from_millis(400));
        assert_eq!(d2, Duration::from_millis(1600));
        assert!(d0 < d1 && d1 < d2);
    }
}
