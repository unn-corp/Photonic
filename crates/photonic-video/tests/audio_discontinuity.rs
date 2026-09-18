//! 31 §9 item 5 — mixer-level discontinuity policy (§2.3), end to end through
//! `Mixer::render_block` with synthetic `PcmSource`s.
//!
//! The policy under test:
//! - `ClipBoundary { track }` resets **only** that track's chain; the master
//!   chain keeps running (a master compressor must not pump at every cut).
//! - `Seek` resets **every** chain.
//! - `GraphChanged { track, from_index }` resets that chain from `from_index`
//!   onward, plus the whole master chain (downstream of every track).
//!
//! Signals are constant-DC so the detector (`max(|L|,|R|)`) is exactly the
//! amplitude, which makes gate open/close and compressor gain-reduction
//! trivially predictable and the assertions deterministic.

use photonic_core::timeline::{
    AudioFxKind, AudioFxUnit, ClipAudio, MasterBus, PropValue, Tick, TrackAudio, TrackId,
};
use photonic_video::audio::mixer::MixerDiscontinuity;
use photonic_video::audio::{ClipVoice, Mixer, PcmSource, TrackVoice, BLOCK_FRAMES, CHANNELS};

const SR: u32 = 48_000;
// A remaining large enough that the mixer never marks a segment boundary
// (which would be irrelevant here and only churns the tail cache).
const NEVER_ENDS: Tick = Tick(1 << 50);

/// Constant DC on both channels — a stateless source we can recreate per block.
struct ConstSource {
    value: f32,
}
impl PcmSource for ConstSource {
    fn channels(&self) -> u16 {
        2
    }
    fn sample_rate(&self) -> u32 {
        SR
    }
    fn read(&mut self, out: &mut [f32], frames: usize) -> usize {
        out[..frames * 2].fill(self.value);
        frames
    }
}

fn gate_unit(threshold_db: f64, hold_ms: f64, release_ms: f64, range_db: f64) -> AudioFxUnit {
    let mut u = AudioFxUnit::new(AudioFxKind::Gate);
    u.params
        .base
        .set("params.threshold_db", PropValue::Float(threshold_db));
    u.params.base.set("params.attack_ms", PropValue::Float(1.0));
    u.params
        .base
        .set("params.hold_ms", PropValue::Float(hold_ms));
    u.params
        .base
        .set("params.release_ms", PropValue::Float(release_ms));
    u.params
        .base
        .set("params.range_db", PropValue::Float(range_db));
    u
}

fn comp_unit(threshold_db: f64, ratio: f64, release_ms: f64) -> AudioFxUnit {
    let mut u = AudioFxUnit::new(AudioFxKind::Compressor);
    u.params
        .base
        .set("params.threshold_db", PropValue::Float(threshold_db));
    u.params.base.set("params.ratio", PropValue::Float(ratio));
    u.params.base.set("params.attack_ms", PropValue::Float(5.0));
    u.params
        .base
        .set("params.release_ms", PropValue::Float(release_ms));
    u.params.base.set("params.makeup_db", PropValue::Float(0.0));
    u
}

fn master_with(units: Vec<AudioFxUnit>) -> MasterBus {
    let mut m = MasterBus::new();
    m.fx_chain = units;
    m
}

/// Render one block of constant-`value` material for `track_audio` through
/// `mixer`, returning the output block.
fn render_one(
    mixer: &mut Mixer,
    track_audio: &TrackAudio,
    master: &MasterBus,
    id: TrackId,
    clip_audio: &ClipAudio,
    value: f32,
) -> Vec<f32> {
    let mut src = ConstSource { value };
    let mut tracks = vec![TrackVoice {
        id,
        audio: track_audio,
        clips: vec![ClipVoice {
            audio: clip_audio,
            elapsed: Tick(0),
            remaining: NEVER_ENDS,
            source: &mut src,
        }],
    }];
    let mut out = vec![0.0; BLOCK_FRAMES * CHANNELS];
    mixer.render_block(Tick(0), &mut tracks, master, &mut out);
    out
}

fn peak(block: &[f32]) -> f32 {
    block.iter().fold(0f32, |m, &s| m.max(s.abs()))
}

fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).fold(0f32, |m, (x, y)| m.max((x - y).abs()))
}

const LOUD: f32 = 0.5; // -6 dBFS, opens the gate / drives the compressor
const QUIET: f32 = 0.02; // -34 dBFS, below both thresholds
                         // The track fader's equal-power center pan applies -3 dB (× 1/√2) per channel,
                         // so a "unity" pass-through of a QUIET DC signal emerges at this level.
const UNITY_PROBE: f32 = QUIET * std::f32::consts::FRAC_1_SQRT_2;

