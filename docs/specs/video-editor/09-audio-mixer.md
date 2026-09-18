# 09 — Audio Mixer

**Crate:** `photonic-video/src/audio/` (`engine.rs` cpal host, `mixer.rs` graph, `dsp/` eq+compressor+ducking, `waveform.rs`). **Data model:** `photonic-core::timeline` (this doc defines `TrackAudio`/`ClipAudio`/`MasterBus`/`AudioFx` referenced but not detailed by 01). **Decisions:** D-05 (full mixer v1), D-03 (ffmpeg sidecar decode), CAP-017. **Depends on:** 01 (Tick, AnimProps/PropSet/PropPath, TimelineCmd), 02 (threading, master clock, decode, export loop).

---

## 0. Term cross-reference

Terms below are defined in 01/02 (normative) and used here without redefinition: `Tick`/`TICKS_PER_SECOND` (01 §1), `AnimProps<T>`/`PropSet`/`PropertyTrack`/`PropPath`/`Keyframe`/`Interp` (01 §6), `ClipEffect`/`EffectParams`/`EffectKind` (01 §6.3, the pattern `AudioFxUnit` mirrors), `TimelineCmd` (01 §10), `AssetKind`/`AudioStreamInfo` (01 §3), engine threading + master clock + decode ring (02 §1, §3), `EngineStatus` (02 §1), frame-graph determinism property (02 §2), export loop (02 §7).

---

## 1. Scope & phasing

Scope per 00 §5: audio engine, mixer graph, automation, EQ/compressor, ducking, waveforms, meters. D-05 locks the full mixer (automation, EQ/compressor, ducking) for v1 — no cut scope. Phasing (00 §6/§7) splits *delivery*, not *schema*: CORE (gain/pan/mute/solo/meters, §2 data types, §4 signal flow minus fx, §5 realtime path) lands P3 with playback. EQ/Compressor/Gate/Limiter DSP, automation lanes UI, ducking presets, loudness normalization (§6, §8, §9) land P8. `TrackAudio.fx_chain` / `MasterBus.fx_chain` exist from P3 (empty `Vec`, no UI) — P8 adds DSP + UI, never a schema break. This avoids the top risk flagged in 00 §7 ("full-mixer scope blows P8"): the expensive part (DSP correctness, UI) is deferred; the cheap part (types, undo plumbing) is not.

---

## 2. Data model

Mirrors 01's generic animation system exactly: audio params are `PropSet` targets addressed by `PropPath`, evaluated by the same `AnimProps<T>` machinery as clip transforms and effects (01 §6). No parallel automation system.

```rust
// Track — sibling of TrackKind, lives on Track.audio (01 §4)
pub struct TrackAudio {
    pub params: AnimProps<TrackAudioParams>,     // volume_db, pan — animatable
    pub mute: bool,
    pub solo: bool,
    pub fx_chain: Vec<AudioFxUnit>,               // ordered, pre-fader (§4)
}
pub struct TrackAudioParams { pub volume_db: f64, pub pan: f64 }  // volume_db: -inf(mute floor)..+12, default 0.0; pan: -1.0(L)..1.0(R), default 0.0

// Clip — lives on Clip.audio (01 §5)
pub struct ClipAudio {
    pub params: AnimProps<ClipAudioParams>,       // gain_db — animatable
    pub fade_in: Option<AudioFade>,
    pub fade_out: Option<AudioFade>,
    pub channel_map: ChannelMap,
}
pub struct ClipAudioParams { pub gain_db: f64 }   // default 0.0; baseline clip trim, distinct from track fader
pub struct AudioFade { pub duration: Tick, pub shape: FadeShape }
pub enum FadeShape { Linear, EqualPower, Log, SCurve }   // EqualPower default (constant perceived loudness through crossfade-style fades)
pub enum ChannelMap { AsSource, MonoDownmix, StereoLR, ChannelSwap }  // v1: mono/stereo sources only (surround = non-goal, SPEC.md); Custom per-pair remap deferred, not needed by any CAP

// Sequence — lives on Sequence.audio_master (01 §4)
pub struct MasterBus {
    pub params: AnimProps<MasterBusParams>,       // volume_db
    pub fx_chain: Vec<AudioFxUnit>,                // Limiter present by default (§6.4)
    pub loudness_target: Option<LoudnessTarget>,   // export-time normalization; None = raw mix
}
pub struct MasterBusParams { pub volume_db: f64 }
pub struct LoudnessTarget { pub integrated_lufs: f32, pub true_peak_dbtp: f32 }
// Presets (concrete, not TBD): Streaming/Social = -14 LUFS / -1 dBTP. Broadcast = -23 LUFS / -1 dBTP.
```

