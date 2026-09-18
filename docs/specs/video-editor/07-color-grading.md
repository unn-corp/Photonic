# 07 — Color Grading

**Depends on:** 01-data-model.md, 02-engine.md, 03-render-color-pipeline.md (color-space authority — its §4.2 boundary table governs every transfer-function placement; working space: linear-light Rec.709, premultiplied alpha, `Rgba16Float`, D-09), 04-ui-mode-timeline.md (§4.1 panel map). **Capability:** CAP-015. **Location:** data model in `crates/photonic-core/src/timeline/grade.rs` (new); IR/eval in `crates/photonic-video/src/graph/ops/grade.rs` (new); UI in `photonic-gui` color page (04 §4.1). Scope per 00 §5: grade data model detail, grade operators as IR ops, wheels/curves/HSL/LUT UI, scopes.

---

## 1. Grade data model

`Clip.grade: Option<Grade>` (`01-data-model.md:154`) holds an ordered corrector stack — the Resolve node-page mental model, not a flat filter list.

```rust
id_newtype!(GradeOpId);   // stable per-op identity; survives reorder, copy/paste

pub struct Grade {
    pub ops: Vec<GradeOp>,     // ordered; user-reorderable via drag or ReorderGradeOps cmd
    pub bypass: bool,          // global grade bypass (color-page toggle); false = active
}

pub struct GradeOp {
    pub id: GradeOpId,
    pub enabled: bool,                       // per-op bypass
    pub kind: GradeOpKind,                   // discriminant, immutable after creation
    pub params: AnimProps<GradeOpParams>,    // 01 §6 convention: base + PropertyTrack keyframes
    pub mask: Option<GradeMask>,             // §4; None = full frame
}

pub enum GradeOpKind { Exposure, Contrast, WhiteBalance, Cdl, Wheels, Curves, HslQualifier, Lut3d }

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GradeOpParams {
    Exposure     { stops: f32 },
    Contrast     { pivot: f32, amount: f32 },                 // amount -1..1
    WhiteBalance { temp: f32, tint: f32 },                     // both -1..1
    Cdl          { slope: [f32;3], offset: [f32;3], power: [f32;3], sat: f32 },
    Wheels       { lift: [f32;3], gamma: [f32;3], gain: [f32;3], sat: f32 },   // compiled to Cdl at eval (§3)
    Curves       { master: Vec<(f32,f32)>, red: Vec<(f32,f32)>, green: Vec<(f32,f32)>,
                   blue: Vec<(f32,f32)>, hue_vs_hue: Vec<(f32,f32)>, hue_vs_sat: Vec<(f32,f32)> },
    HslQualifier { hue: [f32;2], sat: [f32;2], lum: [f32;2], softness: f32, correction: CdlParams },
    Lut3d        { asset: AssetId, intensity: f32, interp: LutInterp },        // interp: Trilinear | Tetrahedral
    #[serde(other)]
    Unknown,      // forward-compat: newer binary's op kind loads inert + flagged, never dropped — mirrors 01 §6.2's orphaned-PropPath handling
}

pub struct CdlParams { pub slope: [f32;3], pub offset: [f32;3], pub power: [f32;3], pub sat: f32 }
```

