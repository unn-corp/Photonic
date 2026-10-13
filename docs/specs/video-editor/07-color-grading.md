# 07 — Color Grading

**Implementation boundary:** this document describes the Legacy SDR grading
semantics. The approved professional grading roadmap and implementation status
are tracked in [professional-color-grading.md](../../implementation/professional-color-grading.md).
Managed ACES/OCIO color, HDR mastering, reference galleries, grading graphs and
tracking are separate acceptance gates, not capabilities implied by this spec.
The additive `linear_offset` and `highlight_rolloff` correctors extend the
Legacy SDR catalog without changing the original operator math. Highlight
roll-off works in scene-linear light and preserves extended-range RGB and alpha;
it is not a managed HDR output transform.

**Correctness update:** viewing grading controls never creates or normalizes a
stored corrector. Parameter gestures coalesce into one undo step; stack changes
remain discrete; clip and track grade edits respect track locks. An enabled
corrector with an unavailable LUT, unknown operator or unresolved mask is bypassed
as a whole in preview with a structured diagnostic. Final export fails on that
diagnostic unless the dependency is resolved or the corrector is explicitly
disabled. An unresolved mask must never mean full-frame correction. GUI RGB parade
uses per-column channel counts and scopes use asynchronous readback with measured
frame provenance.

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

**2026-10-06 continuation:** the original artistic operator remains unchanged. A separate `white_balance_cct` operator now implements explicit source-to-target approximate Planckian-white Bradford adaptation. See the contract at the end of this specification; this does not add a RAW camera setting or daylight-white model.


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

**Professional grading increment:** `hue_vs_luma`, `luma_vs_sat`, and
`sat_vs_sat` are optional curve vectors, omitted on save when empty. Existing
grades therefore retain their serialized shape and evaluation path. A non-empty
curve uses y=0.5 as neutral: hue-vs-luma adds `y−0.5` equally to straight RGB
after the hue/saturation pass (Rec.709 luma changes by that amount until an SDR
channel clips); luma-vs-sat and sat-vs-sat multiply HSL saturation by `2y`,
sampled from pre-correction Rec.709 luma and HSL saturation respectively. These
operators are qualified for the Legacy SDR grading space only; managed grading
will require its own declared domain semantics.

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

**Tracked** power windows were outside the original v1 baseline. The cloud continuation now implements bounded ordinary-clip ellipse/rectangle center translation as an explicit analysis proposal and separate apply step; see the bounded tracking contract below. Scale, rotation, perspective, Bézier and compound tracking remain unqualified. Manual per-frame keyframing of `center`/`size`/`rotation` via `AnimProps` remains available.

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

## Native qualifier spatial-cleanup extension (2026-10-06)

An HSL qualifier may store optional `GradeOp.qualifier_spatial` with static
`denoise: bool`, `grow: f32`, and `blur: f32`. Grow/shrink is bounded to
`[-0.02, 0.02]`; Gaussian sigma is `[0, 0.02]`. Both are fractions of the shorter
logical frame dimension. Denoise is a 3×3 processing-pixel median; therefore it
is explicitly preview-resolution dependent. These controls are not keyframable.
The Color UI displays distances as percentages. The whole-grade history and
MCP `set_grade` paths preserve the optional field.

For a non-neutral setting, the Native Managed compiler builds the existing
unassociated qualifier key (sampled regions and key levels), intersects its
static window, then applies median → grow/shrink → Gaussian. This deliberately
refines the combined key/window boundary, so growth and blur may extend past
the original window. It mixes the unkeyed CDL correction in ACEScct by this
refined weight before decoding to ACEScg. Original source coverage is retained
once. Transparent source pixels remain transparent. Qualifier inspection uses
the same key/refinement and multiplies source coverage only after refinement;
a typed key output remains unassociated. A separate graph refinement utility,
if present, is an additional explicitly authored stage.

Absent or neutral settings retain the original evaluation path and neutral CDL
still bypasses the image correction exactly. Non-neutral spatial settings
require Native Managed graph lowering; Legacy/scalar resolution reports an
unavailable-correction diagnostic rather than silently ignoring them. Invalid
values or a non-qualifier owner are rejected. This extension does not add motion
tracking, animated window geometry or shared/compound inspection ownership.

Precision boundary: the spatial image-correction path now fuses scene-to-log,
CDL, refined-key mixing and log-to-scene conversion in one f32 shader invocation.
Its source and output still use RGBA16F, while the existing separate key-selection
encoding and spatial-refinement semantics are retained. This avoids the earlier
separate half-float logarithmic image stores, and zero-key/identity pixels return
the stored original exactly. The bounded qualification retains explicit binary16
source/output store references and an ideal CPU reference. Other pipeline paths
can still accumulate half-float rounding; this does not qualify unrestricted HDR
mastering or promise float32 intermediate textures.

## Additive contract — explicit Kelvin white adaptation (2026-10-06)

`white_balance_cct` adds source/target Kelvin values within 1667–25000 K.
It uses an approximate Planckian-locus xy model and Bradford adaptation,
converted to linear Rec.709/D65 for Legacy SDR or scene AP1/ACES-white for
Native Managed. Both defaults are 6500 K; equal values bypass exactly.
The existing normalized artistic temperature/tint operator is unchanged.
6500 K on this locus is not exactly D65 or ACES white. This is not a RAW
camera setting, illuminant estimator, daylight-white or Duv/tint model.

Both fields are registered animatable properties; valid source/target values
are checked before shared grade edits and invalid loaded values diagnose.
CPU/GPU paths preserve alpha and accept signed/extended RGB. Native RGB matrix
IR uses hash tag 55; resolved payload uses tag 11. Matrix coefficients and
hash inputs are bounded/finite. Actual MCP full-grade authoring, exact undo/redo,
JSON reload, Native preview/ProRes and three-frame animated Legacy preview/ProRes
pass. Animation was authored in the full-grade payload, not via a separate
keyframe-tool interaction. MCP schema accepts complete grade objects; no
hardcoded grade-kind catalog prevents this new operator.

Independent numerical evidence includes published xy examples, NumPy matrix
references, source-white mapping, inverse/identity/bounds, and 120 CPU/GPU cases
across both RGB domains, signed/extended samples and four alpha values. CPU
bound is 1e-5*max(1,abs(reference)); GPU bound is 0.003*max(1,abs(reference)).
These are bounded tests, not exhaustive spectral/colorimetric qualification.
Source: https://colour.readthedocs.io/en/latest/generated/colour.temperature.CCT_to_xy_Kang2002.html
Polynomial reference: https://colour.readthedocs.io/en/latest/_modules/colour/temperature/kang2002.html

Current-tree verification: 2,775 library tests passed, 3 ignored; core timeline
43; native graph 45; scaling 6; unchanged Legacy golden 1. Clippy and application
build passed with warnings. Broader roadmap and external HDR qualification
remain incomplete. Work remains local and uncommitted; no push or merge.

## Additive contract — tonal exposure (2026-10-06)

`tonal_exposure` is a new creative primary with `stops: [shadows, midtones,
highlights]`, `boundaries: [shadow_boundary, highlight_boundary]` and `softness`.
Stops are bounded to [-8,8], boundaries to [-12,12] with shadow < highlight,
and softness to [0.01,6]. All fields are finite and animatable. Defaults are
zero stops, boundaries [-2,2], softness 1. Invalid authored state is rejected
by shared scoped grade edits; invalid loaded/evaluated values diagnose and
block qualified export. Neutral stops bypass exactly after parameter validation.

Selection uses straight linear RGB relative luminance Y: Rec.709 coefficients
[0.2126,0.7152,0.0722] for Legacy SDR or AP1 coefficients
[0.27222872,0.67408174,0.053689517] for Native Managed. For Y>0, let
x=log2(Y/0.18), a=smoothstep(low-softness,low+softness,x), and
b=smoothstep(high-softness,high+softness,x). The weights are [1-a,a-b,b].
For Y<=0, weights are [1,0,0]. Ordered boundaries and the common half-width
make these a nonnegative partition of unity, including overlapping transitions.
Multiply all three RGB channels by 2^(dot(weights,stops)); alpha is unchanged.
Existing static windows mix this correction in linear light. All-equal stops
reduce to ordinary exposure; signed values and highlight headroom are retained
until the output/storage limits. GPU Native output is bounded by RGBA16F limits.

This is linked relative-luminance-selected exposure, not independent channel
wheels, spatial shadow restoration, absolute luminance mastering, or a display
tone mapper. Extreme opposing corrections can reverse tonal ordering; no
monotonic-tone-map guarantee is made. Existing operators retain their math and
serialization. Native IR tag 56 and resolved payload tag 12 include every
parameter/domain in cache identity. GUI controls expose stop units; boundary
editing maintains their ordering without modifying values merely on view.

## Native Soft Light extension (2026-10-06)

