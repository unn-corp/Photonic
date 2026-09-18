# 11 — Testing & Phasing

**Depends on:** all docs. **Location of new test code:** `crates/photonic-video/tests/`, `crates/photonic-core/src/timeline/` (`#[cfg(test)]`), `tests/golden/` (new, repo root). **Decisions used:** D-03, D-08, D-09, D-10.

Scope (00 §5): test strategy + infra, net-new golden-frame system, and per-phase exit criteria for P1–P8. Existing CI gates (`docs/refactor/README.md` §6, `.github/workflows/ci.yml`) are the floor every phase merge must clear: `cargo build/test/fmt/clippy --workspace --locked`, `cargo deny check`, MCP doc-drift (`docs/mcp-api.md` regen byte-identical). Nothing here replaces those; this doc adds the video-specific layer on top.

---

## 1. Golden-frame infrastructure (net-new)

Today Photonic has **no snapshot/reference-corpus framework** — verification is in-code GPU-vs-CPU tolerance assertions (`crates/photonic-render/src/headless.rs:1568` blend-mode test, `TOL: f32 = 0.03` per-channel abs diff) plus behavioral pixel checks in `compositor.rs` (e.g. `compositor.rs:1847` `assert!(bg > 0.9, ...)`). That pattern is the *right* shape — pixel-value assertions with named tolerance and a diagnostic message — but it doesn't scale to "does this 40-frame timeline still composite correctly." We add a corpus-based layer, not a replacement.

**Why this is possible at all:** the frame-graph IR is defined as "a pure function of (document snapshot, sequence, format, tick, quality flags)... same inputs ⇒ identical graph ⇒ identical pixels" (`02-engine.md` §2). That determinism is the golden-test enabler — without it, corpus comparison would be chasing noise.

### D-09 compatibility lock

Every existing golden remains a `LinearRec709Sdr` compatibility test and must remain pixel-stable after HDR work. HDR cases use separate rights-cleared HLG/PQ/Rec.2020 vectors, explicit sequence color state, nit/code-value assertions, and decode-back bit-depth/tag checks from `22-dji-advanced-workflows.md` §7. No golden is silently reinterpreted as HDR; missing color state defaults to SDR.

### 1.1 Corpus layout

```
tests/golden/
  {case-name}/
    project.photon          # timeline project, minimal — 1 sequence, few clips
    fixtures/                # symlink or copy of shared media (§2) referenced by AssetSource::File
    expected/
      cpu/frame_{n:04}.png    # eval_cpu reference, canonical (see 1.3)
      gpu_tolerance.toml      # per-case looser bounds if GPU legitimately differs (rare; documents why)
    meta.toml                # ticks to sample, format/aspect, doc_generation, notes
```

One case = one `.photon` file + a small explicit list of ticks to render and compare (not every frame — sample cut points, mid-clip, transition midpoints, keyframe extremes). `meta.toml` names the ticks so a test failure log points at "frame at tick X in case Y," not an opaque index.

