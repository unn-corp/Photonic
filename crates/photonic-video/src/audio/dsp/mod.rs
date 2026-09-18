//! Audio DSP units (09 §6) — P8 story.
//!
//! Every unit here is a [`DspUnit`]: it consumes one interleaved-stereo block
//! of already block-resolved params (already evaluated from `AnimProps` and
//! already de-zipper-smoothed by whatever calls it, 09 §5) — "no knowledge
//! of `Tick`/`AnimProps` inside the DSP layer itself" (09 §6 intro). Nothing
//! in this module touches `photonic_video::audio::mixer`, whose `fx_chain`
//! is still an inert pass-through (see that module's doc): the seam a future
//! wiring story needs is each unit's `set_params`/`params()` pair plus its
//! `*Params::from_effect_params(&EffectParams)` conversion (09 §2's
//! `AudioFxUnit.params: AnimProps<EffectParams>`, evaluated once per block
//! the same way `mixer::eval_track_audio_params` already evaluates
//! `TrackAudioParams` — see each submodule's `from_effect_params` doc for the
//! exact `PropPath` strings, which match `photonic_core::timeline::
//! prop_registry`'s `AUDIOFX_*` tables).
//!
//! Submodules: [`biquad`] (RBJ cookbook coefficient design, shared by [`eq`]
//! and [`loudness`]'s K-weighting filter), [`envelope`] (the §6.2 detector
//! shared by [`compressor`] and [`gate`]), [`eq`], [`compressor`], [`gate`],
//! [`limiter`], [`loudness`] (§6.6 EBU R128/BS.1770-4 — an offline analysis
//! pass over a full rendered buffer, not a per-block [`DspUnit`]; see that
//! module's doc for why).
//!
//! The four processing-unit types ([`eq::Eq`], [`compressor::Compressor`],
//! [`gate::Gate`], [`limiter::Limiter`]) are deliberately **not** re-exported
//! at this module's top level: `Eq` mirrors `AudioFxKind::Eq`'s name exactly
//! (for obvious kind<->unit discoverability), which would shadow
//! `std::cmp::Eq` if glob-imported — call sites use the explicit
//! `dsp::eq::Eq` path instead. The `*Params`/band types (no such collision)
//! are re-exported normally.

pub mod biquad;
pub mod compressor;
pub mod envelope;
pub mod eq;
pub mod gate;
pub mod limiter;
pub mod loudness;

use photonic_core::timeline::{AudioFxKind, EffectParams};

pub use compressor::CompressorParams;
pub use eq::{EqParams, PeakBand, ShelfBand};
pub use gate::GateParams;
pub use limiter::LimiterParams;

/// A single DSP processing unit (09 §6): one already-evaluated params set,
/// applied to one block. Every concrete unit ([`eq::Eq`],
/// [`compressor::Compressor`], [`gate::Gate`], [`limiter::Limiter`]) also
/// exposes an inherent `set_params`/`params` pair (not part of this trait,
/// since each takes a different concrete `*Params` type) — call `set_params`
/// once per block with that block's resolved values, then `process`.
pub trait DspUnit: Send {
    /// Process `block` **in place**: interleaved stereo
    /// (`photonic_video::audio::CHANNELS`-interleaved), any positive frame
    /// count divisible by `CHANNELS`. Production callers SHOULD pass exactly
    /// one mixer block (`photonic_video::audio::BLOCK_FRAMES` frames) per 09
    /// §5's "coefficients resolved once per block" budget — this trait
    /// doesn't enforce that length so standalone unit tests can process a
    /// whole tone burst/sweep in one call.
    ///
    /// `sample_rate` is the mixer's configured rate (`Mixer::sample_rate`) —
    /// passed per call rather than fixed at construction so a unit can be
    /// exercised at any rate in isolation.
    ///
    /// `sidechain`, when `Some`, is another buffer of the exact same length
    /// as `block` — the ducking source's post-fx-pre-fader signal (09 §6.3).
    /// Only [`compressor::Compressor`] consults it; every other unit ignores
    /// it.
    fn process(&mut self, sample_rate: u32, block: &mut [f32], sidechain: Option<&[f32]>);

