# 208 — Signed-Distance / Jump-Flood Mask Feather

> **Status: Accepted for the render-only SDF path — partially implemented.**  
> Shipped (wave-1, 2026-08-03): CPU `ops::feather_coverage` — a Jump-Flood
> distance field plus a smoothstep band — with unit tests. It is **not yet
> referenced by any `IrOp`**, so it does not run in either evaluator.  
> Shipped 2026-08-06: **`util.outline` moved onto the SDF path**, which is what
> [30 §5](../specs/video-editor/30-effect-catalogue.md) requires ("signed
> distance fields, not the reference's rasterised dilation"). It bridges through
> K-B16's `raster_bridge` (lowering as `EffectKind::Unknown("util.outline")`, so
> it was never inert) but had shipped on a brute-force `(2t+1)²` box search —
> the very dilation 30 rejects. See §8 for the three defects that closed.  
> Shipped: **GPU twin for `util.outline`** — WGSL solid band + Hermite AA
> matching the CPU oracle (`cpu_gpu_parity::util_outline_sdf_cpu_gpu_parity`).
> Exterior Euclidean distance to the α≥0.5 isosurface (same band profile as CPU
> JFA for the outline stroke).  
> **Still gated:** *replacing* K-B9 / K-B8 Gaussian feather with this path — §4.4
> lists those as optional integrations, and both owners remain Band-5
> (198 additionally behind the 23 §11.1 patent review). Acceptance here covers
> the standalone render path only; it does not authorize roto/mask work.  
> Harvested as a *capability idea* from CapCut-class open editors (OpenCut
> classic exposes a JFA-based SDF path for soft masks). Implementation must be
> Photonic-native under [207](207-opencut-harvest-index.md) §2.

**Owner refs:**  
- [198 K-B9](198-k-b9-rotoscoping-spline-masks.md) — currently specifies feather
  as separable Gaussian on coverage  
- [197 K-B8](197-k-b8-nested-subgraph-masking.md) — mask branch blur for feather  
- [30 §5](../specs/video-editor/30-effect-catalogue.md) — `util.outline` **already
  requires SDF**, not raster dilation  
- [07 §4](../specs/video-editor/07-color-grading.md) — power-window / matte masks  

**Territory:** `photonic-video` graph eval (CPU + GPU twins) + optional
`photonic-render` grade mask path. **Effort:** M. **Format impact:** none
(render-only; no new document fields).

---

## 1. Problem and user outcome

**Today.** Soft mask edges in Photonic are (or will be, per 198/197) produced by
**blurring a hard coverage matte**. That is correct and simple, but:

1. Large feather radii need multi-iteration or huge kernels — quality and cost
   degrade (see [209](209-large-radius-blur-quality.md)).  
2. `util.outline` is specified as **SDF-based** (30 §5) and still needs a real
   distance field path.  
3. Analytic antialiasing at hard angles (outline, rounded crop) is natural in
   SDF space and awkward as a post-blur of a binary matte.

**After 208.** Photonic can produce a **signed distance field** from a binary
or alpha coverage texture and derive:

- soft feather by thresholding the distance with a smoothstep of width
  `feather_px`, and  
- stroke/outline by a band around the zero isosurface,

with CPU/GPU parity and content-hash caching like every other IR op.

**User-visible outcome:** large feather radii on roto / power windows stay
smooth and cheap; outline effects look correct at corners.

---

## 2. Current state in Photonic

| Surface | State |
|---|---|
| `EffectKind::Blur` + GPU dual-pass Gaussian | **Shipped** (K-0.2) |
| `Effect{MaskShapeGen}` hard ellipse matte | **Shipped** both evaluators |
| Power-window grade masks | **Shipped** (ellipse/rect); feather via grade path |
| K-B9 roto feather | Spec’d as **Gaussian** (198 §7.2) — not SDF |
| K-B8 mask feather | Spec’d as **Blur in mask branch** (197) |
| `util.outline` | Catalogue says **SDF**; no IR op yet |
| Jump-flood / SDF pipeline in crates | **None** (`grep` clean for JFA/jump-flood in `crates/`) |