Notes:
- Every field is a plain scalar/array inside `GradeOpParams`; `AnimProps<GradeOpParams>` supplies animation exactly like `ClipEffect.params` (`01-data-model.md:222`). `PropPath` entries (`"params.stops"`, `"params.slope[0]"`, `"params.lift[1]"`, …) register per-kind in `prop_registry.rs` (01 §6.2) — one new registry block per `GradeOpKind`, mirroring the existing effect pattern.
- **Versioning within Grade:** no separate schema-version field. Forward-compat is carried by `#[serde(other)] Unknown` on `GradeOpParams` (older binary opening a file with a newer op kind keeps the op in the stack, inert, flagged in UI as "unsupported op — update Photonic") — same non-destructive philosophy as unknown `PropPath`s. `GradeOpKind` is `#[non_exhaustive]`-style additive only; never remove/renumber a variant across releases.
- `Wheels` is kept as its own variant (not silently converted to `Cdl` on save) so the UI round-trips lift/gamma/gain sliders without lossy reverse-engineering from slope/offset/power. Compilation to CDL-equivalent happens at IR-resolve time only (§3), never mutates the stored `GradeOpParams`.
- `HslQualifier.correction` is a plain inline `CdlParams`, not a boxed recursive `GradeOpParams` — a secondary corrector is CDL-only in v1 (matches Resolve's qualifier+CDL secondary node; a full nested-op secondary is unnecessary complexity for v1 and would recomplicate `PropPath` addressing).
- `Lut3d.asset` is an `AssetId` into `MediaPool`, not an embedded table. `AssetKind::Lut3d` **exists** in [01 §3](01-data-model.md) (`Video | Audio | Image | VectorDoc | Lut3d`) so `.cube` files get the same referenced-file, relink-by-hash, offline-placeholder handling as every other asset — required by the SPEC "media referenced, never embedded" constraint (`SPEC.md:90`), which LUT files fall under.
- Copy/paste grade between clips clones the `Grade` value as-is; `GradeOpId`s are **not** regenerated on paste. Scope is per-`Grade`-instance (keyframe tracks live inside that clip's `AnimProps`), so no cross-clip collision is possible — regenerating ids would only add churn.
- Grade presets/stills gallery: app-level persistence, not document state (`01-data-model.md:327`, "render/export presets → app-level config"). A preset is a serialized `Grade` snippet in a user library dir; applying one issues `SetGrade{old,new}` (`01-data-model.md:311`) like any other edit — undoable, MCP-callable (CAP-019).

## 2. Node-chain semantics & compile position

Grade is a step in the per-clip default chain, **not** a separate graph stage (02 §2, compile step 2): `source → speed/trim → Transform2D → Effect[] (ordered, enabled) → Grade (if set)`. The `Grade` IR op (02 §2) carries `Vec<ResolvedGradeOp>` — the keyframe-resolved sibling of §1's authoring `GradeOp`, with all `AnimProps` evaluated at compile time (02 §2, "the evaluator is time-ignorant") and `GradeMask` power-window params (§4) likewise resolved to fixed values for that tick. Two distinct type names (`GradeOp` authoring / `ResolvedGradeOp` IR) — no module-path disambiguation traps.

**Per-clip composition interaction (D-06):** a composition substitutes only the clip's **source op** (02 §2 step 3, source-substitution model); the clip's `Transform2D`, effects, and **grade all still apply on top of the composition's output** — the Resolve Fusion→Edit→Color model. `clip.grade` is therefore always live; the Color page never disables itself because a composition exists. Grading can *additionally* happen inside a composition via a `GraphOp::Grade` node (01 §8 catalog) when a user wants a correction wired mid-graph (e.g., matching a keyed foreground before the merge) — the clip-level grade then acts as the final per-clip pass, exactly as Resolve's Color page follows its Fusion page.

**Project-level "look":** the project graph (`TimelineProject.project_graph`, 01 §2) splices after the *sequence's* fold+output (02 §2 step 6) — a `GraphOp::Grade` or `GraphOp::Lut` node there is the correct place for a global look/LUT applied after every clip's own grade.

**Adjustment-clip grade (01 §5 `ClipSource::Adjustment`, `01-data-model.md:167`):** at fold step (02 §2 step 4), an Adjustment-source clip contributes no pixels of its own. Its effects+grade chain is instead inserted as a post-processing wrapper around the `Merge` accumulation of the video-track stack **beneath** it, up to the next Adjustment clip or the stack bottom — matching Premiere/Resolve adjustment-layer semantics. Concretely: fold bottom→top normally; on hitting an Adjustment clip covering `t`, wrap the accumulated result so far through that clip's `Effect[]` then `Grade` IR nodes before continuing the fold upward. This lets one Adjustment clip shot-group-grade everything below it without touching individual clips' own `Grade`s.

## 3. GradeOp math (linear working space, D-09)

All ops operate on premultiplied linear-Rec.709 `Rgba16Float` pixels unless stated otherwise. Luma weighting is **Rec.709** (`L = 0.2126R + 0.7152G + 0.0722B`) — deliberately *not* the Rec.601 weights in `crates/photonic-core/src/raster/image.rs:190-192` (`0.299/0.587/0.114`), which are correct for that module's legacy sRGB-gamma still-image path but wrong for linear-light Rec.709 video. Do not share that `luma()` function between raster stills and the grade pipeline.

**Design decision — wheel/CDL "feel" (open in 00 §3, resolved here):**
- Option A: run CDL/Wheels math directly on scene-linear values. Simplest, zero extra transform, matches D-09 with no roundtrip.
- Option B: encode to a fixed video-referred gamma curve before CDL/Wheels/Contrast math, decode back to linear after. Matches how ASC CDL and Lift/Gamma/Gain were designed to be used historically (correcting encoded dailies/tape, not scene-linear light) and gives the perceptually-even midtone spread users expect from Resolve-style wheels; pure-linear power curves crush/blow out asymmetrically and feel wrong to colorists.
- **Recommendation: Option B.** Reuse the exact, already-implemented, already-tested sRGB transfer pair at `crates/photonic-core/src/raster/adjust.rs:70-90` (`srgb_to_linear`/`linear_to_srgb`) as the encode/decode pair around CDL/Wheels/Contrast only — not around Exposure or WhiteBalance (see below). This buys CPU/GPU parity for free (GPU shader implements the identical piecewise formula) and avoids introducing a second gamma curve into the codebase. Note this is an **internal detail of the grade op**, not a pipeline boundary: 03 §4.2's boundary table is unaffected — pixels enter and leave the `Grade` IR op in linear working space; the enc/dec roundtrip lives entirely inside the op's shader, consistent with 03's rule that sRGB owns the asset/display domain.

Per-op formulas below: `in`/`out` are per-channel 0..1 unless noted; `enc()`/`dec()` = the Option-B pair above.

### 3.1 Exposure

Runs in true scene-linear, no encode roundtrip — a stop is a physical light multiply, correct regardless of the wheel-feel debate:

```
out = in_lin * 2^stops
```

Identical model to `crates/photonic-core/src/raster/adjust.rs:371-385`.

### 3.2 Contrast

Runs in encoded space:

```
e = enc(in)
slope = 1 / (1 - amount)   if amount >= 0
slope = 1 + amount         if amount <  0
out = dec(clamp01((e - pivot) * slope + pivot))
```

Identical slope formula to `brightness_contrast` at `adjust.rs:183-189`. Default `pivot = 0.5`.

### 3.3 WhiteBalance

Runs in scene-linear (a physical gain, like Exposure):

```
gain = [1 + k*temp, 1 - 0.5*k*tint, 1 - k*temp]
out  = in_lin * gain
k    = 0.4
```

`k = 0.4` is tuned so the UI's ±1 range spans a "quick balance" feel, not a physically modeled CCT/Planckian-locus shift.

**Recommendation:** ship this simplified additive-axis model for v1. Flag full correlated-color-temperature (Von Kries chromatic adaptation) white balance as a post-v1 enhancement — SPEC has no non-goal blocking it, but OpenColorIO is out of v1 (immature Rust bindings; ACES-style looks can be baked to `.cube` LUTs offline instead) and a from-scratch CCT model is unjustified scope for v1.

### 3.4 CDL (ASC CDL slope/offset/power/sat)

```
e = enc(in)
corr_c = clamp_or_gamut((e_c * slope_c + offset_c) ^ power_c)   // per channel c
L      = luma709(corr)
out_c  = dec(clamp01(L + sat * (corr_c - L)))
```

Saturation is applied **after** slope/offset/power, on the corrected value's own luma (ASC CDL v1.2 convention, not the pre-correction luma).

### 3.5 Wheels (Lift/Gamma/Gain)

Compiled to CDL-equivalent at IR-resolve time via the standard LGG↔CDL identity, then evaluated exactly as §3.4:

```
slope = gain - lift     // per channel
offset = lift
power  = 1 / gamma
sat    = sat            // passes through unchanged
```

This is why `Wheels` params stay distinct in the data model (§1) — the UI never sees derived slope/offset/power, only its own lift/gamma/gain, and round-trips losslessly.

### 3.6 Curves

`master` then per-channel (`red`/`green`/`blue`) LUTs built via the identical Fritsch-Carlson monotone-cubic spline at `adjust.rs:236-335` — 256-entry LUT, reused verbatim (GPU shader samples the same 256-entry 1D texture with linear filtering for smooth in-between values). Composed exactly like `curves()` at `adjust.rs:344-364` (composite first, then per-channel).

If `hue_vs_hue`/`hue_vs_sat` are non-empty, run a second pass in HSL:
1. Convert `out` RGB→HSL (`rgb_to_hsl`, `adjust.rs:117-138`).
2. Sample the curve at the pixel's own hue (x-axis 0..360° normalized 0..1).
3. `hue_vs_hue` output is a hue-delta (y-axis 0.5 = no shift, ±180° mapped to 0..1); `hue_vs_sat` output is a saturation multiplier (y-axis 0..2 mapped to 0..1, 0.5 = ×1).
4. Convert HSL→RGB back (`hsl_to_rgb`, `adjust.rs:141-158`).

**v1 curve set recommendation:** master/RGB + hue-vs-hue + hue-vs-sat only. These two cover the highest-value global corrections (skin-tone hue nudges, selective hue desaturation) without the full custom-curve UI surface (lum-vs-sat, sat-vs-sat). Defer those post-v1 — the `Curves` variant's fields are additive, so adding them later is non-breaking.

### 3.7 HslQualifier

Three independent range gates, each a `smoothstep`-based soft-edge falloff reusing `adjust.rs:34-44`'s `smoothstep` and the tonal-weight-gate pattern at `adjust.rs:414-421`:

```
hue_gate = gate(pixel.hue, op.hue, softness)
sat_gate = gate(pixel.sat, op.sat, softness)
lum_gate = gate(pixel.lum, op.lum, softness)
w = hue_gate * sat_gate * lum_gate
out = lerp(in, cdl(in, correction), w * mask_weight)   // mask_weight from §4
```

Same masked-blend convention as `blend_result`/`map_point` at `adjust.rs:162-167`.

### 3.8 Lut3d

Sample the parsed `.cube` 3D table at `(r,g,b)` in `enc()`-space — LUTs are authored against encoded footage in every NLE/colorist tool, never scene-linear:

```
out = dec(lerp(in_enc, lut_sample(enc(in), interp), intensity))
```

`interp: Trilinear | Tetrahedral` (both specified: trilinear is the shipped baseline, tetrahedral behind a quality toggle — §6.5). `.cube` parsing: hand-rolled ~50-line parser, no crate needed — format is a `LUT_3D_SIZE N` header plus `N³` whitespace-separated float triples, with optional `LUT_1D_SIZE`/`DOMAIN_MIN`/`DOMAIN_MAX`. Lives in `photonic-video/src/graph/ops/grade.rs` — I/O-adjacent, engine-owned per 02's layering rule, not core.

## 4. HSL qualifier + power-window masks

```rust
pub enum GradeMask {
    PowerWindow { shape: WindowShape, center: [f32;2], size: [f32;2], rotation: f32, softness: f32, invert: bool },
    RotoMatte   { source: MaskRef, invert: bool },   // stretch — see below
}
pub enum WindowShape { Ellipse, Rectangle }
```

### 4.1 Power window (v1, recommended included)

Normalized sequence coords, `(x,y)` rotated into window-local space `(x',y')` first.

Ellipse test:
```
d = sqrt((x'/sx)^2 + (y'/sy)^2)
weight = 1 - smoothstep(1-softness, 1+softness, d)   // 1 inside, smooth falloff crossing d=1
```

Rectangle test — same falloff, Chebyshev distance instead of Euclidean:
```
d = max(|x'|/sx, |y'|/sy)
weight = 1 - smoothstep(1-softness, 1+softness, d)
```

`invert` flips the weight. Both are per-pixel closed-form, no rasterization pass — cheap, included in v1.

**Tracked** power windows (auto-follow a moving subject via motion analysis) are explicitly post-v1 — no tracker exists yet anywhere in this SPEC. Manual per-frame keyframing of `center`/`size`/`rotation` via `AnimProps` works today and is not blocked by that gap.

### 4.2 RotoMatte (stretch, flagged)

`MaskRef` names a mask-producing source. The natural v1 candidate is `photonic-matte`'s output (`crates/photonic-matte/src/lib.rs`), which produces a `Mask` (`crates/photonic-core/src/raster/mask.rs:12-17` — 8-bit coverage, 0=unmasked/255=fully masked in that crate's convention), sampled as `weight = data[i]/255`.

No roto **shape** tool (bezier tracked matte) exists yet anywhere in this doc set. `RotoMatte` is kept in the enum for forward compat but is **not** required for CAP-015's v1 exit criteria. Ship `PowerWindow` only for v1; `RotoMatte` becomes real once a roto tool exists (post-v1, no phase currently assigned).

### 4.3 Mask combination rules

- `HslQualifier`: the qualifier's own `hue*sat*lum` gate and the op's `GradeMask` (if attached) **multiply** (intersection, not replace) — a qualifier + power window narrows correction to pixels satisfying both the color range and the spatial region.
- Non-qualifier ops (`Cdl`/`Wheels`/`Curves`/`Lut3d`) with a `GradeMask` attached directly: the op's full per-pixel effect is blended by the mask weight alone, same `lerp(in, corrected, weight)` convention as §3.7.

### 4.4 Default op order

Seed order only — `ops: Vec<GradeOp>` stays fully user-reorderable after creation:

```
WhiteBalance -> Exposure -> Contrast -> CDL/Wheels (primary)
  -> HslQualifier x N (secondaries) -> Curves (fine-tune) -> Lut3d (look, last)
```

Mirrors the conventional Resolve node order: primary correction, then secondaries, then a look pass.

## 5. Color page UI

Coexists with the D-02 layout, as recorded in 04 §4.1: the Color page's controls (wheels, curves, qualifier, LUT browser) live in the **right-drawer `ColorControls` group**; **scopes** are a separate floating/dockable panel the user can park beside the program monitor (Resolve's scopes-beside-monitor convention) — never forced into a drawer, they need width for waveform/vectorscope legibility. 04 §4.1 is the panel-map authority; this section owns the interiors.