    /// Discard all signal-dependent internal state.
    ///
    /// **Mandatory — there is deliberately no default implementation.** A unit
    /// that silently inherits a no-op `reset` is precisely the bug this
    /// contract exists to prevent: on the first seek after the fx chain is
    /// wired, every envelope would smear across the discontinuity and every
    /// biquad would ring, with nothing to indicate why (31 §2.2).
    ///
    /// Reset discards *state*, never *parameters* — a unit is immediately
    /// usable afterwards with the same `set_params` configuration.
    ///
    /// Reset is **not** a fade. Where a discontinuity is audible, the mixer's
    /// boundary declick handles it (31 §4); this only prevents stale state.
    fn reset(&mut self, cause: AudioDiscontinuity);

    /// Samples of delay this unit adds between input and output.
    ///
    /// The mixer sums these along each path and delay-compensates so every
    /// path reaching the master bus stays aligned, and subtracts the total
    /// when deriving the playhead so video does not lead audio (31 §3).
    fn latency_samples(&self) -> u32 {
        0
    }

    /// Samples of output that persist after the input goes silent.
    ///
    /// Offline export pulls this many extra samples of silence after the last
    /// input block so releases and delay lines are not truncated (31 §3.3).
    fn tail_samples(&self) -> u32 {
        0
    }
}

/// Why a [`DspUnit::reset`] is being requested.
///
/// Deliberately payload-free. The mixer-level event carries the `TrackId` and
/// `Tick` that decide *which* units to reset (31 §2.3's policy: `Seek` and
/// `Reconfigure` reset everything, `ClipBoundary` resets only units downstream
/// of that clip so a master-bus compressor does not pump at every edit). This
/// module keeps its stated boundary of "no knowledge of `Tick`/`AnimProps`
/// inside the DSP layer itself" — a unit needs to know *that* it was cut, not
/// *where*.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AudioDiscontinuity {
    /// The playhead jumped. Everything resets.
    Seek,
    /// A clip boundary was crossed on a track feeding this unit.
    ClipBoundary,
    /// The fx chain was edited — unit added, removed, or reordered.
    GraphChanged,
    /// Sample rate or channel count changed.
    Reconfigure,
}

/// A concrete, allocation-free wrapper over the four processing units — the
/// bridge between the model's [`AudioFxKind`] and a live [`DspUnit`] (31 §8
/// step 1). The mixer's fx chain is a `Vec<FxUnit>`, rendered every block, so
/// this is a plain enum (not `Box<dyn DspUnit>`): no per-unit heap indirection
/// on the hot path, matching `mixer.rs`'s existing "no per-block allocation"
/// discipline.
///
/// `dsp/` thereby depends on `photonic_core::timeline::{AudioFxKind,
/// EffectParams}`, but the module's stated "no `Tick`/`AnimProps`" boundary
/// still holds: `EffectParams` is the already-block-evaluated flat bag (each
/// submodule already imports it for `from_effect_params`), and `AnimProps`
/// evaluation to that bag stays in the mixer.
pub enum FxUnit {
    Eq(eq::Eq),
    Compressor(compressor::Compressor),
    Gate(gate::Gate),
    Limiter(limiter::Limiter),
}

impl FxUnit {
    /// Instantiate the concrete unit for `kind` with its own default params.
    ///
    /// A forward-compat [`AudioFxKind::Unknown`] (39 §2.2) has no known unit;
    /// it falls back to a default [`eq::Eq`], which — with every band at
    /// `gain_db == 0` — is a genuine pass-through and reports zero
    /// latency/tail, so an unknown unit renders inert. The mixer skips
    /// unknown-kind slots for processing regardless (see
    /// `mixer::FxChainState::process_block`), and caches the true spec kind
    /// separately for identity comparison, so [`FxUnit::kind`]'s inability to
    /// round-trip an unknown never matters there.
    pub fn new(kind: AudioFxKind) -> Self {
        match kind {
            AudioFxKind::Eq => FxUnit::Eq(eq::Eq::new()),
            AudioFxKind::Compressor => FxUnit::Compressor(compressor::Compressor::new()),
            AudioFxKind::Gate => FxUnit::Gate(gate::Gate::new()),
            AudioFxKind::Limiter => FxUnit::Limiter(limiter::Limiter::new()),
            _ => FxUnit::Eq(eq::Eq::new()),
        }
    }

