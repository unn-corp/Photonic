# 31 — Audio Graph Architecture: Contracts, Declick, Analysis, and Delivery

**Status:** Draft — implementation contract; no code authorization
**Date:** 2026-07-20
**Audience:** audio engine implementers, export owners, mixer GUI owner

**Depends on:** [01-data-model.md](01-data-model.md) (`TrackAudio`, `ClipAudio`, `MasterBus`, `AudioFxKind`), [02-engine.md](02-engine.md) (threading, clock), [09-audio-mixer.md](09-audio-mixer.md) (mixer graph, DSP catalogue), [11-testing-phasing.md](11-testing-phasing.md) (SS-3 sync budget), [26-kdenlive-mlt-parity.md](26-kdenlive-mlt-parity.md), [27-spec-audit.md](27-spec-audit.md).

**Owns:** the **discontinuity and latency contracts** ([26 E-10](26-kdenlive-mlt-parity.md#e-10--the-audio-graph-needs-discontinuity-and-latency-contracts-before-the-dsp-is-wired)), the **pull-based audio analysis service** ([26 E-2](26-kdenlive-mlt-parity.md#e-2--analysis-as-node), audio half), **boundary declick** ([26 K-D5](26-kdenlive-mlt-parity.md#k-d5--declick-at-clip-boundaries)), **FX-chain and mixer binding** ([26 K-0.6](26-kdenlive-mlt-parity.md#8-k-0--foundations)), **loudness on export** ([26 K-0.7](26-kdenlive-mlt-parity.md#8-k-0--foundations)), **per-stream/per-channel handling** ([26 K-D3](26-kdenlive-mlt-parity.md#k-d3--per-stream-and-per-channel-audio-handling)) and **stems export** ([26 K-D4](26-kdenlive-mlt-parity.md#k-d4--per-track-audio-export)).

**Does not own:** the mixer's visual design ([13](13-ux-components.md)), the DSP unit catalogue itself ([09 §6](09-audio-mixer.md)), audio recording ([26 K-D2](26-kdenlive-mlt-parity.md#k-d2--timeline-audio-recording--product-blocked), `product-blocked`), or multicam sync ([23 §6](23-legal-open-source-implementation-routes.md#6-g-20--photonic-multicam-route), G-20).

---

## 1. Why this document exists, and why now

Photonic's audio DSP is **written but disconnected**. `audio/dsp/` holds ~1,900 lines of EQ, compressor, gate, limiter and EBU R128 loudness; `mixer.rs:9` states plainly that `fx_chain` is inert; `LoudnessTarget` rides on every `ExportPreset` and is never applied; the mixer panel renders the literal banner *"meters simulated (wiring seam)"*; and `EngineBridge::master_level()` returns `None`.

That is all planned P8 work. **The problem is what happens the moment it is wired.**

Every stateful unit in that catalogue — compressor and gate envelopes, the limiter's delay line, loudness windows — carries state across blocks. Verified: **there is no `reset()` on any unit**, and no graph-level latency reporting. `limiter.rs` has a fixed 5 ms lookahead with a documented latency invariant, which proves the concept is understood for one unit and generalised for none.

So on the first seek after wiring, every envelope smears; on the first cut, state from the outgoing clip bleeds into the incoming one; and as units accumulate, undeclared lookahead drifts audio against video until SS-3 fails. **Retrofitting these contracts means touching every unit**, which is why this document exists before the wiring rather than after it.

---

## 2. Contract 1 — discontinuity

### 2.1 The event

```rust
pub enum AudioDiscontinuity {
    Seek { to: Tick },
    ClipBoundary { track: TrackId, at: Tick },
    GraphChanged,          // fx chain edited, unit added/removed/reordered
    Reconfigure,           // sample rate or channel count changed
}
```

Delivered to every stateful unit **before** the block that crosses it. The mixer already renders per block (`BLOCK_FRAMES = 512`) with `block_start_tick`, so the boundary is computable without new plumbing.

### 2.2 The unit contract

```rust
pub trait AudioUnit {
    fn process(&mut self, buf: &mut [f32], ctx: &BlockCtx);
    fn reset(&mut self, cause: AudioDiscontinuity);
    fn latency_samples(&self) -> u32 { 0 }
    fn tail_samples(&self) -> u32 { 0 }   // how long output persists after silence in
}
```

`reset` is **mandatory** — the default must not be a no-op, because a silently-unimplemented reset is exactly the bug this contract exists to prevent. Every existing unit in `audio/dsp/` gains one:

| Unit | `reset` must clear |
|---|---|
| `compressor`, `gate` | envelope follower state, gain-reduction memory |
| `limiter` | the 5 ms delay line — **re-primed with silence**, preserving its documented "first `lookahead_samples` are delayed silence" invariant |
| `eq`, `biquad` | biquad `z1`/`z2` histories (otherwise a filter rings across the seek) |
| `loudness` | integration windows and the gating state |

### 2.3 Policy

- `Seek` and `Reconfigure` reset **everything**.
- `ClipBoundary` resets only units **downstream of that clip** — a master-bus compressor must *not* reset on every cut, or it will pump audibly at each edit. This distinction is the whole reason the enum carries the cause.
- `GraphChanged` resets the changed unit and everything after it.
- Reset is **not** a fade. Where a discontinuity is audible, §4's declick handles it; reset only prevents *stale* state, it does not smooth the signal.

---

## 3. Contract 2 — latency

### 3.1 Reporting and compensation

Each unit declares `latency_samples()`. The mixer sums along each path and applies compensation so that **all paths reaching the master bus are aligned**, and the master's total latency is reported to the clock so A/V sync stays correct.

```
track_latency(t)  = Σ latency_samples() over t's fx chain
graph_latency     = max over tracks of track_latency(t) + master chain latency
compensation(t)   = graph_latency - track_latency(t) - master_latency
```

Each track's compensation is a plain delay line. Without this, a track carrying a lookahead limiter arrives late against a dry track and the mix smears — the failure is subtle, sounds like phasing, and is very hard to diagnose after the fact.

### 3.2 Interaction with the master clock

`clock.rs` masters playback on audio frames consumed by the cpal callback. Total graph latency is a **constant offset** between "samples written" and "samples heard", and it must be subtracted when deriving the playhead, or the video will lead the audio by exactly the graph latency.

**Budget:** total graph latency ≥ 40 ms must warn; ≥ 100 ms must refuse to engage in interactive playback and offer offline rendering instead. Some units are genuinely expensive — a Gaussian-smoothed normaliser can cost **seconds** of lookahead — and such a unit is legitimate at export and unusable live. The manifest must say which.

### 3.3 Export

Offline export runs the identical graph ([09 §7](09-audio-mixer.md)) and must **flush tails**: after the last input block, continue pulling `tail_samples()` worth of silence so reverb, delay and limiter release are not truncated. Truncated tails at the end of a render are a classic and embarrassing bug.

---

## 4. Boundary declick

Closes [26 K-D5](26-kdenlive-mlt-parity.md#k-d5--declick-at-clip-boundaries). Cutting mid-waveform produces a click, because the sample value jumps at the splice. `AudioFade`'s four shapes are the wrong tool: fading every cut audibly dips the level on sustained material.

### 4.1 Algorithm

Reproduced from published behaviour, not source ([26 §2](26-kdenlive-mlt-parity.md#2-clean-room-and-licensing-fence)):

1. Engage only on a **clip-boundary block**.
2. Measure `delta_db` between the outgoing clip's final sample and the incoming clip's first, per channel.
3. If `|delta_db| <= threshold` (**default 2 dB**, range 0..30), do nothing — most cuts land at similar amplitudes and need no repair.
4. Otherwise synthesise continuation samples by **time-reversing the outgoing tail**, and linearly crossfade them into the incoming head over a short window (**default 1000 samples**, ~21 ms at 48 kHz):

```
for i in 0..n:
    mix       = (n - i) / n
    out[i]    = tail_reversed[i] * mix + head[i] * (1 - mix)
```

**Reversing is the point.** It keeps the waveform phase-continuous across the splice rather than dipping toward zero, which is why it outperforms a fade on sustained tones.

### 4.2 Structural requirement

This forces the mixer to expose **the previous segment's tail** when rendering the next segment's head. `Mixer::render_block` has no such concept today, and this is a graph-shape decision that is painful to retrofit — hence §8's sequencing.

```rust
pub struct SegmentBoundary { pub track: TrackId, pub at: Tick, pub tail: [Vec<f32>; CHANNELS] }
```

The tail is `max(declick_window, largest downstream tail_samples)` long, cached per track, invalidated on edit.

### 4.3 Interaction

- Declick applies **before** the clip's own fades — an explicit fade means the user wants silence and no repair is needed.
- Disabled when either side is silent.
- Project-level `declick_threshold_db`, with per-clip override. Default **on**: it is inaudible when unnecessary and fixes a defect when it is.
- **Deterministic** — same input, same output, so it is safe under SS-3.

---

## 5. Pull-based analysis

[26 E-2](26-kdenlive-mlt-parity.md#e-2--analysis-as-node)'s audio half, and an explicit rejection of the reference's design.

### 5.1 Why not the reference's shape

MLT's audio-reactive filters keep a sliding sample ring **on the filter instance**, fill it from the audio callback, and stash results as a frame property the image callback reads back. Its own error text states the contract: *"This filter depends on the consumer processing the audio before the video."*

Three consequences disqualify it here: analysis is strictly **causal** (no lookahead expressible), **seeking corrupts the window**, and **parallel or out-of-order frame rendering is illegal** — which is exactly what a wgpu engine wants to do.

### 5.2 The service

```rust
pub trait AudioAnalysis {
    fn samples_at(&self, at: Tick, window: Duration, scope: AnalysisScope) -> Option<SampleBlock>;
    fn spectrum_at(&self, at: Tick, cfg: &FftConfig, scope: AnalysisScope) -> Option<Spectrum>;
    fn levels_at(&self, at: Tick, scope: AnalysisScope) -> Option<Levels>;
}
pub enum AnalysisScope { Master, Track(TrackId), Clip(ClipId) }
```

**Position-addressed, not stream-addressed.** The implementation seeks the audio graph or reads the peak pyramid `audio/waveform.rs` already builds and caches by content hash. Consequences: stateless, order-independent, **seek-correct**, parallelisable, non-causal windows are expressible, and it unifies with the timeline waveform display instead of duplicating it.

### 5.3 Consumers

Live meters (closing G-4 and part of K-0.6) · audio-reactive visualisers · silence detection · two-pass loudness (§6) · beat detection (D-4) · audio align (K-D1/G-20) · waveform display.

Every one of those is currently either bespoke or blocked. That is the leverage argument for E-2.

---

## 6. Loudness and delivery

### 6.1 Two modes, and they are different products

| | Online | Offline |
|---|---|---|
| Gain | time-varying, sliding window | **one constant gain** |
| Use | interactive preview | **export** |
| Effect on dynamics | compresses long-term dynamics | transparent |

Photonic must ship the **offline** path for export — that is what "normalize to −14 LUFS" means to a user — and may offer online for preview. `dsp/loudness.rs` already implements EBU R128 / BS.1770-4 integrated LUFS and true peak; what is missing is applying it.

### 6.2 As a job, not a graph node

Two-pass loudness is **analyse → cache → apply**, not a node ([26 E-10](26-kdenlive-mlt-parity.md#e-10--the-audio-graph-needs-discontinuity-and-latency-contracts-before-the-dsp-is-wired) constraint 3):

1. Analysis pass over the export range via §5, producing integrated LUFS, LRA and true peak.
2. Compute constant gain to hit `LoudnessTarget.integrated_lufs`.
3. If that gain would breach `true_peak_dbtp`, reduce it and **report** — never silently exceed the ceiling, and never silently fall back to a different algorithm.
4. Apply during render; cache the measurement by content hash so a re-render is free.

### 6.3 Target hygiene

Photonic's presets are **−14 LUFS** (streaming) and **−23 LUFS** (broadcast, EBU R128). FFmpeg's `loudnorm` defaults to **−24 LUFS** (ATSC A/85). If any export path ever routes through it, that inconsistency must not reach the UI: the target is Photonic's, stated in the preset, and passed explicitly.

### 6.4 Stems

[26 K-D4](26-kdenlive-mlt-parity.md#k-d4--per-track-audio-export). `ExportPreset` gains `stems: bool`. The mixer already renders per-track buses, so this is an export-loop and muxing change, not new DSP. Rules: stems are **post-fader, post-FX, pre-master**; master processing (limiter, loudness) applies **only** to the mixed output, never to stems — a stem carrying master limiting cannot be re-mixed, which defeats the purpose.

---

## 6a. Sample-rate conversion — an unowned functional gap

[27 SD-13](27-spec-audit.md#3-sd---spec-versus-code-drift) found that [09 §2](09-audio-mixer.md) declares `rubato` "adopt for v1, no alternative under consideration" — and **it is in no `Cargo.toml`**. `mixer.rs` requires every source to arrive already at the mix rate, and there is no resampler in the tree.

Two consequences, both real:

1. **A source whose sample rate differs from the project rate has no correct path.** 44.1 kHz music in a 48 kHz project is the single most common case in editing.
2. **Non-1:1 clip speed has no audio path at all** — `session.rs:1777` defers it explicitly. A speed-changed clip is exactly a resampling problem.

**Recommendation: one resampler, owned here, used by both.** A single deterministic sinc-based converter serves fixed source→project conversion, small drift correction between the decode clock and the device clock, and `SpeedMap`-driven rate change. Determinism (SS-3) requires it be the *same* implementation on the interactive and export paths — two resamplers producing subtly different output would break export goldens in a way that is very hard to attribute.

- **Intake:** `rubato` is `MIT OR Apache-2.0` and is already an `ADOPT` candidate in [23 §5](23-legal-open-source-implementation-routes.md#5-upstream-evidence-and-dispositions). It still needs a [23 §3.3](23-legal-open-source-implementation-routes.md#33-required-evidence-record) evidence record before intake — being listed as a candidate is not approval.
- **Latency** is declared through §3's contract like any other unit.
- **Discontinuity:** the resampler holds filter state and **must** implement `reset` (§2.2), or a seek smears across it.
- **Pitch:** rate conversion for *speed* changes pitch, which is correct default behaviour. Pitch-preserving time-stretch is a separate, much more expensive unit and is **not** in scope here.

Until this lands, [09 §2](09-audio-mixer.md)'s resampling claims describe an intention, and [26 K-D](26-kdenlive-mlt-parity.md#12-k-d--audio) should not be considered closable.

## 7. Per-stream and per-channel

Closes [26 K-D3](26-kdenlive-mlt-parity.md#k-d3--per-stream-and-per-channel-audio-handling). `AudioStreamInfo` is probed and `ChannelMap` exists; there is no UI and no per-clip offset.

```rust
pub struct ClipAudio {
    // existing: params, fade_in, fade_out, channel_map
    pub stream: Option<u32>,     // which source audio stream; None = first
    pub offset: Tick,            // ± sync offset, ms-resolution in the UI
}
```

- **Stream selection** — multi-stream sources (separate mic and camera tracks) currently expose only the first.
- **Offset** is the standard fix for a camera whose audio leads or lags. `Tick` is flicks-based, so millisecond resolution is exact; the UI works in ms, the model in ticks.
- **Per-channel operations** — normalize, swap, copy, gain — are expressed through the existing `ChannelMap` plus a per-channel gain vector, not as new effects. One N×N routing primitive with presets is the right shape; the reference ships a dozen near-duplicate services because it lacked one.

---

## 8. Sequencing

The ordering is the point of this document.

| Step | Work | Why here |
|---|---|---|
| 1 | `AudioUnit` trait: `reset` + `latency_samples` + `tail_samples`, implemented on **all** existing units, with tests asserting reset actually clears | Before anything depends on them |
| 2 | Discontinuity events plumbed to the mixer; §2.3 policy | Before FX are audible |
| 3 | Latency summation, compensation, clock offset | Before more than one unit exists in a chain |
| 4 | §4 segment-boundary/tail plumbing | Graph-shape decision; painful later |
| 5 | **K-0.6** — wire the FX chain, bind the mixer panel, publish the master meter (closes G-4) | Now safe |
| 6 | §5 analysis service | Unblocks meters properly, and D-4/K-D1 |
| 7 | **K-0.7** — export audio muxing + §6 loudness | Needs 1–5 |
| 8 | §4 declick enabled by default; §7 per-stream/offset; §6.4 stems | Feature layer |

**Steps 1–4 are the cost of K-0.6 being done correctly**, and should be scheduled as part of it rather than as separate items. That is the single most important scheduling claim in this document.

---

## 9. Acceptance

1. **Reset correctness** — for each stateful unit: process signal, seek, process silence; assert output is silent. A unit whose envelope survives the seek fails.
2. **Latency compensation** — an impulse through a chain with a lookahead unit and a dry chain arrives sample-aligned at the master.
3. **Clock offset** — SS-3 A/V drift stays within budget with a lookahead unit engaged; this is the test that would have caught the drift silently.
4. **Declick** — synthetic tone cut mid-cycle: assert no sample-to-sample discontinuity above threshold, and assert **no level dip** (which is what distinguishes it from a fade). Assert bit-identical output across runs.
5. **Boundary policy** — a master compressor does **not** reset at clip boundaries; a clip-level gate does.
6. **Loudness** — a signal of known LUFS normalises to target within tolerance; a signal that would breach true peak is reduced **and reported**.
7. **Stems** — summing all stems equals the master **before** master processing; each stem is independently decodable.
8. **Tail flush** — a render ending during a reverb tail contains the tail.
9. **Determinism** — the whole audio path is wall-clock-free (`mixer.rs` already guarantees this; these additions must not break it).
10. **MCP parity** — `get_audio_meters` succeeds after step 5, closing the `NotSupportedV1` gap noted in [26 §16](26-kdenlive-mlt-parity.md#16-k-h--mcp-trail).

---

## 10. Compatibility and protected surfaces

- **Additive serde** — `ClipAudio.stream`/`.offset`, `ExportPreset.stems`, project-level `declick_threshold_db`, all defaulted.
- **Protected:** `Mixer::render_block`'s wall-clock-free contract and its shared use by playback and export ([26 PA-10](26-kdenlive-mlt-parity.md#5-photonic-ahead-register-pa---do-not-port-backwards) becomes true when K-0.6/K-0.7 land — until then it is intent, not a held property). The lock-free SPSC ring and real-time-safe callback must stay allocation- and lock-free.
- **Sidechain ducking** ([26 PA-5](26-kdenlive-mlt-parity.md#5-photonic-ahead-register-pa---do-not-port-backwards)) is an existing model-level advantage — `apply_ducking_preset` with cycle checking — and its cycle guard must survive the latency work, since a sidechain path has its own latency and a naive compensator could reintroduce a cycle.
- **Non-goal, unchanged:** audio recording remains `product-blocked` pending an S13 amendment; nothing in this document authorizes an input stream. Surround remains out of scope — `CHANNELS = 2`.
