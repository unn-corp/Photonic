//! 31 §4 / §9 item 4 — boundary declick, end to end through
//! `Mixer::render_block`.
//!
//! The fixture is a splice built to have a *measurable* click rather than an
//! audible one. A 1 kHz tone is cut so that the outgoing clip's last sample
//! sits on its trough (-0.5) while the incoming clip restarts at zero phase
//! (0.0): a 0.5 step, where the material's own worst sample-to-sample jump is
//! ~0.065. Every assertion below is expressed against that measured material
//! slew, so the fixture proves itself non-vacuous before any property is
//! asserted — the same shape as `cpu_gpu_parity.rs`'s masked-grade test.
//!
//! Four things are under test, and they pull in opposite directions on
//! purpose:
//!
//! 1. the click is **gone** (the splice's jump drops to the material's own);
//! 2. the level is **not** dipped — a fade would put ~0 at the splice, the
//!    declick puts the outgoing clip's own final sample there;
//! 3. everything outside the crossfade window is **bit-identical** to a run
//!    with the declick switched off;
//! 4. a splice that is *not* a click — a phase-continuous cut, or a cut onto a
//!    transient — is left **bit-identical** too, and never counts an
//!    engagement. That is the assertion that stops this dulling every hard
//!    edit, and it is the one a naive always-ramp implementation fails.
//!
//! Every comparison is against a declick-disabled control run, so a no-op
//! declick cannot pass: it would fail (1) with the raw step intact, and a
//! ramp-everything declick cannot pass either — it would fail (4).

use photonic_core::timeline::{ClipAudio, MasterBus, Tick, TrackAudio, TrackId, TICKS_PER_SECOND};
use photonic_video::audio::{
    ClipVoice, DeclickConfig, Mixer, PcmSource, TrackVoice, BLOCK_FRAMES, CHANNELS,
};

const SR: u32 = 48_000;
const TICK_PER_SAMPLE: i64 = TICKS_PER_SECOND / SR as i64;
const BLOCK_TICKS: i64 = BLOCK_FRAMES as i64 * TICK_PER_SAMPLE;
/// Blocks of outgoing material before the cut. Four blocks (2048 samples) is
/// more than the 1000-sample default window, so the tail ring is full and the
/// crossfade runs at its full length.
const OUT_BLOCKS: usize = 4;
const IN_BLOCKS: usize = 4;
/// Frame index of the splice within a concatenated render.
const SPLICE: usize = OUT_BLOCKS * BLOCK_FRAMES;
/// The default 31 §4.1 window.
const WINDOW: usize = 1000;
/// Samples per cycle of the 1 kHz probe tone.
const CYCLE: usize = 48;

/// A tone whose sample values are a closed-form function of an absolute sample
/// index, so a phase relationship across a splice is exact rather than
/// accumulated (and identical on every run).
#[derive(Clone, Copy)]
struct Tone {
    freq: f64,
    amp: f64,
    start_phase: f64,
}

impl Tone {
    fn radians_per_sample(freq: f64) -> f64 {
        std::f64::consts::TAU * freq / SR as f64
    }
    fn at(&self, n: usize) -> f32 {
        ((self.start_phase + Self::radians_per_sample(self.freq) * n as f64).sin() * self.amp)
            as f32
    }
}

struct ToneSource {
    tone: Tone,
    n: usize,
}

impl ToneSource {
    fn new(tone: Tone) -> Self {
        ToneSource { tone, n: 0 }
    }
}

impl PcmSource for ToneSource {
    fn channels(&self) -> u16 {
        CHANNELS as u16
    }
    fn sample_rate(&self) -> u32 {
        SR
    }
    fn read(&mut self, out: &mut [f32], frames: usize) -> usize {
        for f in 0..frames {
            let s = self.tone.at(self.n);
            self.n += 1;
            for c in 0..CHANNELS {
                out[f * CHANNELS + c] = s;
            }
        }
        frames
    }
}

const PROBE_FREQ: f64 = 1000.0;
const PROBE_AMP: f64 = 0.5;