---

## 3. Proposed technique (public literature)

**Jump Flooding Algorithm (JFA)** for approximate Euclidean distance transforms
on the GPU — Rong & Tan, *Jump Flooding in GPU with Applications to Voronoi
Diagram and Distance Transform* (I3D 2006 and follow-ons). Widely implemented
in game engines and tools for soft shadows, outlines, and SDF generation.

**Pipeline (normative sketch):**

1. **Init.** From coverage `C ∈ [0,1]` (or binary matte), seed “seed site”
   coordinates for inside (and optionally outside) regions.  
2. **Jump steps.** `⌈log₂ max(w,h)⌉` passes with step sizes
   `2^{k} … 1`, each pixel adopting the nearer seed from its neighbourhood.  
3. **Distance.** From stored seed coords, write signed distance in pixels
   (inside negative / outside positive — pick one convention and pin it in
   tests).  
4. **Feather.**  
   `alpha = smoothstep(-f, +f, d)` (or one-sided for outer-only feather).  
5. **Outline.**  
   `alpha = smoothstep(t−w, t, |d|) * (1 − smoothstep(t, t+w, |d|))` for
   thickness `t` and AA width `w`.

**Why not only Gaussian.** Gaussian feather is still **allowed** as the
default for K-B9 v1 (198 already locked it). 208 is **Option B**: preferred
when `feather_px` exceeds a cost threshold, or always for `util.outline`.

---

## 4. Data / IR contract

### 4.1 New IR ops (names indicative)

```text
IrOp::CoverageSdf {
    /// Convention: negative inside solid coverage, positive outside.
    /// Documented in module docs + golden fixtures.
}
IrOp::FeatherFromSdf {
    feather_px: f32,   // half-width of soft band in logical pixels
    invert: bool,
}
IrOp::OutlineFromSdf {
    thickness_px: f32,
    aa_px: f32,        // default 1.0
    color: [f32; 4],   // straight or premul per grade/operand space rules
}
```

Alternatively a single `IrOp::SdfProcess { mode: Feather | Outline, … }` —
implementation choice; tests pin behaviour, not enum shape.

### 4.2 Threading / source range

- **Source range:** identity (same as input coverage frame) — declare in
  `source_range_for_op` (E-1 wildcard-free match).  
- **Threading:** `PerInstance` or `Serial` — pick during impl; must not be
  undeclared (E-4).  
- **Content hash:** includes mode, feather/thickness, invert, input hash.

### 4.3 Working colour / alpha

- SDF and feather run on **coverage / alpha**, not on colour RGB.  
- Colour is multiplied after feather for mattes; outline composites per
  existing Merge / overlay rules.  
- Operand space: follow [03](../specs/video-editor/03-render-color-pipeline.md)
  for any colour write; coverage is linear scalar.

### 4.4 Integration points

| Consumer | How |
|---|---|
| K-B9 roto | Optional: `params.feather` → `FeatherFromSdf` when SDF path enabled; else keep Gaussian |
| K-B8 mask branch | Optional replace terminal Blur with SDF feather |
| Power windows | Optional; static shapes can also use analytic SDF (ellipse/rect) **without** JFA — preferred for primitives |
| `util.outline` | **Must** use SDF path (30 already requires it) |

**Analytic SDF for primitives** (ellipse, rect, rounded rect) should be
preferred over JFA when the shape is parametric — JFA is for **rasterised
arbitrary coverage** (roto, painted mattes).

---

## 5. Non-goals

- Motion tracking, magnetic edges, intelligent scissors (198 §7.3).  
- True Euclidean EDT exactness — JFA is approximate; goldens use a **tolerance**,
  not bit-identical to a CPU brute-force EDT.  
