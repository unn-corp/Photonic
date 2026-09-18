# 00 — Overview: Photonic Video Editor Module

**Spec set:** `docs/specs/video-editor/` · **Kernel:** [SPEC.md](SPEC.md) · **Status:** Draft 0.1 · **Date:** 2026-07-07

This doc set is the technical design for the video editor module (Approach A: timeline-first, node-ready frame-graph IR). SPEC.md is the technology-agnostic contract; docs 01–13 are the design layer, 14–18 are historical gap research, 19–27 plus ROADMAP.md are the live backlog, gate and audit layer, and 28–42 are the implementation contracts, model decisions, and cross-cutting hardening specs. Terminology defined in 01/02 is normative for all other docs.

---

## 1. Vision

Photonic gains a video editing mode: Premiere/CapCut-class timeline editing, Resolve-class color and node compositing, CapCut-class captions — with Photonic's unique advantage that vector documents are native, resolution-independent, animatable timeline citizens. One binary, one document model, one undo history, full MCP (agent) parity.

## 2. Acceptance stories (normative)

**AS-1 Social clip.** Import screen recording + music track → cut on timeline → auto-caption via hosted transcription → styled karaoke captions → switch sequence 16:9 → 9:16 with per-clip reframe → add animated Photonic vector title → quick grade → export MP4 (H.264 preset "Social 9:16").

**AS-2 Short film.** Import several 4K clips → generate proxies → multi-track edit with cross-dissolves → one shot opened as per-clip node composition (keyed overlay merge) → full grade pass with scopes (CDL wheels + curves + LUT) → audio mix with keyframed automation, EQ on dialogue track, music ducking → export master (AV1 high-quality) + web H.264.

**AS-3 Motion graphics.** Animate a Photonic vector document with keyframes (transform + path/fill properties) → composite over footage in a node composition → caption + grade → export WebM/ProRes-style with alpha.

Each build phase (§6) ends with a named slice of one story demonstrably working.

## 3. Architecture summary

```
                photonic-app (winit loop, mode dispatch)
                        │
        ┌───────────────┼──────────────────┐
   photonic-gui    photonic-mcp       photonic-video   ← NEW crate (engine)
   (egui panels,   (handlers/video.rs,  (media pool, decode,
    timeline UI,    timeline tools)      frame-graph eval,
    monitor, mode)                       playback clock, audio
        │               │                mixer, export)
        └───────┬───────┴───────┬────────┘
          photonic-render   photonic-core
          (wgpu passes,     (timeline data model:  ← NEW module core/src/timeline/
           video texture     sequences, tracks,
           path, scopes)     clips, keyframes,
                             grades, node graphs)
```

Layering rules (normative):
- `photonic-core::timeline` is pure data + pure functions (interpolation, edit ops). No I/O, no GPU, no threads. Everything serde + undoable.
- `photonic-video` owns all video I/O and the temporal engine. Depends on core + render. Never depends on gui.
- `photonic-gui` renders panels and forwards intents; it never decodes or evaluates graphs itself.
- `photonic-mcp` calls the same core edit ops and `photonic-video` services the GUI uses (CAP-019 parity by construction).

### The frame-graph IR (the load-bearing idea of Approach A)

The timeline never renders directly. For any (sequence, time) the **compiler** (`photonic-video::graph::compile`) produces a `FrameGraph`: a DAG of typed image/audio operations (decode, rasterize-vector, transform, effect, merge, grade, LUT, caption-overlay, output). The evaluator executes it on wgpu with per-node caching.

Per-clip node compositions (D-06) are user-authored subgraphs stored in the data model; the compiler splices them in place of that clip's **source op** — clip-level transform, effects, grade, and reframe still apply on top (the Resolve Fusion→Edit→Color model). The project-level graph is spliced between sequence output and the output node. Fusion pages (Phase 8) therefore add UI + node types, not a new engine.