/// The outgoing tone, phased so its **final** sample (index `SPLICE - 1`)
/// lands exactly on the trough, `-PROBE_AMP`.
fn outgoing() -> Tone {
    let step = Tone::radians_per_sample(PROBE_FREQ);
    Tone {
        freq: PROBE_FREQ,
        amp: PROBE_AMP,
        start_phase: -std::f64::consts::FRAC_PI_2 - step * (SPLICE - 1) as f64,
    }
}

/// Incoming variants, all of the same tone family.
mod incoming {
    use super::*;

    /// Restarts at zero phase against an outgoing trough: a 0.5 step.
    pub fn cut_mid_cycle() -> Tone {
        Tone {
            freq: PROBE_FREQ,
            amp: PROBE_AMP,
            start_phase: 0.0,
        }
    }

    /// Picks up exactly where the outgoing clip stopped: no step at all.
    pub fn phase_continuous() -> Tone {
        Tone {
            freq: PROBE_FREQ,
            amp: PROBE_AMP,
            start_phase: -std::f64::consts::FRAC_PI_2 + Tone::radians_per_sample(PROBE_FREQ),
        }
    }

    /// A cut straight onto a loud, fast attack — legitimate material whose own
    /// slew dwarfs the splice's.
    pub fn transient() -> Tone {
        Tone {
            freq: 6000.0,
            amp: 0.9,
            start_phase: 0.05,
        }
    }
}

/// Render `OUT_BLOCKS` of `out_tone` followed by `IN_BLOCKS` of `in_tone`
/// through one `Mixer`, with the outgoing clip's `remaining` falling to one
/// block on its last block (which is what makes the mixer flag a segment
/// boundary). Returns the concatenated interleaved output and the mixer's
/// declick engagement count.
fn render_splice(cfg: DeclickConfig, out_tone: Tone, in_tone: Tone) -> (Vec<f32>, u64) {
    let mut mixer = Mixer::new(SR);
    mixer.set_declick(cfg);
    // No master limiter: the declick is a pre-fx repair and a non-linear master
    // unit would only obscure the sample-exact assertions below.
    let mut master = MasterBus::new();
    master.fx_chain.clear();

    let id = TrackId::new();
    let track_audio = TrackAudio::new();
    let clip_audio = ClipAudio::new();
    let mut all = Vec::with_capacity((OUT_BLOCKS + IN_BLOCKS) * BLOCK_FRAMES * CHANNELS);
    let mut out = vec![0.0f32; BLOCK_FRAMES * CHANNELS];

    let mut src_out = ToneSource::new(out_tone);
    for b in 0..OUT_BLOCKS {
        let t = Tick(b as i64 * BLOCK_TICKS);
        // The last outgoing block is the boundary block.
        let remaining = if b + 1 == OUT_BLOCKS {
            Tick(BLOCK_TICKS)
        } else {
            Tick((OUT_BLOCKS - b) as i64 * BLOCK_TICKS)
        };
        let mut tracks = vec![TrackVoice {
            id,
            audio: &track_audio,
            clips: vec![ClipVoice {
                audio: &clip_audio,
                elapsed: t,
                remaining,
                source: &mut src_out as &mut dyn PcmSource,
            }],
        }];
        mixer.render_block(t, &mut tracks, &master, &mut out);
        all.extend_from_slice(&out);
    }

    let mut src_in = ToneSource::new(in_tone);
    for b in 0..IN_BLOCKS {
        let t = Tick((OUT_BLOCKS + b) as i64 * BLOCK_TICKS);
        let mut tracks = vec![TrackVoice {
            id,
            audio: &track_audio,
            clips: vec![ClipVoice {
                audio: &clip_audio,
                elapsed: Tick(b as i64 * BLOCK_TICKS),
                remaining: Tick(1 << 40),
                source: &mut src_in as &mut dyn PcmSource,
            }],
        }];
        mixer.render_block(t, &mut tracks, &master, &mut out);
        all.extend_from_slice(&out);
    }

    (all, mixer.declick_engagements())
}

fn enabled() -> DeclickConfig {
    DeclickConfig::default()
}

fn disabled() -> DeclickConfig {
    DeclickConfig {
        enabled: false,
        ..DeclickConfig::default()
    }
}

/// Largest sample-to-sample jump on channel 0 over `frames`, exclusive of the
/// first.
fn max_slew(pcm: &[f32], frames: std::ops::Range<usize>) -> f32 {
    let mut m = 0f32;
    for f in frames {
        let d = (pcm[f * CHANNELS] - pcm[(f - 1) * CHANNELS]).abs();
        m = m.max(d);
    }
    m
}