`AudioFxUnit` reuses `ClipEffect`'s exact shape (01 §6.3) rather than inventing a parallel pattern — same registry, same `EffectParams` ordered-map, same animation infra:

```rust
pub struct AudioFxUnit {
    pub kind: AudioFxKind,
    pub enabled: bool,
    pub params: AnimProps<EffectParams>,          // ordered PropPath→PropValue, seeded from kind defaults, all animatable
}
pub enum AudioFxKind { Eq, Compressor, Limiter, Gate }
```

Per-kind param registry entries (`prop_registry.rs`, 01 §6.2), published like any effect kind:

| Kind | Params |
|---|---|
| `Eq` | `low_shelf.freq_hz`, `low_shelf.gain_db`; `band1..3.freq_hz`/`.gain_db`/`.q`; `high_shelf.freq_hz`, `.gain_db` |
| `Compressor` | `threshold_db`, `ratio`, `attack_ms`, `release_ms`, `makeup_db`, `sidechain: Option<TrackId>` (non-animated ref, not a `PropValue`) |
| `Gate` | `threshold_db`, `attack_ms`, `hold_ms`, `release_ms`, `range_db` |
| `Limiter` | `ceiling_db`, `release_ms` (`lookahead_ms` fixed at 5ms, not exposed — §6.5) |

`sidechain` on `Compressor` is the ducking mechanism: `Some(track)` routes that track's post-fx-pre-fader signal into the envelope detector instead of the compressor's own input (§6.3).

---

## 3. Stack & dependencies

- **Device I/O:** `cpal` (Apache-2.0). Recommended over `rodio` — rodio's mixer/source abstractions target playback convenience, not sample-accurate multi-source mixing with a custom real-time graph; cpal gives the raw callback the mixer worker needs.
- **Mixer graph:** custom, built on cpal's callback. `kira` (MIT/Apache) considered as a middle option (it already has a scene-graph mixer + automation) — position: **don't adopt kira wholesale**, its clock/scene model doesn't match 01's `AnimProps`/`Tick` system or 02's master-clock-is-audio design; instead borrow its block-scheduling and parameter-smoothing approach as prior art. Custom graph stays a thin layer over cpal + `dsp/`.
<!-- spec-assert: dep-absent rubato -->
<!-- SD-13 (27 §3): `rubato` is asserted-but-absent; pinned so the "not yet a dependency" prose below can never silently become false without a doc edit. -->
- **Resampling:** `rubato` (MIT) — **not yet a dependency**; it appears in no `Cargo.toml`, and `mixer.rs` currently requires sources to arrive already at mix rate, so non-1:1 clip speed has no resampling path. High-quality sinc-based, handles both fixed source→device rate conversion and small drift correction (decode clock vs. device clock over long playback). Position: adopt for v1, no alternative under consideration — determinism (SS-3) needs one consistent resampler for both interactive and export paths.
- **Decode:** ffmpeg sidecar PCM pipe (02 §3, `-f f32le`), same process/pipe machinery as video — this covers ALL v1 audio decode. `symphonia` is deliberately **not** in the v1 stack: it is MPL-2.0, and the repo's `deny.toml` allows MPL only per-crate (the `option-ext` precedent), so adding it as-written fails `cargo deny` (SPEC constraint: CI gates green). If ever added (in-process decode for tiny UI cues), it requires its own per-crate `[[licenses.exceptions]]` entry in `deny.toml` mirroring `option-ext`. Same discipline applies to the proptest/insta/criterion dev-deps doc 11 recommends — each must pass `cargo deny` transitively at add-time.
- **Loudness measurement:** own R128/BS.1770-4 implementation (§6.6) — no `ebur128`-binding crate (avoids a native C dependency under cargo-deny; algorithm is small and fully specified).

