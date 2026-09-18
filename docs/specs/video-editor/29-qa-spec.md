# 29 — QA Spec: Scenario Matrix & TDD Scaffolding

> **⚠ Corrections, 2026-07-20.** Audited alongside 12 and 13; this document owns the CAP-019 gate, so its accuracy is load-bearing.
>
> - **P0 — §3's acceptance-story harness does not exist.** §3 states each of AS-1/2/3 "is one `#[test]`… separately compared against a GUI-driven run of the same story (**that comparison pair is the CAP-019 gate**)". There is no story script, no GUI-driven run and no comparison harness anywhere in the workspace — `crates/photonic-mcp/tests/` holds only per-verb parity tests. SS-2, SS-3 and [ROADMAP §10](ROADMAP.md#10-definition-of-done) item 10 all resolve to this non-existent artifact. §3 must either be built or restated as a design for an unbuilt harness; it currently reads as delivered.
> - **The matrix stops at CAP-021; SPEC ships CAP-022** (crash recovery, locked as D-12), and [12](12-agent-execution-plan.md) makes it a P3 exit gate. It has shipped surfaces (`app/recovery.rs`, `app/autosave.rs`) with **no scenario row, no fixture and no pass criterion**. [37 §2](37-robustness.md#2-crash-recovery) now owns the contract; this document owes it a section.
> - **§5's "P1 TDD scaffolding landed" snapshot is stale in every quantitative claim** — the golden corpus is 31 blessed cases, not 10 unblessed; there are 3 tolerance files, not 1; `photonic-core` has 560 tests, not 376; the "16 compile errors, expected" note describes an API that now exists. It also **contradicts [12](12-agent-execution-plan.md) §5**, which planned a blessed corpus. Replace §5.1/§5.3 with a pointer to the harness's own README — a hand-copied test census in a spec is guaranteed drift, and [40](40-spec-verification.md) exists to make exactly this class of claim checkable.
> - **The revision-contract tests are still feature-gated after the API shipped.** §5 records the decision to gate rather than `#[ignore]` because the target API "does not exist in the crate at all", and commits to removing the gate when P1 lands. It landed — `revision()`, `changes_since`, `affected_nodes` all ship — and `revision_contract.rs:28` is still `#![cfg(feature = "video-p1-contract")]`, so **8 passing tests never run under `cargo test --workspace`**. `photonic-core/Cargo.toml` still tells readers the API "does not exist yet". **Recommended action (code): delete the `#![cfg(...)]` line, the `video-p1-contract` feature, and the Cargo.toml comment block.**
> - **§4's "`no_tofu_glyphs` currently fails" note is stale** — replicating its scan over `photonic-gui/src` yields 0 violations, and the test is named `source_has_no_tofu_glyphs`. A stale expect-this-to-fail note is worse than none: it trains a reviewer to wave through a real failure.
> - **CAP-019 coverage is story-shaped, so capabilities outside AS-1/2/3 have no parity scenario.** CAP-005 (nested sequences) is the concrete hole — there is **no nesting verb in the MCP surface at all**, which [ROADMAP §3a](ROADMAP.md) independently confirms. CAP-019 is therefore presently unsatisfiable for CAP-005 and this document does not say so. Add a per-capability coverage row with an explicit "no MCP verb exists" marker.
> - **The `manual` layer is unfalsifiable against a DoD that requires green budgets.** CAP-017's meter row is not merely manual but currently unsatisfiable — `EngineBridge::master_level()` returns `None` unconditionally. Mark manual rows with an artifact requirement (checklist file + date + build hash) and mark the meter row blocked on G-4/[K-0.6](26-kdenlive-mlt-parity.md#8-k-0--foundations). See [37 §4](37-robustness.md#4-performance-gating) on the gate-tier split.
> - **§6 gap 2 (no `.cube` fixture) is still open and now load-bearing** — AS-2's `apply_lut` step and the CAP-015 3D-LUT row are both unexecutable without one.
> - **The matrix is frozen at SPEC-CAP granularity** while the live backlog moved to `G-`/`D-`/`K-`/`E-`/`X-`. Shipped verbs with tests but no owning scenario spec include insert/overwrite/lift/extract, match-frame, link/unlink, proxy attach/detach, adjustment clips, source marks and horizon levelling — the inverse of [27](27-spec-audit.md)'s `O-*` problem. Contracts in 28 and 30–41 are likewise uncovered.


**Depends on:** SPEC.md, 00-overview.md, 01-data-model.md, 02-engine.md, 03-render-color-pipeline.md, 10-mcp-tools.md, 11-testing-phasing.md. **Status:** Draft 0.1.

Scope: concrete test scenarios per capability (CAP-001..021), MCP-scripted walkthroughs of the three acceptance stories, the regression gate on existing vector-editing tests, and a report on the P1 TDD safety-net code landed alongside this doc (`golden_vector_equivalence.rs` + the `photonic-core` revision-counter contract tests). 11-testing-phasing.md owns test *infrastructure design* (corpus layout, comparison metrics, CI wiring, perf harness, dependency choices) and the CAP-to-phase closure table (11 §9) — this doc does not repeat that design, it fills in per-capability scenarios and is the executable complement.

---

## 1. Test-layer legend

| Layer | Means | Where |
|---|---|---|
| unit | `#[cfg(test)]` inline, pure function | `photonic-core::timeline`, DSP (09) |
| property | `proptest`-generated inputs, invariant check | `photonic-core` (edit-op non-overlap) |
| golden | Pixel-corpus comparison (two systems — see 03 §2.6 / 11 §1.1 note) | `crates/photonic-render/tests/golden/` (P1 vector corpus), repo-root `tests/golden/` (video/timeline corpus) |
| integration | Headless engine session, real fixture media | `crates/photonic-video/tests/` |
| MCP-script | Tool-call sequence, headless, asserts final state | `crates/photonic-mcp/tests/` |
| manual | Interactive GUI verification, no automated assertion | Phase exit criteria checklists (11 §6) |

---

## 2. CAP-001..021 scenario matrix

Each capability: happy path, edge cases, and failure modes, each mapped to a test layer, the fixture(s) it needs, its pass criteria, and the phase it activates in (11 §9's "closes here" phase is **bold**; a phase name alone means partial coverage lands there).

### 2.1 Per-capability CAP-019 coverage map

The AS-1/2/3 script-vs-GUI pairs (§3) are the headline CAP-019 gate, but they do not touch every capability. This table makes the rest explicit so nothing is silently ungated: for each capability, which acceptance story exercises it, which non-story test file covers it directly, and — critically — where **no MCP verb exists** so a capability can never be reached by any headless script. "NO MCP VERB" rows are the durable-anti-drift targets: once 40 §3.2's spec-assert checker lands they should become `spec-assert: symbol-absent` assertions that self-invalidate the moment a verb is added.

| CAP | AS story | Non-story test coverage | MCP-verb status |
|---|---|---|---|
| 001 Import + metadata | AS-1/2/3 (`import_media`) | `photonic-video/tests/preview_media_load.rs`, `export_synthetic.rs` (probe) | `import_media` (live) |
| 002 Arrange/trim/split/delete | AS-1 | `photonic-core` timeline-ops unit tests; `photonic-gui/tests/video_ui_paths.rs` | `insert_clip`/`split_clip`/`trim_clip`/`remove_clip` (live). **Snapping row is `manual`** — see artifact note below |
| 003 Ripple/roll/slip/slide | AS-1 (ripple delete) | `photonic-core` ops unit tests; `ops_bridge.rs` (GUI arm) | `trim_clip`/`remove_clip` ripple flags (live); **NO MCP VERB** for standalone `slip`/`slide`/`roll` (present in `ops_bridge`, GUI-only) |
| 004 Play/pause/scrub/step, A/V sync | indirect (export duration/sync) | `photonic-video/tests/playback_soak.rs`, `ss3_sync_drift.rs` (`#[ignore]`, nightly) | **NO transport MCP verb** — headless reaches frames only via `render_frame_at`; interactive transport is GUI-only |
| 005 Nested sequences | — | `photonic-core::timeline::ops::create_nested_sequence` tests (`ops.rs:1615`); `insert_clip {kind:nested_sequence}` exercised at `handlers/video.rs:7690` | `insert_clip {kind:nested_sequence}` + `delete_sequence` dangling-ref guard (`schema_gen.rs:4951`) are live. **NO wrap-selection verb** exposing `create_nested_sequence` — the one genuine MCP gap here (corrects this doc's earlier "nesting absent from MCP" claim) |
| 006/021 Vector clip + alpha export | AS-3 (`.webm` alpha) | `photonic-render/tests/golden_vector_equivalence.rs`; `photonic-video/tests/export_synthetic.rs` (alpha round-trip) | `insert_clip {kind:vector}` (live) |
| 007 Keyframes + easing | AS-1 (opacity), AS-3 (Bezier x / Linear color) | `photonic-core::timeline::anim` unit tests | `set_keyframe`/`batch_set_keyframes` (live) |
| 008 Effects/transitions | AS-2 (`cross_dissolve`) | golden corpus effect cases | `set_transition`, effect nodes via graph verbs (live) |
| 009 Auto captions | AS-1, AS-3 | `photonic-video/src/captions/mock.rs`; handler tests | `auto_caption` w/ `provider:"mock"` (live, offline) |
| 010 Caption edit | AS-1 (`set_caption_style` karaoke) | `photonic-core` captions unit tests | `set_caption_style` etc. (live) |
| 011 TTS voiceover | — | mock `TtsProvider` (`captions/mock.rs`) | `generate_voiceover` (live, mock path) |
| 012 Aspect switch + reframe | AS-1 | `photonic-gui/src/app/reframe.rs`; `video_ui_paths.rs` | `set_sequence_format`/`set_active_format`/`set_clip_prop` (live) |
| 013 Export presets/codec/container | AS-1/2/3 | `photonic-video/tests/export_synthetic.rs` | `export_sequence` (live) |
| 014 Proxy transcode | AS-2 (blocked on 4K stand-in, §6 gap 1) | `preview_media_load.rs` (L7) | `generate_proxies` (live) |
| 015 Color grade + LUT + scopes | AS-2 | golden `grade_cdl`/`grade_curve`/`grade_lut3d`; `photonic-render/src/lut.rs` (`.cube` parse + `channel_swap_rgb_to_gbr.cube` fixture) | `set_grade`/`apply_lut`/`get_scopes` (live) |
| 016 Node composition | AS-2, AS-3 | golden `node_blur_merge`, `project_graph_vignette` | `create_clip_composition`/`add_graph_node`/`add_graph_edge` (live) |
| 017 Audio mixer | AS-2 (ducking) | DSP unit tests (09) | `set_clip_audio`/`audio_fx`/`batch_set_keyframes` (live). **Master-bus meter row BLOCKED**: `EngineBridge::master_level()` (`crates/photonic-gui/src/app/engine.rs:203`) returns `None` unconditionally; the 3-step closure plan is written at `engine.rs:185-197`. No AS story may assert a meter value until that seam lands (G-4 / 26 §8 K-0.6) |
| 018 Undo/redo, all domains | indirect (every AS edit routes through `dispatch_tool`'s undo side-effect) | `photonic-core::history` tests; `history/revision_contract.rs` | undo/redo is a side-effect of the tool-call path, not a distinct verb |
| 019 MCP parity (headless) | **the AS harness itself** — `photonic-app/tests/acceptance_stories.rs` (script vs GUI arm) | `photonic-mcp/tests/mcp_parity.rs`, `mcp_parity_round2.rs` (per-verb) | n/a (this row IS the gate) |
| 020 Save/reopen, backward compat | — | `photonic-core/tests/forward_compat.rs`; `photonic-gui/tests/timeline_recovery.rs` | n/a — save/open is a document-path operation, not a timeline verb |
| 022 Crash recovery | — | `photonic-gui/tests/timeline_recovery.rs:94 recovery_round_trip_preserves_timeline` (drives `recovery_write_for_test`/`recovery_load_for_test` → `write_photon_file`/`load_document`) | n/a — contract is 37 §2; recovery is autosave-path, not an MCP verb |

**Manual-row falsifiability requirement.** Every `manual` row in the matrix below (snapping under CAP-002; caption-accuracy spot-check under CAP-009; master-bus meters under CAP-017) is only a real gate if it produces a checkable artifact. A manual row is considered satisfied for a phase only when its exit-criteria checklist file (11 §6) records: the checklist path, the verification date, and the build hash tested. A manual row with no such artifact is treated as **uncovered**, not passed.

### CAP-001 — Import + metadata

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| Import each fixture format, read back `MediaProbe` | integration | `color_bars.mp4`, `beep_flash.mp4`+`.wav`, `multi_audio.mkv` | duration/resolution/fps/channel-count fields match known source values exactly | **P3** |
| Import a VFR source | integration | `vfr_sample.mp4` | `MediaProbe::frame_rate.is_exact() == false`; a GUI/MCP-surfaced warning field is set, import still succeeds | **P3** |
| Import a path that doesn't exist | unit/integration | — | `ImportError`-class result, no asset added to pool, no panic | P3 |
| Re-probe after external file edit | integration | `color_bars.mp4` copy, truncate after import | `probe_media` updates `MediaProbe`, doesn't crash on a corrupt/truncated container | P3 |

### CAP-002 — Arrange/trim/split/delete, snapping

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| `move_clip` onto an empty region of the same track | unit | synthetic track fixture | clip's `start` updates; `Sequence::validate()` passes | **P2** |
| `move_clip` that would overlap a neighbor | unit | synthetic track fixture | op rejected with a typed `EditError`, track unchanged | **P2** |
| `trim_clip` in-edge past 0 / out-edge past source duration | unit | synthetic clip w/ known `source_in`/duration | clamps at source bounds, doesn't produce negative duration | **P2** |
| `split_clip` at an exact clip boundary (degenerate) | unit | synthetic track fixture | either a documented no-op or a typed rejection — not a zero-duration clip | **P2** |
| `remove_clip` with `ripple: false` vs `true` | unit | synthetic track fixture | non-ripple leaves a gap; ripple shifts every later clip on the track by the removed duration | **P2** |
| Snapping to clip edge / playhead / marker | manual | interactive timeline | pointer-dragged edit lands exactly on the snap target within pixel tolerance | P2 (UI, 04) |
| `proptest`: any sequence of move/trim/split ops never produces overlap | property | synthetic random track | non-overlap invariant holds after every op, every generated case | **P2** |

### CAP-003 — Ripple/roll/slip/slide

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| `ripple_edit` on a clip with 3 downstream clips | unit | synthetic track fixture | all downstream clips shift by exactly `delta`, none overlap | **P2** |
| `roll_edit` at a shared edge between two clips | unit | synthetic track fixture | shared edge moves; total combined duration of the pair is unchanged | **P2** |
| `slip_clip` (shifts `source_in` only) | unit | synthetic clip with source longer than its duration | clip's `start`/`duration` on the track are unchanged; only `source_in` moves, clamped to source bounds | **P2** |
| `slide_clip` (moves clip, trims neighbors) | unit | synthetic 3-clip track | moved clip's duration unchanged; left/right neighbors trim to close the gap exactly | **P2** |
| Roll/slide at a track boundary (no left or right neighbor) | unit | synthetic 2-clip track | edge case rejected cleanly or clamped — documented which, not a panic | **P2** |

### CAP-004 — Play/pause/scrub/step, A/V sync

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| Play a sequence with 1 video + 1 audio clip | integration | `beep_flash.mp4` + `.wav` | measured flash/beep offset < 1 frame (33.3 ms @ 30fps) | **P3** |
| `step` forward/backward across a cut boundary | integration | `counter.mp4`, 2 clips split mid-file | `EngineFrame.time`/displayed frame index matches `frame_truth.json` exactly at each step | **P3** |
| Seek to a tick with a cached GOP vs. cold seek | integration + perf | `counter.mp4` | cached-GOP seek < 50 ms, cold seek < 150 ms (02 §8 budgets, `criterion`) | **P3** |
| Pause mid-playback, resume | integration | `color_bars.mp4` | no dropped-frame spike on resume, playhead resumes at paused tick | P3 |
| Play across a nested-sequence boundary | integration | nested-sequence fixture | A/V sync tolerance holds across the boundary, not just within one sequence | P3 (CAP-005 overlap) |

### CAP-005 — Nested sequences

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| Compile a sequence containing a `NestedSequence` clip | snapshot | synthetic 2-sequence project | IR snapshot shows the inner sequence's compiled subgraph spliced at the clip's source position | **P3** |
| Edit the inner sequence, replay the outer | integration | synthetic 2-sequence project | outer sequence's next `render_frame_at`/play reflects the inner edit without an explicit outer-sequence touch | **P3** |
| `NestedSequence` referencing itself (direct cycle) | unit | synthetic project | `create_sequence`/edit-time cycle guard rejects with a typed error, no infinite compile recursion | **P3** |
| `NestedSequence` referencing a sequence that (transitively) nests the first (indirect cycle) | unit | synthetic 3-sequence project | same cycle guard catches the transitive case | **P3** |
| `delete_sequence` on a sequence still referenced by a `NestedSequence` elsewhere | unit | synthetic 2-sequence project | delete rejected (dangling-ref guard, 01 §5), sequence + reference both intact | P3 |

### CAP-006 / CAP-021 — Vector clip on timeline, resolution-independent, alpha export

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| Place a `ClipSource::Vector` clip, render at 3 preview sizes | golden | a `.photon` fixture from `tests/golden/` (P1 corpus) referenced as a vector asset | edges stay sharp at each size — PSNR/SSIM at each size meets 11 §1.2 thresholds, no resampling artifacts from a fixed raster cache | **P3** (opaque) |
| Animate the vector document's own properties (transform/fill) via keyframes | golden | vector fixture + keyframe track | rendered frames at 3 sampled ticks match interpolated property values | **P6** |
| Export with alpha (transparent background) | integration + export-verify | `alpha_gradient.mov`-class output target | decoded export shows correct alpha ramp per §5's export-verification procedure | **P8** |
| Vector clip whose source document is deleted from disk mid-session | integration | vector fixture, deleted before render | diagnostic-placeholder frame, no panic, no engine stall (mirrors 02 §3's sidecar-failure containment guarantee) | P3 |
| Tier A vs Tier B render path equivalence (03 §2.5) — same vector doc through CPU roundtrip and GPU-direct paths | golden | any pure-vector fixture from the P1 corpus | Tier B output within tolerance of Tier A (both should be visually identical; Tier A is the correctness baseline) | P1 (renderer prereq) → **P3** (video-path wiring) |

### CAP-007 — Keyframes + easing

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| `Hold` interpolation, sample past `at` | unit | hand-built `PropertyTrack` | returns left keyframe's value unchanged past its `at` | **P6** |
| `Linear` interpolation at t=0.25/0.5/0.75 | unit | hand-built `PropertyTrack` | matches exact linear-interpolation formula at each sample | **P6** |
| `Bezier` interpolation at a few t values | unit | hand-built `PropertyTrack` w/ known handles | matches hand-computed cubic-bezier-ease values within float epsilon | **P6** |
| Bool/Enum property with `interp: Linear` requested | unit (regression) | hand-built `PropertyTrack` on a `PropValue::Bool` | interpolation is silently forced to `Hold` regardless of requested interp — explicit regression test per 11 §3.1 | **P6** |
| Two keyframes at the same `at` (upsert via `set_keyframe`) | unit | — | second call replaces the first; `keyframes` stays sorted + unique `at` | **P6** |
| `batch_set_keyframes` across two different targets | MCP-script | — | one undo step covers both targets (CAP-018 interaction) | P6 |

### CAP-008 — Effects/transitions: apply, reorder, remove

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| `add_effect` then `render_frame_at` before/after | golden | effect-stack `.photon`-class fixture (reuse P1 corpus `effect_stack_color_overlay_stroke` pattern, video-clip form) | render changes measurably vs. no-effect baseline | P3 (chain exists) → **P6/P8** |
| `reorder_effects` (order-dependent effects, e.g. two color ops) | golden | 2-effect stack fixture | reordered render differs from original order in the expected direction | P8 |
| `remove_effect` restores pre-effect render | golden | 1-effect fixture | post-removal render matches the no-effect baseline within byte-exact/PSNR per 11 §1.2 | P6/P8 |
| Cross-dissolve transition at 0%/50%/100% | golden | 2-clip fixture w/ `set_transition` | midpoint frame is a blend of both clips per the transition's blend function; 0%/100% match each source clip alone | **P6** |
| Animatable effect parameter (keyframed blur radius) | golden + unit (CAP-007 overlap) | effect fixture + keyframe track | rendered frame at a sampled tick matches the interpolated parameter value | P6 |

### CAP-009 — Auto captions, word-level timing

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| `auto_caption` against `FakeProvider` returning a canned transcript | MCP-script (mock) | canned transcript fixture | new caption track appears, each cue carries word-level start/end times matching the canned data exactly | **P5** |
| `auto_caption` job polling (`get_job_status`) | MCP-script (mock) | — | status transitions pending → running → complete; `cancel_job` mid-run leaves no orphaned track | P5 |
| Provider returns an error (network/auth failure simulated in mock) | MCP-script (mock) | `FakeProvider` error variant | job reports failure status, no partial/corrupt caption track left behind | P5 |
| Accuracy sample against known speech (manual, non-CI per offline constraint) | manual | real hosted-provider smoke test | transcript text spot-checked against known source speech | P5 (real-provider path, not CI-covered) |

### CAP-010 — Caption edit: text/timing/grouping/styling

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| `set_caption_word` text edit | unit (`CaptionEdit` undo cmd) | canned caption track | word text updates; undo restores exact prior text | **P5** |
| `set_caption_cue` timing edit | unit | canned caption track | cue start/end update; overlapping-cue validation (if any) holds | **P5** |
| `split_caption_cue` / `merge_caption_cues` round trip | unit | canned caption track | split then merge restores the original cue's text/timing exactly | **P5** |
| `set_caption_style` — karaoke highlight/animation | golden | styled caption fixture | rendered frame at a mid-word tick shows the highlighted word per the animation curve | **P5** |
| Style edit persists through export | export-verify | styled caption fixture | exported frame at the same tick matches preview rendering within tolerance | P5/P8 |

### CAP-011 — TTS voiceover

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| `generate_voiceover` against `FakeProvider` returning canned audio bytes | MCP-script (mock) | canned audio fixture | audio clip appears on the target track; clip duration matches the canned audio's actual duration | **P5** |
| Empty/whitespace-only text submitted | MCP-script (mock) | — | typed rejection (validation error), no clip inserted | P5 |
| Voice/provider param passthrough | MCP-script (mock) | — | mock provider receives the exact `voice`/`provider` args passed | P5 |

### CAP-012 — Aspect-ratio switch + per-clip reframe

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| `set_sequence_format` add 9:16 alongside existing 16:9 | unit | synthetic sequence | both `SequenceFormat` entries persist; `set_active_format` switches which is active | **P4** |
| Per-clip `reframe` override set for one format, absent for another | unit | synthetic clip | reframe override applies only when that format is active; other format uses default framing | **P4** |
| Export dimensions match the active format at export time | export-verify | any sequence fixture | `ffprobe` dimensions equal the active `SequenceFormat`'s width/height exactly | **P4** |
| Mobile-frame preview toggle | manual | interactive monitor | preview shows the 9:16-cropped frame overlaid/cropped as designed | P4 (UI) |

### CAP-013 — Export presets/codec/container

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| Export H.264 with an explicit preset | export-verify | any golden-corpus sequence case | container/codec/dimensions/duration probe correctly via `ffprobe` | **P4** |
| Export AV1 high-quality + a second web-H.264 pass (AS-2) | export-verify | AS-2-slice fixture | both outputs probe correctly, independently | P8 |
| Export with alpha (WebM/ProRes-style) | export-verify | `alpha_gradient.mov`-class fixture | decoded alpha channel matches expected ramp | P8 |
| Export GIF | export-verify | short sequence fixture | valid GIF, frame count/duration match sequence | P8 |
| Full codec/container matrix (H.264/openh264, AV1, WebM/VP9, alpha, GIF) | export-verify | full matrix run | every entry in 02 §7's matrix probes correctly | **P8** |
| Invalid/unsupported preset+container combination requested | MCP-script | — | typed rejection before job starts, not a silent bad export | P4 |

### CAP-014 — Proxy transcode + toggle

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| `generate_proxies` on a 4K-class fixture, then scrub | integration | a synthetically-tagged "high-res" fixture (real 4K media out of fixture-size budget — see §7 gap below) | scrubbing reads the proxy file, not the original (assert via file-handle/path introspection, not just visually) | **P3** |
| `set_proxy_mode(ForceOriginal)` while a proxy exists | integration | same fixture | playback/scrub reads the original despite a proxy being present | **P3** |
| `remove_proxy` | integration | same fixture | proxy file deleted, mode falls back to original, no dangling reference in the asset's `MediaProbe`/proxy fields | P3 |
| Export always uses originals regardless of proxy mode | export-verify | same fixture | exported frame quality/bytes trace to the original, not the proxy (spot-check via resolution or a marked pixel difference) | P3/P4 |

### CAP-015 — Color grade: exposure/wheels/curves/HSL/LUT + scopes

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| Exposure/contrast/temperature adjustment | golden | one CDL-class fixture | scope (histogram) shifts in the expected direction; grade persists in export | **P7** |
| Lift-gamma-gain (CDL) wheels | golden | CDL fixture | matches expected CDL transfer function within tolerance | **P7** |
| Tone curve | golden | curve fixture | matches expected curve-applied output | **P7** |
| HSL adjustment (hue shift on one channel range) | golden | HSL fixture | only the targeted hue range shifts; others unchanged | **P7** |
| 3D LUT application | golden | LUT fixture + a known `.cube`-class test LUT | output matches LUT-applied reference within tolerance | **P7** |
| `copy_grade` onto N clips as one undo step | unit (CAP-018 overlap) | multi-clip sequence | one `Command::Batch` undo entry reverts all N clips' grades together | P7 |
| Waveform/vectorscope/histogram live update | manual | interactive color page | scope redraws on grade-control drag, matches the rendered frame | P7 (UI) |

### CAP-016 — Node composition (per-clip + project graph)

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| Two-input merge composition on a clip | snapshot + golden | synthetic 2-source graph fixture | IR snapshot shows the composition spliced in place of the clip's source op (02 §2 step 3); render shows the merged result | P3 (splice point) → **P8** (full) |
| Project-level graph operator affects final output only | golden | 2-sequence project, one w/ project graph set | the sequence WITHOUT the project graph active is unaffected; final composited output reflects the operator | **P8** |
| Cycle in a per-clip graph (node feeds back into itself) | unit | synthetic graph fixture | `add_graph_edge` rejected at edit time, cycle-checked (never reaches compile) | P3+ |
| Graph fails to compile (missing required input) | integration | synthetic malformed graph | falls back to the clip's default chain, `get_graph` surfaces the type-check diagnostic (10 §3.11) | P3+ |
| Composited clip rendered across ≥2 `SequenceFormat`s | snapshot | composed-clip + 2-format fixture | per-format reframe applies ON TOP of the composition's output (positive case — previously a documented gap, per 11 §3.2) | P8 |

### CAP-017 — Audio mixer

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| Per-clip gain + fade in/out | unit (DSP) | synthetic sine-wave signal | measured output level matches applied gain in dB within tolerance; fade curve matches its shape at sampled points | P3 (core) |
| Per-track volume/pan/mute/solo | unit (DSP) | synthetic multi-track signal | soloed track's output isolated; muted track contributes zero energy | P3 (core) → P8 (full UI) |
| Keyframed gain automation | unit (CAP-007 overlap) | synthetic signal + keyframe track | measured gain at sampled ticks matches interpolated automation curve | **P8** |
| EQ band (known-gain test) | unit (DSP) | sine-sweep signal | frequency response matches expected EQ curve within tolerance | **P8** |
| Compressor (known threshold/ratio) | unit (DSP) | synthetic signal exceeding threshold | output level compressed per configured ratio above threshold | **P8** |
| Music ducking under dialogue | integration | dialogue + music synthetic mix | music level drops when dialogue track is active, per ducking config | **P8** |
| Master bus level meters | manual/integration | any mixed sequence | `get_audio_meters` reports levels consistent with the actual mixed signal | P3 (core) → P8 |

### CAP-018 — Undo/redo, all domains

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| Apply → inverse → apply-again idempotency, per `TimelineCmd` variant | unit | synthetic doc per variant | document state after "apply, undo, redo" equals state after the original "apply" | Incremental every phase, **fully closed P8** |
| Mixed edit session (timeline + grade + caption + audio + graph edits interleaved), undo to start, redo to end | integration | full-feature synthetic project | document state identical at both endpoints (SS-2's own literal wording, CAP-018's test hook) | **P8** |
| Coalescing: drag-move/trim and keyframe-drag | unit | synthetic clip drag sequence | one continuous gesture collapses to a single undo step (mirrors existing `UpdateNode` coalescing) | P2+ |
| Undo past the history size/byte limit | unit | synthetic long edit session | oldest steps drop per existing limit-enforcement behavior; no panic, no corrupt state | P2+ (existing mechanism, extended) |

### CAP-019 — MCP parity (headless, all capabilities)

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| AS-1 scripted end-to-end | MCP-script | AS-1 fixture set (§4 below) | exported MP4 probes 9:16, has a burned/rendered caption track, duration matches the cut timeline | **P4 (partial)/P5 (full)** |
| AS-2 scripted end-to-end | MCP-script | AS-2 fixture set | export matches full grade + node-composition + full-mixer expectations | **P8** |
| AS-3 scripted end-to-end | MCP-script | AS-3 fixture set | export matches animated-vector + composite + alpha expectations | **P8** |
| Script output vs. equivalent GUI-driven run, same story | golden (comparison) | same fixture set, GUI-driven | script-produced and GUI-produced final renders match within the golden-frame corpus's tolerance — this pair **is** the CAP-019 test (10 §9.1) | Per-story phase (above) |

### CAP-020 — Save/reopen, backward compat

| Scenario | Layer | Fixture | Pass criteria | Phase |
|---|---|---|---|---|
| v2 (pre-video) file loads on the video-era build unchanged | unit | a pre-video-era `.photon` fixture (checked in from `main` pre-P2) | document loads with `timeline: None`, no behavior change vs. pre-video build | **P2** |
| v3 file with no timeline features loads on a build one version back | unit | v3 file generator, `timeline: None` | opens within `COMPAT_WINDOW` on the older build (existing policy, extended) | **P2** |
| Round-trip a timeline-only project (no media) | unit | synthetic timeline project | save → load produces an identical document (deep-eq or serialized-JSON-eq) | **P2** |
| Round-trip a project touching every feature class (timeline, grade, captions, node graphs, audio automation) | integration | full-feature synthetic project | diff before/after save/reopen is empty | **P8** |
| Corrupted-on-disk JSON (deliberately malformed) | unit | hand-corrupted fixture | `Sequence::validate()` (or load path) rejects cleanly, no panic, no silent data loss | P2 |
| Unknown `PropPath` on load (asset re-registers later) | unit | keyframe track w/ unknown path | track kept, flagged `orphaned`, not dropped (01 §6.2 constraint) | P2/P6 |

### CAP-021 — Vector-to-video render incl. alpha export

Covered jointly with CAP-006 above (shared row group — both capabilities share their test hooks per 11 §9). See CAP-006 table.

---

## 3. Acceptance-story walkthroughs (MCP tool sequences)

Tool names below are exact `10-mcp-tools.md` §3 entries. Each story is one `#[test]` per 10 §9.1, run headless, and separately compared against a GUI-driven run of the same story (that comparison pair is the CAP-019 gate — not repeated as a manual step here).

### AS-1 — Social clip

```
import_media(paths: ["screen_recording.mp4", "music.mp3"])
create_sequence(name: "social", frame_rate: 30, formats: [{name: "16:9", width: 1920, height: 1080}])
add_track(sequence_id, kind: "video")
add_track(sequence_id, kind: "audio")
insert_clip(track_id: video_track, start: 0, source: {Asset: screen_recording_asset})
insert_clip(track_id: audio_track, start: 0, source: {Asset: music_asset})
split_clip(clip_id: screen_clip, at: <cut point>)
trim_clip(clip_id: screen_clip_b, edge: "in", new: <trimmed in>)
remove_clip(clip_id: <unwanted segment>, ripple: true)
auto_caption(sequence_id)                              # job
get_job_status(job_id)                                  # poll to completion
set_caption_style(track_id: caption_track, style: {karaoke highlight/animation per 01 §7})
set_sequence_format(sequence_id, op: "add", format: {name: "9:16", width: 1080, height: 1920})
set_active_format(sequence_id, format_index: 1)
set_clip_prop(clip_id: screen_clip_b, path: "reframe.9:16", value: {<per-clip offset>})
insert_clip(track_id: title_track, start: 0, source: {Vector: title_asset})
set_keyframe(target: {clip_id: title_clip, path: "transform.opacity"}, at: 0, value: 0.0, interp: "Linear")
set_keyframe(target: {clip_id: title_clip, path: "transform.opacity"}, at: <t>, value: 1.0, interp: "Linear")
set_grade(clip_id: screen_clip_b, grade: {<quick grade — exposure/contrast only>})
export_sequence(sequence_id, out_path: "social_9x16.mp4", preset: "Social 9:16")
get_job_status(job_id)                                  # poll to completion
```

Pass criteria (CAP-019/SS-2): exported file probes 9:16 at the configured resolution; caption track present with correct word timing and karaoke styling baked/rendered in; title clip visible with the authored fade; duration matches the cut timeline's total; A/V sync within tolerance (SS-3).

### AS-2 — Short film

```
import_media(paths: [<several 4K clip paths>])
generate_proxies(asset_ids: [<all imported>])          # job
get_job_status(job_id)                                  # poll
create_sequence(name: "film", frame_rate: 24, formats: [{name: "16:9", width: 3840, height: 2160}])
add_track(sequence_id, kind: "video") x N, add_track(sequence_id, kind: "audio") x M
insert_clip(...) x N                                     # multi-track edit
set_transition(clip_id, edge: "in", transition: {kind: "cross_dissolve", duration: <t>})
create_clip_composition(clip_id: keyed_shot)             # per-clip node comp
add_graph_node(graph_id, op: {kind: "ChromaKey"})
add_graph_node(graph_id, op: {kind: "Merge"})
add_graph_edge(graph_id, from: {...}, to: {...}) x N
set_grade(clip_id, grade: {CDL wheels})
set_grade(clip_id, grade: {tone curve})
apply_lut(clip_id, lut_path: "<lut>.cube")
get_scopes(clip_id, at: <t>)                             # verify scope reads mid-grade
set_clip_audio(clip_id: dialogue_clip, gain_db: <g>)
audio_fx(track_id: dialogue_track, op: "add", kind: "EQ", params: {...})
audio_fx(track_id: music_track, op: "add", kind: "Ducking", params: {trigger_track: dialogue_track})
batch_set_keyframes(ops: [<automation points on music_track volume>])
export_sequence(sequence_id, out_path: "master.mkv", overrides: {codec: "av1", quality: "high"})
get_job_status(job_id)
export_sequence(sequence_id, out_path: "web.mp4", preset: "Web H264")
get_job_status(job_id)
```

Pass criteria: both exports probe correctly and independently; keyed-overlay composition renders correctly in both timeline playback and export; full grade stack (CDL + curves + LUT) matches golden reference within tolerance; ducking measurably lowers music level under dialogue; keyframed automation matches interpolated values at sampled ticks.

### AS-3 — Motion graphics

```
import_media(paths: ["footage.mp4"])
create_sequence(name: "motion-graphics", frame_rate: 30, formats: [{name: "16:9", width: 1920, height: 1080}])
add_track(sequence_id, kind: "video") x 2
insert_clip(track_id: footage_track, start: 0, source: {Asset: footage_asset})
insert_clip(track_id: vector_track, start: 0, source: {Vector: title_doc_asset})
set_keyframe(target: {clip_id: vector_clip, path: "transform.x"}, at: 0, value: -200, interp: "Bezier", ...)
set_keyframe(target: {clip_id: vector_clip, path: "transform.x"}, at: <t>, value: 0, interp: "Bezier", ...)
set_keyframe(target: {clip_id: vector_clip, path: "node.<node_id>.fill.color"}, at: 0, value: <c0>, interp: "Linear")
set_keyframe(target: {clip_id: vector_clip, path: "node.<node_id>.fill.color"}, at: <t>, value: <c1>, interp: "Linear")
create_clip_composition(clip_id: vector_clip)
add_graph_node(graph_id, op: {kind: "Merge"})            # composite over footage
add_graph_edge(graph_id, from: {footage source}, to: {Merge input A})
add_graph_edge(graph_id, from: {vector source}, to: {Merge input B})
auto_caption(sequence_id)
get_job_status(job_id)
set_grade(clip_id: footage_clip, grade: {quick pass})
export_sequence(sequence_id, out_path: "motion.webm", overrides: {alpha: true})
get_job_status(job_id)
```

Pass criteria: exported frames show the vector title animating per its keyframe curves (transform + fill color); composite-over-footage renders correctly in both preview and export; alpha channel present and correct in the export (CAP-021).

### Fixture set needed for §3's scripts (beyond 11 §2's corpus)

| Fixture | Purpose | Notes |
|---|---|---|
| `title_asset` (`.photon` vector doc) | AS-1's animated title | Reuse/extend a `tests/golden/` P1-corpus-style fixture; author via the same `Document`-builder pattern as `crates/photonic-render/examples/gen_p1_golden_fixtures.rs` |
| `title_doc_asset` (`.photon` vector doc, multi-node) | AS-3's keyframed vector | Needs ≥2 named nodes for the fill-color keyframe target path |
| `<lut>.cube` test LUT | AS-2's `apply_lut` | A small, known-transform LUT (e.g. a pure channel swap) so its effect is assertable, not just "looks different" |
| 4K-class clip stand-ins | AS-2's `generate_proxies` | Real 4K media is out of the ≤5 MB fixture budget (11 §2) — flagged as a gap, §7 below |

---

## 4. Regression gate — existing vector-editing suite

Per SPEC.md constraints ("Existing vector-editing behaviour and performance must not regress") and 11 §6's per-phase floor:

- **Every phase's exit criteria includes, unconditionally**: `cargo test --workspace --locked` green across all pre-video-era test files (`photonic-core`, `photonic-gui`, `photonic-render`, `photonic-mcp` — for the current per-crate counts see CI, not a hand-copied number here; a frozen census rots the moment a test is added).
- P1 carries the sharpest version of this gate (00 §7's named top risk): the golden-vector-equivalence corpus (§5 below) IS the concrete mechanism, not a restatement of "be careful."
- P7's color-space unification work re-runs the SAME P1 corpus as its own gate (03 §2.6 note, 11 §6 P7 row) — the corpus is written once, reused twice.
- The glyph-coverage guard is `photonic-gui`'s `source_has_no_tofu_glyphs` (`crates/photonic-gui/tests/no_tofu_glyphs.rs`); it is part of the unconditional phase floor above like any other test. (Historical note: an earlier draft of this section claimed the test was named `no_tofu_glyphs` and "currently fails" as a pre-existing unrelated gap — both are stale and were deleted, because a standing "expected failure" note trains reviewers to wave a real failure through.)

---

## 5. P1 TDD scaffolding landed with this doc

Two pieces of test code, both written to compile and run against the codebase **today** (before any P1 implementation), verified via `cargo build --workspace` and `cargo test --workspace --no-fail-fast`:

### 5.1 `crates/photonic-render/tests/golden_vector_equivalence.rs` (03 §2.6 harness)

- Renders every case under `crates/photonic-render/tests/golden/` through `HeadlessRenderer::render_rgba_with_opts` and compares against `expected/reference.png`.
- Skips with a printed message when no GPU adapter is available (mirrors `headless.rs`'s `try_renderer()` convention exactly).
- Bless mode: `PHOTONIC_BLESS_GOLDEN=1 cargo test -p photonic-render --test golden_vector_equivalence -- --test-threads=1`.
- Comparison rule: byte-exact by default; a case carrying `tolerance_db.txt` switches to a PSNR-floor comparison (`blend_nonseparable/tolerance_db.txt` = `45.0`, per 03 §2.6's recommended threshold for `COMPOSITE_SHADER`-touched cases). On failure, writes a per-pixel abs-diff heatmap PNG next to the case (11 §1.2's "dump a diff PNG" convention).
- **Fixture corpus**: the hand-authored `.photon` documents under `crates/photonic-render/tests/golden/`, generated via the checked-in `crates/photonic-render/examples/gen_p1_golden_fixtures.rs` (a dev tool, not CI-run — same convention as `tools/gen-mcp-docs.py`). The corpus has since grown well past this doc's original 10 cases (currently 31 case dirs — a P1 architect-gate expansion); the authoritative, self-updating census lives in `crates/photonic-render/tests/golden/README.md`, not a hand-copied list here. It spans the surface area 03 §2.6 named (paths+fills, strokes, gradients, blend modes incl. non-separable, text, raster placement, effect-stack, boolean groups) plus the batch-2 expansion (symbols, compound/nested/boolean variants, pattern/mesh/fluid gradients, masks, adjustment layers).
- **Verified end-to-end**: bless mode was run once locally (GPU adapter available in this environment) to confirm the harness renders every fixture without panicking and writes valid PNGs, then comparison mode was run and passed against those blessed references. **The blessed PNGs were then deleted** before committing — per the task's "keep PNGs unblessed" instruction, the corpus ships in its pre-bless state; the harness reports "no blessed reference — run with PHOTONIC_BLESS_GOLDEN=1 first" as its current (expected) `cargo test --workspace` result, not a compile error.
- This corpus is deliberately **separate** from the repo-root `tests/golden/` (11 §1's video/timeline corpus) — documented in both this file's `tests/golden/README.md` and inline in the harness's module doc comment, per 03 §2.6 / 11 §1.1's mutual cross-reference requirement.

### 5.2 `photonic-core` revision-counter contract tests (03 §2.1)

Location: `crates/photonic-core/src/history/revision_contract.rs`, wired into `history/mod.rs` via `#[cfg(test)] mod revision_contract;`.

**Status: graduated to always-on.** This section originally shipped the module behind a `#[cfg(feature = "video-p1-contract")]` gate as TDD scaffolding for an API (`CommandHistory::revision()`/`changes_since()`, `Command::affected_nodes()`, `ChangeSummary`) that did not yet exist. That API has since landed, so the gate, the empty `video-p1-contract` feature in `photonic-core/Cargo.toml`, and the Cargo.toml comment block were all removed (commit `1ccbeea`): the module is now an ordinary `#[cfg(test)]` module compiled and run by the default `cargo test --workspace`. See the module's own doc comment at `revision_contract.rs:1` for the graduation record. The workspace now carries **zero cargo features** in any of its `Cargo.toml` files.

Eight tests, each citing its 03 §2.1 spec line in an assertion message: `revision()` bumps on execute and on undo/redo (2 tests), `Command::affected_nodes()` reports the right `NodeId` for `AddNode` and `UpdateNode` (2 tests), and `changes_since()` covers a single-command touch, a multi-command union, the `from == current revision` empty case, and the `from` predates the ring-overflow case (4 tests) — `revision_contract.rs:42,:65,:101,:120,:140,:169,:204,:227`.

CI runs `cargo test --workspace --locked --all-features` (`.github/workflows/ci.yml`); with zero features declared that is identical to the default run, and the comment there records the `video-p1-contract` graduation so the `--all-features` flag is not mistaken for still-live scaffolding.

### 5.3 Verification run

For current pass/fail state and per-crate test counts, consult CI (`.github/workflows/ci.yml`) rather than a frozen census — the numbers this section used to hardcode (376/63/…) drift on every added test and are not a durable record. The durable facts:

- The default `cargo test --workspace --locked` build compiles with no cargo features (there are none) and the revision-contract module runs as an ordinary `#[cfg(test)]` module (§5.2).
- `crates/photonic-render/tests/golden_vector_equivalence.rs` skips (not fails) when no GPU adapter is present and, on a machine with an adapter, compares against the blessed corpus under `crates/photonic-render/tests/golden/`.
- `photonic-gui`'s glyph guard is `source_has_no_tofu_glyphs` (§4) and is expected to pass, not fail.

The anti-drift home for these facts is 40 §3.2's spec-assert checker once it lands, not another hand-count in this doc.

---

## 6. Spec gaps found while writing this doc

1. **11 §2's fixture corpus has no "high-resolution stand-in."** CAP-014 (proxy generation) and AS-2's `generate_proxies` step both need something that behaves like real 4K footage to meaningfully exercise proxy-vs-original selection, but 11 §2's budget (≤5 MB total) rules out an actual 4K clip. Position for P3 implementers: either (a) a small-but-high-resolution synthetic fixture (e.g. 3840×2160 but 1–2 s, solid-color/pattern content compresses tiny), or (b) a `MediaProbe` override/test-only hook that reports a fixture as "high-res" regardless of its real dimensions, purely to exercise the proxy-selection branch. Flagging rather than deciding — 02 §6 (proxies) owns the actual mechanism.
2. **No fixture LUT (`.cube` file) is named anywhere in 11 §2 or 07.** CAP-015/AS-2 both need one for `apply_lut` testing. Added to §3's fixture-set table above as a gap; 07-color-grading.md should name or generate one when its own test section is written.
3. **`Sequence::validate()`'s exact rejection rules aren't fully enumerated in 01 §4** (it names "overlap/negative-duration" but not, e.g., zero-duration clips or a clip fully outside the sequence's declared range) — the CAP-002/CAP-020 unit tests above assume "overlap and negative-duration only" per 01's own wording; if 01 is amended with more rules, this doc's scenario rows need the same amendment (cross-reference, not blocking).
4. **CAP-016's cycle-guard test (per-clip graph) and CAP-005's cycle-guard test (nested sequence) are structurally identical patterns** (self-reference + transitive reference) applied to two different graph types (node graph vs. sequence-nesting graph) — 02 §2 doesn't say whether they share an implementation (e.g. a generic cycle-detection utility) or are independently implemented per-domain. Not blocking, but worth a one-line note in 02 if P3 implementers want to share the utility rather than duplicate it.