/// Peak |sample| on channel 0 over `frames`.
fn peak(pcm: &[f32], frames: std::ops::Range<usize>) -> f32 {
    frames.fold(0f32, |m, f| m.max(pcm[f * CHANNELS].abs()))
}

/// The material's own worst jump, measured well inside the outgoing segment
/// (block 1) so it is unaffected by anything happening at the splice.
fn material_slew(pcm: &[f32]) -> f32 {
    max_slew(pcm, (BLOCK_FRAMES + 1)..(2 * BLOCK_FRAMES))
}

/// 31 §9 item 4, the whole of it: the click goes away, the level does not, the
/// rest of the render does not move, and it is bit-reproducible.
#[test]
fn declick_removes_the_splice_step_without_dipping_the_level() {
    let (raw, raw_hits) = render_splice(disabled(), outgoing(), incoming::cut_mid_cycle());
    let (fixed, fixed_hits) = render_splice(enabled(), outgoing(), incoming::cut_mid_cycle());

    let slew = material_slew(&raw);
    assert!(slew > 0.0, "the probe tone must actually move");

    // ── The fixture is non-vacuous: there really is a click. ─────────────
    let raw_step = (raw[SPLICE * CHANNELS] - raw[(SPLICE - 1) * CHANNELS]).abs();
    assert!(
        raw_step > slew * 5.0,
        "fixture must contain a real discontinuity: splice step {raw_step} vs \
         material slew {slew}"
    );
    assert_eq!(raw_hits, 0, "a disabled declick must never engage");

    // ── (1) the click is gone. ───────────────────────────────────────────
    assert_eq!(
        fixed_hits, 1,
        "exactly one boundary should have been repaired"
    );
    let fixed_step = (fixed[SPLICE * CHANNELS] - fixed[(SPLICE - 1) * CHANNELS]).abs();
    assert!(
        fixed_step <= slew,
        "the splice's own jump must fall to the material's: {fixed_step} vs {slew}"
    );
    // Not just the one sample pair — nothing anywhere in the repaired region
    // jumps further than the source material does.
    let repaired_slew = max_slew(&fixed, (SPLICE - 4)..(SPLICE + WINDOW));
    assert!(
        repaired_slew <= slew * 1.5,
        "no sample pair across the repair may exceed the material's own slew \
         (worst {repaired_slew}, material {slew})"
    );
    assert!(
        raw_step > repaired_slew * 5.0,
        "and that must be a real improvement on the raw splice ({raw_step} -> \
         {repaired_slew})"
    );

    // ── (2) it is the reversed tail, not a fade, and there is no level dip. ─
    // A fade would put ~0 at the splice; the declick puts the outgoing clip's
    // own final sample there, so the level is continuous to the sample.
    assert!(
        (fixed[SPLICE * CHANNELS] - fixed[(SPLICE - 1) * CHANNELS]).abs() < 1e-6,
        "the splice sample must carry the outgoing clip's level, not a fade's zero"
    );

    // The strong form: 31 §4.1 step 4's formula, checked against closed-form
    // signals across the whole window. Both tones and the mixer's gain
    // staging are exactly known, so this pins the *material* the crossfade is
    // made of — a repair that ramps toward zero instead of playing the
    // outgoing tail backwards produces the same splice sample and the same
    // envelope, and still fails here.
    //
    // The one unknown is the constant the track fader applies (unity volume,
    // equal-power centre pan); derive it from the control run rather than
    // restating the pan law.
    let fader = raw[(SPLICE - 1) * CHANNELS] / outgoing().at(SPLICE - 1);
    assert!(fader.is_finite() && fader > 0.1, "derived fader {fader}");
    let (out_tone, in_tone) = (outgoing(), incoming::cut_mid_cycle());
    for i in 0..WINDOW {
        let mix = (WINDOW - i) as f32 / WINDOW as f32;
        // tail_reversed[i] is the outgoing clip counted back from the splice.
        let reversed = out_tone.at(SPLICE - 1 - i) * fader;
        let head = in_tone.at(i) * fader;
        let expect = reversed * mix + head * (1.0 - mix);
        for c in 0..CHANNELS {
            let got = fixed[(SPLICE + i) * CHANNELS + c];
            assert!(
                (got - expect).abs() < 2e-6,
                "31 §4.1 step 4 at i={i} c={c}: got {got}, expected {expect} \
                 (reversed tail {reversed}, head {head}, mix {mix})"
            );
        }
    }

    // And the consequence that matters by ear: the repaired region keeps the
    // source's own motion instead of flattening toward zero (§4.1: "reversing
    // is the point"). A decay-to-zero repair goes slack here.
    let reference_peak = peak(&raw, BLOCK_FRAMES..(2 * BLOCK_FRAMES));
    for start in (SPLICE..(SPLICE + WINDOW)).step_by(CYCLE) {
        let p = peak(&fixed, start..(start + CYCLE));
        assert!(
            p > reference_peak * 0.6,
            "level dipped to {p} (reference {reference_peak}) at frame {start} — a \
             linear crossfade of the reversed tail may lose up to 3 dB, never more"
        );
        let moved = max_slew(&fixed, (start + 1)..(start + CYCLE));
        assert!(
            moved > slew * 0.5,
            "waveform went slack at frame {start} (slew {moved} vs material {slew}) \
             — the repair must carry the tail's motion, not a ramp toward zero"
        );
    }

    // ── (3) everything outside the window is untouched. ──────────────────
    assert_eq!(
        &fixed[..SPLICE * CHANNELS],
        &raw[..SPLICE * CHANNELS],
        "nothing before the splice may move"
    );
    assert_eq!(
        &fixed[(SPLICE + WINDOW) * CHANNELS..],
        &raw[(SPLICE + WINDOW) * CHANNELS..],
        "nothing past the crossfade window may move"
    );
    // And the window itself genuinely did move (guards the two `assert_eq`s
    // above from passing because the declick did nothing at all).
    assert_ne!(
        &fixed[SPLICE * CHANNELS..(SPLICE + WINDOW) * CHANNELS],
        &raw[SPLICE * CHANNELS..(SPLICE + WINDOW) * CHANNELS],
        "the crossfade window must actually differ"
    );

    // ── (4) determinism (SS-3). ──────────────────────────────────────────
    let (again, again_hits) = render_splice(enabled(), outgoing(), incoming::cut_mid_cycle());
    assert_eq!(fixed, again, "same call sequence ⇒ bit-identical output");
    assert_eq!(fixed_hits, again_hits);
}