/// §9 item 5, the two halves that matter most: `ClipBoundary` resets the
/// **track** chain (a gate) but leaves the **master** chain (a compressor)
/// running.
#[test]
fn clip_boundary_resets_track_gate_but_not_master_compressor() {
    let id = TrackId::new();
    let clip_audio = ClipAudio::new();

    // ── Part A: the track Gate IS reset by ClipBoundary ──────────────────
    let empty_master = master_with(vec![]);
    let mut gate_track = TrackAudio::new();
    gate_track.fx_chain = vec![gate_unit(-20.0, 500.0, 2000.0, 60.0)];

    // Loud opens the gate (and arms a long hold), then ClipBoundary resets it.
    let mut m_cb = Mixer::new(SR);
    for _ in 0..40 {
        render_one(&mut m_cb, &gate_track, &empty_master, id, &clip_audio, LOUD);
    }
    m_cb.notify_discontinuity(MixerDiscontinuity::ClipBoundary {
        track: id,
        at: Tick(0),
    });
    let cb_probe = render_one(
        &mut m_cb,
        &gate_track,
        &empty_master,
        id,
        &clip_audio,
        QUIET,
    );

    // A fresh gate on the same quiet probe.
    let mut m_fresh = Mixer::new(SR);
    let fresh_probe = render_one(
        &mut m_fresh,
        &gate_track,
        &empty_master,
        id,
        &clip_audio,
        QUIET,
    );

    // Control: loud then the probe with NO discontinuity — the gate is still
    // held open, so the probe passes. Guards against the reset being vacuous.
    let mut m_hold = Mixer::new(SR);
    for _ in 0..40 {
        render_one(
            &mut m_hold,
            &gate_track,
            &empty_master,
            id,
            &clip_audio,
            LOUD,
        );
    }
    let held_probe = render_one(
        &mut m_hold,
        &gate_track,
        &empty_master,
        id,
        &clip_audio,
        QUIET,
    );

    assert!(
        max_abs_diff(&cb_probe, &fresh_probe) < 1e-6,
        "a ClipBoundary-reset gate must behave exactly like a fresh one \
         (max diff {})",
        max_abs_diff(&cb_probe, &fresh_probe)
    );
    assert!(
        peak(&held_probe) > peak(&cb_probe) * 10.0,
        "without the reset the gate stays open (held={}, reset={}) — the reset \
         is not vacuous",
        peak(&held_probe),
        peak(&cb_probe)
    );

    // ── Part B: the master Compressor is NOT reset by ClipBoundary ───────
    let empty_track = TrackAudio::new();
    let comp_master = master_with(vec![comp_unit(-20.0, 8.0, 2000.0)]);

    // Loud drives the compressor's envelope up; ClipBoundary must leave it be.
    let mut c_cb = Mixer::new(SR);
    for _ in 0..60 {
        render_one(&mut c_cb, &empty_track, &comp_master, id, &clip_audio, LOUD);
    }
    c_cb.notify_discontinuity(MixerDiscontinuity::ClipBoundary {
        track: id,
        at: Tick(0),
    });
    let cb_comp = render_one(
        &mut c_cb,
        &empty_track,
        &comp_master,
        id,
        &clip_audio,
        QUIET,
    );

    // Seek DOES reset the compressor: the quiet probe then passes ~unity.
    let mut c_seek = Mixer::new(SR);
    for _ in 0..60 {
        render_one(
            &mut c_seek,
            &empty_track,
            &comp_master,
            id,
            &clip_audio,
            LOUD,
        );
    }
    c_seek.notify_discontinuity(MixerDiscontinuity::Seek { to: Tick(0) });
    let seek_comp = render_one(
        &mut c_seek,
        &empty_track,
        &comp_master,
        id,
        &clip_audio,
        QUIET,
    );

    assert!(
        peak(&cb_comp) < peak(&seek_comp) * 0.8,
        "ClipBoundary must NOT reset the master compressor: its leftover gain \
         reduction should still pull the probe down (clip_boundary={}, seek={})",
        peak(&cb_comp),
        peak(&seek_comp)
    );
    // And the seek-reset compressor really does pass the sub-threshold probe.
    assert!(
        peak(&seek_comp) > UNITY_PROBE * 0.9,
        "a seek-reset compressor should pass a sub-threshold probe ~unity, got {}",
        peak(&seek_comp)
    );
}