    /// The concrete kind this unit realizes. Meaningful for units built from a
    /// known kind (the only case the mixer relies on).
    pub fn kind(&self) -> AudioFxKind {
        match self {
            FxUnit::Eq(_) => AudioFxKind::Eq,
            FxUnit::Compressor(_) => AudioFxKind::Compressor,
            FxUnit::Gate(_) => AudioFxKind::Gate,
            FxUnit::Limiter(_) => AudioFxKind::Limiter,
        }
    }

    /// Push a block-resolved param bag into the inner unit. Dispatches to the
    /// matching `*Params::from_effect_params` then the inner `set_params` —
    /// touches params only, never signal state (the inverse of the `reset`
    /// contract: reset clears state and keeps params, this sets params and
    /// keeps state).
    pub fn set_params_from(&mut self, p: &EffectParams) {
        match self {
            FxUnit::Eq(u) => u.set_params(EqParams::from_effect_params(p)),
            FxUnit::Compressor(u) => u.set_params(CompressorParams::from_effect_params(p)),
            FxUnit::Gate(u) => u.set_params(GateParams::from_effect_params(p)),
            FxUnit::Limiter(u) => u.set_params(LimiterParams::from_effect_params(p)),
        }
    }
}

impl DspUnit for FxUnit {
    fn process(&mut self, sample_rate: u32, block: &mut [f32], sidechain: Option<&[f32]>) {
        match self {
            FxUnit::Eq(u) => u.process(sample_rate, block, sidechain),
            FxUnit::Compressor(u) => u.process(sample_rate, block, sidechain),
            FxUnit::Gate(u) => u.process(sample_rate, block, sidechain),
            FxUnit::Limiter(u) => u.process(sample_rate, block, sidechain),
        }
    }

    fn reset(&mut self, cause: AudioDiscontinuity) {
        match self {
            FxUnit::Eq(u) => u.reset(cause),
            FxUnit::Compressor(u) => u.reset(cause),
            FxUnit::Gate(u) => u.reset(cause),
            FxUnit::Limiter(u) => u.reset(cause),
        }
    }

    fn latency_samples(&self) -> u32 {
        match self {
            FxUnit::Eq(u) => u.latency_samples(),
            FxUnit::Compressor(u) => u.latency_samples(),
            FxUnit::Gate(u) => u.latency_samples(),
            FxUnit::Limiter(u) => u.latency_samples(),
        }
    }

    fn tail_samples(&self) -> u32 {
        match self {
            FxUnit::Eq(u) => u.tail_samples(),
            FxUnit::Compressor(u) => u.tail_samples(),
            FxUnit::Gate(u) => u.tail_samples(),
            FxUnit::Limiter(u) => u.tail_samples(),
        }
    }
}

/// Per-sample one-pole coefficient for a time constant `tau_ms` at `fs` Hz —
/// the exact form 09 §6.2 gives for the envelope follower's attack/release,
/// reused for every other one-pole smoothing need in this module. Mirrors
/// `mixer.rs`'s identically-behaving private helper; duplicated rather than
/// imported so `dsp/` has zero dependency on `mixer.rs`, per this story's
/// scope boundary (mixer.rs is not to be edited or leaned on).
fn one_pole_coeff(tau_ms: f64, fs: f64) -> f64 {
    1.0 - (-1.0 / (tau_ms * 0.001 * fs)).exp()
}

#[inline]
fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

#[inline]
fn lin_to_db(lin: f64) -> f64 {
    20.0 * lin.max(1e-12).log10()
}

#[cfg(test)]
mod determinism_tests {
    //! "process twice = bit-identical" (09 §11's determinism test hook),
    //! covering every [`super::DspUnit`] impl against a fixed synthetic
    //! signal. A deterministic (non-crate, seeded) LCG stands in for noise —
    //! no `rand` dependency needed for a fixed, reproducible bit pattern.