/// 31 §4.1 step 3: a boundary that is not a click is left completely alone.
/// This is the assertion an "always ramp" declick fails.
#[test]
fn a_phase_continuous_cut_is_left_bit_identical() {
    let (raw, _) = render_splice(disabled(), outgoing(), incoming::phase_continuous());
    let (fixed, hits) = render_splice(enabled(), outgoing(), incoming::phase_continuous());

    // Non-vacuity: the boundary really was crossed, it just wasn't a click.
    let slew = material_slew(&raw);
    let step = (raw[SPLICE * CHANNELS] - raw[(SPLICE - 1) * CHANNELS]).abs();
    assert!(
        step <= slew,
        "fixture is meant to be seamless: step {step} vs material slew {slew}"
    );

    assert_eq!(hits, 0, "a seamless splice must not be repaired");
    assert_eq!(fixed, raw, "and must therefore be bit-identical");
}

/// 31 §4.1 step 3, the case that matters most for sounding right: cutting
/// straight onto a transient. The incoming attack's own slew is larger than
/// the splice's, so the boundary measures below threshold and the attack keeps
/// every sample of its edge.
#[test]
fn a_cut_onto_a_transient_is_not_ducked() {
    let (raw, _) = render_splice(disabled(), outgoing(), incoming::transient());
    let (fixed, hits) = render_splice(enabled(), outgoing(), incoming::transient());

    // Non-vacuity: the incoming material really is faster and louder.
    let out_slew = material_slew(&raw);
    let in_slew = max_slew(&raw, (SPLICE + 1)..(SPLICE + BLOCK_FRAMES));
    assert!(
        in_slew > out_slew * 5.0,
        "fixture must be a genuine transient: incoming slew {in_slew} vs outgoing \
         {out_slew}"
    );

    assert_eq!(
        hits, 0,
        "a cut onto a transient must not engage — repairing it would smear the attack"
    );
    assert_eq!(fixed, raw, "the attack must survive sample for sample");
}