Native-managed track and graph merges additionally accept `soft_light`.
The existing CPU and shader equation is unchanged. Its bounded-channel
reference is [W3C Compositing and Blending §10.1.10](https://www.w3.org/TR/compositing-1/#blendingsoftlight).
Photonic applies it to straight scene-linear AP1 channels, then performs the
existing premultiplied source-over composition. This is an explicit creative
extension, not a claim of display-referred CSS/Photoshop appearance matching.
The source pivot is 0.5; the backdrop uses a cubic branch through 0.25 and
square root only above that threshold. Negative backdrop values stay on the
cubic branch, so square root never receives them. Negative values may be
amplified substantially; highlights are not clamped to reference white.
Existing RGBA16F storage limits still apply. This is not a bounded HDR tone mapper.
Other unsupported blend modes retain their refusal paths, and Legacy math is
unchanged. No new serialized enum, shader code or cache tag is introduced;
the existing mode/opacity content hash distinguishes each merge.


## Additive contract — primitive window scalar animation (2026-10-06)

A GradeOp's existing params.tracks can address mask.center[0/1], mask.size[0/1],
mask.rotation and mask.softness on an existing PowerWindow. Operator params.*
and mask.* remain distinct paths. Window shape and invert are static. Gradient
uses center, Y falloff and rotation; its ignored width/feather are not keyable.
The local clip surface uses clip-relative timeline ticks, independent of source
retiming; geometry is normalized sequence canvas after clip transform. Sizes
are half-extents and positive; rotation is radians; feather keys and samples
are within 0..1. All sampled values must be finite and fit f32. Invalid animated
geometry diagnoses/bypasses the corrector and blocks qualified export. Static
legacy documents preserve their previous path. Missing/changed mask shapes
retain dormant lanes. Older software may preserve unknown tracks without
evaluating them, so use a runtime with this feature for these projects.

The mask is sampled by the shared resolver for preview, inspection and export;
resolved geometry participates in existing content hashes. GUI animated-window
base values and on-viewer handles are read-only; edit keys through Clip Inspector
/ Keyframes or the same MCP grade_op keyframe tools. This is manual primitive
animation, not optical tracking or Bézier/multiple-window geometry. See the
MCP authoring companion and current roadmap for qualification and remaining gates.

## Additive contract — geometric window matte graph source (2026-10-06)

`GradeGraphNode::WindowMatte {input, op, label}` references an existing corrector's
animated primitive window and emits a typed, unassociated matte. Its image input
supplies the same sequence-canvas geometry as the grade; source RGB and alpha,
color parameters and qualifier cleanup do not gate this key. An enabled operator
without a mask supplies one, a disabled operator supplies zero, and unresolved
roto or invalid sampled geometry diagnoses the source and blocks qualified export.
Explicit MatteRefine nodes supply optional cleanup after the source.

Connect multiple window keys through KeyMixer: union=max(a,b), intersect=min(a,b),
subtract=max(a-b,0), multiply=a*b. Feathered weights remain continuous; these are
soft-mask arithmetic contracts rather than polygon topology editing. MatteApply
mixes the separate corrected image against the original while retaining image
coverage. The referenced operator remains the shared store for mask edits and
manual scalar keys. Deleting an image corrector retains its operator when a window
source still references it; whole-grade copying remaps both kinds of references.

The authoring route is Color Advanced / Add key utility / Window from a masked
corrector, followed by key mixing and matte application. For independent window
owners, use neutral correctors and a separate unmasked correction; leaving a
non-neutral owner on a reachable image branch also executes its ordinary masked
color correction. This does not add Bézier paths, tracking, stabilized coordinates,
or group/shared/compound inspection. Older readers without the new graph variant
are not qualified to load these projects. Existing stack arithmetic is unchanged.

## Additive contract — paired program/output measurement (2026-10-06)

NativeSceneProgram is an exact internal tap immediately before the qualified SDR
output transform, after the sequence's final program stages. The dedicated MCP
measurement reads it and the displayed output from one Full/original frame. The
ordinary SDR scope signal/scale contract is unchanged; this tap must not be fed
through the SDR scopes' implicit OETF or normalized histogram clamp.

`measure_native_output` returns separate straight AP1-working and sRGB-output
statistics with source coverage accounted for once. Positive AP1 Y is binned by
log2(Y/0.18) over 128 half-open bins spanning [-12,12), with explicit nonpositive
and under/overflow counts. Color averages use equal covered-pixel weights; no
covered signal gives null extrema/means. These relative units neither infer a
physical scene from display-referred inputs nor establish calibrated nits/HDR
monitoring. Existing waveform/vectorscope UI and output format gates remain as
previously qualified. See the MCP companion for readback/provenance limits.


### Qualified Native arithmetic blend contract (2026-10-06)

Native Managed track and composition Merge nodes support four explicit unclamped
AP1 arithmetic blend functions on unassociated RGB: Linear Dodge `B + S`, Linear
Burn `B + S - 1`, Subtract `B - S`, and Linear Light `B + 2S - 1`. The constant one
is a scene-working numeric value, not physical white or a calibrated exposure.
These are creative arithmetic operations, not perceptual contrast modes. They
use ordinary source-over compositing and track/clip/graph opacity; alpha is not
added or subtracted by the RGB equation. Transparent inputs cannot introduce
hidden RGB. Final premultiplied working RGB is bounded to finite RGBA16F storage
(-65504 to 65504), not clipped to display range 0..1.

A dedicated NativeArithmeticMerge IR and content-hash tag separate this behavior
from the original bounded Legacy equations. Native graphs reject a Legacy Merge
IR carrying an arithmetic mode; the compiler lowers native authoring to the
explicit operation. Two AP1 inputs and finite opacity in 0..1 are required.
ACESCCT, matte-weight, display-encoded and Legacy inputs are rejected. No saved
blend identifiers or Legacy raster/rendering semantics change. Other unqualified
division, bounded-color and luminance blend modes remain gated.


### Qualified Native whole-color comparison contract (2026-10-06)

Darker Color and Lighter Color select an entire unassociated AP1 RGB triple by
its scene-linear AP1-to-XYZ Y value, then apply ordinary source-over alpha and
opacity. They do not independently minimize/maximize the RGB channels. The Y
weights are approximately [0.27222872, 0.67408174, 0.053689517], consistent with
the AP1 primaries and ACES white. Signed values and highlights remain unclipped
except at finite RGBA16F storage bounds. Exactly equal computed Y values choose
the backdrop. Half-float input quantization may affect selections near a tie;
this hard selection is not a smooth/perceptual color-match operation.

NativeLuminanceMerge has its own IR/cache identity (tag59), validates two AP1
inputs, a qualified whole-color mode and finite opacity in 0..1, and cannot
consume Legacy/log/display/matte inputs. The original Legacy whole-color modes
keep their 0.30/0.59/0.11 luma selector. Stored mode names are unchanged; the
native track dialog identifies the AP1 selection semantics.


### Bounded ordinary-clip window tracking contract

`track_grade_window` analyzes ellipse/rectangle center translation against an
isolated Full/original pre-grade sequence canvas. Required start/end sequence
ticks snap to frames; reverse intervals are supported. Results are proposals,
not edits. `apply_window_track` creates one undo step with clip-local linear
center keys after exact document/revision and source-metadata checks. Existing
center keys require explicit replacement permission; partial confidence-stop
results require explicit acceptance. Outside keys survive, but adjoining
interpolation may change and endpoint values hold outside the tracked interval.

Bounds: 121 samples, 120 seconds, 16,777,216 input pixels, analysis side ≤256,
reference half-patch 4–16, search radius 2–24. The default radius 12, minimum NCC 0.75
and margin 0.05 are conservative starting settings, not footage-independent
accuracy guarantees. Fixed-reference normalized correlation stops on missing
texture/coverage, ambiguity, confidence/cut failure or search-edge motion.
Cancellation is cooperative and releases admission after in-flight work drains.

Native Color Controls exposes “Track window translation...” only for a selected
enabled ellipse/rectangle in an enabled ordered clip grade. Its frame endpoints
are sequence frame numbers. Analyze is read-only, Apply is explicit, and close,
context/revision changes and app destruction cancel/discard stale work. The
analysis session does not seek the user's transport. Scale/rotation/perspective,
Bézier, graph, shared/compound and stabilization-coordinate tracking are not
qualified by this feature. GPU/hardware throughput and long footage remain
separate acceptance work.


### Bounded plain title input — 2026-10-07 qualification

Native Managed plain text clips use the existing glyph rasterizer with sRGB
fill bytes, then explicitly convert premultiplied linear Rec.709/D65 into
AP1/ACES-white before scene grading and output. This is the same input convention
as native solid swatches, not an output-referred graphics plane. Outline,
background, karaoke, caption-style animation, graph Text, vector/logo and caption
stages remain gated until independently implemented. The normal clip transform
and grade stages apply. Font fallback is platform dependent; external font
packaging and CPU glyph parity are not qualified. See the implementation roadmap
for parameter bounds, checked render failures and numerical/native UI evidence.

### Managed whole-document vector boundary — 2026-10-07 qualification

The native timeline can use embedded whole-document vector clips and static
composition/project VectorIn sources containing bounded solid paths and plain
structural groups. Their encoded-sRGB authored paint is decoded before the
vector scene's linear source-over. Its encoded premultiplied readback is decoded
without another coverage multiplication, then converted from linear Rec.709 to
AP1. The source participates in the same scene-referred transform/grade order as
native stills and title glyphs. Rasterization is at the logical canvas size with
centered contain framing and no editor-view inset. The 8-bit scene intermediate
is an explicit precision boundary.

This contract does not qualify gradients, mixed raster/vector compositing,
external vector files, live vector text, symbols, vector effects, non-Normal
blends, clipping/isolation/group transforms, painted/dashed/variable strokes, or
artboard/node subreferences. Unsupported content is an explicit color-pipeline
error. Immutable drawing state and native/canvas identity participate in caches
and reproducibility manifests. The current implementation roadmap records the
finite geometry/paint limits and actual GPU, export and native-application tests.

Earlier dated plain-title limits above are historical. The implementation
roadmap's latest title, graph Text, timed-caption and decorated-paint entries
supersede those particular gates, within their recorded bounds.

### Native transition source extension — 2026-10-07 qualification

Qualified title, whole-document vector and matching native nested-sequence
sources may be either participant in the six native input transitions. They
retain their normal source/clip processing; track, master and display conversion
apply once after the transition. This extends source admission only, never the
qualification of an otherwise unsupported source/style or nested output plane.
Generator handles are unbounded; nested handles use the existing content-end
and speed mapping and the clamp-and-diagnose contract in 38 §1.2. CrossDissolve
end fades include these sources. Nonadjacent cuts and other end-fade kinds are
still rejected instead of being silently approximated.

The read-only input finding for a qualified embedded vector reports
`interpretation: intrinsic` and `color_space: sRGB vector paint`. Video/still
input tags on the vector asset or clip are rejected, rather than ignored or
presented as a missing interpretation. This describes source paint; complex
vector feature qualification remains a separate rendered-frame diagnostic.


### Clip/look graph output inspection (2026-10-07 continuation)

The selected Native Managed clip exposes read-only image and raw-matte output
selectors for its own graph and its actual independent or linked shared-look
graph. Graph node IDs are namespaced by clip plus explicit stage (and shared-look
ID). The controls remain available on locked tracks and must not follow an
unlinked item merely selected in the shared-look library editor. The monitor
identifies Clip, Local look or Linked shared look and provides an Exit control.
A target/cache key includes stage, revision, snapshot generation, tick, quality,
proxy mode and content identity. Changed links, missing/bypassed grades, inactive
nodes and stale readbacks must not display an old or different stage's picture.
Only one output-inspection worker may remain pending through target changes or
Exit. Group/compound scopes and look qualifier-input picking remain unqualified.

### Selected-stage correction-input inspection (cloud continuation)

The strict `GradeInspectionStage` address also applies to read-only curve/HSL
input sampling and qualifier-key isolation. Local/linked-shared stages are
Native Managed only; absent stage retains the ordinary-clip default and existing
Legacy behavior. An automatic operator address means its sole image corrector;
an explicit graph node may instead name its qualifier-key utility. Stage, clip,
operator and optional node are all part of the address. Detaching a shared look
invalidates that old shared address, even though its local snapshot keeps the
same operator IDs. The selected stage is resolved from the clip's actual link,
not an arbitrary shared-library selection.

Read-only correction-input images use the SDR preview transform. Numerical
samples use bounded ACEScct/AP1 coordinates before the correction. Qualifier-key
views include source-alpha coverage; raw graph-matte output views remain
unassociated weights. Typed metadata and generation/revision/document identity
checks prevent cross-stage or replaced-document results from being relabeled as
the requested input. These contracts do not authorize edits or qualify
shared/local eyedropper authoring, group/compound inspection, or HDR scopes.

### Selected look authoring (cloud continuation)

The Color panel can explicitly switch its corrector editor to the selected
clip's independent local look or actual linked shared look. Independent edits
keep that stage separate from the ordinary clip grade and its versions. Shared
edits affect every linked shot and are rejected when any linked track is locked.
Stage addresses are rechecked by the pure edit operation; readback addresses
alone do not authorize mutations. All three stage kinds use the same checked
parameter and graph validation rules. Drag edits coalesce; structural changes
and sampled edits are discrete undo steps.

Native Managed curve and qualifier eyedroppers address stage, operator and
optional graph node. They sample the input before that correction in bounded
ACEScct/AP1 coordinates and write only the addressed stage. Selecting an
unlinked library look does not redirect sampling. Look-stage sampling in Legacy
SDR remains disabled rather than silently sampling the display picture.
Independent look updates are also available through shared_look's explicit
update_independent operation, requiring clip_id and a complete grade. It never
automatically detaches a shared link and rejects look_id/name arguments.

Pending samples carry document identity, revision, selection, tick and native
frame identity. Scope changes, Escape, selection changes, replaced documents or
new sampling intent cancel old results. A canceled readback must drain before
another eyedropper worker can start. Look key coverage uses the existing
read-only stage inspection controller. This authoring increment does not yet
qualify group/compound picking, look-specific keyframe authoring or HDR scopes.

### Selected-member group and compound inspection (cloud continuation)

GroupPre and GroupPost addresses name a group on the selected clip's complete,
valid ancestor chain. Each group correction processes a member's picture; a
group is not a separately rendered composite. Consequently all input/output
addresses include the selected member clip, group ID, pre/post stage and
operator or graph node. Pre stages run root-to-leaf and post stages leaf-to-root.
Reused operator/node IDs and two members of one group must remain distinct.
Missing, cyclic, unsupported or non-member ancestry is unavailable, and an
ambiguous duplicate compiled output must not select the first instance.

Native Managed read-only input/key/image/raw-matte inspection remains available
on locked tracks. Native curve and qualifier picking uses the same exact
selected-member input and the existing checked group edit operation; another
locked member protects the shared grade. Owner/scope changes cancel pending
samples. Group-specific window animation is preserved but is not authored by
this panel. A compound clip means a nested-sequence source inspected in its
owning sequence, after the inner scene output enters the outer pipeline. To
inspect an inner correction, select that inner sequence and clip; no arbitrary
outer context is invented for it. Raw matte weights remain unassociated, while
qualifier-key coverage includes source alpha.

### Selected-stage animation authoring contract

grade_stage_op addresses a selected clip, a strict stage and an operator ID.
It never silently detaches a shared look or chooses a different group. Keys are
evaluated in each member clip's local clock (sequence tick minus clip start),
including group pre/post and linked shared looks. They do not use source-media
time or a shared sequence-global clock. Changing a shared lane affects every
linked/member clip; any affected locked track blocks the write. Read/copy and
native inspection remain available. Scalar and primitive-window property paths
use the same checked registry as ordinary clip grades. Existing grade_op data
remains compatible. GUI stage rows retain exact identity for drag/Undo; native
look/group window handles are not substituted for the keyframe editor.

### Native catalog LinearStraight HSL contract (2026-10-08)

This is Photonic's explicitly defined managed extension for the catalog
`color.hue_saturation` and `color.vibrance`. It is separate from the grading
AP1 luma/chroma operator. The catalog declares `LinearStraight`: unpremultiply
AP1, convert to linear Rec.709, apply component HSL, convert back to AP1 and
restore the original alpha. There is no sRGB transfer-function step. Existing
Legacy raster formulas and byte quantization are unchanged. Qualification
status is recorded in the implementation roadmap; this contract alone is not
a release-acceptance claim.

For component extrema m and M, the extended envelope is
lo=2*min(m,0), hi=1+2*max(M-1,0). Normalize RGB into that envelope before the
existing bounded HSL primitives. The envelope is exactly [0,1] on bounded
input and continuous at its boundaries, while reserving saturation headroom
for nearly neutral HDR or negative inputs. Hue rotates modulo360. Saturation
scales S by1+amount and clamps this normalized saturation to0..1, without
clamping the resulting physical RGB. Full desaturation gives the physical
midpoint (M+m)/2 directly, avoiding cancellation in envelope reconstruction.

Vibrance adds amount*(1-S) to normalized saturation. Its delta is multiplied
by a Hermite smoothstep over relative component spread1e-6..1e-4, where spread
is (M-m)/max(1,abs(m),abs(M)). At or below1e-6, preserve the original pixel.
This gives achromatic pixels no arbitrary hue and removes a discontinuity
near neutrals. Non-neutral8bit bounded RGB is outside the tiny feather band;
its existing saturation formula is retained. This deliberate native neutral
behavior does not alter the Legacy raster behavior.

For lightness l in[-1,1], t=abs(l), physical Lnew=(1-t)*(M+m)/2+t*target,
with target1 for positive l and0 otherwise. Fade extended envelope residuals:
lo_new=(1-t)*lo and hi_new=1+(1-t)*(hi-1). Reconstruct using Lnew normalized
inside that new envelope. Thus the endpoints reach nominal neutral black or
white, while zero lightness retains extended headroom. An all-zero operation
is an exact source passthrough. Logical zero-alpha pixels and physical padding
stay transparent. Final color saturation is solely the finite half-storage
boundary +/-65504; source alpha is preserved. These formulas are not claimed
to reproduce another product's extended-range HSL definition.

## 2026-10-09 — Native scene-linear reference balance contract

A Native Managed reference capture may store an optional, versioned AP1 scene-linear measurement from the same Full/original frame as its display PNG. It is not reconstructed from the display image. Old display-only references remain readable and comparable; native automatic matching requires recapture. OCIO Managed capture remains unavailable.

Version 1 uses positive straight AP1 channels from pixels with alpha at least 0.98. Each channel excludes values outside log2 [-20,20), trims 10% from each end, and requires at least 16 samples and 5% image coverage. A 4096-bin histogram with exact sums uses a bin mean only at partial trim boundaries (at most 40/4096 stop uncertainty); persisted means use signed integer 2^-20-stop units for exact JSON recovery. Histogram memory is 144 KiB and assistance is bounded to 16,777,216 pixels. Rendering retains negative and extended values unchanged. Nonfinite eligible color or invalid alpha refuses measurement.

The stored measurement is bound to the full reference-image hash, dimensions, domain and version. A freshly measured current frame yields an editable Printer Lights proposal of 12 points per log2-stop difference, limited to ±24 points per channel. This is a global statistical estimate, not chart, semantic, skin or scene-group matching. Different framing, subjects, masks or later nonlinear grades can make the correction inappropriate. Users must inspect the proposal.

GUI and MCP preserve exact-frame/revision, selection, single-visible-clip, lock, color, format and image-integrity checks. Capturing and applying are separate undo steps; proposal generation does not edit the grade. Missing or incompatible optional measurements refuse assistance without inventing an SDR substitute.

Qualification evidence is recorded in the current roadmap/handoff; this contract alone does not establish completion of the broader end-to-end grading roadmap.

### Antialiased tracking analysis and external-footage observations — 2026-10-09 EDT

Tracking input reduction now integrates exact pixel-area overlap instead of
nearest-center subsampling. Premultiplied RGB/coverage are accumulated before
unpremultiplication, so hidden transparent RGB is excluded and signed/highlight
values remain available. The input16,777,216-pixel, analysis256-pixel-side,
121-frame and120-second limits and NCC confidence/margin thresholds are unchanged.
Tests independently cover alternating-pixel alias rejection, signed/HDR signals,
alpha weighting/hidden RGB and fractional overlap. Twenty focused tracking tests
pass across numerical, native/Legacy engine, MCP and GUI lifecycle paths.

On the first31frames of the pinned public OpenCV vtest sample, an unoccluded
building-window control remains stable for31frames with both samplers. CPU
moving-subject traces stop at6/3/2frames with nearest sampling and13/4/2 with area
filtering. These are trace lengths, NOT accuracy scores. A previously selected
sign control has passing occluders; the new filter correctly exposes a confidence
stop there, so the unoccluded building window is the actual stability control.
The original failed sign assertion and subsequent comparison are retained.

Actual Full/original video decoding and Native AP1 pre-grade tracking were also
observed:31samples for the fixed window,13 for the left walker before Ambiguous,
and22 for the occluded sign before LowConfidence. Analysis leaves the document
and history unchanged. Diagnostic input interpretation explicitly uses BT.709
limited-range/left chroma; the source's actual colorimetry is not established.
The initial missing-chroma refusal is preserved, not bypassed. No footage or
derived images are bundled; source URL/hash and reproducible opt-in tests are.

Fresh aggregate: 3122 library tests passed,
10 explicitly ignored; 239 integration checks
passed, 2 explicitly ignored. Two external-footage observations were
run explicitly and separately. Clippy, native build, tool documentation,
formatting and whitespace passed. No GUI interaction changed in this increment;
native UI version/preview acceptance remains the checkpoint114 baseline.

The fixed-reference translation tracker remains limited for appearance changes,
occlusion, scale and rotation. Longer traces do not establish general tracking
readiness; stronger tracking and broader workflow/hardware acceptance remain
open. No Git publication or work on the user's PC.

### Optional bidirectional short-term window tracking — 2026-10-09 EDT

`track_grade_window.method` now accepts `fixed_reference` (unchanged default) or
`adaptive_bidirectional`. The optional mode updates a previous-frame template,
uses the same NCC confidence, ambiguity and search-edge guards in both directions,
and rejects forward/backward roundtrip error above one analysis pixel. It retains
one bounded prior analysis plane and commits it only after both directions and a
final cancellation check succeed. Existing 121-sample/120-second/input/analysis
bounds remain. Integer analysis anchors are separated from the original user
center offset: an initial prototype accumulated subpixel drift on static footage;
that failure is retained and corrected by a sixty-frame static regression.
This is a translation-only NCC tracker using the forward/backward consistency
principle, not an implementation of MedianFlow or a semantic object tracker.

Proposals identify the method and adaptive samples report roundtrip error.
Old proposals without these fields decode as fixed-reference; invalid/missing
adaptive error provenance refuses apply. Existing partial-result, locked-target,
stale-revision/source and key-replacement checks remain. GUI selection is explicit,
warns about residual drift, clears prior proposals on method changes and resets to
fixed-reference whenever the dialog opens. Analyze remains read-only; apply is one
undoable edit. Scale, rotation, perspective and occlusion recovery remain open.

Twenty-five focused tracking checks pass, including Legacy/Native forward/reverse
engine cases, gradual appearance changes, static stability, cancellation/rejected
state, old serialization and invalid-provenance refusals, and the complete adaptive
sixteen-frame balance/match/version/save/export/reimport/archive workflow.
The native GUI produced sixteen samples, maximum roundtrip error 0.005 analysis
pixels, one apply edit and exactly restored current grade on undo. The synthetic
GUI setup had a stale selected-version snapshot after manually removing current
keys; normal editing synchronized that snapshot. This fixture difference is
recorded, not misreported as byte-identical full-document undo. The consistent
MCP version/save/archive workflow passes. Reopening defaults to fixed-reference.

Eight actual Native Full/original video observations compare both methods on the
same pinned three-second OpenCV sample. Fixed window: 31 samples for both, within
one analysis pixel. Left walker: 13 fixed versus 4 adaptive; middle walker: 5 fixed
versus 15 adaptive, then the roundtrip guard stops it. Occluded sign: 22 fixed
versus 31 adaptive. These are trace lengths, NOT accuracy scores. Adaptive can
perform worse, so it is opt-in and does not establish subject identity, occlusion
recovery or general tracking acceptance. All eight observations preserve document
and history. Source colorimetry remains unknown; explicit BT.709 limited/left is
diagnostic. No external footage or derived images are bundled.

Fresh full qualification: 3127 library tests passed,
10 ignored; 239 integration checks passed,
2 ignored. External CPU/native observations ran separately.
Clippy, native build, generated tool documentation, formatting and whitespace pass.
The full roadmap remains incomplete; no Git publication or PC work.

### Saved alternate-grade dependency protection — 2026-10-09 EDT

A regression reproduced a real cleanup defect: `unused_assets` considered a LUT
unused when its only reference was in an inactive saved clip-grade version.
The scan now counts every saved version's grade, including disabled operations,
bypassed grades and an empty current grade. The existing Media Pool count and
Remove unused action use this same scanner. No user media has been cleaned.
Explicit single-asset removal behavior is unchanged.

Locked-track LUT interpretation/hash changes now also inspect saved versions,
including those reached through a nested sequence protected by a locked parent.
Both mutations fail without state changes until the relevant track is unlocked
or the dependency is removed. Saved alternatives are project dependencies even
when they are not the current rendered grade.

Focused regressions verify actual orphan removal, retained inactive dependencies,
one-batch exact undo, idempotent repeated cleanup, serialization, direct/nested
lock protection and release after the final version reference disappears. An
archive integration test cleans a synthetic project, archives it, removes the
original LUT, moves/reopens the archive and activates the formerly parked version;
its LUT bytes and full-content hash remain exact. Existing archive copying of all
pool assets is unchanged. These checks do not claim every unknown future schema
or undo-history-only dependency is portable.

Fresh full qualification: 3129 library tests passed,
10 ignored; 239 integration checks passed,
2 ignored. Clippy, native build, generated tool documentation,
formatting and whitespace passed. No GUI controls changed; native interaction
acceptance remains the checkpoint116 baseline. No Git publication or PC work.
The full professional-grading roadmap remains incomplete.

### Explicit shared-look named versions — 2026-10-09 EDT

Project shared looks now support up to 64 named versions with unique trimmed
1–128-character names. The selected linked clip must address its exact shared
look stage. GUI Advanced controls explicitly label SHARED LOOK VERSIONS and warn
that activation changes every linked shot. MCP `grade_version` accepts the same
explicit shared-look stage; omitted/null stage retains ordinary clip behavior.
Local and group stages are rejected rather than silently changing clip versions.

Add, activate, rename and remove are one undoable whole-look command. Any linked
locked track across sequences refuses edits. Shared grade and scalar keyframe
edits synchronize the selected snapshot. Deleting an active version keeps the
current rendered grade. Make Independent copies only that current look. Saved
inactive versions retain LUT dependencies and protect locked metadata changes.
Empty additive fields are omitted, retaining old-document compatibility.

Seven focused tests cover ownership, no-op/refusal/read-only behavior, bounds,
locks, serialization and exact history. Actual Native AP1 renders verify repeated
version switching on two sequences and an independently detached shot. A moved
archive cold-opens and activates a saved version, then exports 30 ProRes frames;
all decoded RGB/alpha samples and the manifest are checked. The native GUI also
activated Bright, saved it, added/renamed/deleted a copy, preserved the rendered
grade after deletion and undid all four actions. The final saved document payload
matches the original exactly, excluding only format/history save envelopes.

Fresh full qualification: 3136 library tests passed,
10 ignored; 239 integration checks passed,
2 ignored. Clippy, native build, generated MCP documentation,
formatting and whitespace passed. Existing compiler warnings are retained. These
synthetic SDR checks do not qualify HDR, representative footage, local/group
versions or shared reference ownership. The full roadmap remains incomplete.

### Explicit immutable reference-gallery reuse — 2026-10-09 EDT

References can now be copied to another sequence-owned gallery through a GUI
explicit destination selector or MCP `copy_reference_still`. A copy receives a
fresh gallery ID while preserving the exact image asset, full image hash, Native
balance summary, source clip/time/revision and original capture-gallery provenance.
The origin remains stable on repeated copies and can outlive its source gallery.
No live link is implied: removing a gallery entry does not remove the other's
entry or image. No image files are copied or deleted by the action itself.

Copying requires an identical color configuration and canvas dimensions. It maps
the reference's local format index to the destination active format only after
validation. Same-sequence, missing, incompatible, invalid-name and destination
capacity (256) cases refuse without mutation. Existing comparison independently
verifies image bytes; a changed or offline file is not made trustworthy by copying.
The shared core action uses one undoable gallery edit and does not clone the whole
project for validation.

Three focused tests qualify provenance, identity, exact undo/redo, independent
removal and asset reachability. An actual Native workflow captures a nonactive
sequence, copies to a matching destination at a different format index, compares
and applies a one-stop AP1 shot match, rejects a changed PNG, removes the original
gallery entry, archives/moves the project and deletes the original PNG. Cold
archive comparison still succeeds with exact bytes and Native summary. The first
fixture attempt bypassed the required history reset after direct document setup
and timed out; the corrected fixture follows the normal revision contract.

The native UI selected a destination, copied one still, saved through File Save,
verified the destination ID/hash/summary/provenance on disk, and undid the single
history entry. Saved document payload returns to the original after only the
recorded serde-default empty-gallery normalization and save-envelope exclusion.
Control+S did not save in this observed automation session; File Save and Save all
and quit did. This observation is not yet attributed to product or input tooling.

Fresh full qualification: 3139 library tests passed,
10 ignored; 239 integration checks passed,
2 ignored. Clippy, native build, 523-tool generated MCP
 documentation, formatting and whitespace passed. Existing warnings remain.
Live shared galleries, broader reference ownership arrangements and the full
professional-grading roadmap remain incomplete.

### Native Save shortcut and menu parity — 2026-10-09 EDT

The earlier Control+S no-op is now traced to a missing file.save command/shortcut
route, rather than left as an input-tooling uncertainty. A remappable primary-
modifier S command now invokes the existing atomic save_tab path, including saved
history, dirty-state acknowledgement, failed-write handling and the existing
untitled Save As flow. File > Save uses that same command. The existing text-input
shortcut gate is preserved; this is not a claim of shortcut activation in every
focused text widget.

Three focused tests exercise actual egui Ctrl/Cmd key events, key remapping,
plain-S refusal, exact persistence without document/history edits, and failed
writes preserving dirty state and destination contents. Native desktop validation
made a reference-gallery edit, pressed Control+S, checked the saved file, repeated
the shortcut with identical serialized content/history and closed cleanly without
an unsaved prompt. The source gallery and all copied image metadata remained exact;
the one gallery edit remained the only history entry.

Fresh full qualification passes 3142 library tests (10 ignored), 239 integration
checks (2 ignored), Clippy, native build, generated 523-tool MCP documentation,
formatting and whitespace. Earlier warnings and explicit platform/footage/HDR
limits remain. No Git commit/push or work on the user's PC occurred. The complete
professional-grading roadmap remains in progress.

### Editable closed Bézier grading mattes — 2026-10-09 EDT

Typed grade graphs now provide `bezier_matte {input,window,label}` alongside
primitive and qualifier keys. A closed path has 3..32 knots with absolute
normalized anchor/incoming/outgoing coordinates in -4..4. It uses even-odd fill,
symmetric pixel-metric feather (0..0.25 of the shorter frame dimension), inversion
and a minimum half-pixel antialias transition. It produces an opaque unassociated
key independent of the input image's RGB/alpha. Existing Union/Intersect/Subtract/
Multiply and Matte Apply nodes combine these keys and isolate corrections without
changing the established AP1 stage order. External `RotoMatte` remains unresolved;
this path does not silently reinterpret that older placeholder.

The GUI has an inset curve editor, draggable anchors/tangents, numeric control
coordinates, shape-preserving midpoint subdivision, knot removal, feather and
invert. MCP authoring uses the existing checked graph/whole-grade actions, retaining
locks, one-edit undo, named-version serialization and saved history. Static paths
are qualified here; animated control points and Bézier tracking are still open.

Adaptive de Casteljau flattening uses a 0.25 processing-pixel control-to-chord
bound, maximum subdivision depth 12 and 512 segments. Nonfinite or out-of-range
geometry is refused. Frames are capped at 16 megapixels and 512-million pixel-edge
work; exceeding the limit reports a computation error rather than truncating the
curve. This is a bounded implementation, not unrestricted high-resolution roto.
GPU keys agree with f64 CPU raster arithmetic across rectangle, curved oval,
self-crossing and off-frame paths, Legacy/AP1 inputs, source alpha 0/0.5/1,
multiple resolutions and all four Boolean modes. Rectangle coverage also matches
an independent analytic distance oracle. The CPU and GPU share flattened geometry.

Native MCP checks cover authoring/undo/redo, invalid and locked edits, raw-matte
inspection, save/history reload and three-frame ProRes delivery. Every decoded
RGB16 channel is checked against explicit AP1 interpolation and the output
transform (tolerance 500/65535); alpha is at least 65000/65535. The initial test's
fixed brightness-difference assumption, then its ACEScct interpolation assumption,
were corrected to the actual documented scene-linear AP1 graph edge contract;
those failed attempts remain in the evidence. The delivery manifest verifies.

Hands-on native testing exposed an existing graph-editor history classification
issue: a single tangent drag produced four discrete Undo entries. The fix captures
non-graph discrete changes before graph controls, then treats only reported graph
topology changes as discrete. A raw-pointer full-panel regression failed before
and passes after the fix. Native retesting confirms one drag/one Undo, separate
split and invert steps, saved coordinates and exact original document restoration
after three undos (excluding only format/history save envelopes).

Qualification is intentionally staged: the full pre-UI workspace passed 3147
library tests (10 ignored) and 241 integration checks (2 ignored), plus Clippy,
native build, formatting and generated 523-tool MCP docs. The later repair changes
only two GUI source files; all non-GUI candidate hashes are identical. After that
repair, all 592 GUI library tests, focused cross-layer Bézier checks, workspace
Clippy, native build, formatting and whitespace pass. This is not misreported as
a second fresh full-workspace run. Eight new focused tests cover this increment.
Original screenshots, native files, failed-before logs and export evidence are
retained. The complete professional-grading roadmap remains in progress.

### Bounded artistic Native blend modes — 2026-10-09 EDT

Native-managed track and clip/project Merge nodes now accept Color Dodge, Color
Burn, Divide, Vivid Light and Hard Mix. These are explicitly bounded artistic
overlap operations, not extended-range arithmetic. Straight AP1 source/backdrop
channels are clipped to [0,1] only for B(Cb,Cs); source-over retains the original
signed/highlight RGB in the non-overlap contributions, preserves alpha and applies
track/graph opacity normally. Full overlap can intentionally discard negative and
highlight values. The native track dialog states this limitation. This retains
spec03 section4.5's linear-light operand contract and does not claim Photoshop or
CSS encoded-color parity. Legacy raster/merge equations are unchanged.

Color Dodge/Burn follow the branch priorities in the
[W3C Compositing draft](https://www.w3.org/TR/2024/CRD-compositing-1-20240321/#blendingcolordodge).
Divide retains the repository's zero-source result of one. Vivid Light combines
Burn below the half-channel pivot and Dodge above it. Hard Mix uses the equivalent
algebraic threshold B+S >= 1, except black backdrop/white source remains zero,
avoiding division-roundoff flips at the exact mathematical threshold. This is a
Native-only arithmetic stabilization; the old Legacy function was not changed.

The dedicated NativeBoundedMerge IR/hash tag98 and GPU mode IDs32..36 keep the
bounded contract separate from Legacy, unclamped arithmetic and AP1 whole-color
selection. Domain, mode, arity and finite-opacity checks reject malformed input.
Track and graph lowering share the same path; animated opacity changes cache
identity. GUI draft roundtrip/undo and actual MCP track authoring/history,
PNG/scopes and five ProRes exports pass.

An independent f64 oracle exercises 525 CPU/GPU cases across five modes, seven
boundary/signed/highlight/storage vectors, five alpha pairs and three opacities.
Half-float storage can move a discontinuous Hard Mix threshold: the initial
unquantized GPU comparison failed and is retained. The final GPU oracle reads
actual source textures, independently verifies bounded input quantization, then
computes expected blending from those stored inputs. CPU checks use the original
f32 source contract. This qualifies the arithmetic in each storage domain, not
unquantized CPU/GPU pixel identity at discontinuities. Ordinary output tolerance
is .01; extreme half-storage channels use an absolute tolerance32. Zero opacity,
transparent inputs, black/white special cases and signed non-overlap are covered.

The current tree passes 3152 library tests (10 ignored) and 242 integration checks
(2 ignored), all-target Clippy, native build, formatting and whitespace checks.
There are five new focused tests plus expanded animated graph coverage. Generated
MCP documentation still describes 523 tools. Native desktop interaction details
and original screenshots are recorded in the attached checkpoint. Nonseparable
Hue/Saturation/Color/Luminosity and the wider roadmap remain open. No Git commit,
push or merge was made.

### Native AP1 nonseparable blending — 2026-10-09 EDT

Hue, Saturation, Color and Luminosity now use the bounded Native artistic overlap
contract. NativeBoundedMerge uses AP1 Y coefficients rather than Legacy's artistic
0.30/0.59/0.11 luma, with RGB max-minus-min chroma, SetSat/SetLum and bounded
luminance-preserving clipping. The pure existing SetSat kernel is shared without
changing its equations. The four choices have separate GPU IDs 37..40; the stable
serialized blend names and mode-sensitive cache identity are retained. Legacy
blending is unchanged. Existing Native track and clip/project Merge authoring,
including animated opacity, lowers through the checked shared builder.

As with the five preceding bounded modes, clipping applies only to the overlap
function. Original signed/extended color participates in source-over outside the
overlap; alpha and opacity retain their established meaning. These are creative
linear AP1 coordinates, not display-encoded or perceptual HSL matching and not
HDR-preserving overlap operations. The native dialog explains both bounded
clipping and AP1 luminance. The
[W3C nonseparable structure](https://www.w3.org/TR/2024/CRD-compositing-1-20240321/#blendingnonseparable)
is adapted to the project's normative linear-light space and AP1 Y.

An independent f64 reference constructs the requested chroma/luminance and clips
a ray from the neutral target luminance against the RGB cube. It does not reuse
the implementation's extrema-branch clipping. 720 CPU/GPU cases cover the four
modes, twelve neutral/tied/primary/signed/highlight/extreme vectors, five alpha
pairs and three opacities. Actual GPU source quantization is independently checked
before comparing its arithmetic. CPU tolerance is 2e-6 for ordinary output and
0.004 at extreme f32 magnitudes; GPU tolerance is 0.01, or 32 at extreme half-storage
magnitudes. The initial extreme CPU check incorrectly used sub-ULP absolute
tolerance; its failure is retained, and the corrected tolerance follows f32
spacing. AP1 luminance-target tests and an explicit Legacy green-channel reference
prove the intended difference in weights, while unchanged Legacy goldens pass.

Five new focused library tests and one GPU integration test cover validation,
track lowering/cache separation, AP1-Y identity, GUI roundtrip/undo and four actual
MCP ProRes deliveries. Existing animated graph coverage is extended. The initial full aggregate passed every non-video library, but a session test
still expected the newly supported Color blend to be rejected. Its negative
fixture now uses malformed opacity 2.0, preserving the fail-closed check. No
production implementation changed after that aggregate; source hashes and the
one-test diff are retained. The complete affected video library was rerun, then
all 243 integration checks, workspace Clippy, native build, formatting and
whitespace. Combined qualification is 3157 library passes (10 ignored) plus 243
integration passes (2 ignored); it is not represented as a fresh single full run.
Generated MCP docs retain 523 tools. Native desktop evidence covers the visible
contract, checked edit/save/history and restart/undo behavior described in the
qualification receipt. All 26 serialized blend names now have Native contracts,
with the explicit boundaries above; this is not complete pipeline/HDR acceptance.
The broader professional-grading roadmap remains incomplete. No Git publication.

### Native Soft Light finite-intermediate repair — 2026-10-09 EDT

A targeted precision check found a real boundary failure in existing Native Soft
Light: opposite signed finite RGBA16F colors with alpha near 2e-6 overflowed the
straight-color f32 cubic. CPU produced NaN at zero opacity, and GPU produced
infinite channels. Ordinary generic Merge finite-overflow concerns were not
reproduced in the initial same-sign test; the retained failure specifically
establishes the Soft Light intermediate problem.

Native CPU Soft Light now evaluates the normative extended equation in f64 and
saturates only the final result to finite half-storage range. The GPU uses an
algebraically factored alpha-weighted overlap (internal mode 41), avoiding the
unbounded straight-color quartic. Valid finite half inputs and the established
1e-6 unpremultiply cutoff bound these f32 intermediates. Zero opacity returns the
backdrop exactly. Source alpha below the cutoff, backdrop alpha below it, signed
non-overlap and extended highlights retain the existing contract. Native-only
dispatch leaves the Legacy equations and serialized mode names unchanged.

Two new integration tests cover 198 generic extreme cases and 200 Soft Light
f64-reference cases, including opposite signs, source/backdrop alpha at zero,
cutoff and just above cutoff, unit alpha, and opacity 0, 1e-30, 0.125, 0.5 and 1.
The latter validates actual GPU source quantization separately before computing
its oracle. RGB CPU tolerance is max(2e-6, |reference|*2e-7); GPU is
max(0.01, |reference|*0.001), with alpha tolerance 0.0005. This makes no claim of
bit-identical CPU/f16 storage. Original failing logs are preserved. An initial
oracle test referenced an unavailable half crate; the harness instead checks
source quantization bounds without adding a dependency.

The entire workspace library suite passes 3157 tests (10 ignored), and all 245
integration checks pass (2 ignored), including unchanged Legacy goldens and the
existing actual Soft Light MCP ProRes export. Workspace all-target Clippy, native
build, regenerated 523-tool MCP documentation, formatting and whitespace checks
pass. No UI controls changed, and this increment makes no new manual UI claim.
This closes the reproduced numerical failure, not the broader professional
roadmap, representative-footage or external-hardware acceptance. No Git push.

### Native Normal small-alpha parity repair — 2026-10-09 EDT

An all-26-mode matrix at exact half-float alpha neighbors around the artistic
1e-6 unpremultiply cutoff reproduced twelve Normal CPU/GPU differences. The CPU
reference discarded a valid nonzero premultiplied source below that cutoff;
the existing GPU correctly used direct source-over. Signed or extended straight
color can retain meaningful premultiplied RGB even at very small coverage.

Native Normal now uses direct premultiplied source-over on CPU, without dividing
by alpha, matching the existing GPU fast path and the normative reduction in
03 §3.3. Final RGB obeys the existing finite RGBA16F storage boundary. The GPU,
Legacy CPU equations, serialized mode names and alpha cutoff for artistic blend
functions are unchanged. This does not silently clamp scene color to 0..1.

One new GPU integration test covers 1248 cases: all 26 modes, eight alpha pairs
including zero and exact half-float neighbors below/above 1e-6, three opacity
values and signed/positive source variants. Every source RGBA channel is read
back and asserted exactly equal before output comparison, so conversion rounding
cannot disguise a disagreement. GPU RGB tolerance is max(0.003, |CPU|*0.001),
and alpha tolerance max(6e-8, |CPU|*0.001). A new 60-case CPU library test checks
Normal against an independent f64 source-over reference across zero/tiny/unit
coverage, four opacity values and three magnitudes up to 65504; it explicitly
asserts the unchanged Legacy cutoff result. Zero-alpha fixtures carry zero RGB.
The reproduced failures and corrected checks are retained. An initial focused
unit invocation selected zero tests because of an exact short-name filter; the
corrected invocation and full suite execute the actual test.

Fresh qualification passes all 3158 library tests (10 ignored) and 246 integration
tests (2 ignored), including unchanged Legacy goldens and actual export checks.
Workspace all-target Clippy, native build, 523-tool MCP documentation regeneration,
formatting and whitespace checks pass. No UI controls changed and no new manual
UI claim is made. The broader roadmap, representative-footage and external
hardware acceptance remain unfinished. No Git publication or PC work.

### Fixed-topology animated Bézier windows — 2026-10-09 EDT

Closed Bézier graph windows now support explicit fixed-topology shape keyframes.
A separate AnimatedBezierMatte node serializes 1–128 strictly ordered nonnegative
integer-tick keys. Older readers reject this new node tag rather than silently
ignoring its animation. Existing static BezierMatte serialization is unchanged.
All keys retain the same knot count. Anchors, incoming/outgoing handles and feather
use hold or linear interpolation; invert remains held. Sampling clamps before
and after the key range and computes time differences in i128 before conversion.
Invalid or degenerate sampled geometry produces a located native diagnostic and
transparent failed frame; actual export refuses it without a partial final movie.

The Color controls explicitly enable animation, add/select a sampled key at the
stage playhead, edit the selected key, remove keys, choose Hold/Linear and freeze
the current sample to a static shape. Knot insertion/removal is disabled while
animated. Clip, asset, local/shared look and group scopes use the selected member's
clip-relative time; track/master use sequence time. Playback outside the selected
member disables key creation/freezing. Existing checked whole-grade history,
locks and MCP graph authoring remain in use; no new shader or MCP tool is required.

Nine new library tests cover bounded serde, topology/times, interpolation at large
ticks, atomic insertion, read-only viewing, live stage clocks, single-gesture
undo, eight compiler scopes and actual three-frame PNG/ProRes delivery. The MCP
fixture checks every RGB16 pixel within500 of its independently evaluated window
reference and alpha at least65000, save/load, locked edits, undo/redo, read-only
inspection and degenerate-export rejection. All18 focused Bézier tests pass.
Test-only compilation/assertion issues and the precisely restored scripted fixture
edit are recorded in retained logs; no implementation was silently dropped.

Fresh full qualification passes3167 library tests (10 ignored) and 246 integration
tests (2 ignored), all-target Clippy, native build, 523-tool documentation and
formatting. The final whitespace gate initially caught trailing blank lines in
three previously appended documentation files; trimming only their EOF whitespace
and rerunning the gate passed, without any implementation change after tests.

Native desktop review created two keys at0 and682080000 ticks, dragged the second
anchor, saved, froze the current shape, and performed two clean app restarts.
Visible History controls undo the drag in one transaction, redo it exactly, undo
freezing, and restore the original document exactly after the three authoring
transactions (excluding only persisted format/history metadata). Ctrl+S worked.
An attempted Ctrl+Z did not mutate that native state; keyboard undo is not claimed
verified by this review. Original screenshots and checked saved documents are
retained. The app is closed. Automatic Bézier tracking, changing topology and
compound/stabilization coordinate contracts remain open, as does the full roadmap.

### Qualification preferences isolation — 2026-10-09 EDT

The prior animated-window review's Ctrl+Z observation was explained by the
configured shortcut: the existing keymap roundtrip unit test had persisted
edit.undo=Ctrl+U into the isolated test home. It was not an animated-window undo
defect. The actual native Ctrl+U binding has now been exercised and restores the
original document exactly; history and Ctrl+S remain verified. One immediately
following Save preceded the UI's completed undo frame, so the final save was
performed after observing the new state and its file was checked.

Unit-test builds now resolve preference persistence to a per-thread UUID temporary
path, without mutating HOME/XDG_CONFIG_HOME. Production preferences paths and
behavior are unchanged; no existing preferences were reset or rewritten by this
fix. The import-keymap test now asserts the saved/reloaded values as well as the
in-memory result. Two additional tests cover parallel thread separation and
leaving the installed preferences file untouched. A hash comparison around the
complete suite confirms the configured application preferences stayed unchanged.

Fresh qualification passes3169 library tests (10 ignored) and 246 integration
tests (2 ignored), all-target Clippy, native build,523-tool documentation,
formatting and whitespace. This closes a test-isolation/qualification problem,
not another rendering capability. The full roadmap remains incomplete.

### Bounded 1080p multi-clock observation — 2026-10-09 EDT

The explicit ignored temporal harness now also accepts1080p; default720p and
ordinary32x16 tests remain unchanged. Raw source planes stream to FFmpeg stdin,
avoiding a2.49GB raw temporary. Every lossless decoded source sample is checked
before the independent native AP1/display reference is used. Production renderer,
cache, source-conversion and decoder code did not change in this increment.

On this unoptimized Cargo test build with software Vulkan llvmpipe, Full/original
1080p produced119 frames in6seconds with one source clock (59 dropped), and34
frames with four separated source clocks (137 dropped). Four-clock Draft960x540
produced39 frames (134 dropped). Removing the playback readback probe produced33
frames (138 dropped), so that probe alone does not explain the slow observation.
These measurements do not establish release-build or physical-GPU performance.

Each pixel-checked run also passed a60second repeated-seek loop across four target
times:5558 one-clock seeks,5299 four-clock Full and5307 four-clock Draft. Those are
repeated hot/cold target cycles with cache reuse, not thousands of distinct cold
frames. Peak decoded rings were62,208,000/248,832,000bytes for1/4 clocks; peak graph
GPU cache was about1.07GB, within reported budgets. Maximum checked pixel errors
were below0.00082 (gate0.012); all paused output pixels were verified, with one
pixel sampled during playback/seeks. Uniform synthetic frames isolate temporal
correctness and do not qualify representative footage. Documents/history stayed
unchanged. The no-readback control still checked its complete paused frame, but
makes no playback-pixel claim and had three transient evaluation misses.

Two ordinary temporal tests and all four explicit observations pass their existing
correctness/progress gates. Workspace all-target Clippy, formatting and whitespace
pass. The last complete production-tree qualification remains checkpoint127:
3169 library tests plus246 integrations,12 ignored. That whole suite was not rerun
for this harness-only extension. Real-time high-resolution acceptance remains open;
next diagnostics must distinguish development-build overhead, software-GPU work,
decode and source conversion before choosing a production optimization.

### 1080p diagnostic controls — 2026-10-09 EDT

A separate control compiled only photonic-core/render/video at opt-level2, with
incremental compilation disabled. Other dependencies remained development builds;
this is not the shipped release profile. Four-clock1080p Full produced36 frames
in6seconds with 138 drops, a small change from34/137. Removing the playback probe
had likewise not improved the baseline. Neither control establishes a specific
production bug or rules out all build/hardware overhead.

Temporary per-node CPU wall-time instrumentation then observed source-node median
23.1ms, Merge11.2ms and SDR-output31.8ms. These are enqueue/readiness/backpressure
wall times, not hardware GPU timestamps; startup waits inflate aggregate sums.
They indicate source handling and downstream passes both contribute on llvmpipe,
without proving an exact hardware bottleneck. That diagnostic published33 frames
with 137 drops and preserved checked pixels/document/history. No performance gate
was relaxed. The temporary evaluator instrumentation was removed and its complete
source hash verified equal to qualified checkpoint128 before packaging. The
harness and reports remain available; no production optimization was guessed from
these observations. Real-time performance requires further optimized-build and
physical-GPU qualification; correctness and bounded-cache results remain valid.

### Native title SlideUp entrance — 2026-10-09 EDT

Native titles now accept the existing None and SlideUp animation values. SlideUp
moves the entire painted title, including outline and background, from five percent
of authored frame height below its settled position while fading from transparent
to full opacity over the first 200 milliseconds of clip-relative timeline time.
The effect precedes clip composition and grading. At and after 200 milliseconds
the compiled graph is identical to the static title graph. No new persisted enum,
IR node, shader or MCP tool is introduced; Legacy rendering is unchanged.

The native Titles panel offers an explicit None / Slide up choice. This creates
one lock-aware, undoable source edit; merely viewing the panel leaves the document
unchanged. Existing numeric gesture coalescing remains intact. Typewriter,
FadeWords and karaoke are still refused for native titles without qualified
word-timing/paint semantics; this does not finish general title animation.

Four new library tests cover clip-clock graph structure and static endpoint,
read-only UI, discrete reversible/locked edits, and actual MCP preview/export.
Seven 128x80 Full PNG frames verify whole-paint opacity and displacement against
the settled frame; midpoint is two pixels down at half opacity. Three half-size
Preview API outputs verify the corresponding one-pixel displacement. Retained
files labeled draft use quality=preview with scale=0.5, not an independent raw
native Draft full-frame assertion. Seven decoded ProRes frames verify alpha at
16-bit tolerance 700; an independent ProRes RGB oracle is not claimed here.
Export manifest, document/history stability, save/load and undo/redo are checked.

Native desktop review visibly exercised start, frame 3 and frame 6, saved the
single edit, cold-reloaded its history, then used the configured Ctrl+U shortcut
and saved an exact restoration of the original document (excluding only format
and history envelopes). Original screenshots and saved documents are retained.
Fresh full qualification passes 3173 library tests and 246 integration tests,
with 12 explicit ignores, all-target Clippy, native build, generated 523-tool
documentation, formatting and whitespace checks. The full roadmap remains open.

### Exact JSON precision and real-footage delivery workflow — 2026-10-09 EDT

A new opt-in actual-footage combined workflow exposed a reproducible integrity
failure: the default JSON parser changed a tracked subpixel f64 key by one ULP
on reopening, so a legitimately published render manifest failed its exact
snapshot identity check. The value 0.09638437298605207 changed from bits
4591609646683394096 to 4591609646683394097. This was a persistence/parser defect,
not a reason to relax manifest verification or round authored tracking data.

The workspace now enables serde_json's existing float_roundtrip feature, without
changing dependency versions or Cargo.lock. Three ordinary regressions cover the
observed values and numeric extremes, 10,000 deterministic bit patterns each for
finite f64/f32, and complete timeline/embedded-document manifest roundtrip plus
tamper rejection. Both timeline and embedded-document integrity checks remain
strict. Native application open/save/cold-restart/save preserves the complete
fixture document exactly, excluding only its format/history envelopes; the
formerly failing key retains its exact bits and no history edit is introduced.

The opt-in end-to-end test uses the first 16 frames of the pinned public OpenCV
vtest sample, 768x576 at 10fps. It captures a reference, proposes and applies a
known exposure correction, verifies undo/redo, tracks a static architectural
window, switches untracked/tracked versions, saves/reloads, renders all frames,
exports and verifies a ProRes manifest, decodes delivery, reimports/probes and
cold-renders a portable archive. All 21,233,664 RGB comparisons between sRGB PNG
preview and decoded BT709 ProRes, with explicit transfer conversion, give maximum
error 0.0114191 and RMSE 0.00206092 against preselected 0.04/0.01 limits; alpha
is also checked. This is delivery consistency, not an independent color oracle.
Camera colorimetry is unknown and the diagnostic interpretation is explicitly
BT709 scene, limited range and left chroma. A static target and 1.6 seconds do
not qualify moving-subject accuracy, occlusion or broad representative acceptance.
External media and derivative images/video are not redistributed in this archive;
source URL/hash and numeric reports are retained for an explicit local rerun.

Fresh full qualification passes 3176 library tests and 246 integration tests,
with 13 explicit ignores, plus the separately invoked external-footage test.
All-target Clippy, native build, generated 523-tool documentation, formatting and
whitespace checks pass. An earlier interrupted aggregate produced no complete
result and is preserved as interrupted; the later full rerun supplies these
counts. This closes a concrete persistence defect and expands workflow evidence;
it does not complete the full roadmap or hardware/HDR acceptance.

### Catalog outgoing fades and narrow Inspector controls — 2026-10-09 EDT

All six known catalog transition kinds now follow the existing sequence contract
for outgoing edges: an eased opacity fade to transparent at a sequence end or
gap. Duration and easing apply; the stored kind and other parameters are preserved
but do not create a two-source wipe, push or color dip into empty space. Incoming
adjacent-cut transitions retain their existing kind-specific behavior. Unknown
kinds, invalid durations and nonadjacent incoming edges remain visibly refused.
No renderer, shader, serialization format or additional MCP tool is introduced.

The Inspector explicitly presents Out as Fade out and hides inert kind/direction
controls. A native review caught a clipped Remove button and explanation in the
narrow drawer; wrapped rows/text and stacked easing controls fix that layout.
Tests cover 140/180/220/320-pixel drawers without horizontal overflow or document
mutation. Native Remove makes one edit, Undo restores the exact original document,
and cold reopening/saving retains its original Wipe kind and all authored data.
The title is visibly faded at frame 22/30 after reopening. Only format/history
wrappers are excluded from exact document comparison.

Compiler coverage compares 432 programs across six kinds, four curves, nine
clocks and sequence-end/gap contexts. Actual PNG and ProRes delivery is exercised
for Solid, Title, Vector and Nested sources across all six kinds, with 24 retained
case folders. Three PNG center-alpha samples and five decoded ProRes center-alpha
samples per case meet their existing tolerances; manifest identity, save/undo/redo
and read-only render behavior are checked. These center samples are not an
independent all-pixel RGB oracle.

Verification is staged: the complete pre-layout run passes 3179 library and 246
integration tests, with 13 explicit ignores. After the GUI-only layout correction,
all 603 GUI tests pass, and final-tree all-target Clippy, native build, formatting
and whitespace checks pass. The unchanged core/render/MCP code retains the full
run's evidence; these counts are not described as a fresh full final-tree run.
Generated documentation still contains 523 tools. The full roadmap remains open.

### Caption clock overflow and complete current-tree regression — 2026-10-09 EDT

A boundary regression reproduced a panic in the existing FadeWords opacity
helper on an accepted native cue: cue 0..i64::MAX, word 0..1 and tick
 i64::MAX-1. Although the cue duration passes the existing checked-range guard,
subtracting the word's 150ms lead-in from that late tick overflowed i64.
The helper now computes elapsed ticks in i128 and clamps before converting to
its existing f32 opacity. Ordinary 150ms lead-in behavior is unchanged; no
animation timing, persisted format, shader or UI contract is added.

A native-batch regression failed with the original helper and passes with the
fix. A second test covers signed tick extremes and lead-in endpoints/midpoints.
All 34 focused caption tests pass. Fresh complete current-tree qualification
passes 3182 library and 246 integration tests, with 13 explicit ignores;
all-target Clippy, native build, 523-tool generated documentation, formatting
and whitespace checks pass. This complete run also includes the preceding
outgoing-fade narrow-Inspector correction. No native visual change is claimed
for this arithmetic-only fix. The full professional roadmap remains incomplete.

### Continuous scene-relative AP1 spatial scopes — 2026-10-09 EDT

Native scene-linear ACEScg taps now expose luminance waveform, RGB parade,
RGB waveform and Y/R/G/B histogram views through the existing bounded,
single-pending asynchronous scope readback. Positive straight AP1 values are
plotted as log2(value/0.18) over [-12,+12) in 256 bins; these are relative scene
stops, not nits, IRE, HDR mastering or calibrated display measurements.
All logical pixels contribute; source columns aggregate to at most 256 columns,
with at most 16,777,216 pixels and approximately 1.05 MiB accumulator storage.
Transparent/invalid pixels and per-channel nonpositive/below/above-range values
are counted separately instead of being clamped onto the graph edge.

Existing get_scopes and asynchronous measure_scopes expose the same tagged
scene domain and excluded counts. AP1 vectorscope and video-legal results are
explicitly unavailable/null; Program retains its qualified sRGB display path.
Existing Legacy/sRGB scope production math is unchanged. No new persisted
format, dependencies or renderer operation is introduced.

Four focused CPU/GPU tests cover independent numeric bin expectations, alpha,
extended values, logical padding, spatial/channel ordering and count conservation.
The initial GPU fixture used an older upload helper that flushes half subnormals;
rewriting that fixture with exact half bits resolved the test discrepancy without
changing the production algorithm. An initial MCP command matched zero tests
and is not evidence; the corrected exact native-managed ProRes workflow test
passes and verifies scene measurement/job parity and unchanged document/history.

Native cloud UI inspection covers floating waveform/parade/histogram, unavailable
AP1 chroma, Program sRGB switching, persisted docking on cold reopen, docked
RGB waveform and playback to the final valid frame. Measurements identify their
frame tick, revision and domain. Native visual checks used Draft preview; MCP
checks used Full/original. No full-resolution claim is made for Draft plots.
Close removes the panel and saving after viewing/playback preserves the authored
document exactly, with no history edit. Narrow monitor transport time readout
can still truncate when both side drawers are open; scope timestamps and bottom
status remain visible. Scope Close correctly wraps. Further workspace layout
and per-measurement quality/proxy labeling remain useful follow-up work.

Fresh complete current-tree qualification passes 3186 library and 246 integration
tests, with 13 explicit ignores. All-target Clippy, native build, generated
523-tool documentation, formatting and whitespace checks pass. Original UI
captures, native fixtures and receipts are retained. No repository commit/push
or release acceptance is claimed. HDR absolute scales/output, high-resolution
scope throughput and the broader professional roadmap remain incomplete.

### Measured scope sampling provenance — 2026-10-09 EDT

Scope measurements now label the measured frame's Draft/Full quality, logical
tap dimensions and proxy policy. The proxy label describes policy (auto,
prefer proxy or original), never an unsupported claim about actual source use.
Results are invalidated when quality, policy or logical dimensions change,
while normal playback retains the existing explicitly timestamped prior result
until the next asynchronous measurement arrives. Labels derive from the stored
measurement stamp, not newer monitor settings. Rendering and scope numerical
math are unchanged.

The existing context-invalidation regression now covers these three settings
and every quality/policy label combination. All 603 GUI tests, all-target
Clippy and native build pass after the change. This is staged validation over
the preceding complete 3186-library/246-integration checkpoint; it is not a new
full-workspace test run. Native UI inspection verifies Close/reopen, Draft/auto,
Full/original and Draft/prefer-proxy switching on the same 128x80 title fixture.
Save preserves the authored document exactly. Original screenshots and receipts
are retained. Broader HDR, proxy footage qualification and the full roadmap
remain incomplete.

### High-resolution scene-scope timing diagnostic — 2026-10-09 EDT

An explicitly ignored, opt-in synthetic GPU test now exercises scene-scope
histogram and waveform count conservation at 512x288, 1920x1080 and 3840x2160,
with three timed measurements at each size. Output storage remains 1,052,728
bytes. Timings include pipeline creation, dispatch and readback, excluding
input upload; they are development-build llvmpipe observations rather than
hardware GPU throughput, interactive acceptance or statistical percentiles.

Initial total readback observations (milliseconds) were:
- 512x288: 13.44, 10.30, 7.12
- 1920x1080: 31.59, 22.63, 24.51
- 3840x2160: 109.20, 69.86, 68.20

A workgroup-local histogram experiment preserved the focused numerical tests
but was slower: 1080p 49.52/36.38/35.76ms and 4K157.79/119.87/118.05ms.
It was rejected, and scene_scopes.rs was restored byte-for-byte to checkpoint135.
A repeated baseline passed count conservation and recorded 1080p40.20/29.86/33.24ms
and 4K116.16/82.80/97.02ms, demonstrating timing variability as well as the
remaining software-GPU cost. No performance improvement is claimed.

Correction: this intermediate snapshot also retained an in-progress scope-pipeline reuse draft; see the subsequent complete-regression entry. The diagnostic test is not the only source change in that snapshot.
All 240 ordinary render tests pass, with that one test explicitly ignored;
its explicit diagnostic invocation passes separately. Four focused scene-scope
tests pass after restoration, and all-target Clippy passed for the retained test
with the baseline shader. This is staged validation over checkpoints134/135,
not a fresh aggregate run; a future aggregate now includes one additional
intentional ignore (14 total if the other suites remain unchanged).
Original before/experiment/restored timing receipts and rejected patch are
retained. Broader footage, scope scheduling, hardware performance and the full
professional roadmap remain unfinished.

### Scope pipeline reuse and complete regression — 2026-10-09 EDT

Correction to the preceding timing checkpoint: its source snapshot also retained
an in-progress ScopeKernel extraction and GUI reuse draft. The statement that
only the diagnostic test had changed was incomplete; that intermediate snapshot
was not a newly fully qualified release. This checkpoint supersedes it with
complete current-tree validation and native UI review.

Scope views now retain one immutable compute pipeline and binding layout for
their current shader/entry/dimension/configuration signature. Per-request
counters, uniforms and staging buffers remain fresh, preventing counts from
accumulating across frames or signals. A different GPU device identifier resets
the view's pipeline and pending/result state. This is a per-view cache within
the current GUI renderer lifetime, not a cross-device or persisted global cache.
Existing source-signal interpretation, sample math and measured provenance are
unchanged. The rejected workgroup-histogram shader remains reverted.

A focused GPU regression compares cached and uncached output exactly while
changing logical extent and Legacy/sRGB signal, then repeats the same check for
scene scopes. The high-resolution diagnostic now measures both routes in the
same invocation. Observed submit/setup times fell from roughly 4–6ms to
0.4–2.1ms with reuse. Total readback examples were 1080p uncached
27.06/19.73/19.89ms versus cached15.74/15.63/16.97ms, and 4K uncached
101.98/61.76/61.03ms versus cached58.63/57.32/74.03ms. These are three-sample
synthetic development-build llvmpipe observations, not percentiles, interactive
frame-rate acceptance or a hardware-GPU guarantee. Pipeline setup is excluded
from the cached measurements; input upload is excluded in both routes.

Fresh full current-tree validation passes 3187 library and 246 integration tests,
with 14 explicit ignores (including the opt-in diagnostic, which separately
passes). All-target Clippy, native build, generated523-tool docs, formatting and
whitespace checks pass. Native review of the final build verifies cold-open
Program SDR, selected Clip AP1, repeated playback to the final valid frame and
Full/original switching. Saving after viewing preserves the authored document
exactly. Original screenshots and verification logs are retained.

The full professional roadmap remains incomplete. High-resolution interactive
scope performance, broader tracking/animation, HDR finishing and end-to-end
acceptance still require work. Cloud disk headroom is now1.5GB; permission has
been requested to remove only11GB of obsolete, regenerable incremental compiler
cache before further heavy builds. No source/assets/checkpoints/evidence were
deleted, and no repository publication occurred.
