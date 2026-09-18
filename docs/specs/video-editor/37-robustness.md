# 37 — Robustness: Device Loss, Crash Recovery, Scale Limits, Performance Gating

**Status:** Draft — implementation contract; no code authorization
**Date:** 2026-07-20
**Audience:** engine maintainers, app shell owner, CI owner

**Depends on:** [02-engine.md](02-engine.md), [03-render-color-pipeline.md](03-render-color-pipeline.md), [04-ui-mode-timeline.md](04-ui-mode-timeline.md) §1.4, [11-testing-phasing.md](11-testing-phasing.md), [25-performance.md](25-performance.md), [36-error-model.md](36-error-model.md).

**Owns:** [27 MC-3](27-spec-audit.md#mc-3--p1--gpu-device-loss-and-adapter-fallback) (device loss), [27 U-5](27-spec-audit.md#5-u---under-specified-contracts) (crash recovery), [27 MC-5](27-spec-audit.md#mc-5--p1--large-project-scale-limits) (scale limits), [27 MC-6](27-spec-audit.md#mc-6--p1--performance-regression-gating-is-advisory) (performance gating).

---

## 1. GPU device loss

### 1.1 The gap

Zero coverage anywhere. [03](03-render-color-pipeline.md) assumes a device forever; [02 §1](02-engine.md) shares one `Arc<GpuContext>` between renderer and engine with no recovery path. On device loss — driver update, TDR, laptop GPU switch, suspend/resume, or eGPU unplug — the texture pool, node cache and **every `Arc<wgpu::Texture>` inside `EngineFrame`** become invalid simultaneously. There is also no stated adapter-capability floor, and [11](11-testing-phasing.md) treats "no GPU adapter" purely as a test-skip condition rather than a runtime state.

Device loss is not exotic. A Windows TDR on a long export is a routine occurrence.

### 1.2 Capability floor

Declared once, checked at startup, reported as a `Render::AdapterCapabilityMissing` diagnostic:

| Requirement | Why |
|---|---|
| `Rgba16Float` as a render target with filtering | The working format (D-09) |
| `max_texture_dimension_2d ≥ 8192` | 4K with headroom; already clamped against at `frame_manager::resize` |
| Compute shaders + storage textures | Scopes ([07 §5](07-color-grading.md)) |
| Non-uniform buffer sizes for the composite path | `COMPOSITE_SHADER` |

Below floor, Photonic **refuses to enter video mode with a clear reason** rather than starting and failing later. Vector editing may still run.

### 1.3 Recovery protocol

```rust
pub enum GpuState { Healthy, Lost { at: Instant }, Recovering, Unrecoverable }
```

On `SurfaceError::Lost`/`Outdated` or a device-lost callback:

1. **Pause playback and cancel in-flight GPU work.** Do not attempt to present.
2. **Drop every GPU-resident cache** — texture pool, node cache, decode uploads, stills, vector rasters. All are reconstructible from the document and the content hash; **nothing user-authored lives only on the GPU**, which is the property that makes recovery possible at all and must be preserved.
3. **Re-request adapter and device.** On success, rebuild pipelines and the surface, re-check the §1.2 floor, republish a frame, resume paused.
4. On failure, retry with backoff up to 3 times, then `Unrecoverable` → `Fatal` diagnostic with a save path ([36 §3.1](36-error-model.md#31-severity)).

**During export**, device loss must not silently truncate the output: cancel the job, report `Export::EncoderFailed` with the frame index, and remove the partial file from the job registry. An export that stops at frame 40 000 and reports success is the worst outcome available.

`frame_manager::resize` already handles `Lost`/`Outdated` with reconfigure-and-retry — this generalises that from the surface to the whole device.

### 1.4 No CPU fallback

**Recommend against a CPU rendering fallback.** `eval_cpu` exists as a *reference* for goldens, not as a shippable renderer — it is orders of magnitude too slow for interactive use, and offering it would produce an experience users would report as a hang. Below the capability floor, refuse clearly. That is a better product than a fallback nobody can use.

---

## 2. Crash recovery

### 2.1 The gap

[04 §1.4](04-ui-mode-timeline.md) is four bullets asserting "zero new subsystems". Unaddressed: **orphaned `ffmpeg` children** after a hard kill (one per live source, up to `MAX_LIVE_SOURCES = 8`, plus encoder and PCM readers), in-flight export and proxy jobs with **partial output files**, sidecar-cache corruption, and the fact that [SPEC.md](SPEC.md) promises "at most a few minutes" of loss against a **300 s** autosave default — which is exactly "a few minutes" only if you round generously.

### 2.2 Child-process reaping

Sidecars are spawned kill-on-drop, which covers a graceful exit and a panic-unwind. It does **not** cover `SIGKILL`, a power loss, or an OS-level kill. Recommended, in order of cost:

1. **A pid/session file** in the sidecar cache dir listing spawned children with their start times. On startup, reap any recorded child still alive whose parent is gone. Cheap, portable, and handles the common case.
2. **Platform parent-death signalling** where available (`PR_SET_PDEATHSIG` on Linux, job objects on Windows). Stronger, and worth having on those two platforms.

An orphaned decoder holding a file handle and a CPU core is a support burden out of proportion to the fix.

### 2.3 Partial outputs

Every job that writes a file — export, proxy, preview chunk ([33](33-timeline-preview-render.md)), transcode — writes to a **temporary path and renames on success**. A rename is atomic on every supported platform, so a crashed job leaves a temp file rather than a truncated output that looks finished. Startup sweeps stale temps in the cache directory.

This matters most for **proxies and preview chunks**, where a truncated file would otherwise be indistinguishable from a valid one and would be served silently.

### 2.4 Cache integrity

Sidecar caches are keyed by content hash and are **always reconstructible**. On a decode or read failure of any cached artifact, delete and regenerate rather than reporting an error — a corrupt cache entry should be a hiccup, not a failure. The cache index tolerates missing files by construction.

### 2.5 Autosave interval

**Recommend lowering the default from 300 s to 120 s**, and making the promise in [SPEC.md](SPEC.md) match whatever is chosen. Autosave writes to a separate branch/recovery path and is cheap relative to an edit session; 300 s is a defensible number only if nobody states "a few minutes" as a guarantee elsewhere.

*(For contrast, Shotcut's periodic backup defaults to **daily**, which is an anti-example, not a benchmark.)*

---

## 3. Scale limits

### 3.1 The gap

No stated ceilings for tracks, clips per track, assets, keyframes per track, cues, or graph nodes. [04 §7](04-ui-mode-timeline.md) names "hundreds of clip rects" as a risk with no target.

The sharpest structural edge: [02 §1](02-engine.md) promises the engine snapshots "the parts it needs… cheap `Clone`", but `session.rs:678` does `Arc::new(p.clone())` on the **whole `TimelineProject`** on every `doc_generation` bump. That is an O(project) deep clone **per edit**, unbounded, on the interactive path. The doc and the code disagree, and the code is the expensive one.

### 3.2 Declared targets

Not hard limits — **budget targets** that tests assert and that tell an implementer when a linear scan is acceptable:

| Dimension | Target | Notes |
|---|---|---|
| Tracks per sequence | 100 | Beyond this, header layout dominates |
| Clips per sequence | 10 000 | The number that decides whether per-frame linear scans are viable |
| Assets per project | 5 000 | |
| Keyframes per property track | 10 000 | |
| Caption cues per track | 50 000 | A feature-length subtitle file |
| Graph nodes per composition | 500 | |
| Sequences per project | 200 | |

**A project exceeding a target must still work**, degrading in speed rather than failing. The targets exist so "is this fast enough?" has an answer.

### 3.3 The snapshot cost

**Recommend: make the snapshot structurally shared rather than deep-cloned.** `TimelineProject`'s large members become `Arc`-wrapped (`sequences`, `media`, `graphs`), so a snapshot clones a handful of `Arc`s and edits pay copy-on-write only for the touched sequence. This is what [02 §1](02-engine.md) already claims happens, so it aligns code with contract rather than changing the design.

Interim, if that refactor is deferred: **snapshot only the active sequence plus referenced assets**, which is literally what 02 says. The current whole-project clone is neither.

### 3.4 Large-project behaviour

- Timeline painting is **viewport-culled** — cost scales with visible clips, not total clips.
- Thumbnail and waveform generation is viewport-driven and budgeted per frame (already true).
- `clips_in_link_group`-style whole-project scans are replaced by indexed lookups — the group tree in [35 §3](35-model-decisions.md#3-groups) does this for grouping specifically.

---

## 4. Performance gating

### 4.1 The contradiction

[11 §4](11-testing-phasing.md) makes benches "CI-advisory, not blocking… a human reviews the trend line", and the SS-1 zero-dropped-frames gate and the SS-3 sync test are `#[ignore]` + `continue-on-error` nightly. But [ROADMAP §10](ROADMAP.md#10-definition-of-done) requires budgets "green" as a condition of done.

Both cannot be true. Either the budgets gate, or "done" does not mean what it says.

### 4.2 Recommendation: two tiers, and be honest about which is which

| Tier | Content | Enforcement |
|---|---|---|
| **Hard gates** — deterministic, machine-independent | Graph compile < 0.5 ms · SS-3 A/V drift · export determinism (byte-identical) · cache hit-rate invariants · scale-invariance and CPU/GPU parity ([32 §7](32-engine-contracts.md#7-scale-invariance), [§8](32-engine-contracts.md#8-cpugpu-equivalence)) | **Blocking in PR CI** |
| **Trend metrics** — hardware-dependent | Eval ms/frame · seek latency · decode throughput · export wall time | Recorded per run, **regression alert on a rolling baseline**, not a hard fail |

The distinction is *machine independence*, not importance. A frame-time budget on a shared CI runner is noise; a determinism check is not. Gating on noise trains people to ignore CI, which is worse than not gating.

### 4.3 Making it real

- Publish trend metrics to a tracked artifact per run so the "human reviews the trend line" step has something to review — today it does not.
- **Alert on a >20 % regression** against a rolling 10-run median, as a review comment rather than a failure.
- Keep the throughput bench (`playback_throughput_bench.rs`) `#[ignore]`d for PR runs and run it nightly on a known machine, where its numbers mean something.
- **Amended [ROADMAP §10](ROADMAP.md#10-definition-of-done) item 7 on 2026-07-20** to say "hard gates green; trend metrics reviewed and not regressed beyond threshold" — which is honest and achievable, where the current wording is neither.

---

## 5. Acceptance

| # | Test |
|---|---|
| 1 | Simulated device loss during playback: caches drop, device rebuilds, playback resumes, no panic |
| 2 | Device loss during export: job cancelled, diagnostic carries the frame index, **no partial file registered as complete** |
| 3 | Adapter below the capability floor: video mode refused with a clear reason; vector editing still works |
| 4 | Hard-kill the app with decoders running: no orphaned `ffmpeg` after restart |
| 5 | Crash mid-export: no truncated output presented as finished; temp swept on startup |
| 6 | Corrupt a cached proxy/preview chunk: detected, deleted, regenerated, no user-visible failure |
| 7 | A project at each §3.2 target opens, plays and exports within budget |
| 8 | Snapshot cost is O(1) in project size (§3.3) — asserted by timing an edit on a 10 000-clip project |
| 9 | Hard gates fail the build when violated; trend metrics never fail it |
| 10 | Autosave interval matches whatever [SPEC.md](SPEC.md) promises |

Test 8 is the one that would have caught §3.1: it is a property, not a number, so it holds on any machine.

---

## 6. Sequencing

| Order | Item | Rationale |
|---|---|---|
| 1 | §4.2 gate split + §4.3 amendment to ROADMAP §10 | Costs nothing; makes every later claim checkable |
| 2 | §2.3 temp-file-and-rename | Small; removes a class of silent corruption |
| 3 | §1.2 capability floor + §1.3 recovery | Before shipping to varied hardware |
| 4 | §2.2 child reaping | Small, high support value |
| 5 | §3.3 snapshot sharing | Before the 10 000-clip target is credible |
| 6 | §3.2 targets asserted in tests | With §3.3 |
| 7 | §2.5 autosave interval | Trivial; do it with §1 |

Item 1 first because everything else in this document claims a budget, and today no budget can actually fail a build.
