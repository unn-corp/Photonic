# 207 — OpenCut Harvest Index (valuable ideas for Photonic)

> **Status: Index live — first implementation wave landed 2026-08-03.**  
> This is the **index** for a set of implementation proposals that capture
> *capability ideas* observed in OpenCut (CapCut-class open NLE), rewritten
> against Photonic’s architecture. Child proposals track remaining depth.

**Date:** 2026-08-03  
**Territory:** cross-cutting (`core-timeline`, `photonic-video-engine`,
`panels-video`, GUI command/keymap).  
**Authority:** owner contracts remain in `docs/specs/video-editor/` and the
linked proposals below. This index only ranks, scopes, and fences provenance.

---

## 1. Why this harvest exists

OpenCut ([github.com/OpenCut-app/OpenCut](https://github.com/OpenCut-app/OpenCut))
is a free CapCut alternative. As of 2026-08:

| Tree | What it is | Useful to Photonic? |
|---|---|---|
| **OpenCut main** (rewrite) | GPUI desktop shell + web scaffold; engine crates **not** present yet | Almost nothing — ambitions (MCP, headless, plugins) already exist in Photonic |
| **opencut-classic** (archived) | Working Next.js NLE + thin Rust/WASM GPU (compositor, blur, JFA masks, time) | **Selective algorithms + UX patterns** — this harvest |

Photonic’s video module on `feat/video-editor-module` already outruns classic
OpenCut on pro NLE depth (frame-graph IR, scopes/grade, audio mixer, export
queue, MCP parity, Kdenlive/MLT inventory). The harvest is **not** “port
OpenCut.” It is “steal the few CapCut-class ideas we still lack.”

---

## 2. Clean-room and licensing fence (normative)

Both OpenCut trees are **MIT**. That is **not** a free pass to copy structure.

1. **No vendoring of OpenCut source.** No git submodule, no copy-paste of TS
   controllers or Rust crates into the Photonic tree.
2. **No re-export of OpenCut identifiers.** Types, module layout, shader
   filenames, and public API names must be Photonic-native.
3. **Algorithms only when re-spec’d here.** Each child proposal must name the
   *technique* (public literature where possible), the *Photonic owner doc*, and
   acceptance fixtures. Implementation is from this suite, not from a second
   browser tab open on OpenCut source.
4. **Patent / 23 §11.** Jump-flood and Gaussian convolution are foundational CG
   (see [208](208-sdf-jfa-mask-feather.md) §7, [209](209-large-radius-blur-quality.md)).
   Anything adjacent to tracking/matting re-opens [23](../specs/video-editor/23-legal-open-source-implementation-routes.md)
   §11 and [198](198-k-b9-rotoscoping-spline-masks.md) §7.
5. **Stop/go.** [23 §14](../specs/video-editor/23-legal-open-source-implementation-routes.md#14-stopgo-checklist-before-any-code)
   applies before any child proposal moves from *Proposed* to *Accepted* code
   work. Band-5 items that already own a mini-spec (K-B8/K-B9) absorb harvest
   *options* into those docs rather than forking a second data model.

This fence matches [26 §2](../specs/video-editor/26-kdenlive-mlt-parity.md#2-clean-room-and-licensing-fence)
point 5 and 198 §11.

---

## 3. What was **not** harvested (explicit non-goals)

| OpenCut surface | Why skip |
|---|---|
| Full timeline data model / MediaTime@120k ticks | Photonic `Tick` / 01 already own time |
| Browser WebCodecs / canvas export | Photonic is desktop + ffmpeg sidecar |
| WASM dual-target bridge | No browser video product today |
| IndexedDB project storage | Photonic document/file lifecycle (39) |
| Next.js / shadcn / React UI | Photonic is egui |
| GPUI desktop rewrite | Wrong UI toolkit |
| Cloud auth / DB / fal.ai generative pipeline | Out of SPEC video scope |
| “Editor API + plugins” rewrite ambitions | Photonic MCP + headless already exist; plugin surface is a later product decision |

---

## 4. Harvest inventory

| # | Proposal | Value | Photonic gap it closes | Effort | Format impact | Gate |
|---|---|---|---|---|---|---|
| **208** | [SDF / Jump-Flood mask feather](208-sdf-jfa-mask-feather.md) | High | Soft large-radius feather for K-B9 / K-B8 / `util.outline` without multi-iter Gaussian cost | M | none if pure IR/render | Algorithm provenance recorded; no patent gate expected |
| **209** | [Large-radius multi-pass blur quality](209-large-radius-blur-quality.md) | High | Blur / Glow / feather fallback quality at large σ | S | none | none |
| **210** | [Timeline interaction velocity pack](210-timeline-interaction-velocity-pack.md) | High | Clip gain envelope on body; bookmark UX; edge-pan polish; snap feel | M | optional additive (bookmark alias only) | none |
| **211** | [Keyframe graph editor](211-keyframe-graph-editor.md) | High | Visual bezier easing edit on the timeline (model already has `Interp` / `EasePreset`) | L | none if UI-only | none |
| **212** | [Keymap schema migrations](212-keymap-schema-migrations.md) | Medium | Ship new default shortcuts without stranding existing users | S | **none** (prefs only) | none |
| **213** | [Social-first editing velocity](213-social-first-editing-velocity.md) | Medium | AS-1 CapCut-class “2-minute social cut” UX — **implemented** | M | none | done wave 2026-08-03 |
| **214** | [Declarative compositor job boundary](214-declarative-compositor-job-boundary.md) | Low–Med | Optional clean GUI→engine “frame job” descriptor if seams thicken | L | none if internal | only if a concrete seam pain appears |

---

## 5. Relationship to existing backlog

Harvest items **do not** invent parallel K/E/X IDs unless they expand an open
item. Prefer attaching to owners:

| Harvest | Existing owner |
|---|---|
| 208 SDF feather | [198 K-B9](198-k-b9-rotoscoping-spline-masks.md) §6–7 (feather was Gaussian); [197 K-B8](197-k-b8-nested-subgraph-masking.md); [30](../specs/video-editor/30-effect-catalogue.md) `util.outline` |
| 209 blur quality | [30](../specs/video-editor/30-effect-catalogue.md); shipped `EffectKind::Blur` / Glow |
| 210 velocity pack | [04](../specs/video-editor/04-ui-mode-timeline.md), [09](../specs/video-editor/09-audio-mixer.md), [15](../specs/video-editor/15-thumbnails-waveforms.md), [35](../specs/video-editor/35-model-decisions.md) markers, K-A4 (done), K-A10 (done) |
| 211 graph editor | [01 §6](../specs/video-editor/01-data-model.md) anim; K-B12 easing presets (done); [13](../specs/video-editor/13-ux-components.md) |
| 212 keymap migrations | [69](69-customizable-keyboard-shortcuts-searchab.md) (MVP shipped) |
| 213 social velocity | [00](../specs/video-editor/00-overview.md) AS-1; [29](../specs/video-editor/29-qa-spec.md) |
| 214 job boundary | [02](../specs/video-editor/02-engine.md), [32](../specs/video-editor/32-engine-contracts.md) — **optional refactor**, not a feature |

---

## 6. Suggested delivery order

1. **209** large-radius blur — small, pure engine, unlocks quality for 208 fallbacks  
2. **212** keymap migrations — prefs only, unblocks shipping new video defaults  
3. **210** timeline velocity — high user-visible payoff, reuses shipped model  
4. **208** SDF feather — feed into K-B9/K-B8 acceptance as **option B** for feather  
5. **211** keyframe graph editor — larger GUI surface  
6. **213** social-first checklist — continuous UX polish against AS-1  
7. **214** job boundary — only if compile/eval seams force it  

---

## 7. Acceptance for *this* index

This index is “done” when:

- [x] Every harvested idea has a child proposal with problem, Photonic current
  state, proposed contract, non-goals, and tests  
- [x] ROADMAP §0 links this harvest (OpenCut harvest table under Band-5)  
- [x] [00-overview](../specs/video-editor/00-overview.md) document map points here  
- [x] No child proposal claims code authorization without its own Accepted status
  — reconciled 2026-08-06. Wave-1 landed code while 208–212 still read "Proposed
  — pre-code"; each status header now records what actually shipped. 209/211/212
  are Accepted+Implemented, 210 and 208 Accepted+partial with their residuals
  named. 208's acceptance is **scoped to the standalone render path** and
  explicitly does *not* authorize the K-B9/K-B8 mask integration, which stays
  Band-5 gated. 214 remains deferred and unimplemented.  

---

## 8. Sources (research only — not implementation inputs)

- OpenCut main README: rewrite status, MIT, ambitions  
- opencut-classic README: web NLE + `rust/` compositor/effects/masks/time  
- opencut-classic docs: `effects-renderer.md`, `keyframes.md`, `actions.md`  
- opencut-classic `rust/crates/{masks,effects,compositor,time}` — **read for
  technique inventory only; reimplement from child proposals**  

Do not cite OpenCut line numbers in implementation PRs. Cite Photonic owner
docs and these proposals.