### Working color space (D-09)

Video working state is explicit per sequence. `LinearRec709Sdr` remains default for new and existing projects: linear-light Rec.709 primaries, premultiplied alpha, `Rgba16Float` textures. `LinearRec2020Hdr` is opt-in for approved HLG/PQ workflows; `1.0` = 203 cd/m² HDR Reference White and default mastering peak = 1,000 cd/m². Decoded media converts into selected linear working state at upload; export converts from it. Existing SDR goldens remain pixel-stable. `03-render-color-pipeline.md` owns SDR boundaries; `22-dji-advanced-workflows.md` §7 owns HDR substitutions.

## 4. Locked decisions

D-01…D-12 in [SPEC.md](SPEC.md#decisions) — including **D-09** (working colour state) and **D-12** (crash recovery), both of which §3 and §5 depend on. Every doc cites decisions it depends on. *(Corrected 2026-07-20; see [27 SD-15](27-spec-audit.md).)*

## 5. Document map

| Doc | Contents | Depends on |
|---|---|---|
| 01-data-model.md | Time representation, timeline/track/clip/keyframe/grade/graph/caption/audio types, markers, groups, serialization (v4), undo commands, memory strategy | — |
| 02-engine.md | photonic-video crate: frame-graph compile/eval, decode (ffmpeg-sidecar), caches, playback clock, A/V sync, proxies, threading, export loop | 01 |
| 03-render-color-pipeline.md | Renderer prerequisite work (dirty tracking, persistent buffers, COMPOSITE_SHADER), video texture path, color-space design, f16 pipeline | 01, 02 |
| 04-ui-mode-timeline.md | AppMode mechanism, bottom timeline panel, program monitor, mode-adaptive panels, keyboard model | 01, 02 |
| 05-import-export.md | Media pool, probing, import flows, export presets, encoders, aspect-ratio/reframe system, compression options | 01, 02 |
| 06-captions-ai.md | Caption data model usage, provider trait (hosted transcription + TTS default), styling/karaoke, SRT/VTT/ASS interchange | 01, 02 |
| 07-color-grading.md | Grade node stack, CDL/wheels/curves/HSL/LUT operators, scopes (compute-shader histograms), color page UI | 01, 02, 03 |
| 08-fusion-node-flows.md | Node type catalog, per-clip + project graphs, egui-snarl UI, eval semantics, caching | 01, 02, 04 |
| 09-audio-mixer.md | Audio engine (cpal), mixer graph, automation, EQ/compressor, ducking, waveform pyramid, meters | 01, 02 |
| 10-mcp-tools.md | Tool surface for all video domains, schema/dispatch/args wiring, headless renderer access | all |
| 11-testing-phasing.md | Golden-frame corpus, A/V sync tests, perf budgets, per-phase exit criteria | all |
| 12-agent-execution-plan.md | Implementation agent roles, model tiers (Fable/Opus/Sonnet), parallelism waves, file-conflict boundaries | 11 |
| 13-ux-components.md | Component inventory for every net-new GUI surface, against DESIGN.md tokens | DESIGN.md, 01, 02, 04, 05–09 |
| ROADMAP.md | Live authoritative G/D/K/E/X inventory, priorities, gates, waves, owner links | SPEC, 01–27 |
| 14-nle-parity.md | Round-1 NLE parity gap list. **Superseded** by 17 + ROADMAP; historical rationale only | 01, 02, 04 |
| 29-qa-spec.md | Per-capability test scenarios, MCP-scripted acceptance walkthroughs, vector-regression gate | SPEC, 00–03, 10, 11 |
| 15-thumbnails-waveforms.md | Timeline thumbnail and audio-waveform generation, caching, and paint budget | 01, 02, 04 |
| 16-insert-overwrite-editing.md | The 3/4-point edit spine: insert, overwrite, lift, extract | 01, 04 |
| 17-nle-parity-round2.md | Round-2 parity gap list (G-1–G-21). **Historical**; ROADMAP owns live status | 14, 15, 16, 04, 01, 09 |
| 18-dji-parity.md | DJI-workflow gap list (D-1–D-15). **Historical**; ROADMAP owns live status | 01, 02, 05 |
| 19-editing-velocity-shot-management.md | G-1–G-5, G-7–G-9 residual editing velocity; G-6 protected context; G-13–G-15, G-21 | 01, 02, 04, 09–11, 13 |
| 20-pro-workflows.md | G-10–G-12 and G-16–G-20 pro workflows and product gates | 01, 02, 04, 11, 13, 19 |
| 21-dji-core-workflows.md | D-1–D-9 core DJI workflows, including D-5 completion context | 01–11, ROADMAP |
| 22-dji-advanced-workflows.md | D-10–D-15 advanced/gated DJI workflows | 11, 21, ROADMAP |
| 23-legal-open-source-implementation-routes.md | Accepted S1–S5 amendments, permissive/native routes, provenance gates, and evidence-based stop/go policy for G-20/D-3/D-8/D-12/D-13/D-14 | SPEC, ROADMAP, 20–22 |
| 24-preview-media-load.md | Single context-driven monitor; import readiness ladder; Draft/Full preview tiers; time-to-paint and scrub budgets; thread ownership for load/preview | 01, 02, 04, 05, CAP-001/004/014 |
| 25-performance.md | Linux/Windows interactive performance: ring depths, engine poll, release profiles, proxy priority, GUI repaint cadence | 02, 24 |
| 26-kdenlive-mlt-parity.md | Round-3 parity pass vs Kdenlive/MLT: `K-*` feature gaps, `E-*` engine lessons, `X-*` interop, and the Photonic-ahead register. Clean-room under 23 | SPEC, 01, 02, 11, 23, ROADMAP |
| 27-spec-audit.md | Cross-cutting audit of the spec set: contradictions, spec-vs-code drift, unowned capabilities, under-specified contracts, missing coverage. Tracker only — findings resolve in their owner docs | all |

| 30-effect-catalogue.md | Effect manifest schema, the raster-kernel bridge and its operand contract, the catalogue, luma wipes, nested-subgraph masking | 01, 02, 03, 11, 26, 27 |
| 31-audio-architecture.md | Audio discontinuity + latency contracts, boundary declick, pull-based analysis, FX/mixer binding, loudness, stems | 01, 02, 09, 11, 26, 27 |
| 32-engine-contracts.md | Source-range contract, analysis nodes, threading capability, playback policy, seek budgets, scale invariance, CPU/GPU parity, interlacing | 02, 03, 11, 24, 25, 26, 27 |
| 33-timeline-preview-render.md | Chunked preview rendering keyed on the frame-graph content hash | 02, 24, 25, 26, 32 |
| 34-interchange.md | MLT XML / `.kdenlive` import, OpenTimelineIO, EDL; the keyframe animation grammar | 01, 05, 23, 26, 30 |

| 35-model-decisions.md | Data-model decisions with rationale: marker scope/categories/ranges/anchoring, effect-scope pipeline order vs adjustment clips, the group tree | 01, 02, 26, 30 |

| 28-security-model.md | Trust boundaries, path containment, MCP transport hardening, subprocess and parser rules | 10, 02, 05, 01, 27 |
| 36-error-model.md | Diagnostic taxonomy, severities, surfaces, compile diagnostics, MCP mapping | 02, 08, 10, 13, 28 |
| 37-robustness.md | GPU device loss, crash recovery, scale targets, performance gate tiers | 02, 03, 04, 11, 25, 36 |
| 38-sequence-semantics.md | Transition borrowed-handle model, nested-sequence semantics, frame-rate conform | 01, 02, 05, 08, 32, 36 |
| 39-document-lifecycle.md | Undo contract and bounds, forward compatibility, document identity across tabs and MCP | 01, 04, 10, SPEC, 36 |

| 40-spec-verification.md | Drift gating (anchored code blocks, inline assertions) and the aggregated acceptance index | 11, 27, ROADMAP |
| 41-accessibility.md | Keyboard operability, focus model, hit targets, WCAG contrast gate, reduced motion, non-colour encoding | DESIGN.md, 01, 04, 06, 13, 33, 36 |

| 42-localization.md | Localization scope, Fluent string externalization, technical-vs-human formats, script-aware caption budgets, font fallback | 01, 06, 10, 13, 36, 37 |

**External research harvests** (proposal-only until Accepted; not part of the 00–42 numbering):

| Proposal | Contents |
|---|---|
| [207 OpenCut harvest index](../../proposals/207-opencut-harvest-index.md) | CapCut-class ideas from OpenCut (classic + rewrite survey): clean-room fence, inventory, delivery order → child proposals 208–214 |

**Numbering note.** The former `14-qa-spec.md` collided with `14-nle-parity.md`; it was renumbered to **`29-qa-spec.md`** on 2026-07-20 and its two inbound code references (`photonic-core/Cargo.toml`, `history/revision_contract.rs`) were updated with it. Numbers 28–39 are now in use; see the map above before allocating a new one.

## 6. Phases

| Phase | Delivers | Story slice unlocked |
|---|---|---|
| P1 Renderer foundation | Dirty tracking, persistent GPU buffers, COMPOSITE_SHADER wired, f16 video texture path (D-10) | — (prerequisite; vector editing gets faster) |
| P2 Time + timeline core | `core::timeline` data model, format v3 (now at v4), undo commands, timeline panel UI, cut/trim/split/ripple, mode switch | AS-1: arrange + cut |
| P3 Playback + media | photonic-video engine: decode, frame graph v1 (decode→transform→merge→output), A/V playback, media pool, proxies | AS-1: play; AS-2: proxy edit |
| P4 Import/export + reframe | Export presets, encoder integration, aspect-ratio system, mobile preview | AS-1 complete except captions |
| P5 Captions + AI audio | Provider trait, hosted transcription + TTS, caption track UI, styling | AS-1 complete |
| P6 Keyframes + motion | Keyframe curves UI, animatable vector documents in timeline, transitions catalog (08 §2.0b), starter vector title-template set (D-11), effect params animatable | AS-3 core |
| P7 Color page | Grade operators, wheels/curves/LUT UI, scopes | AS-2 grade pass |

> **The phase table is historical.** Work has not proceeded strictly in this order — grade operators, the DSP units and grade/graph goldens all exist while earlier-phase items (GUI export, effect rendering, audio binding) remain open. [ROADMAP.md](ROADMAP.md) owns live status; treat these rows as the original plan, not a progress report.
| P8 Fusion + full mixer | Per-clip/global node UI + node catalog, audio EQ/compression/automation/ducking | AS-2, AS-3 complete |

Phase ordering constraints and intra-phase parallelism: `12-agent-execution-plan.md`.

## 7. Top risks

| Risk | Mitigation |
|---|---|
| Renderer rework (P1) destabilizes vector editing | P1 lands behind golden-output comparison against current renderer; existing tests + new snapshot corpus gate the merge |
| Frame-graph IR designed too narrow for fusion phase | 08 authors review 02's IR before P3 code starts (explicit gate in 12) |
| ffmpeg-sidecar seek latency hurts scrub feel | Keyframe index at import + decoded-frame ring cache + proxies (02 §5); measured budget in 11 |
| Full-mixer scope (D-05) blows P8 | Mixer engine core lands in P3 with playback (gain/pan only); EQ/comp/automation are additive DSP nodes later |
| Color-space unification breaks existing canvas==export guarantee | Video path is additive; vector paths keep current behaviour until P7 revisits unification with tests |
