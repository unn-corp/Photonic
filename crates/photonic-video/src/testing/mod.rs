//! Test-support surface shared across crates (29 §3 / CAP-019).
//!
//! Deliberately an ordinary always-compiled `pub mod` (NOT `#[cfg(test)]`):
//! integration tests in OTHER crates — the acceptance-story harness in
//! `crates/photonic-app/tests/` — cannot see a `cfg(test)` module, and the
//! whole point of this module is to be the single home of the frame-comparison
//! metric so `golden_frames.rs` and that harness cannot drift apart.

/// The 11 §1.2 two-layer frame-comparison metric + its sRGB PNG round-trip and
/// diff-heatmap helpers.
pub mod frame_compare;