/// §9 item 5: `Seek` resets **both** chains — the combined mixer then behaves
/// exactly as if freshly constructed.
#[test]
fn seek_resets_both_track_and_master() {
    let id = TrackId::new();
    let clip_audio = ClipAudio::new();
    let mut gate_track = TrackAudio::new();
    gate_track.fx_chain = vec![gate_unit(-20.0, 500.0, 2000.0, 60.0)];
    let comp_master = master_with(vec![comp_unit(-20.0, 8.0, 2000.0)]);

    let mut m_seek = Mixer::new(SR);
    for _ in 0..60 {
        render_one(
            &mut m_seek,
            &gate_track,
            &comp_master,
            id,
            &clip_audio,
            LOUD,
        );
    }
    m_seek.notify_discontinuity(MixerDiscontinuity::Seek { to: Tick(0) });
    let seek_probe = render_one(
        &mut m_seek,
        &gate_track,
        &comp_master,
        id,
        &clip_audio,
        QUIET,
    );

    let mut m_fresh = Mixer::new(SR);
    let fresh_probe = render_one(
        &mut m_fresh,
        &gate_track,
        &comp_master,
        id,
        &clip_audio,
        QUIET,
    );

    let mut m_hold = Mixer::new(SR);
    for _ in 0..60 {
        render_one(
            &mut m_hold,
            &gate_track,
            &comp_master,
            id,
            &clip_audio,
            LOUD,
        );
    }
    let held_probe = render_one(
        &mut m_hold,
        &gate_track,
        &comp_master,
        id,
        &clip_audio,
        QUIET,
    );

    assert!(
        max_abs_diff(&seek_probe, &fresh_probe) < 1e-6,
        "after Seek the whole graph must match a fresh mixer (max diff {})",
        max_abs_diff(&seek_probe, &fresh_probe)
    );
    assert!(
        peak(&held_probe) > peak(&seek_probe) * 10.0,
        "without Seek the held-open gate passes the probe (held={}, seek={}) — \
         guards vacuity",
        peak(&held_probe),
        peak(&seek_probe)
    );
}

/// §9 item 5: `GraphChanged { track, from_index: 1 }` leaves unit 0 on that
/// track unreset while resetting unit 1 onward; and `GraphChanged { Some, .. }`
/// resets the whole master chain.
#[test]
fn graph_changed_from_index_resets_downstream_only() {
    let id = TrackId::new();
    let clip_audio = ClipAudio::new();
    let empty_master = master_with(vec![]);

    // Track chain: [Gate(0), Compressor(1)]. Loud opens the gate and engages
    // the compressor.
    let mut fx_track = TrackAudio::new();
    fx_track.fx_chain = vec![
        gate_unit(-20.0, 500.0, 2000.0, 60.0),
        comp_unit(-20.0, 8.0, 2000.0),
    ];

    // from_index = 1: Gate(0) survives (held open), Compressor(1) is reset.
    let mut m_from1 = Mixer::new(SR);
    for _ in 0..60 {
        render_one(
            &mut m_from1,
            &fx_track,
            &empty_master,
            id,
            &clip_audio,
            LOUD,
        );
    }
    m_from1.notify_discontinuity(MixerDiscontinuity::GraphChanged {
        track: Some(id),
        from_index: 1,
    });
    let from1 = render_one(
        &mut m_from1,
        &fx_track,
        &empty_master,
        id,
        &clip_audio,
        QUIET,
    );

    // from_index = 0: Gate(0) is reset too — it closes on the quiet probe.
    let mut m_from0 = Mixer::new(SR);
    for _ in 0..60 {
        render_one(
            &mut m_from0,
            &fx_track,
            &empty_master,
            id,
            &clip_audio,
            LOUD,
        );
    }
    m_from0.notify_discontinuity(MixerDiscontinuity::GraphChanged {
        track: Some(id),
        from_index: 0,
    });
    let from0 = render_one(
        &mut m_from0,
        &fx_track,
        &empty_master,
        id,
        &clip_audio,
        QUIET,
    );

    // Unit 0 (gate) survived at from_index 1: the probe passes. At from_index 0
    // the gate is closed and the probe is crushed.
    assert!(
        peak(&from1) > peak(&from0) * 10.0,
        "from_index 1 must leave unit 0 unreset (from1={}, from0={})",
        peak(&from1),
        peak(&from0)
    );
    // Unit 1 (compressor) WAS reset at from_index 1: the surviving open gate
    // passes the probe, and a reset compressor (sub-threshold) leaves it at
    // ~unity rather than pulling it down with leftover gain reduction.
    assert!(
        peak(&from1) > UNITY_PROBE * 0.9,
        "from_index 1 must reset unit 1 (compressor): probe should pass ~unity, got {}",
        peak(&from1)
    );

    // GraphChanged { Some, .. } also resets the master chain (downstream of the
    // track). Contrast with `clip_boundary_..._not_master_compressor`, where
    // the same compressor survives.
    let empty_track = TrackAudio::new();
    let comp_master = master_with(vec![comp_unit(-20.0, 8.0, 2000.0)]);
    let mut m_master = Mixer::new(SR);
    for _ in 0..60 {
        render_one(
            &mut m_master,
            &empty_track,
            &comp_master,
            id,
            &clip_audio,
            LOUD,
        );
    }
    m_master.notify_discontinuity(MixerDiscontinuity::GraphChanged {
        track: Some(id),
        from_index: 0,
    });
    let master_probe = render_one(
        &mut m_master,
        &empty_track,
        &comp_master,
        id,
        &clip_audio,
        QUIET,
    );
    assert!(
        peak(&master_probe) > UNITY_PROBE * 0.9,
        "GraphChanged {{ Some, .. }} must reset the master compressor too: probe \
         should pass ~unity, got {}",
        peak(&master_probe)
    );
}
