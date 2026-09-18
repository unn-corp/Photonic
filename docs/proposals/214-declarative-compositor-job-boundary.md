# 214 — Declarative Compositor Job Boundary (optional architecture)

> **Status: Proposed — optional refactor, deferred by default.**  
> Some CapCut-class stacks (OpenCut classic) build a **FrameDescriptor** in the
> UI layer and hand a pure GPU job to a Rust compositor. Photonic already has a
> stronger boundary: **timeline → frame-graph IR → eval**. This proposal records
> when a *descriptor-like* job object is worth adding, and when it is not.
> Clean-room under [207](207-opencut-harvest-index.md) §2.  
> **Default recommendation: do not build until a concrete seam pain appears.**

**Owner refs:**  
- [02](../specs/video-editor/02-engine.md) engine / compile / eval  
- [32](../specs/video-editor/32-engine-contracts.md)  
- [00](../specs/video-editor/00-overview.md) layering rules  

**Territory:** `photonic-video` (+ maybe GUI presenter). **Effort:** L if done.  
**Format impact:** none (runtime only).

---

## 1. What OpenCut-style descriptors solve (for them)

In a browser TS + WASM split:

- TypeScript owns timeline, effects registry, animation resolve.  
- WASM owns textures, pipelines, pass execution.  
- A serialisable **frame job** (layers, quads, mask refs, effect pass list)
  is the ABI across that language boundary.

Photonic is **not** in that split today: one process, Rust GUI, Rust engine,
shared document.

---

## 2. Photonic’s existing boundary (keep it)

Normative layering (00 §3):

```text
GUI / MCP  →  pure timeline ops (core)  →  EngineCmd
Engine     →  compile(sequence, tick) → FrameGraph IR
           →  eval(IR) on CPU/GPU
```

The IR **is** the declarative job. A second descriptor that duplicates IR
semantics would drift.

---

## 3. When 214 becomes justified

Build a job object **only if** one of these pains is measured:

| Pain | Symptom |
|---|---|
| P1 | GUI builds ad-hoc GPU draws that bypass compile/eval (parity bugs) |
| P2 | MCP/headless/export/preview take four different paths to “show this frame” |
| P3 | Need to snapshot a frame request for crash repro / golden harness without cloning full `Document` |
| P4 | Future WASM/embed (`photonic-embed`) needs a stable ABI |

If none apply, **status stays deferred**.

---

## 4. Proposed shape (if triggered)

```rust
/// Pure, serialisable request to produce one program frame.
/// Does not own GPU resources.
pub struct FrameJob {
    pub sequence_id: SequenceId,
    pub at: Tick,
    pub canvas: CanvasSize,          // logical
    pub quality: PreviewQuality,     // Draft | Full
    pub scope_tap: Option<ScopeTap>, // K-E2
    pub compare: Option<CompareMode>,// K-B5
    pub present: PresentChannel,     // Color | Alpha (K-B17)
}
```

Rules:

1. `FrameJob` is **inputs only** — no textures, no pipelines.  
2. Execution is always `compile + eval` (or a cached compiled graph hit).  
3. Golden tests may serde `FrameJob` + content hash of output.  
4. GUI presenter and export loop both construct `FrameJob` — no third path.

This is deliberately **thinner** than OpenCut’s layer list: Photonic layers
live in the timeline/IR, not re-described by the GUI.

---

## 5. Non-goals

- Moving effect registries to TypeScript.  
- Replacing FrameGraph with a layer stack compositor.  
- Browser export.  
- Plugin ABI in this proposal (separate product decision).

---

## 6. Acceptance (only if built)

| ID | Case |
|---|---|
| T1 | Preview, export frame extract, and MCP `get_program_frame` (or equivalent) all take `FrameJob` |
| T2 | Serde round-trip of `FrameJob` |
| T3 | No GUI code uploads video textures except through engine eval output |

---

## 7. Recommendation

**Recorded decision (draft):** Defer. Revisit when P1–P4 is evidenced in
ROADMAP findings or a refactor proposal. Harvest value is **architectural
awareness**, not a near-term feature.

---

## 8. Provenance

Cross-language GPU job descriptors are a common pattern (game engine command
buffers, browser WASM compositors including OpenCut classic). Photonic’s IR
already occupies that niche in-process.