Note: repo-root `tests/golden/` (this corpus — full playback/export pipeline, video/timeline scope) and `crates/photonic-render/tests/golden/` (03 §2.6's P1 vector-renderer-equivalence corpus, pre- vs post-refactor pixel comparison) are two deliberately separate systems with different scope, lifecycle, and owning doc. Do not merge them; 03 carries the mirror of this note.

### 1.2 Comparison metric — two-layer, both required

Single aggregate metrics (SSIM/PSNR) hide localized corruption (a single wrong node in a corner dilutes to a passing average); a lone per-pixel tolerance loop misses diffuse drift (a color-space rounding error spread evenly). Use both, same pattern already established in `headless.rs`:

| Layer | Metric | Threshold | Rationale |
|---|---|---|---|
| Per-channel | max abs diff, linear-light RGBA | **≤ 0.02** (CPU-vs-CPU, same platform) | Tighter than the existing `TOL 0.03` blend-mode test since `eval_cpu` is float-deterministic — no 8-bit quant/sRGB round-trip in the reference path itself. Catches single-node regressions. |
| Aggregate | PSNR | **≥ 40 dB** CPU-vs-CPU-reference; **≥ 35 dB** GPU-vs-CPU-reference | GPU threshold looser: driver/precision variance across the 3-OS CI matrix (`ci.yml` `ubuntu-latest, windows-latest, macos-latest`) is real and not a regression. |
| Aggregate | SSIM | **≥ 0.98** GPU-vs-CPU-reference | Structural check — catches transitions/composites that pass PSNR (energy-close) but are visibly wrong (shifted, mirrored). |

Failure reports: dump a diff PNG (abs-diff heatmap) next to the test output dir — same "assert with a helpful message" ethic as the existing tests, just with an image artifact instead of a printed float.

### 1.3 GPU nondeterminism handling

`eval_cpu` (02 §2, f32 CPU implementation of every `IrOp`) is the **canonical golden source** — it's checked into `expected/cpu/`, is what's diffed against on every platform, and is what a human reviews when blessing a new case (§1.4). GPU output is *never* the corpus; it's compared against the CPU reference at the looser GPU threshold above, exactly mirroring the existing `separable_blend_modes_match_reference` pattern (CPU-computed `expected()` vs GPU `render_center_pixel()`, `headless.rs:1561-1580`) but promoted from single-pixel to whole-frame.

Consequence: `eval_cpu` must exist and be wired for every `IrOp` variant before that op's golden case can be written — a natural per-op test-readiness gate as `graph/ops/*.rs` land in P3+.

### 1.4 Corpus generation / blessing workflow

Two options considered:

- **Option A — `cargo xtask bless-golden`.** Standard in larger Rust workspaces, but this repo has **no `xtask` crate today** (checked: no `xtask*` anywhere, no precedent in `Cargo.toml` members). Introducing one adds a new build-system concept for one feature.
- **Option B (recommended) — env-var-gated bless mode inside the golden test binary itself.** `crates/photonic-video/tests/golden_frames.rs` is a normal `cargo test` integration test; when `PHOTONIC_BLESS_GOLDEN=1` is set it writes `expected/cpu/*.png` instead of comparing, using the same harness code. Run as `PHOTONIC_BLESS_GOLDEN=1 cargo test -p photonic-video --test golden_frames -- --test-threads=1`. Matches the repo's existing lightweight-tooling convention (`tools/gen-mcp-docs.py` is a plain script invoked by CI, not a build-graph node) and needs zero new Cargo.toml plumbing.

Every bless run must be a reviewed diff (`git diff --stat tests/golden/`) — a human confirms the new/changed PNGs are the *intended* pixels before commit, same review bar as any other test-baseline change. Blessing is never automatic in CI.

### 1.5 CI storage strategy

**Recommendation: commit small PNGs in-repo, no Git LFS.** Repo has no `.gitattributes` LFS filters today and `.git` is 5.4 MB total — introducing LFS is infra churn for a feature that can stay small if fixtures are disciplined. Budget: **≤ 10 MB total** for `tests/golden/` (SPEC's own budget note echoed here) — at ~200 sampled frames × ~15–40 KB/frame (small PNG, mostly flat color + a few gradients/text), this comfortably fits. Enforce with a CI step: `du -sh tests/golden | awk '{if ($1+0 > 10) exit 1}'`-style check, or simpler — a comment in `tests/golden/README.md` + code-review discipline; revisit LFS only if corpus growth (P6+ keyframe/transition cases) threatens the budget.

### 1.6 CI wiring

Golden-frame comparison (§1.2) runs as part of the normal `cargo test --workspace --locked` step in `ci.yml`'s existing `build-test` job once `crates/photonic-video/tests/golden_frames.rs` exists — no new job needed for the fast-path CPU-vs-CPU cases, since `eval_cpu` needs no GPU adapter (mirrors how `headless.rs`'s GPU tests already self-skip via `try_renderer()` when no adapter is present, `headless.rs:1561`). GPU-vs-CPU golden cases follow the same skip-with-message convention on adapter-less runners.

Two test classes are **not** appropriate for the default per-PR `build-test` job and get a separate, non-blocking job (new `.github/workflows/ci.yml` job, name suggestion `video-nightly`, `schedule:` cron trigger on `main` only, `continue-on-error: true` at the job level so a soak-test flake never blocks merges):
- The 10-minute SS-3 sync-drift export test (§5).
- The playback soak test (§4).

Both are `#[ignore]`-annotated in source so a stray `cargo test` locally or in the default CI job never accidentally runs a 10-minute test; the nightly job invokes them explicitly via `cargo test -- --ignored`.

---

## 2. Test media corpus

CI's Linux job installs GTK/X11/wgpu/OpenSSL system deps (`ci.yml` lines ~30–38) and **installs `ffmpeg` on all three platforms** (`apt-get` / `brew install ffmpeg` / `choco install ffmpeg`). Decode-path tests that need a real container/codec therefore *can* run in CI, and fixtures can be generated there.
<!-- spec-assert: ci-step-contains ffmpeg -->
<!-- SD-4 (27 §3): historically flagged "CI has no ffmpeg"; it is installed on all three runners now, pinned here so a workflow edit that drops it reds the gate. -->


**Position:** commit tiny, pre-generated fixture files; provide a **generation script** (`tools/gen-test-fixtures.py`, ffmpeg-dependent, run by a developer locally when a fixture needs regenerating — same shape as `tools/gen-mcp-docs.py`, a checked-in script CI *consumes the output of* rather than *runs*). Do not attempt runtime synthesis of media in the test binary itself (adds a codec dependency to the test harness for no benefit — the fixtures are static test data, not something that needs to vary per run).

| Fixture | Purpose | Spec | Approx. size |
|---|---|---|---|
| `color_bars.mp4` | Deterministic known-value video source; probe metadata tests (CAP-001) | 4s, 320×180, yuv420p, H.264, 30fps | < 200 KB |
| `counter.mp4` | Frame-number burn-in (visual + embedded as SEI or filename-adjacent JSON `frame_truth.json`); seek-accuracy tests | 10s, 320×180, 30fps, one keyframe per 2s (tests GOP-seek path, 02 §3) | < 300 KB |
| `beep_flash.mp4` + `.wav` | A/V sync ground truth: video flashes white 1 frame every 1s, audio has a 5ms beep at the same instants | 60s, 320×180, 30fps + 48kHz mono | < 1 MB |
| `vfr_sample.mp4` | Variable-frame-rate source — exercises `FrameRate::is_exact()` warning path (01 §1) and non-integral `ticks_per_frame` rounding | 8s, mixed 24/30fps segments | < 400 KB |
| `alpha_gradient.mov` | Straight/premultiplied alpha ramp, known values; CAP-021 transparent export correctness | 3s, 160×90, yuva444p or ProRes 4444 | < 500 KB |
| `multi_audio.mkv` | 3 discrete audio streams (dialogue/music/fx), different channel counts (mono/stereo/5.1-downmix-to-stereo) | 15s | < 800 KB |

Total corpus: well under 5 MB, inside the 10 MB `tests/golden/` + fixtures combined budget. Fixtures live in `crates/photonic-video/tests/fixtures/` (not `tests/golden/`, which is the *comparison* corpus) and are referenced by golden-case `project.photon` files via relative `AssetSource::File` paths (01 §9 relative-path-first load order makes this portable across checkout locations).

---

## 3. Test pyramid per layer

### 3.1 `core::timeline` — pure unit tests

Location: `#[cfg(test)]` inline per module, matching the repo's dominant convention (51 of ~55 test-bearing files use inline `mod tests`, only 4 use a `tests/` integration dir: `photonic-core/tests/raster_editing_session.rs`, `raster_integration.rs`, `photonic-gui/tests/no_tofu_glyphs.rs`, `photonic-embed/tests/smoke.rs`).

- **Edit-op invariants** (`timeline/ops.rs`, 01 §4/§10): sorted-non-overlapping-within-track holds after every op (`move_clip`, `trim_clip`, `split_clip`, ripple/roll/slip/slide). Table-driven unit tests per op, plus:
- **Property tests via `proptest`** for the sort/non-overlap invariant specifically — generate random sequences of edit ops on a synthetic track and assert the invariant after each. **Position: adopt `proptest`.** It's not in `Cargo.toml` today (checked — no `insta`/`proptest`/`criterion` anywhere in the workspace), but this is exactly the class of bug (an edit op that violates non-overlap under some ordering/boundary case) that example-based tests reliably miss and property tests reliably catch; the maintenance cost is one new dev-dependency on one crate (`photonic-core`).
- **`Sequence::validate()`** (01 §4): unit test that it rejects overlap/negative-duration on hand-built fixtures, and that it's called on load (round-trip test loading a deliberately-corrupted-on-disk JSON).
- **Keyframe eval** (01 §6, `fn eval`): closed-form tests per `Interp` variant — `Hold` returns left value past its `at`, `Linear` matches exact interpolation at t=0.25/0.5/0.75, `Bezier` matches hand-computed cubic-bezier-ease values at a few t (CAP-007's own test hook). Bool/Enum-always-Hold rule gets an explicit regression test since it's an easy accidental regression (someone "fixes" interpolation generically and breaks this carve-out).
- **`PropPath` registry** (01 §6.2): unknown-path-on-load keeps the track and flags `orphaned` rather than dropping it — explicit test, since silent data loss here would violate the "no exception, everything undoable/round-trippable" constraint (SPEC.md constraints).
- **Undo round-trip**: every `TimelineCmd` variant gets an apply→inverse→apply-again idempotency test (mirrors existing `UpdateNode` coalescing test pattern per 01 §10).

### 3.2 Graph compile — snapshot tests

`graph::compile` (02 §2) output (the `FrameGraph` IR, pre-eval) is a deterministic data structure — ideal **insta snapshot** target: dump IR as a stable textual form (`Debug` or a custom pretty-printer) and diff against a committed `.snap` file. **Position: adopt `insta`.** Same rationale as proptest — not currently a dependency, small addition (`photonic-video` dev-dependency), and the alternative (hand-written assert-equal on giant IR structs) is unmaintainable churn every time a node gains a field. Snapshot review (`cargo insta review`) is a deliberate, reviewed step — same blessing discipline as §1.4.

Cases: one compile snapshot per interesting compile-path branch in 02 §2's compile steps: bare clip chain, clip with `composition` set (D-06 source-substitution splice — the snapshot must show the composition's Output feeding the clip's default Transform2D→Effects→Grade chain, per 02 §2 step 3), a composited clip rendered across ≥2 `SequenceFormat`s (positive case: per-format reframe applies on top of the composition — this was once a documented gap and is now required behavior), Adjustment-clip re-rooting, project-graph splice, dead-branch elimination (opacity-0 clip should not appear in the IR at all — snapshot proves absence, not just correctness of what's present).

### 3.3 Engine integration tests (headless)

`crates/photonic-video/tests/`, using the fixture corpus (§2):
- **Seek accuracy:** seek to N ticks, assert `EngineFrame.time` lands on the correct frame boundary (`FrameRate::snap`), using `counter.mp4` + `frame_truth.json` as ground truth.
- **A/V sync measurement:** play `beep_flash.mp4`, sample decoded-frame flash timestamps and audio-beep timestamps from the engine's own output streams, assert offset < 1 frame — this is the CAP-004/SS-1 test hook and doubles as the SS-3 export-sync procedure's playback-side counterpart (§5).
- **Cache-hit assertions:** exercise `graph::eval` twice with an unchanged IR content hash (02 §5), assert the second call is a cache hit via an injected counter/hook on the LRU pool (not wall-clock timing — timing-based cache tests are flaky; assert on hit-counter state).
- **Failure containment:** kill/starve a sidecar mid-decode (or a fixture that force-fails the sidecar spawn), assert diagnostic-placeholder frame renders and the engine thread never blocks past its read deadline (02 §3 "wedged pipe never blocks" guarantee) — the one test class here that needs a real subprocess, so gate it behind `#[ignore]` unless `ffmpeg` is confirmed present in the environment (`which ffmpeg` check in a `#[cfg(test)]` helper, skip with a printed reason otherwise — same skip-with-message pattern as `headless.rs`'s `try_renderer()` GPU-adapter check).

### 3.4 DSP fixtures, provider mocks, MCP end-to-end

- **09 (audio DSP):** unit tests on synthetic signals (sine sweep through EQ, known-gain compressor test) — pure-function DSP code, no fixtures needed beyond generated-in-test signals (unlike video, audio synthesis in-test is cheap and doesn't need ffmpeg).
- **06 (captions/TTS providers):** provider trait is mocked at the trait boundary (a `FakeProvider` returning canned word-timed transcripts / canned audio bytes) — no real network call in CI, matching SPEC's "all non-AI capabilities work fully offline" constraint and letting caption *editing* logic (CAP-010) be tested without any provider at all.
- **MCP end-to-end (CAP-019 / SS-2):** the three acceptance stories (AS-1/2/3, `00-overview.md` §2) scripted as MCP tool-call sequences, run headless, asserting each story's stated outcome (e.g. AS-1: exported MP4 probes as 9:16, has a burned caption track, duration matches cut timeline). Tool wiring itself is `10-mcp-tools.md`'s scope; this doc owns *that these scripts exist and run in CI as the CAP-019 acceptance gate*, one script per story, one test per script.
- **CI ffmpeg: resolved.** `ffmpeg` is installed on all three runners, so §3.3's sidecar-dependent tests and export-driving MCP-script tests can run in CI.

### 3.5 GUI smoke

Checked: no egui-specific test-automation infra exists (the one GUI test today, `photonic-gui/tests/no_tofu_glyphs.rs`, is a glyph-availability check, not an interaction test — no headless-input-simulation harness present). **Position:** don't build one for this feature. Factor timeline/monitor GUI logic into plain functions callable without an `egui::Context` (mirrors 01 §10's "edit ops are pure functions GUI and MCP both call" principle — same discipline, applied to read-side rendering logic, not just write-side edits) and unit-test those directly. For actual visual verification, there is no Playwright-equivalent for a native wgpu/egui app; rely on the MCP `render_frame_at`-class tool (10) driving the same headless render path as golden tests (§1) for visual checks — i.e., GUI visual QA piggybacks on the golden-frame infra rather than a bespoke second system.

---

## 4. Perf harness

**Position: adopt `criterion`** as a `photonic-video` (and `photonic-core::timeline`) dev-dependency for the compile/eval budget items — not currently in the workspace, standard choice, integrates with `cargo bench`. All three recommended dev-deps (proptest, insta, criterion) must pass `cargo deny check` transitively at add-time — same licensing gate as runtime deps (09 §3 carries the matching note; symphonia was dropped from 09's stack for exactly this reason).

02 §8's budget table, with measurement method per row:

| Item | Budget | Measurement method |
|---|---|---|
| Graph compile (10 tracks, 3 active clips) | < 0.5 ms | `criterion` bench, `graph::compile` on a synthetic 10-track project fixture, wall time, warmed |
| Eval 1080p, 3 layers + grade + captions | < 8 ms GPU | `criterion` bench with `black_box`-guarded GPU submit + `wgpu` timestamp queries (not CPU wall-clock around an async submit — must measure actual GPU pass time) |
| Seek-to-photo (cached GOP) | < 50 ms | Engine integration test (§3.3) with an injected clock, decoded-frame-ring pre-warmed, measuring `EngineCmd::Seek` → `EngineFrame` latency |
| Cold seek (index + 1 GOP decode, proxy) | < 150 ms | Same harness, ring cache cold-started (fresh `EngineSession`) |
| Cut-ahead warmup | ≥ 500 ms before cut | Playback soak test (below) asserting prefetcher issues the next clip's decode request ≥ 500 ms before its `start` tick, using `counter.mp4` cut points |
| Export overhead vs pure encode | < 25% wall time | Export a fixture sequence twice: once through `export::render_loop` (compile+eval+encode), once feeding the *same pre-rendered* rawvideo directly to the encoder sidecar (pure-encode baseline); compare wall time |

### 4.1 Preview & media load budgets (24-preview-media-load §6)

Owned by [24-preview-media-load.md](24-preview-media-load.md). Headless harnesses:

- `crates/photonic-video/tests/preview_media_load.rs` — import ladder L0–L5 + L7 proxy, Draft fit, dual-input audio (skips when ffmpeg/GPU absent).
- `crates/photonic-video/tests/seek_budgets.rs` — formal §6 **seek** + cut-ahead scan + soft warm-index budget (no full GPU play).
- `photonic_video::playback::prefetch` units — `cut_ahead_targets`, `lru_eviction_victims`.
- `photonic_gui::panels::media_pool::l0_register_n_stubs_before_any_probe` — multi-file L0.
- `photonic_gui::app::source_marks` — G-10 marks→insert + Match Frame range.
- `crates/photonic-gui/tests/video_ui_paths.rs` — real UI code paths: media pool import ladder drain, source marks→Insert, engine peek/play-wins/proxy, attach proxy, cut-ahead scan.

| Metric | Budget | Hardness | Measurement |
|---|---|---|---|
| L0 row register | ≤ 100 ms | **Hard** (tiny fixture) | Stub `MediaAsset::from_file` wall time (no I/O beyond path) |
| L2 probe (typical short MP4) | ≤ 500 ms p95 product; test soft ≤ 5 s | Soft | `probe_asset` on `counter.mp4` |
| L3 poster available | ≤ 1.0 s p95 product; test soft ≤ 10 s | Soft | `ensure_poster` + decode PNG |
| L4 keyframe index build | warm after import; test soft ≤ 15 s | Soft | `KeyframeIndex::load_or_build` |
| Warm keyframe lookup (CPU) | ≤ 1 ms p95 (100× random ticks) | **Hard** | `seek_budgets::warm_keyframe_index_lookup_budget` |
| Exact Draft frame after seek | ≤ 150 ms p95 product (warm index + proxy) | Soft / integration-only | Not hard-gated in CI; full decode path remains soak |
| Draft long-edge fit | 1920×1080 → 960×540 | **Hard** | pure unit (`fit_long_edge`) |
| Draft quality → proxy flag | `Quality::PREVIEW.proxy == true`, FULL false | **Hard** | `seek_budgets::draft_quality_selects_proxy_flag_in_compile` |
| Dual-input audio (Windows path) | temp f32le file stages without FIFO | **Hard** | `stage_audio_tempfile` + `build_ffmpeg_args` second `-i` |
| Scrub seek coalesce | Drop intermediate seeks; latest-wins | **Hard** | `seek_budgets::scrub_seek_coalesce_latest_wins` (+ drag sim) |
| Play start / ring hit | ring API + coalesce+Play surface | Smoke | `seek_budgets::ring_api_existence_for_play_start_path` (no GPU play) |

**Hard vs soft:** hard asserts are pure-CPU or tiny-fixture bounds that must not flake on shared CI. Soft rows log/eprintln or use generous ceilings (5–15 s). Product p95 numbers remain the normative contract in 24 §6; full decode-to-frame Draft seek p95 and GPU play-start are still integration/soak, not default-PR hard gates.

**Playback soak test** (SS-1 procedure): reference timeline = 1080p30, 3 concurrent video-track clips (using `color_bars.mp4` tiled/scaled — a real decode load, not a solid-color no-op), one grade node, one caption track with dense cues. Play the full fixture duration in a headless engine session, record `EngineStatus.dropped` (02 §1). **Pass threshold: zero dropped frames** on the reference dev machine (matches SS-1's literal wording — "plays at full frame rate without dropped frames"). Run as a `#[ignore]`-by-default long-running test (soak tests don't belong in the default fast `cargo test --workspace` path) invoked explicitly in a dedicated CI step or pre-release checklist — flag which; **recommendation: dedicated CI job, not blocking on PR, scheduled nightly on `main`** (mirrors the "advisory, not blocking" stance below).

**Regression tracking policy: bench results are CI-advisory, not blocking.** Position: run `cargo bench` in CI, upload results as a build artifact / append to a tracked file (`docs/perf-history.md` or similar, append-only), but do not fail the build on regression. Rationale: perf benches are noisy on shared CI runners (the existing 3-OS matrix already shows this kind of variance is tolerated — see the GPU-tolerance thresholds in §1.2); a hard gate here would produce flaky red builds unrelated to real regressions. A human reviews the trend line periodically (e.g. at each phase exit, §6) rather than every PR blocking on a noisy number. Revisit to "blocking with generous margin" only if regressions are repeatedly missed under advisory-only in practice.

---

## 5. A/V sync + export verification (SS-3)

SS-3: *"Exported frames match preview rendering within a defined pixel tolerance on a golden-frame corpus, and exported A/V sync error stays under one video frame across a 10-minute sequence."*

**Export-frame verification:** export a golden-corpus case (§1) through the real `export::render_loop` (02 §7) to a file, then **decode it back** via an `ffmpeg`/`ffprobe` sidecar call in the dev/CI environment (same ffmpeg-presence gate as §3.4) and compare decoded frames against the same case's `expected/cpu/` PNGs using §1.2's PSNR threshold (the export path round-trips through a real codec, so use the **GPU threshold** — PSNR ≥ 35 dB — not the tighter CPU-vs-CPU one, since lossy encode is in the loop even at high-quality presets).

**Frame-count / duration check:** `ffprobe -show_format -show_streams` on the export, assert stream duration and frame count match the sequence's declared tick range exactly (integer frame count at the sequence's `FrameRate` — this is a hard equality check, not a tolerance one; a duration mismatch is always a bug, never acceptable drift).

**Sync-drift measurement over a 10-minute synthetic sequence:** extend `beep_flash.mp4`'s ground-truth pattern (§2) into a synthetic 10-minute sequence built from repeated/looped `beep_flash` clips on the timeline (not a single 10-min source file — keeps the fixture corpus small; the timeline repeats a short fixture N times, which is a legitimate timeline under test regardless). Export it, decode the export, detect flash-frame timestamps and beep-onset timestamps in the *decoded output* (simple thresholding — flash = frame luma spike, beep = audio RMS spike), compute offset at each of the ~600 flash/beep pairs, assert **max abs offset < 1 frame duration** (33.3 ms at 30fps) across the full 10 minutes — this directly instruments SS-3's stated tolerance rather than approximating it.

This is one test, marked `#[ignore]` by default (10-minute synthetic export is not a fast-loop test) and run in the same nightly/pre-release CI slot as the playback soak test (§4).

---

## 6. Per-phase exit criteria (P1–P8)

Every phase, in addition to its row below, must satisfy: **zero regressions in the existing vector-editing test suite** (`cargo test --workspace --locked` green, including all 47+ pre-video-era test files), **all CI gates pass** (build/test/fmt/clippy/deny/MCP-doc-drift), and **docs updated** — `docs/Features.md` gets the phase's capability entries, `docs/mcp-api.md` is regenerated (mechanically enforced by the existing CI step) whenever `10-mcp-tools.md`'s tool surface changes in that phase.

### P1 — Renderer foundation (D-10 prerequisite; 00 §7 top risk)

This phase touches the *existing* renderer (dirty tracking, persistent GPU buffers, `COMPOSITE_SHADER` wiring, f16 video texture path) before any video feature exists — the highest-risk phase per 00 §7 ("Renderer rework destabilizes vector editing"). Exit criteria:
- [ ] **Golden-output equivalence corpus for existing vector rendering, captured before P1 starts.** Not the video golden-frame system (§1, which doesn't exist yet at P1) — a targeted before/after snapshot of current vector-doc rendering (reuse the existing `HeadlessRenderer::render_rgba_with_opts` output, hash + store N representative existing test/demo documents' rendered frames) taken on frozen pre-P1 `main`.
- [ ] Post-P1, every one of those documents re-renders to pixel-identical (or within the existing `headless.rs` `TOL 0.03` class of tolerance where GPU path legitimately changed, e.g. new blend shader) output. This is the literal risk mitigation named in 00 §7 — do it as the concrete gate, not a vague "be careful."
- [ ] Existing `compositor.rs` / `headless.rs` test suites pass unchanged.
- [ ] No interactive-canvas responsiveness regression when no video features are active (SPEC.md constraint) — spot-check via existing perf expectations, no new tooling needed since no video code runs yet.
- [ ] No new capability (CAP-XXX) closes this phase — it's foundation-only, correctly reflected as "—" in 00 §6's story-slice column.

### P2 — Time + timeline core (AS-1: arrange + cut)

- [ ] `core::timeline` module lands with all types from 01, `Sequence::validate()`, and the full `timeline/ops.rs` edit-op set.
- [ ] §3.1's unit test suite exists and passes, including the `proptest` non-overlap property test (position adopted: proptest is now a `photonic-core` dev-dependency).
- [ ] Format v2→v3 migration (01 §9) lands with a `docs/format-versions.md` entry (house convention) and a round-trip test: v2 file loads unchanged, v3 file with no timeline loads on a v2 build within `COMPAT_WINDOW`.
- [ ] Timeline panel UI + mode switch (04) demonstrates CAP-002/CAP-003 manually (arrange/trim/split/ripple/roll/slip/slide) — no playback yet, so this is UI+data-model only, verified interactively since the engine doesn't exist until P3.
- [ ] CAP-020 (save/reopen) partially closes: round-trip a timeline-only project (no media playback needed to prove serialization).
- [ ] Undo/redo (CAP-018) closes for all `TimelineCmd` variants introduced so far — apply/inverse tests from §3.1 pass for every variant.

### P3 — Playback + media (AS-1: play; AS-2: proxy edit)

- [x] `photonic-video` crate exists and is in `Cargo.toml` workspace `members` (8 crates).
- [ ] Frame-graph IR + `compile`/`eval`/`eval_cpu` for the v1 op set (decode→transform→merge→output per 02 §2) — `insta` snapshot tests (§3.2) exist for the compile paths available at this phase.
- [ ] **§1's golden-frame infra stands up here** — first real golden cases, since `eval_cpu` now exists for the v1 op set. This is the natural point to build §1's harness (not P1, since there's no video IR yet then).
- [x] CI ffmpeg: `ffmpeg` present in the CI matrix across all 3 OSes.
- [ ] Engine integration tests (§3.3) pass: seek accuracy, A/V sync (CAP-004 test hook), cache-hit assertions, sidecar-failure containment.
- [ ] Perf budgets from 02 §8 that apply at this phase (compile, eval, seek, cut-ahead) measured via `criterion` (§4) and meet threshold on the reference dev machine.
- [ ] CAP-022 crash recovery (D-12): kill the process mid-edit on a timeline project; relaunch offers recovery; restored document contains the timeline state (extends the existing `recovery_path` machinery — test asserts timeline survives the round-trip).
- [ ] Proxies (CAP-014): generate + toggle proxy/original, verify scrubbing uses proxy path (mixer engine core — gain/pan only per 00 §7's mixer-scope risk mitigation — lands here too, ungated by full audio UI).
- [ ] CAP-005 (nested sequences), CAP-006/CAP-021 (vector clip via `RasterVector`, CPU-composited per 02 §3) demonstrated.
- [ ] AS-1 up through "play" and AS-2 up through "proxy edit" run manually end-to-end; MCP script versions (§3.4) exist for the slice of each story completable so far, expanded incrementally in later phases rather than written once at the end.

### P4 — Import/export + reframe

- [ ] Export presets + encoder integration (05) — `export::render_loop` (02 §7) functional for at least H.264 and one alpha-capable format.
- [ ] SS-3's export-verification tests (§5) run for the first time: frame-match PSNR, frame-count/duration exact-match. Full 10-minute sync-drift test can wait until audio mixing is richer (P8) but a shorter (~1 min) sync-drift smoke check should pass here.
- [ ] Aspect-ratio system + per-clip reframe (CAP-012), mobile preview.
- [ ] AS-1 complete except captions — full manual + MCP-script run.

### P5 — Captions + AI audio

- [ ] Provider trait + `FakeProvider` mock (§3.4) — caption editing (CAP-010) fully testable offline.
- [ ] Hosted transcription/TTS integration behind the pluggable interface (D-04); real-provider path smoke-tested manually (not CI, per offline constraint), mock-provider path is the CI-covered contract test.
- [ ] CAP-009/010/011 demonstrated; caption-overlay `IrOp` gets its `eval_cpu` + golden case.
- [ ] AS-1 fully complete — full MCP-script acceptance test for AS-1 passes end-to-end (CAP-019 slice).

### P6 — Keyframes + motion (AS-3 core)

- [ ] Keyframe curve UI + closed-form eval tests (§3.1, CAP-007 hook) for all shipped `Interp` variants.
- [ ] Animatable vector documents on timeline, transitions (CAP-008 partial — transitions; full effect stack may extend into P8's fusion catalog per 08).
- [ ] New golden cases covering keyframe-driven transform + a cross-transition (exercises `Merge` node blending over time, not just a static composite).
- [ ] AS-3 up through animate+composite (before grade/caption/export-with-alpha finish it) demonstrated.

### P7 — Color page (AS-2 grade pass)

- [ ] Grade node stack, CDL/wheels/curves/HSL/LUT operators, scopes (07) — `Grade` `IrOp` gets `eval_cpu` + golden cases per operator class (at minimum: one CDL case, one curve case, one LUT case — these are visually distinct enough that one generic "grade" golden case would under-test).
- [ ] Color-space unification work here (00 §7 risk: "breaks existing canvas==export guarantee") is gated by re-running **P1's vector-equivalence corpus** (§ P1 exit) — if unification changes vector-path output at all, that corpus must still pass or the change is rejected/deferred, per 00 §7's stated mitigation ("vector paths keep current behaviour until P7 revisits... with tests").
- [ ] AS-2's grade-pass slice demonstrated (scopes + full grade stack on a proxy-edited multi-clip sequence).

### P8 — Fusion + full mixer (AS-2, AS-3 complete)

- [ ] Full node-flow UI + catalog (08), per-clip and project-level graphs exercised in golden cases (splice-point cases from §3.2's compile-snapshot list now get real render-level golden coverage, not just IR-snapshot coverage).
- [ ] Full audio mixer (EQ/compressor/automation/ducking, D-05) — DSP unit tests (§3.4) for each processor; mixer-scope risk (00 §7) closed since P3 already landed gain/pan core.
- [ ] **Full SS-3 suite runs**: 10-minute sync-drift test (§5), full golden corpus, perf soak test (§4) — all three run in the nightly/pre-release CI slot and must be green before this phase (and the module) is considered release-ready.
- [ ] AS-2 and AS-3 both fully complete — MCP scripts for all three acceptance stories pass (CAP-019/SS-2 fully closed).
- [ ] CAP-013 (export presets) exercised across full codec/container matrix stated in 02 §7 (H.264/openh264, AV1, WebM/VP9, alpha-capable, GIF).
- [ ] CAP-020 full round-trip: a project touching every feature class (timeline, grade, captions, node graphs, audio automation) diffs identical before/after save/reopen; pre-video-era file still loads unchanged.

---

## 7. Rollout guards

<!-- SD-2 (27 §3): the workspace is already 8 crates (an earlier draft said "7, becomes 8"). One representative public item per crate is pinned so deleting a crate reds the gate. -->
<!-- spec-assert: symbol-exists crates/photonic-core/src/annotation.rs::Annotation -->
<!-- spec-assert: symbol-exists crates/photonic-render/src/canvas.rs::CanvasView -->
<!-- spec-assert: symbol-exists crates/photonic-gui/src/app/engine.rs::EngineBridge -->
<!-- spec-assert: symbol-exists crates/photonic-mcp/src/auth.rs::TokenStoreError -->
<!-- spec-assert: symbol-exists crates/photonic-app/src/args.rs::Args -->
<!-- spec-assert: symbol-exists crates/photonic-embed/src/lib.rs::Embedder -->
<!-- spec-assert: symbol-exists crates/photonic-matte/src/lib.rs::remove_background -->
<!-- spec-assert: symbol-exists crates/photonic-video/src/session.rs::EngineSession -->

**Feature-gating strategy.** Two layers, not one:
- **Compile-time:** a cargo feature `video` on `photonic-app` (and gating `photonic-video` as an optional workspace dependency). **Position: default-on once P3 lands** (the first phase where the crate does anything runtime-visible) — before P3, the feature doesn't need to exist since there's no code to gate. Keeping it default-on (rather than opt-in) from P3 onward matches this repo's existing practice of not shipping long-lived feature-flagged forks (no evidence of feature-flag-gated modules elsewhere in `Cargo.toml`) and avoids a second matrix of "with/without video" CI configurations, which the existing 3-OS matrix is already carrying enough of.
- **Runtime:** the video *mode* (UI entry point, 04) stays hidden/unreachable in the GUI until P2's timeline-panel UI merges — this is naturally gated by the UI simply not existing yet, no explicit flag needed for P1-P2. From P2 onward, the mode is reachable but each phase only exposes the capabilities that phase actually shipped (e.g., P2 users can arrange/cut but "Play" does nothing useful until P3 — either disable the control or let it no-op with a clear "playback coming soon" state; **recommendation: disable + tooltip**, cheaper than a real stub and avoids a confusing dead click).

**Branch strategy.** Matches the repo's existing promote-to-main flow (visible in recent history: `9794647 Promote to main: ...`, `d34f694 Merge pre-deploy: ...`) — phases land on `main` behind the compile-time feature gate above, incrementally, rather than accumulating on a long-lived `video` branch. Each phase's PR is reviewable independently and the existing CI gates (build/test/fmt/clippy/deny/doc-drift) apply per-PR, not just at a final merge — this is also why every phase's exit criteria (§6) explicitly repeats "all CI gates pass" rather than assuming it: each phase is a real merge to `main`, not a squash-at-the-end.

**Golden-corpus growth control.** Each phase adds golden cases (§6) but the §1.5 10 MB budget is a standing constraint, not just a P1-time concern — if a phase's cases would blow the budget, prefer sampling fewer ticks per case over dropping case coverage, and revisit the in-repo-vs-LFS decision (§1.5) explicitly at whichever phase first threatens the limit rather than silently exceeding it.

---

## 8. New test dependencies

`proptest` (`Cargo.toml:79`) and `criterion` (`photonic-video/Cargo.toml:88`) are **present**. **`insta` is absent**, so §3.2's IR-snapshot strategy is **unimplemented** — an open task, not a shipped capability.
<!-- spec-assert: dep-present proptest -->
<!-- spec-assert: dep-present criterion -->
<!-- spec-assert: dep-absent insta -->
<!-- SD-5 (27 §3): the earlier "none of proptest/insta/criterion exist" claim hid that two are present and only insta is absent. These pin the split so §3.2's snapshot strategy can never be silently re-described as shipped. -->


**CI must run `cargo test --all-features`.** Verified 2026-07-20: `photonic-core`'s `video-p1-contract` feature gates 8 passing contract tests out of the default test run, and nothing detects that. This is one line of CI config and it catches the whole class — see [40 §3.6](40-spec-verification.md#36-the-complement-lints-and---all-features), which ranks it as the cheapest of the three verification mechanisms and the one that should land first.

**Recommendation: do not adopt `insta`.** The IR is content-hashed by construction ([02 §2](02-engine.md)), so an IR "snapshot" is more precisely expressed as an **assertion on the compiled graph's hash and its diagnostic set** — deterministic, tiny, reviewable in a diff, and free of a snapshot-file review workflow. Where a human-readable form is wanted, generate a canonical text dump of the `FrameGraph` and compare it as a plain string with a normal `assert_eq!`. That keeps the guarantee and removes the dependency, and it composes with [40 §4](40-spec-verification.md#4-acceptance-index)'s acceptance tracking rather than adding a parallel review surface. Either way, §3.2 should be rewritten to describe what is actually built. All three are positioned above (§3.1, §3.2, §4) as recommended additions, not requirements the phases are blocked on — a phase can ship without its bench/snapshot/property-test coverage in the worst case, but should not without its plain `#[test]` coverage. Proposed `[workspace.dependencies]` additions, versions as of this doc's date (pin exact via `cargo add` at implementation time, not hand-typed here):

| Crate | Scope | Added to |
|---|---|---|
| `proptest` | dev-dependency | `photonic-core` (timeline invariants, §3.1) |
| `insta` | dev-dependency | `photonic-video` (IR compile snapshots, §3.2) |
| `criterion` | dev-dependency + `[[bench]]` targets | `photonic-video`, `photonic-core` (compile/eval budgets, §4) |

All three are widely-used, permissively-licensed (MIT/Apache-2.0/Unlicense-class) crates — `cargo deny check` (existing gate, `deny.toml` allow-list already covers MIT/Apache-2.0/BSD/ISC/Zlib/0BSD/Unlicense) should pass without an allow-list edit; confirm at implementation time since transitive deps of `proptest`/`insta` aren't audited here.

No new **non-dev** dependency is proposed by this doc — the golden-frame harness, fixture-gen script, and bless mode are all built from std + existing workspace crates (`image` is already a workspace dependency for canvas screenshots and covers PNG read/write for golden comparison, `Cargo.toml` line ~30).

---

## 9. CAP-to-test traceability

Every SPEC.md capability, mapped to where its test hook lives and which phase closes it. "Partial" means the phase demonstrates part of the capability per 00 §6's story-slice column; full closure is the phase listed in **bold**.

| Cap | Test hook | Phase(s) |
|---|---|---|
| CAP-001 (import + metadata) | Probe test against `color_bars.mp4`/`counter.mp4` fixtures (§2), asserting `MediaProbe` fields match known values | **P3** |
| CAP-002 (arrange/trim/split/snap) | `timeline/ops.rs` unit tests (§3.1) | **P2** |
| CAP-003 (ripple/roll/slip/slide) | Same, per-op table-driven tests (§3.1) | **P2** |
| CAP-004 (play/pause/scrub/step, A/V sync) | Engine integration A/V-sync test on `beep_flash.mp4` (§3.3) | **P3** |
| CAP-005 (nested sequence) | Compile-recursion test with cycle guard (§3.2, compile snapshot) + engine integration | **P3** |
| CAP-006 (vector clip, resolution-independent) | Golden case: vector clip rendered at multiple preview sizes, edges stay sharp (PSNR/SSIM at each size) | **P3** |
| CAP-007 (keyframe + easing) | Closed-form `eval` tests per `Interp` (§3.1) | **P6** |
| CAP-008 (effects/transitions, reorder/remove) | Golden cases for transition midpoints (§6 P6); effect-stack IR snapshot (§3.2) | P3 (partial, effect chain exists) → **P6/P8** (transitions, full catalog) |
| CAP-009 (auto captions, word timing) | `FakeProvider` mock contract test (§3.4) | **P5** |
| CAP-010 (caption edit: text/timing/style) | `CaptionEdit` undo-command tests (§3.1) + mock-provider round-trip | **P5** |
| CAP-011 (TTS voiceover) | `FakeProvider` mock, duration-match assertion | **P5** |
| CAP-012 (aspect-ratio switch + reframe) | Export-dimension assertion (§5) + per-format reframe persistence unit test | **P4** |
| CAP-013 (export presets/codec/container) | `ffprobe` container/codec/dimension/duration checks (§5) across full matrix | **P4** (H.264) → **P8** (full matrix) |
| CAP-014 (proxy transcode + toggle) | Proxy-mode engine integration test, scrub-uses-proxy assertion | **P3** |
| CAP-015 (color grade: exposure/wheels/curves/HSL/LUT + scopes) | Per-operator-class golden cases (§6 P7) | **P7** |
| CAP-016 (node composition, per-clip + project graph) | Compile-splice snapshot tests (§3.2) + render-level golden cases (§6 P8) | P3 (splice point exists in IR) → **P8** (full UI + catalog) |
| CAP-017 (audio mixer: gain/pan/automation/EQ/comp/meters) | DSP unit tests (§3.4) per processor + mixer integration | P3 (gain/pan core) → **P8** (full mixer) |
| CAP-018 (undo/redo, all domains) | Apply/inverse/apply-again idempotency test per `TimelineCmd` variant (§3.1), extended per-domain as each `*Cmd` sub-enum lands | Incremental every phase, **fully closed P8** |
| CAP-019 (MCP parity, all capabilities headless) | Three AS-1/2/3 MCP scripts (§3.4), each testing the slice available at that phase, full scripts green at story completion | Incremental (P4 for AS-1, **P8** for AS-2/AS-3) |
| CAP-020 (save/reopen, backward compat) | Format round-trip test (§3.1, P2) extended to cover each new feature class as it lands | Incremental every phase, **fully closed P8** |
| CAP-021 (vector-to-video render incl. alpha export) | `RasterVector` golden case (P3, opaque) + `alpha_gradient.mov`-based alpha-export verification (§5) | P3 (opaque) → **P6** (animated) → **P8** (alpha export via full pipeline) |
| CAP-022 (crash recovery of timeline projects, D-12) | Kill-and-relaunch recovery integration test (§6 P3 exit criterion) | **P3** |

Rows without a single "closes here" phase are intentionally incremental — SPEC's own phase table (00 §6) treats CAP-018/019/020 as cross-cutting, so their tests are cross-cutting too: each phase's exit criteria (§6) extends the existing test rather than deferring it whole to P8.