---

## 4. Signal flow & mix policy

```
per clip (audio track, clip covering tick t):
  decoded PCM (source rate)
    → resample to mix rate (rubato)
    → channel_map (mono→stereo dup / downmix / swap)
    → gain_db (ClipAudioParams, animated) + fade_in/fade_out envelope (shape-weighted)
    ↓ sum into track bus
per track:
  track bus sum
    → fx_chain (AudioFxUnit list, ordered: typically Eq → Compressor/Gate) — pre-fader
    → volume_db + pan (TrackAudioParams, animated, equal-power pan law)
    → solo/mute gate (see below)
    ↓ sum into master bus
master:
  master bus sum
    → fx_chain (typically ends in Limiter, §6.5)
    → volume_db (MasterBusParams)
    → output (device or export encoder)

  meter taps: post-clip-gain, post-track-fx (pre-fader), post-track-fader (pre-mute-gate),
              post-master-fx, final output — each tap feeds the corresponding UI meter (§8).
```

Solo/mute gating: standard solo-safe behavior — if any track has `solo == true`, only soloed tracks contribute to the master sum (muted-but-soloed track = still excluded, mute wins); multiple simultaneous solos are additive (all soloed tracks play together). No solo active → normal mute-only gating.

**Sample-rate policy:** mix at output device's negotiated rate (cpal `SupportedStreamConfig`, typically 48kHz). Every source resampled to this rate via rubato at decode-cache time (§5), never per-block in the mixer hot path. Export uses the sequence's configured audio rate (project setting, default 48kHz) — device rate is irrelevant offline (§7).

**Channel policy:** v1 output bus is stereo only. Mono sources duplicate to L+R at unity per channel (pan law then applies center-to-side spread); multichannel sources (>2ch, rare v1 input) downmix to stereo at decode time (`ffmpeg -ac 2`), per source `AudioStreamInfo.channels` from probe (01 §3). Surround output is a SPEC.md non-goal — not revisited here.

**Pan law (equal-power, constant perceived loudness across the sweep):**

```
angle  = (pan + 1.0) * π/4        # pan: -1.0(L)..1.0(R) → angle: 0..π/2
gain_L = cos(angle)
gain_R = sin(angle)
# pan=0 (center): gain_L = gain_R = 0.707 (-3dB each, sums to unity power, not unity amplitude)
```