- **Wheels widget:** three circular lift/gamma/gain dials (Resolve-style: drag within the disc for hue/sat offset, radius = luminance offset; numeric readout beside each). Backed by `GradeOpParams::Wheels`. **Keyboard fallback (13 §16):** each dial focusable; arrows nudge the active axis (Shift = ×10), Tab cycles hue-offset/luminance axes, and every readout is a click-to-type numeric field — the wheel is never the only input path.
- **Curve editor:** draggable Bezier-free spline control points over a live histogram backdrop (reads the pre-curve luma histogram from the scopes compute pass, §5 below), per-channel tabs (RGB / R / G / B / Hue-Hue / Hue-Sat), snap-to-grid toggle. **Keyboard fallback:** Tab cycles control points, arrows move the selected point (Shift = ×10), Insert/Delete add/remove at the playline position.
- **Qualifier picker:** eyedropper sampling a pixel off the program monitor seeds `hue`/`sat`/`lum` center ± a default range/softness — implemented by **extending the existing `EyedropperTarget` enum** (`panels/mod.rs`), not a parallel picking mechanism (one-idiom rule, 13 §16). A "highlight" toggle previews the isolated matte (white = fully qualified, black = excluded) in place of the graded image, standard secondary-correction workflow.
- **LUT browser:** thumbnail grid scanning a configured LUT folder (+ a "recently used" strip); drag-drop onto a clip's grade stack creates a `Lut3d` op; intensity slider default 100%.
- **Scopes panel:** GPU compute-shader histograms with atomic adds into storage buffers, then a visualization pass, reading **03 §3.6's defined readback point: the selected clip's texture after its `Grade` node, before `CaptionOverlay` and the track fold** — graded-but-uncomposited, matching colorist expectation (scopes show the graded shot, not program-with-captions) and matching `get_scopes(clip_id, at)` in 10 §3.10. With no clip selected, scopes fall back to the sequence output pre-`CaptionOverlay`.
  - Waveform: per-x-column intensity histogram (bin by column, accumulate luma or per-channel counts).
  - Vectorscope: Cb/Cr-plane scatter/histogram (convert sampled pixels to YCbCr, bin onto the 2D plane) — standard skin-tone-line overlay as a fixed reference graphic.
  - Histogram: per-bin luma (and optionally per-channel) counts, 256 bins.
  - Refresh policy: every presented frame by default; if the 02 §8 perf budget (8 ms/1080p-3-layer-grade-caption) is exceeded, decimate to 1-in-2 frames — decimation is a fallback triggered by measured frame time, not a fixed default, so scopes stay maximally responsive when headroom exists.