    use super::compressor::{Compressor, CompressorParams};
    use super::eq::{Eq, EqParams, PeakBand};
    use super::gate::{Gate, GateParams};
    use super::limiter::{Limiter, LimiterParams};
    use super::DspUnit;
    use crate::audio::CHANNELS;

    const FS: u32 = 48_000;

    /// A fixed pseudo-random interleaved-stereo signal (deterministic LCG,
    /// not `rand`) mixed with a tone, so every unit sees varied, non-trivial
    /// input.
    fn fixed_signal(frames: usize) -> Vec<f32> {
        let mut out = vec![0f32; frames * CHANNELS];
        let mut state: u32 = 0x2545F491;
        for f in 0..frames {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (state as f32 / u32::MAX as f32) * 2.0 - 1.0;
            let t = f as f64 / FS as f64;
            let tone = (2.0 * std::f64::consts::PI * 440.0 * t).sin() as f32;
            let s = 0.5 * tone + 0.3 * noise;
            out[f * CHANNELS] = s;
            out[f * CHANNELS + 1] = s * 0.8;
        }
        out
    }

    /// Process `buf` forward in the given frame-count chunks (mimicking
    /// successive mixer blocks, to exercise cross-call state — envelope
    /// followers, limiter delay lines — exactly as the real mixer-
    /// worker/export loop would), then whatever's left in one final call.
    fn process_in_chunks<U: DspUnit>(unit: &mut U, buf: &mut [f32], chunk_frames_seq: &[usize]) {
        let mut offset = 0;
        for &chunk in chunk_frames_seq {
            let end = (offset + chunk * CHANNELS).min(buf.len());
            unit.process(FS, &mut buf[offset..end], None);
            offset = end;
            if offset >= buf.len() {
                return;
            }
        }
        let len = buf.len();
        unit.process(FS, &mut buf[offset..len], None);
    }

    fn run_twice<U: DspUnit, F: Fn() -> U>(make: F) -> (Vec<f32>, Vec<f32>) {
        let input = fixed_signal(4000);
        let chunks = [512, 512, 1024, 512, 512, 400, 240];

        let mut out_a = input.clone();
        let mut a = make();
        process_in_chunks(&mut a, &mut out_a, &chunks);

        let mut out_b = input;
        let mut b = make();
        process_in_chunks(&mut b, &mut out_b, &chunks);

        (out_a, out_b)
    }

    #[test]
    fn eq_is_deterministic() {
        let (a, b) = run_twice(|| {
            let mut u = Eq::new();
            let p = EqParams {
                band1: PeakBand {
                    freq_hz: 800.0,
                    gain_db: 6.0,
                    q: 1.2,
                },
                ..EqParams::default()
            };
            u.set_params(p);
            u
        });
        assert_eq!(a, b);
    }

    #[test]
    fn compressor_is_deterministic() {
        let (a, b) = run_twice(|| {
            let mut u = Compressor::new();
            u.set_params(CompressorParams {
                threshold_db: -18.0,
                ratio: 3.0,
                attack_ms: 5.0,
                release_ms: 80.0,
                makeup_db: 2.0,
            });
            u
        });
        assert_eq!(a, b);
    }

    #[test]
    fn gate_is_deterministic() {
        let (a, b) = run_twice(|| {
            let mut u = Gate::new();
            u.set_params(GateParams {
                threshold_db: -30.0,
                attack_ms: 1.0,
                hold_ms: 15.0,
                release_ms: 60.0,
                range_db: 40.0,
            });
            u
        });
        assert_eq!(a, b);
    }

    #[test]
    fn limiter_is_deterministic() {
        let (a, b) = run_twice(|| {
            let mut u = Limiter::new();
            u.set_params(LimiterParams {
                ceiling_db: -1.0,
                release_ms: 50.0,
            });
            u
        });
        assert_eq!(a, b);
    }
}

#[cfg(test)]
mod factory_tests {
    //! 31 §8 step 1 — the kind→unit factory and param binding.
    use super::*;
    use crate::audio::CHANNELS;
    use photonic_core::timeline::AudioFxUnit;