Applied at track level (`TrackAudioParams.pan`) after the fx chain, before the fader multiply. Clips have no pan of their own in v1 — `ClipAudio` carries `gain_db` only; stereo placement is a track-level concern (matches CAP-017's scoped control list: "per-track volume/pan").

**Meter tap table** (§8 UI reads these; each is a running peak+RMS pair, not persisted):

| Tap | Point | Feeds |
|---|---|---|
| Clip | post-gain, post-fade, pre-sum | clip audio overlay (§8, optional, off by default — avoids per-clip meter clutter) |
| Track post-fx | after `fx_chain`, before fader | compressor/gate GR meters (§8) |
| Track post-fader | after volume_db/pan, before mute gate | channel-strip meter (§8) |
| Master post-fx | after master `fx_chain` (post-limiter) | limiter GR meter, clip-indicator LED |
| Output | final, post `MasterBusParams.volume_db` | master strip meter + live LUFS readout (§6.6, §8) — **not yet delivered**: `EngineBridge::master_level()` returns `None` and the mixer strip is synthetic. Closed by G-4 + [26 K-0.6](26-kdenlive-mlt-parity.md#8-k-0--foundations); contracts in [31](31-audio-architecture.md) |

---

## 5. Realtime architecture

Threading recap (02 §1): audio thread = cpal callback, real-time-safe (no locks/allocs), owns the master clock, drains a lock-free ring. Mixer worker (separate thread, not the callback) renders ahead into that ring. This doc specs the worker.

- **Block size:** 512 frames (≈10.7ms @ 48kHz). Coefficients/params resolved once per block from `AnimProps` eval at block-start tick, not per-sample — keeps DSP coefficient recompute cost bounded.
- **Ring depth:** 3 blocks (~32ms @48kHz). The <40ms interactive-latency figure is a design budget set by this doc, not an external citation: 3×512-frame blocks at 48kHz ≈ 32ms of buffered audio plus one block of worker jitter margin stays under the ~40ms threshold where UI-fader-to-ear delay starts feeling laggy. This is deliberately shallow: it decouples mixer worker jitter from the cpal deadline, it is **not** a prefetch/lookahead cache (that's the decode ring, 02 §3, a separate and deeper structure).
- **Parameter smoothing (de-zipper):** every gain/pan/eq/compressor param ramps from previous-block smoothed value to new target via a one-pole filter, applied per-sample across the block — never stepped. Time constants: gain/pan = 10ms, filter freq/Q/threshold = 20ms (slower, avoids audible zipper on EQ sweeps and comp threshold rides). Applies uniformly whether the change came from a live UI fader move or an automation keyframe boundary.
- **Seek/loop:** on seek, flush mixer ring + per-source decode rings (02 §3), then prefill (render blocks synchronously until ring is full) before resuming cpal drain. Loop point treated identically to seek (flush + prefill) — no seamless-loop crossfade in v1; no CAP requires gapless audio looping and export mixing is offline anyway.
- **Scrub audio (grains):** confirmed per 02 §4, off by default. When enabled: short windowed grain (40–80ms, Hann window to avoid clicks) of raw source samples at scrub target, gain-only (bypasses fx_chain — minimal latency), retriggered throttled to ≤20 grains/sec matched to scrub cursor speed. Recommendation: keep implementation minimal (single-source, gain-only) — expanding it competes with video decode for sidecar bandwidth during fast scrub, and it's explicitly optional scope per 02.

---

## 6. DSP specs

All DSP lives in `audio/dsp/`, operates on resolved (already-smoothed, already-animated) params per block; no knowledge of `Tick`/`AnimProps` inside the DSP layer itself — clean separation, matches 02's IR pattern of "params already keyframe-evaluated" (02 §2).

### 6.1 EQ (biquad, RBJ cookbook forms)

Peaking band (representative; low/high shelf use the cookbook's shelf-specific A/cos(ω0) forms with the same ω0/α inputs):

```
ω0 = 2π·freq_hz/fs
α  = sin(ω0)/(2·Q)
A  = 10^(gain_db/40)

b0 = 1 + α·A     b1 = -2·cos(ω0)     b2 = 1 - α·A
a0 = 1 + α/A     a1 = -2·cos(ω0)     a2 = 1 - α/A
(all coeffs normalized by a0)
```

Low-shelf/high-shelf follow the RBJ cookbook's shelving-filter section (same ω0, A, shelf slope folded into α via `Q`). Five bands per `Eq` unit: low-shelf, three parametric (band1–3), high-shelf, each independently enabled by `gain_db == 0` shortcut (skip processing, cheap no-op) — not a separate `enabled` flag per band, keeps `EffectParams` flat.

Default seed values (concrete, applied when an `Eq` unit is first added — every param remains editable/animatable after):

| Band | `freq_hz` default | `q` default | `gain_db` default |
|---|---|---|---|
| `low_shelf` | 120 | — (shelf slope fixed at `Q=0.707`) | 0 |
| `band1` | 500 | 1.0 | 0 |
| `band2` | 2000 | 1.0 | 0 |
| `band3` | 8000 | 1.0 | 0 |
| `high_shelf` | 10000 | — (shelf slope fixed at `Q=0.707`) | 0 |

Processing order within the unit: low_shelf → band1 → band2 → band3 → high_shelf, cascaded biquads (each stage's output feeds the next). Coefficients recomputed once per block (§5) from that block's smoothed param values.

### 6.2 Envelope follower (shared by Compressor + Gate)

```
coeff_attack  = 1 - exp(-1 / (attack_ms  * 0.001 * fs))
coeff_release = 1 - exp(-1 / (release_ms * 0.001 * fs))
env = env + (|x| - env) * (coeff_attack if |x| > env else coeff_release)
```

Detection signal: RMS of a short (5ms) window by default — smoother gain reduction than instantaneous peak, avoids pumping on transient-heavy sources (dialogue, percussion). No user-facing detection-mode toggle in v1 (not in the requested param list; avoids UI overscope).

### 6.3 Compressor

```
env_db = 20·log10(env)
if env_db > threshold_db:
    gr_db = (threshold_db - env_db) * (1 - 1/ratio)
else:
    gr_db = 0
# fixed internal soft-knee, 6dB width, smoothed quadratic interpolation around threshold — implementation
# constant, not a user param (keeps the param surface to threshold/ratio/attack/release/makeup as scoped)
out = in * 10^((gr_db + makeup_db)/20)
```

**Ducking (sidechain):** `sidechain: Some(track)` redirects envelope detection to read `track`'s post-fx-pre-fader signal (§4 tap point) instead of the host track's own input; gain reduction still applies to the host track's signal. Mixer computes tracks in sidechain-dependency topological order; a cycle (A ducks B, B ducks A) is rejected at edit time — same cycle-check discipline as `NodeGraph` edges (01 §8), never a runtime panic.

**One-click ducking preset** (AS-2 "duck music by voiceover"): applying the preset to (music_track, voiceover_track) ensures a `Compressor` `AudioFxUnit` exists in `music_track.fx_chain` (inserted if absent) with concrete defaults: `threshold_db: -24.0, ratio: 4.0, attack_ms: 5.0, release_ms: 250.0, makeup_db: 0.0, sidechain: Some(voiceover_track)`. One `AudioCmd::ApplyDuckingPreset` undo entry (§10), not a multi-step edit sequence.

### 6.4 Gate

Same envelope follower, inverted logic and no makeup/knee (cheap, as recommended):

```
if env_db < threshold_db:
    gr_db = -range_db     # smoothed toward via attack/release, held for hold_ms after last above-threshold sample
else:
    gr_db = 0
out = in * 10^(gr_db/20)
```

Default `range_db = 60.0` (effective silence below threshold, not a hard mute — avoids clicks a true mute would cause).

### 6.5 Limiter (master default)

Brick-wall, lookahead-based: fixed `lookahead_ms = 5.0` (not user-exposed — a lookahead buffer this size is inaudible as added latency in context and removes a knob that has one correct-ish answer). Gain reduction computed from the lookahead buffer's peak, applied with fast attack (~1ms, effectively as fast as the lookahead allows) and user-configurable `release_ms` (default 50.0), holding true-peak at or below `ceiling_db` (default -1.0 dBTP). `MasterBus::fx_chain` is seeded with one `Limiter` unit at `MasterBus` creation (default project settings, 01 §2) — removable, but the mixer panel (§8) warns when master has no limiter.

### 6.6 Loudness normalization (export)

Own minimal EBU R128 / ITU-R BS.1770-4 implementation: K-weighting pre-filter (two cascaded biquads — same biquad machinery as §6.1) + mean-square gated block loudness (400ms gating blocks, -70 LUFS absolute gate then -10 LU relative gate per the standard) → integrated LUFS. True peak via 4x oversampled peak detection.

Export flow when `MasterBus.loudness_target` is `Some`: render the full work-range master-bus PCM once (in-memory, offline — §7), measure integrated LUFS + true peak against that buffer, compute a single static `gain_offset_db = target.integrated_lufs - measured_lufs`, clamp so `measured_true_peak_dbtp + gain_offset_db <= target.true_peak_dbtp` (reduce offset if the clamp binds), then scale the already-rendered PCM buffer in place by `10^(gain_offset_db/20)`. One measurement pass + one scale pass — no second full mixer re-render needed since export already holds the whole buffer resident.

---

## 7. Offline export mix path

Reuses the identical mixer graph (same `TrackAudio`/`ClipAudio`/`MasterBus`, same DSP code) with no device clock (02 §7): blocks render as fast as CPU allows across the work range, producing one interleaved f32le PCM stream at the sequence's export sample rate. Sample position for tick `t`: `t * sample_rate / TICKS_PER_SECOND` — exact integer arithmetic, since `TICKS_PER_SECOND` (705,600,000) factors cleanly into 44.1/48/88.2/96/192kHz (01 §1). Piped to the encoder sidecar as a second stream alongside video rawvideo (02 §7); container muxing handled by the encoder step.

**Determinism (SS-3):** export-mode param evaluation is purely block-index-driven — the one-pole smoothing in §5 is redefined for export as a function of exact sample count elapsed since the previous keyframe/edit boundary, never wall-clock time. Same project + same preset ⇒ bit-identical PCM on every run. This is what makes the export mix comparable across CI runs and matches the frame-graph IR's determinism property (02 §2) on the audio side.

---

## 8. UI spec

Coordinates with 04 §4.1's panel map: the AudioMixer is a right-drawer group there; 04 owns overall panel docking, this section owns the mixer's interior.

- **Mixer panel:** one channel strip per `Sequence.audio_tracks` entry (order matches track order) + one master strip. Faders and pan knobs are keyboard-operable (focus + arrows, Shift = ×10, click-to-type numeric readouts — 13 §16's fallback rule). Each strip: vertical fader (dB scale, -inf..+12, default 0dB), pan knob (equal-power), dual meter (peak, fast ballistics + RMS, ~300ms window, VU-like) with a clip-indicator LED (latches red above -0.3 dBTP, click to reset), mute/solo buttons (§4 solo-safe logic), fx-slot rack (ordered `AudioFxUnit` list — add/remove/reorder; double-click opens a kind-specific editor: `Eq` = interactive frequency-response curve with draggable band handles; `Compressor`/`Gate` = threshold/ratio curve + live gain-reduction meter; `Limiter` = ceiling slider + GR meter). Master strip additionally shows a live integrated-loudness readout (LUFS) — a rolling, ungated real-time estimate from the same R128 code (§6.6), explicitly approximate; the authoritative value is always the export-time gated measurement.
- **Clip audio overlays (on timeline, 04):** gain line drawn on the clip body (drag to set `gain_db` baseline, or to add an automation keyframe if the clip already has a `ClipAudioParams` `PropertyTrack`), fade handles at clip in/out corners (drag = set `fade_in`/`fade_out` `duration`; right-click = pick `FadeShape`), waveform rendered behind the gain line (§8.1).
- **Automation lanes:** track expand reveals `PropertyTrack` lanes for `TrackAudioParams` (`volume_db`, `pan`) and any `AudioFxUnit` param the user has pinned to a visible lane (not all params shown by default — same expand/pin pattern used for any other `AnimProps`-driven param elsewhere in the app). Keyframe add/drag/interp editing (Hold/Linear/Bezier) matches 01 §6/§10 exactly — same coalesce-by-(variant, id) rule.

### 8.1 Waveform rendering (peak pyramid)

On import, decode full audio stream once → compute per-bucket min/max + RMS at multiple resolutions: level 0 = 256 samples/bucket, each higher level downsamples ×4 until the top level covers the whole asset in ≈1000 buckets. Stored in the cache sidecar dir (`<project>.photon.cache/`, 01 §9) keyed by asset content hash — never in the JSON document. Timeline selects the pyramid level whose bucket width ≈ 1–2 screen pixels at current zoom (avoids both aliasing at low zoom and wasted detail at high zoom); renders a filled min/max envelope with an RMS overlay (two-tone fill, standard DAW convention).

---

## 9. Voiceover/TTS interplay

TTS-generated clips (06, CAP-011) are ordinary audio `Clip`s with a normal `ClipAudio` — no special-cased clip variant in the mixer or data model. The ducking preset (§6.3) is the only special UI affordance: selecting a music track + a voiceover/dialogue track and applying "duck music" wires the sidechain automatically with the concrete defaults given in §6.3. Nothing else in this doc treats TTS audio differently from imported audio.

---

## 10. Undo integration

Nested inside `TimelineCmd::AudioEdit(AudioCmd)` (01 §10):

```rust
pub enum AudioCmd {
    SetTrackAudioProp { track: TrackId, old: TrackAudioParams, new: TrackAudioParams },
    SetTrackMuteSolo   { track: TrackId, old: (bool, bool), new: (bool, bool) },
    SetClipAudioProp   { clip: ClipId, old: ClipAudioParams, new: ClipAudioParams },
    SetClipFade        { clip: ClipId, edge: FadeEdge, old: Option<AudioFade>, new: Option<AudioFade> },
    SetChannelMap      { clip: ClipId, old: ChannelMap, new: ChannelMap },
    AddAudioFx         { owner: FxOwner, index: usize, unit: AudioFxUnit },
    RemoveAudioFx      { owner: FxOwner, index: usize, unit: AudioFxUnit },
    ReorderAudioFx     { owner: FxOwner, old_order: Vec<usize>, new_order: Vec<usize> },
    SetMasterBusProp   { old: MasterBusParams, new: MasterBusParams },
    SetLoudnessTarget  { old: Option<LoudnessTarget>, new: Option<LoudnessTarget> },
    ApplyDuckingPreset { track: TrackId, sidechain: TrackId, old_fx_chain: Vec<AudioFxUnit>, new_fx_chain: Vec<AudioFxUnit> },
}
pub enum FxOwner { Track(TrackId), Master }
pub enum FadeEdge { In, Out }
```

Fx-param keyframe edits (any `AudioFxUnit.params` `PropertyTrack`) reuse the existing generic `TimelineCmd::SetKeyframe`/`RemoveKeyframe`/`SetKeyframeInterp` variants (01 §10) — already generic over any `AnimProps` target via `PropPath`, no new variant needed. Fader/pan/gain drags coalesce by (variant, track/clip id) matching the existing `UpdateNode`-style coalesce rule (01 §10). Memory rule unchanged: O(bytes of changed structs) — `AudioFxUnit` is small, no media referenced.

---

## 11. Risks + test hooks (for 11-testing-phasing.md)

| Risk / test | Detail |
|---|---|
| DSP unit fixtures | Known signals → measured response: sine sweep vs. `Eq` frequency response curve; step/tone-burst vs. `Compressor`/`Gate` attack-release timing; full-scale noise vs. `Limiter` ceiling. Compared against closed-form expected curves within tolerance — same pattern as 01 §6's keyframe-eval closed-form tests. |
| Sync test | Beep-flash media (known audio beep aligned to a known flash frame) through full playback + export path; measure A/V offset; must stay under SS-3's one-frame-over-10-minutes tolerance (master-clock-is-audio design, 02 §4). |
| Determinism test | Export same project N times; hash PCM; must be bit-identical (§7). |
| Xrun/underrun telemetry | cpal callback increments an atomic counter on ring-empty (underrun) or ring-full-on-write (overrun/mixer-worker-too-slow). `EngineStatus` (02 §1) gains `audio_xruns: u32`. Budget: zero xruns during the SS-1 reference scenario (1080p30, 3 layers) on the reference dev machine — hard CI gate. |
| Full-mixer scope vs. P8 budget | Already mitigated at the phase level (§1): schema exists from P3, DSP/UI additive in P8 — no late schema break. |
| Sidechain cycle | Rejected at edit time (§6.3), mirrors `NodeGraph` cycle-check (01 §8) — never a runtime deadlock/panic. Test: attempt A↔B mutual duck, verify edit op returns `EditError`, document unchanged. |
| Resampler CPU cost | Multiple simultaneous multi-rate sources resampling per-block would spike mixer-worker CPU. Mitigation: resample once per decoded GOP into the decode cache (§3), never per-mixer-block on raw decode output — keeps the real-time mixer path allocation/lock-free per 02 §1. Add a perf-budget row to 02 §8 (`N concurrent audio sources resample + mix` — measured in 11). |
