# 32 — Engine Contracts: Source Range, Analysis, Threading, Playback Policy, Parity

**Status:** Draft — implementation contract; no code authorization
**Date:** 2026-07-20
**Audience:** engine maintainers, effect implementers, test owners

**Depends on:** [02-engine.md](02-engine.md) (the contracts amended here), [03-render-color-pipeline.md](03-render-color-pipeline.md), [11-testing-phasing.md](11-testing-phasing.md), [24-preview-media-load.md](24-preview-media-load.md), [25-performance.md](25-performance.md), [26-kdenlive-mlt-parity.md](26-kdenlive-mlt-parity.md), [27-spec-audit.md](27-spec-audit.md).

**Owns:** the IR **source-range contract** ([26 E-1](26-kdenlive-mlt-parity.md#e-1--source-range-declaration-in-the-node-contract)), **analysis nodes** ([26 E-2](26-kdenlive-mlt-parity.md#e-2--analysis-as-node), video half), **declared threading capability** ([26 E-4](26-kdenlive-mlt-parity.md#e-4--declared-threading-capability-per-node)), **playback drop/prefill policy** ([26 E-5](26-kdenlive-mlt-parity.md#e-5--explicit-playback-drop-and-buffer-policy)), **scale invariance** ([26 E-6](26-kdenlive-mlt-parity.md#e-6--preview-scale-invariance-is-a-bug-class)), **seek policy and decode budgets** ([26 E-7](26-kdenlive-mlt-parity.md#e-7--split-seek-policy-and-byte-budgeted-decode-window)), **CPU/GPU equivalence** ([26 E-9](26-kdenlive-mlt-parity.md#e-9--cpugpu-evaluator-equivalence-as-a-bug-class)), **interlaced source support** ([26 K-G6](26-kdenlive-mlt-parity.md#k-g6--interlaced-source-support)), and the **still-image cache key** ([26 K-C8](26-kdenlive-mlt-parity.md#k-c8--key-the-still-image-cache-on-requested-size)).

**Does not own:** effects ([30](30-effect-catalogue.md)), audio ([31](31-audio-architecture.md)), preview rendering ([33](33-timeline-preview-render.md)), or colour-space decisions ([03](03-render-color-pipeline.md), and the open [27 A-1](27-spec-audit.md#a-1--p0--the-live-canvas-composites-in-gamma-headless-composites-in-linear)/[A-3](27-spec-audit.md#a-3--p0--grade-operators-apply-transfer-functions-to-premultiplied-alpha)).

---

## 1. Source range — the one mechanism for temporal access

### 1.1 The problem, stated from the reference's failure

MLT accreted **three overlapping speed mechanisms** — a playback-rate multiplier, a time-warping producer, and finally a chain/link retiming primitive — and needed a further wrapper class for filters wanting future frames. The root cause is single and instructive: *a filter receives frames and cannot ask its producer for a different one.* Every temporal capability therefore had to be bolted on somewhere else.

Photonic has `IrOp::TimeOffset`, which compiles by duplicating its upstream subgraph re-evaluated at `t − offset` (deduplicated by content hash, soft-capped at four distinct offsets), and `SpeedMap` for per-clip source-time mapping. Both work. Neither is a **general contract**, and there is no way for a node to declare what it needs.

### 1.2 The contract

```rust
pub trait TemporalNode {
    /// Given an output tick, the upstream tick range this node must read.
    fn source_range(&self, out: Tick, rate: FrameRate) -> FrameRange;
}

pub struct FrameRange { pub first: Tick, pub last: Tick, pub stride: Option<Tick> }
```

Default is the identity range `[out, out]` — the overwhelming majority of nodes, costing them nothing.

### 1.3 What falls out of it

| Capability | `source_range` |
|---|---|
| Frame blending / retime | arbitrary, from the speed or time map |
| Motion blur | `[out − shutter/2, out + shutter/2]`, strided by sub-frame |
| Echo / trails | `[out − n·offset, out]` |
| Deinterlace (§6) | `[out − 1, out + 1]` — the reason it must not be an effect |
| Optical flow | `[out − 1, out + 1]` |
| Frame-rate conform | source-rate neighbours around `out` |

One mechanism replaces what the reference needed four for.

### 1.4 Consequences for the compiler

- **Prefetch is derived, not guessed.** The union of source ranges over a compiled graph is exactly the decode window needed for that tick. `playback/prefetch.rs` currently infers this from clip layout; with the contract it can be computed.
- **Bounded expansion.** A node reading `k` upstream frames multiplies its subtree by `k`. Content hashing dedups shared frames, but the compiler must enforce a budget and emit a `CompileDiagnostic` past it — extending the existing four-offset cap on `TimeOffset` rather than inventing a second rule.
- **Reverse playback.** Under negative rate, a node whose range is asymmetric must declare `reverse_safe: false` ([30 §2.3](30-effect-catalogue.md)) so it is bypassed or flushed rather than reading the wrong side.

### 1.5 Gates

[26 G-11](26-kdenlive-mlt-parity.md#19-priority-and-dependencies)'s rubber-band speed depth, §6 interlacing, motion blur, and any frame-blended retime. **This gets harder the more temporal nodes exist**, so it precedes them all.

---

## 2. Analysis nodes

Video half of [26 E-2](26-kdenlive-mlt-parity.md#e-2--analysis-as-node); the audio half is [31 §5](31-audio-architecture.md).

```rust
pub enum AnalysisResult { Levels(..), Motion(..), SceneCuts(..), Histogram(..), Transform(..) }

pub trait AnalysisNode {
    fn analyze(&self, input: &Frame, at: Tick, ctx: &AnalysisCtx) -> AnalysisResult;
}
```

Analysis nodes emit **typed metadata**, not pixels — deliberately typed, not the reference's string property bag ([26 E-8](26-kdenlive-mlt-parity.md#e-8--protected-properties-that-are-already-right)). They are cached by the same `ContentHash` as pixel nodes, so re-analysis after an undo is free.

**Unlocks:** motion tracking ([26 K-B10](26-kdenlive-mlt-parity.md#k-b10--motion-tracking), `product-blocked`) · scene detection (D-15) · audio align (K-D1/G-20) · beat detection (D-4) · loudness on export ([31 §6](31-audio-architecture.md)) · live meters · stabilization analysis (D-12).

**Two-pass rule.** Anything needing whole-range measurement is a **job**, not a node: analyse → cache by content hash → apply. Consumers read the cached result. A node that silently blocks on a full traversal would stall interactive playback.

---

## 3. Threading capability

```rust
pub enum Threading {
    Any,          // pure function of inputs — parallelisable freely
    PerInstance,  // holds state; one instance may not run concurrently with itself
    Serial,       // must run in frame order
}
```

Declared per node kind. Today evaluation is GPU-serial per frame with CPU worker ops (`MatteExtract`) alongside, so the failure mode has not arisen — but it will as [30](30-effect-catalogue.md)'s catalogue and §2's analysis nodes multiply CPU-side work.

The reference's cautionary tale: its only parallelism is whole-frame, which breaks every temporal and stateful effect, forbids its GPU path entirely, and stalls on one slow frame. Declaring capability is cheap now and expensive to retrofit — the same argument as [31 §2](31-audio-architecture.md), for the same reason.

**Rule:** a node that fails to declare defaults to `Serial`. Fail safe, not fast.

---

## 4. Playback policy

Closes [26 E-5](26-kdenlive-mlt-parity.md#e-5--explicit-playback-drop-and-buffer-policy). The mechanism exists — `FramePresenter`'s cover-interval rule with late-drop counting, `EngineStatus.{dropped, buffering, audio_xruns}`. The **policy** is undocumented, which is [27](27-spec-audit.md)'s complaint about this area generally.

| Knob | Value | Meaning |
|---|---|---|
| `prefill_frames` | 6 (Draft) / 12 (Full) | Decoded before playback starts |
| `max_consecutive_drops` | 5 | Beyond this, recover rather than continue dropping |
| Recovery | force-render the next frame, then re-evaluate | Never drop indefinitely |
| Ring depth | 24 fwd / 6 back | Superseded by §5's byte budget |
| Underrun | report `buffering`, hold last frame | Never present black |

**Stating the policy is most of the work**, and it belongs in [02 §4](02-engine.md) and [25](25-performance.md), not only here — [27 A-11](27-spec-audit.md#a-11--p2--ring-depths-and-cut-ahead-lead-conflict-three-ways) already records that those two documents disagree with each other and with the code about ring depths.

**Watch-out inherited from the reference:** its recovery advances a work cursor toward the tail when dropping, which trades latency for throughput invisibly. Photonic must make the trade explicit and *reportable* rather than adaptive-and-silent.

---

## 5. Seek policy and decode budgets

### 5.1 Three policies, not one knob

The reference governs nearly all playback smoothness with a single global integer ("seek if the target is more than N frames ahead"), which is fine for playback and imprecise for scrubbing long-GOP media.

| Context | Policy |
|---|---|
| **Playback** | Sequential; seek only when the target leaves the ring |
| **Scrub** | Keyframe-only (`scrub_to`), no decode-forward; a trailing exact seek settles it |
| **Export** | Exact, sequential, never approximate; correctness over latency |

Photonic already implements all three (`DecodeSource::scrub_to`, `EngineCmd::ScrubSeek`, the export loop). What is missing is the **statement** that they are three policies — so a future change to one does not silently alter the others.

### 5.2 Byte budget

Ring depths are **frame counts** (`DEFAULT_FWD = 24`, `DEFAULT_BACK = 6`), so memory scales with resolution: 24 frames of 1080p ≈ 75 MB, of 4K ≈ 300 MB, per active source, with up to `MAX_LIVE_SOURCES = 8`. Worst case is multiple gigabytes with no ceiling.

**Fix:** budget in bytes against `ProjectVideoSettings::cache_limit_mb`, deriving frame counts from resolution, with a floor (≥ 4 forward) so a 4K source still prefetches enough to play.

### 5.3 Targeted invalidation

`InvalidateRange` currently clears the whole node cache and all decode sources (`session.rs:167-170`). With content hashing, invalidation should be **hash-natural**: a relink or proxy swap changes the affected source's hash and its dependents age out. Only asset identity changes need explicit eviction.

---

## 6. Interlaced sources

Closes [26 K-G6](26-kdenlive-mlt-parity.md#k-g6--interlaced-source-support). **Photonic has no field handling at all** — zero hits for `interlac`, `field_order`, `tff`/`bff`. Interlaced footage is decoded, composited and exported as though progressive, producing combing with no diagnosis and no remedy. This is a **silent wrong-output path**, not a missing convenience.

### 6.1 Detection

```rust
pub enum ScanType { Progressive, Interlaced { order: FieldOrder }, Unknown }
pub enum FieldOrder { TopFirst, BottomFirst }
```

`ProbeDetails` gains `scan: ScanType`, from ffprobe's field-order metadata plus a heuristic where absent. Surfaced through [26 K-C7](26-kdenlive-mlt-parity.md#k-c7--import-time-media-triage-report)'s triage report with the consequence spelled out, alongside the existing `is_vfr` flag.

### 6.2 Deinterlace is a source-range node, not an effect

It reads neighbouring fields, so it lives on §1's contract with `source_range = [out−1, out+1]`. **The reference moved deinterlacing from a filter to a link for exactly this reason**; building it as an effect would reproduce a mistake whose fix required an API break.

Ship one good algorithm — a motion-adaptive spatial-temporal deinterlacer — plus `Weave` (no-op, for progressive-in-interlaced-container) and `Bob` (field-doubling, when frame rate should double). Field-order handling and an interlaced-output option belong to `ExportPreset`.

### 6.3 Relationship to pulldown

[27 U-7](27-spec-audit.md#5-u---under-specified-contracts) records that mixed frame rates and pulldown are unspecified. Telecine detection is a **different and harder** problem than deinterlacing and is explicitly **out of scope here**; the two are related only in both being "this is not progressive 1:1 footage". §1's contract is what would make an inverse-telecine node expressible later.

---

## 7. Scale invariance

Closes [26 E-6](26-kdenlive-mlt-parity.md#e-6--preview-scale-invariance-is-a-bug-class). Every geometry-carrying op has one hazard: a parameter denominated in **pixels** must scale with the render target, or Draft and Full disagree. The reference's own documentation names this a recurring bug source.

**Rule.** Every `ParamSpec` is one of:
- **Normalised** (0..1 of frame dimension) — scale-free by construction. **Preferred for all new parameters.**
- **Pixel-denominated** — must declare `scales: true`, and the evaluator multiplies by the render scale factor.
- **Scale-free** (angles, opacity, counts, colours) — never scaled.

**Test.** Render a geometry-heavy fixture at Draft and at Full, downsample Full to Draft, compare within tolerance. Add it to [11](11-testing-phasing.md) **before** [30](30-effect-catalogue.md)'s catalogue grows, so every new effect inherits the guard rather than the bug.

---

## 8. CPU/GPU equivalence

Closes [26 E-9](26-kdenlive-mlt-parity.md#e-9--cpugpu-evaluator-equivalence-as-a-bug-class). Unlike §7, this one has **already produced a live defect**: `eval_cpu.rs:154` passes `mode` into `ops::merge`, which blends all 26 modes; `eval.rs:319` destructures `mode` away and runs a hard-coded premultiplied `over`. The two evaluators disagree on every non-`Normal` blend mode, and since the CPU path is the golden reference, goldens generated on one path silently disagree with the other.

**Three actions, in order:**

1. **Diagnostic now.** The GPU `Merge` emits a `CompileDiagnostic` on any non-`Normal` mode instead of compositing wrongly — the same treatment `Wipe`/`Push` already get (`compile.rs:580-586`). A visible wrong result beats a silent one, and this is hours of work.
2. **Equivalence sweep.** Iterate **every variant of every enum the IR carries** — blend modes, `Interp`, `GradeOpKind`, `LutInterp`, `Sampling`, `FitMode`, `TransitionKind`, and [30](30-effect-catalogue.md)'s manifest table — asserting CPU and GPU agree within tolerance. This is the test that makes the next divergence fail CI instead of ship.
3. **Converge**, as part of [26 K-0.3](26-kdenlive-mlt-parity.md#8-k-0--foundations).

**⚠ Ordering.** [27 A-1](27-spec-audit.md#a-1--p0--the-live-canvas-composites-in-gamma-headless-composites-in-linear) questions whether the **CPU** path's operand encoding is itself right. Converging (3) and freezing goldens (2) before A-1 is settled would ratify a wrong answer behind a green suite. **Decide A-1 first.**

---

## 9. Still-image cache key

Closes [26 K-C8](26-kdenlive-mlt-parity.md#k-c8--key-the-still-image-cache-on-requested-size). `stills: HashMap<AssetId, GpuFrame>` (`session.rs:1006`) is keyed on asset alone, so a 6000-px JPEG is decoded and uploaded at full resolution regardless of preview scale, and stays resident that way.

Key on `(AssetId, width, height)`, mirroring the existing `uploads` key shape and the `VectorStateKey` pattern (which already includes size — stills are the outlier). Decode at the requested size, honour `PreviewQuality`, evict by the same byte budget as §5.2. Both reference image producers key on requested scale specifically because this is the classic still-image performance bug.

---

## 10. Acceptance

| # | Test |
|---|---|
| 1 | `source_range` identity default leaves compiled graphs byte-identical — the contract is free for non-temporal nodes |
| 2 | A node declaring `[out−1, out+1]` causes prefetch to warm exactly those frames; expansion past budget emits a diagnostic |
| 3 | An analysis node's result is cached by content hash; undo/redo does not re-run it |
| 4 | A node not declaring `Threading` is scheduled `Serial` |
| 5 | Playback prefills `prefill_frames` before presenting; after `max_consecutive_drops` it force-renders; underrun reports `buffering` and never presents black |
| 6 | Decode memory stays within the byte budget for 1080p, 4K and 8 concurrent sources |
| 7 | Scrub uses keyframe-only decode; the trailing seek lands exact |
| 8 | Interlaced fixture: detected, reported, deinterlaced without combing; field order round-trips through export |
| 9 | **Scale invariance** — Draft vs downsampled Full within tolerance, on a geometry-heavy fixture |
| 10 | **CPU/GPU equivalence** across every IR enum variant |
| 11 | Still cache holds one entry per `(asset, size)`; a preview-scale change does not force a full-res upload |
| 12 | SS-1 and SS-3 budgets green throughout ([02 §8](02-engine.md#8-perf-budgets-verified-in-11)) |

Tests 9 and 10 are **regression guards, not feature tests** — they must land before the work they protect.

---

## 11. Sequencing

| Order | Item | Rationale |
|---|---|---|
| 1 | §8.1 blend diagnostic | Hours; converts a silent wrong-pixels path into a visible one |
| 2 | §7 scale-invariance test | Days; guards everything in [30](30-effect-catalogue.md) |
| 3 | §8.2 equivalence sweep | Same, for the other bug class |
| 4 | §4 policy documented + soak coverage | Cheap; unblocks tuning |
| 5 | §9 still cache key | Small, self-contained |
| 6 | §3 threading capability | Cheap now, expensive later |
| 7 | §1 source-range contract | IR change; gates §6 and G-11 |
| 8 | §2 analysis nodes | Unblocks six downstream features |
| 9 | §5 byte budget + policy statement | Independent |
| 10 | §6 interlacing | Needs §1 |

Items 1–5 are **cheap correctness work that protects everything after them**, which is why they lead. Items 7–8 are the primitives with the widest downstream reach, and both get harder with time.

## 12. Amendments to [02-engine.md](02-engine.md)

This document does not silently supersede the engine spec. On acceptance, 02 takes: the `TemporalNode` and `AnalysisNode` traits in §2's IR section · `Threading` on the node contract · §4's policy table in §4 · §5's three seek policies and byte budget in §3/§5 · corrected cache-key and eviction descriptions ([27 SD-9](27-spec-audit.md#3-sd---spec-versus-code-drift), [SD-10](27-spec-audit.md#3-sd---spec-versus-code-drift)) · a pointer to [25](25-performance.md) as the owner of ring depths, resolving [27 A-11](27-spec-audit.md#a-11--p2--ring-depths-and-cut-ahead-lead-conflict-three-ways).