    const KINDS: [AudioFxKind; 4] = [
        AudioFxKind::Eq,
        AudioFxKind::Compressor,
        AudioFxKind::Gate,
        AudioFxKind::Limiter,
    ];

    #[test]
    fn new_reports_its_kind() {
        for k in KINDS {
            assert_eq!(FxUnit::new(k).kind(), k, "FxUnit::new({k:?}).kind()");
        }
    }

    /// `set_params_from` on the wrapper must be identical to calling the inner
    /// `*Params::from_effect_params` directly on the model's default bag.
    #[test]
    fn set_params_from_matches_direct_conversion() {
        for k in KINDS {
            let base = AudioFxUnit::new(k).params.base;
            let mut fx = FxUnit::new(k);
            fx.set_params_from(&base);
            match (&fx, k) {
                (FxUnit::Eq(u), AudioFxKind::Eq) => {
                    assert_eq!(*u.params(), EqParams::from_effect_params(&base));
                }
                (FxUnit::Compressor(u), AudioFxKind::Compressor) => {
                    assert_eq!(*u.params(), CompressorParams::from_effect_params(&base));
                }
                (FxUnit::Gate(u), AudioFxKind::Gate) => {
                    assert_eq!(*u.params(), GateParams::from_effect_params(&base));
                }
                (FxUnit::Limiter(u), AudioFxKind::Limiter) => {
                    assert_eq!(*u.params(), LimiterParams::from_effect_params(&base));
                }
                _ => panic!("FxUnit variant does not match kind {k:?}"),
            }
        }
    }

    /// `latency_samples` forwards to the inner unit: the limiter reports its
    /// lookahead once it has processed a block, the others always zero.
    #[test]
    fn forwards_latency_samples() {
        for k in KINDS {
            let mut fx = FxUnit::new(k);
            let mut block = vec![0.1f32; 512 * CHANNELS];
            fx.process(48_000, &mut block, None);
            let lat = fx.latency_samples();
            if k == AudioFxKind::Limiter {
                assert!(
                    lat > 0,
                    "limiter should report lookahead latency, got {lat}"
                );
            } else {
                assert_eq!(lat, 0, "{k:?} should report zero latency, got {lat}");
            }
        }
    }
}

#[cfg(test)]
mod discontinuity_tests {
    use super::*;
    use crate::audio::CHANNELS;

    const FS: u32 = 48_000;
    const N: usize = 4096;

    fn tone(n: usize, amp: f32) -> Vec<f32> {
        (0..n * CHANNELS)
            .map(|i| {
                let t = (i / CHANNELS) as f32 / FS as f32;
                (t * 440.0 * std::f32::consts::TAU).sin() * amp
            })
            .collect()
    }

    fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b)
            .fold(0.0f32, |m, (x, y)| m.max((x - y).abs()))
    }

    /// 31 §9 item 1 — the real contract: **after `reset`, a unit behaves
    /// exactly as a freshly constructed one** (same params, no history).
    ///
    /// An earlier version of this test asserted "loud input, reset, silence in
    /// ⇒ silence out". That is **vacuous for every gain-based unit** — a
    /// compressor, gate or flat EQ multiplies its input, so silence in gives
    /// silence out whether or not the reset happened. Deleting the entire body
    /// of `Eq::reset` still passed it. The assertion below instead compares
    /// against a fresh unit on a *signal* probe, which is sensitive to exactly
    /// the state `reset` is supposed to discard.
    fn assert_reset_equals_fresh<U: DspUnit>(
        mut make: impl FnMut() -> U,
        configure: impl Fn(&mut U),
        name: &str,
    ) {
        // Path A: loud material, then reset, then the probe.
        let mut used = make();
        configure(&mut used);
        let mut loud = tone(N, 0.9);
        used.process(FS, &mut loud, None);
        used.reset(AudioDiscontinuity::Seek);
        let mut probe_a = tone(N, 0.05);
        used.process(FS, &mut probe_a, None);

        // Path B: a fresh unit, same params, same probe.
        let mut fresh = make();
        configure(&mut fresh);
        let mut probe_b = tone(N, 0.05);
        fresh.process(FS, &mut probe_b, None);

        assert!(
            max_abs_diff(&probe_a, &probe_b) <= 1e-6,
            "{name}: output after reset differs from a fresh unit by {:.3e} —              state survived the discontinuity",
            max_abs_diff(&probe_a, &probe_b)
        );

        // Guard against the inverse vacuity: if the unit is configured such
        // that history cannot possibly matter, this test proves nothing. A
        // *non*-reset run must differ, or the fixture is inert.
        let mut unreset = make();
        configure(&mut unreset);
        let mut loud2 = tone(N, 0.9);
        unreset.process(FS, &mut loud2, None);
        let mut probe_c = tone(N, 0.05);
        unreset.process(FS, &mut probe_c, None);
        assert!(
            max_abs_diff(&probe_c, &probe_b) > 1e-6,
            "{name}: fixture is inert — skipping the reset changes nothing, so              this test cannot detect a missing reset"
        );
    }

    #[test]
    fn compressor_reset_equals_fresh() {
        assert_reset_equals_fresh(
            compressor::Compressor::new,
            |c| {
                let mut p = *c.params();
                p.threshold_db = -50.0; // actively compressing at both levels
                p.ratio = 8.0;
                p.release_ms = 2000.0; // long release ⇒ envelope persists
                c.set_params(p);
            },
            "compressor",
        );
    }

    #[test]
    fn gate_reset_equals_fresh() {
        assert_reset_equals_fresh(
            gate::Gate::new,
            |g| {
                let mut p = *g.params();
                p.threshold_db = -30.0; // loud opens it, probe alone would not
                p.release_ms = 2000.0;
                p.hold_ms = 500.0;
                g.set_params(p);
            },
            "gate",
        );
    }

    #[test]
    fn eq_reset_equals_fresh() {
        assert_reset_equals_fresh(
            eq::Eq::new,
            |e| {
                let mut p = *e.params();
                p.band1.gain_db = 18.0; // non-flat, so the biquads hold energy
                p.band1.q = 8.0;
                e.set_params(p);
            },
            "eq",
        );
    }

    #[test]
    fn limiter_reset_equals_fresh() {
        assert_reset_equals_fresh(
            limiter::Limiter::new,
            |l| {
                let mut p = *l.params();
                p.ceiling_db = -12.0;
                l.set_params(p);
            },
            "limiter",
        );
    }

    /// Reset discards state, never configuration.
    #[test]
    fn reset_preserves_params() {
        let mut c = compressor::Compressor::new();
        let mut p = *c.params();
        p.threshold_db = -24.0;
        c.set_params(p);
        c.reset(AudioDiscontinuity::Seek);
        assert_eq!(c.params().threshold_db, -24.0);
    }

    /// The limiter re-primes its delay line rather than shortening it, so its
    /// reported latency is stable across a reset. Clearing the deques without
    /// re-priming would silently desync this path against every other path
    /// feeding the master bus (31 §3.1).
    #[test]
    fn limiter_latency_is_stable_across_reset() {
        let mut l = limiter::Limiter::new();
        let mut b = tone(N, 0.9);
        l.process(FS, &mut b, None);
        let before = l.latency_samples();
        assert!(before > 0, "limiter should report its lookahead");

        l.reset(AudioDiscontinuity::Seek);
        let mut b2 = tone(N, 0.9);
        l.process(FS, &mut b2, None);
        assert_eq!(before, l.latency_samples(), "latency changed across reset");
    }

    #[test]
    fn stateless_units_report_no_latency_or_tail() {
        for (u, name) in [
            (
                Box::new(compressor::Compressor::new()) as Box<dyn DspUnit>,
                "compressor",
            ),
            (Box::new(gate::Gate::new()), "gate"),
            (Box::new(eq::Eq::new()), "eq"),
        ] {
            assert_eq!(u.latency_samples(), 0, "{name}");
            assert_eq!(u.tail_samples(), 0, "{name}");
        }
    }
}