/// 31 §4.3: disabled when either side is silent. A clip ending into a gap is a
/// boundary, but reversing its tail into the gap would add ~21 ms of material
/// the timeline does not contain.
#[test]
fn a_boundary_into_silence_is_not_repaired() {
    let silence = Tone {
        freq: PROBE_FREQ,
        amp: 0.0,
        start_phase: 0.0,
    };
    let (raw, _) = render_splice(disabled(), outgoing(), silence);
    let (fixed, hits) = render_splice(enabled(), outgoing(), silence);

    // Non-vacuity: the raw splice IS a step (into silence).
    let slew = material_slew(&raw);
    let step = (raw[SPLICE * CHANNELS] - raw[(SPLICE - 1) * CHANNELS]).abs();
    assert!(
        step > slew * 5.0,
        "fixture must step into the gap: {step} vs material slew {slew}"
    );

    assert_eq!(hits, 0, "silence on one side disables the repair");
    assert_eq!(fixed, raw);
    assert!(
        peak(&fixed, (SPLICE + 1)..(SPLICE + WINDOW)) == 0.0,
        "the gap must stay a gap — no reversed tail bleeding into it"
    );
}

/// The threshold is a real dial, not a constant: raising it past the fixture's
/// measured `delta_db` turns the same repair off. A hard-coded "always repair"
/// cannot pass this.
#[test]
fn raising_the_threshold_disengages_the_same_boundary() {
    let (raw, _) = render_splice(disabled(), outgoing(), incoming::cut_mid_cycle());
    let (_, low_hits) = render_splice(
        DeclickConfig {
            threshold_db: 2.0,
            ..enabled()
        },
        outgoing(),
        incoming::cut_mid_cycle(),
    );
    // The fixture's step is ~7.7x the material slew, i.e. ~17.7 dB. A 30 dB
    // threshold ("only repair a jump 31x the material's own") is above it.
    let (high, high_hits) = render_splice(
        DeclickConfig {
            threshold_db: 30.0,
            ..enabled()
        },
        outgoing(),
        incoming::cut_mid_cycle(),
    );

    assert_eq!(low_hits, 1, "the default threshold repairs this boundary");
    assert_eq!(high_hits, 0, "a 30 dB threshold does not");
    assert_eq!(high, raw, "and leaves the output bit-identical");
}

/// The crossfade is longer than a block, so it must survive the block boundary
/// that falls inside it: `WINDOW` (1000) against `BLOCK_FRAMES` (512).
#[test]
fn the_repair_spans_the_block_boundary_inside_its_window() {
    // Compile-time: this test is only meaningful while the window exceeds one
    // block. If the default window is ever shortened below `BLOCK_FRAMES` the
    // build fails here rather than the test silently going vacuous.
    const _: () = assert!(WINDOW > BLOCK_FRAMES);
    let (raw, _) = render_splice(disabled(), outgoing(), incoming::cut_mid_cycle());
    let (fixed, _) = render_splice(enabled(), outgoing(), incoming::cut_mid_cycle());

    // The tail of the window lives in the block *after* the incoming clip's
    // first: it must be repaired, and the first untouched sample must be
    // exactly at SPLICE + WINDOW.
    let second_block = SPLICE + BLOCK_FRAMES;
    assert!(
        (0..(WINDOW - BLOCK_FRAMES))
            .any(|i| fixed[(second_block + i) * CHANNELS] != raw[(second_block + i) * CHANNELS]),
        "the crossfade must continue into the second incoming block"
    );
    assert_eq!(
        fixed[(SPLICE + WINDOW) * CHANNELS],
        raw[(SPLICE + WINDOW) * CHANNELS],
        "and stop exactly at the window's end"
    );

    // Continuity holds across that internal block boundary too.
    let slew = material_slew(&raw);
    let d = (fixed[second_block * CHANNELS] - fixed[(second_block - 1) * CHANNELS]).abs();
    assert!(
        d <= slew * 1.5,
        "no seam where the repair crosses a block boundary ({d} vs {slew})"
    );
}