- 3D / volume SDF.  
- Shipping OpenCut’s WGSL or texture-pool code.

---

## 6. Tests and acceptance

| ID | Case | Pass |
|---|---|---|
| T1 | Hard disk coverage → SDF zero isosurface within 1 px of geometric edge | CPU |
| T2 | Feather_px = 8 → soft ramp; energy conserved in the sense that interior solid stays ~1 and far exterior ~0 | CPU+GPU parity |
| T3 | Outline thickness 4, aa 1 — corner continuity (no boxy dilation) | golden |
| T4 | Content-hash stable under identical inputs; changes when feather changes | unit |
| T5 | `source_range` declared; undeclared op fails compile | unit |
| T6 | Large feather (64 px) completes under [25](../specs/video-editor/25-performance.md) preview budget on Draft 720p fixture | perf gate advisory |

---

## 7. Provenance and legal

| Technique | Origin | Gate |
|---|---|---|
| Jump Flooding distance transform | Rong & Tan (I3D 2006); widespread public CG literature | Foundational; no special patent review beyond standing 23 §11 habit |
| Smoothstep feather of distance | Standard SDF texturing (Valve / GPU Gems era) | Foundational |
| Separable Gaussian (fallback) | Already in Photonic | — |

**Clean-room:** reimplement from this document + public JFA descriptions. Do
not copy OpenCut `masks/src/sdf.rs` or its WGSL.

---

## 8. What landed for `util.outline` (2026-08-06)

`raster_bridge::util_outline` now derives its band from the shared Jump-Flood
field (`ops::coverage_signed_distance`, made `pub(crate)` — it had been reachable
only from `feather_coverage`'s own unit tests). Three defects in the box-search
version closed:

| Defect | Was | Now |
|---|---|---|
| **Cost** | O(W·H·thickness²) — 16,641 samples *per pixel* at the manifest's own 64px max | O(W·H·log max(W,H)), independent of thickness, so 30 §5's "no thickness ceiling" is real rather than aspirational |
| **Boxy corners** | distances quantised to integer offsets, coverage thresholded at `alpha > 0.5` | continuous field; outer rim resolves with a smoothstep of half-width `OUTLINE_AA_PX` (§4.1 `aa_px`, 1.0) |
| **Glow, not stroke** | coverage `1 - d/thickness` — a linear ramp across the whole band, no solid core | solid to `thickness - aa`, antialiased rim centred on `thickness` |

Band convention: the edge is **centred** on `thickness` with half-width `aa`, so
a 4px stroke is solid to 3px and falls off 3→5px. `aa` is a constant, not a
manifest param — `OUTLINE_PARAMS` is part of the frozen catalogue schema and
30 §5 asks for analytic antialiasing, not a user knob.

Tests: `outline_band_is_solid_out_to_thickness`, `outline_band_ends_at_thickness`,
`outline_corner_is_round_not_boxy` (T3 — same Chebyshev offset, different
Euclidean coverage, which is exactly what a box dilation cannot reproduce),
`outline_leaves_the_source_interior_untouched`,
`outline_zero_thickness_or_opacity_is_identity`, `outline_honours_large_thickness`.

## 9. Follow-ups

- **GPU twin for `util.outline`** — **done** (solid-band + Hermite AA WGSL; parity test). Full multi-pass JFA on GPU remains optional if a future consumer needs unbounded exterior distance beyond the outline search radius.  
- **`feather_coverage` still has no consumer.** The CPU op and its tests exist,
  but no `IrOp` reaches it; its distance field is now shared with the outline,
  so only the feather *entry point* is unwired. Its natural consumers are the
  K-B9/K-B8 mask branches, which stay Band-5 gated — hence no IR op invented
  here for a caller that cannot yet exist.  
- Fold Accepted 208 into 198 §6 as “feather implementation choices.”  
- Analytic SDF for `MaskShapeGen` / power windows (cheaper than JFA).  
- 209 quality knobs if Gaussian remains the small-radius path.