- **Before/after + bypass:** `Grade.bypass` = full-grade toggle (keyboard shortcut, e.g. `D`); per-op `enabled` = individual op bypass; a monitor-level before/after split or full A/B toggle compares graded vs ungraded — standard NLE convention.
- **Working colour state is explicit per sequence (D-09).** `LinearRec709Sdr` is the default; `LinearRec2020Hdr` is opt-in for approved HLG/PQ workflows. The former "SDR-only" non-goal was **repealed** by the S3 amendment ([23 §S3](23-legal-open-source-implementation-routes.md#s3--d-13-hdr-and-10-bit)), which scopes 10-bit HLG/PQ decode, HDR scopes and deterministic SDR tone mapping **in**. HDR substitutions for the operators below are owned by [22 §7](22-dji-advanced-workflows.md#7-d-13--hdrhlg-10-bit-color-pipeline).

## 6. Risks & test hooks (feeds 11-testing-phasing.md)

### 6.1 GPU/CPU parity drift (golden grade renders)

**Risk:** the GPU grade eval path drifts from the CPU reference, breaking the SS-3 golden-frame tolerance.

**Mitigation:** extend `eval_cpu` (`02-engine.md:92`) with an f32 CPU implementation of every `GradeOpKind`, sharing the exact formulas in §3 rather than reimplementing them — reuse the raster `adjust.rs` code directly where the formulas are identical (Contrast slope at `adjust.rs:183-189`, Curves LUT at `adjust.rs:236-335`, HSL conversions at `adjust.rs:117-158`).

**Test hook:** golden corpus of known input ramps (0-255 horizontal gradient, primary/secondary color swatches) run through each `GradeOpKind` at fixed params; GPU-vs-CPU pixel diff gated under SS-3's tolerance.

### 6.2 CDL interchange round-trip (.cdl / .ccc)

**Risk:** CDL import/export breaks compatibility with 3rd-party grading tools (Resolve, Baselight, other ASC CDL producers).

**Mitigation:** spec a small pure-function XML parser/writer for ASC CDL XML:
```
<ColorCorrection>
  <SOPNode><Slope/><Offset/><Power/></SOPNode>
  <SatNode><Saturation/></SatNode>
</ColorCorrection>
```
`.ccc` is a `<ColorCorrectionCollection>` of multiple `.cdl` entries keyed by `id`. Lives in `photonic-core::timeline::grade` as:
```rust
pub fn parse_cdl_xml(s: &str) -> Result<Vec<(String, CdlParams)>, CdlXmlError>;
pub fn write_cdl_xml(entries: &[(String, CdlParams)]) -> String;
```
Pure string in/out, satisfies core's "no I/O" rule (`01-data-model.md:3`) — file read/write is the caller's (GUI/MCP) job.

**Test hook:** round-trip a hand-authored `.cdl` fixture through parse→write→parse, assert value equality within float epsilon.

### 6.3 Scope accuracy (waveform / vectorscope / histogram)

**Risk:** compute-shader scope math doesn't match the signal it claims to visualize — silently wrong scopes are worse than no scopes, since colorists trust them over their eyes.

**Mitigation/test hook:** fixture ramps with closed-form expected scope output — a 0..255 horizontal gradient must produce a diagonal waveform line; a fixed SMPTE-bar-derived patch set must produce known vectorscope cluster positions at known Cb/Cr coordinates. Compute-shader output compared to expectation within bucket-rounding tolerance, same golden-fixture philosophy as the rest of 11's corpus.

### 6.4 Atomic-histogram compute perf floor

**Risk:** atomic-add histogram compute shaders (waveform/vectorscope/histogram, §5) are slow on lower-end/integrated GPUs, blowing the 02 §8 8 ms/1080p-3-layer-grade-caption budget.

**Mitigation:** bin-count fallback (256→128 histogram bins) behind a capability/perf check, measured against the 02 §8 budget in 11.

### 6.5 LUT tetrahedral interpolation complexity

**Risk:** tetrahedral 3D-LUT interpolation (more accurate than trilinear at LUT-grid edges, but a materially more complex shader) ships with subtle bugs.

**Mitigation:** ship trilinear first — simple, ubiquitous, correct baseline. Gate tetrahedral behind a quality toggle. Both paths tested against the same `.cube` fixture with known interpolated values at off-grid sample points.

### 6.6 Adjustment-clip / composition interaction ambiguity

**Risk:** an Adjustment clip (§2 re-rooting) that also has `composition` set has no meaningful "source" for the composition to substitute (02 §2 step 3's source-substitution model) — allowing it invites undefined compile behavior.

**Mitigation:** explicit rule: setting `composition` on a `ClipSource::Adjustment` clip is **rejected at edit time** (`EditError`, same discipline as graph cycle refusal) — an Adjustment clip's contribution is its effects+grade wrap of the stack beneath it, which has no source op to substitute. Users who want graph-based processing of the stack below use the project graph or a nested sequence.

**Test hook:** attempt to set a composition on an Adjustment clip via `timeline/ops.rs` and via MCP `create_clip_composition`; assert both return the edit error and the document is unchanged.

### 6.7 HslQualifier secondary-correction scope creep

**Risk:** a future change boxes `GradeOpParams` recursively into `HslQualifier.correction`, reintroducing unbounded nesting complexity this doc explicitly rejected (§1).

**Mitigation:** data model hard-restricts `correction` to inline `CdlParams` (§1) — not a runtime test, but a golden constraint: reject/flag any future PR that widens `HslQualifier.correction`'s type without a corresponding SPEC decision update.
