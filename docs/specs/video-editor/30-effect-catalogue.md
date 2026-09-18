# 30 — Effect Catalogue, Manifest, and the Raster Bridge

**Status:** Draft — implementation contract; no code authorization
**Date:** 2026-07-20
**Audience:** effect implementers, engine maintainers, GUI and MCP owners

**Depends on:** [01-data-model.md](01-data-model.md) (`EffectKind`, `AnimProps`, `prop_registry`), [02-engine.md](02-engine.md) (`IrOp`, compile/eval), [03-render-color-pipeline.md](03-render-color-pipeline.md) (working colour state), [11-testing-phasing.md](11-testing-phasing.md) (goldens), [26-kdenlive-mlt-parity.md](26-kdenlive-mlt-parity.md) (the K/E/X inventory this implements), [27-spec-audit.md](27-spec-audit.md) (A-1/A-3 colour findings).

**Owns:** the effect **manifest schema** ([26 E-3](26-kdenlive-mlt-parity.md#e-3--effects-as-data-not-code), [26 X-4](26-kdenlive-mlt-parity.md#x-4--effect-manifest-as-a-versioned-schema)), the **raster-kernel bridge** ([26 K-B16](26-kdenlive-mlt-parity.md#k-b16--bridge-the-raster-kernel-library-into-the-video-catalogue)), the **effect catalogue** and its tiering, **luma-map wipes** ([26 K-B7](26-kdenlive-mlt-parity.md#k-b7--luma-map-wipes)), **nested-subgraph masking** ([26 K-B8](26-kdenlive-mlt-parity.md#k-b8--nested-subgraph-masking)), and **alpha/debug views** ([26 K-B17](26-kdenlive-mlt-parity.md#k-b17--alpha-view-and-unpremultiply-debug-filters)).

**Does not own:** grading operators ([07](07-color-grading.md)), the node-graph UI ([08](08-fusion-node-flows.md)), transitions' *timing* model ([01 §5](01-data-model.md), and [27 U-1](27-spec-audit.md#5-u---under-specified-contracts) which must be resolved first), or the engine contracts in [32-engine-contracts.md](32-engine-contracts.md).

---

## 1. Why this document exists

`EffectKind` has **7 variants, six of which render as blit-passthrough** (`graph/eval.rs:20-22`). Every parity item in [26 §10](26-kdenlive-mlt-parity.md#10-k-b--effects-and-compositing) is blocked behind that. Two decisions shape the fix:

1. **The catalogue is data, not code** — one manifest per effect drives the runtime table, the inspector UI, validation, the MCP schema and the generated docs. Adding an effect must not mean editing five files and an enum.
2. **The maths is already written.** `photonic-core/src/raster/` holds **~61 tested CPU kernels** built for the photo editor. Porting them beats authoring from scratch, and each port arrives with a ready-made parity oracle.

**Hard prerequisite.** Both [27 A-1](27-spec-audit.md#a-1--p0--the-live-canvas-composites-in-gamma-headless-composites-in-linear) (canvas composites in gamma, headless in linear) and [27 A-3](27-spec-audit.md#a-3--p0--grade-operators-apply-transfer-functions-to-premultiplied-alpha) (grade ops run on premultiplied alpha) must be **decided before the first effect ships**. This document's §4 operand contract is the same question in a third place; settling it once and applying one answer everywhere is the point. Shipping effects first would triple the surface that has to be corrected later.

---

## 2. The manifest

### 2.1 Shape

One manifest per effect, versioned, stored in-tree as data and compiled into a static table. Runtime lookup replaces today's hand-maintained `EffectKind` → `prop_registry` correspondence.

```rust
pub struct EffectManifest {
    pub id: EffectId,                 // stable, serialized — e.g. "blur.gaussian"
    pub version: u16,                 // bump on any parameter-meaning change (§2.6)
    pub name: &'static str,           // display name
    pub category: EffectCategory,
    pub params: &'static [ParamSpec],
    pub caps: Caps,
    pub applies: Applicability,
    pub kernel: KernelRef,            // WGSL entry point + CPU reference fn
    pub arity: u8,                    // 0 = generator, 1 = filter, 2 = combiner
}
```

`EffectId` is a stable string, **not** an enum discriminant. `EffectKind`'s enum survives only as a deprecated alias during migration (§6) — a growing enum is exactly what this replaces.

### 2.2 Parameters

```rust
pub struct ParamSpec {
    pub path: &'static str,           // "params.radius" — matches today's PropPath
    pub kind: ParamKind,
    pub default: PropValue,
    pub range: Option<(f64, f64)>,    // inclusive; validated, never silently clamped
    pub animatable: bool,
    pub display: Display,             // §2.4
    pub ui: UiHint,                   // Slider | Dial | Angle | ColorSwatch | Enum | Point | Rect
    pub group: Option<&'static str>,  // inspector section
}

pub enum ParamKind { Float, Vec2, Color, Bool, Enum(&'static [&'static str]), Path }
```

`ParamKind` extends today's `PropValueKind` with `Path` (LUTs, luma maps) and named `Enum` variants. `PropEntry`'s `{path, kind, range}` is a strict subset, so `prop_registry` becomes a **projection of the manifest table** rather than a parallel structure — one source of truth, per [26 E-3](26-kdenlive-mlt-parity.md#e-3--effects-as-data-not-code).

**Every parameter is a curve.** A static value is a single-keyframe curve. This deliberately avoids the reference's two-dialect problem (animation strings for some values, plain scalars for others); `AnimProps` already works this way and the manifest must not reintroduce the split.

### 2.3 Capability and applicability

```rust
pub struct Caps {
    pub alpha: AlphaBehaviour,        // Preserves | Modifies | Requires
    pub bit_depth: BitDepth,          // Any | RequiresFloat
    pub linear_light: bool,           // correct in linear working space?
    pub gpu: GpuSupport,              // Native | CpuFallback | CpuOnly
}

pub struct Applicability {           // bitflags
    pub clip: bool, pub track: bool, pub master: bool, pub asset: bool,
    pub reverse_safe: bool,          // survives negative-rate playback?
}
```

Two rules, both taken from the reference's mistakes:

- **Derive defaults from the backend.** Most manifests should declare nothing: a WGSL kernel operating per-pixel on `Rgba16Float` defaults to `linear_light: true`, `bit_depth: Any`, `gpu: Native`. Only exceptions are written down. Kdenlive derives its 10-bit flag heuristically for exactly this reason.
- **Applicability is separate from kind.** Kdenlive conflates them in one `type=` attribute and suffers; Shotcut's bitmask is the better model. Photonic needs this the moment [26 K-B1](26-kdenlive-mlt-parity.md#k-b1--track-and-master-effect-stacks) adds track and master stacks.

`reverse_safe` is load-bearing once [32 §2](32-engine-contracts.md) lands `source_range`: a temporal effect that assumes forward playback must be excluded or flushed under reverse.

### 2.4 Backend value versus display value

```rust
pub struct Display { pub factor: f64, pub offset: f64, pub suffix: &'static str, pub decimals: u8 }
// displayed = backend * factor + offset
```

A kernel parameterised `0.0..=1.0` shown as `0..100 %` declares `factor: 100.0, suffix: "%"`. This removes a class of ad-hoc conversion code from the inspector and keeps the **stored** value canonical, so a display-convention change never migrates project data.

### 2.5 Composite widgets

Some kernels take scalars a user thinks of as one object — `warp::perspective` takes four corner points, a crop takes four edges. The manifest may fuse them:

```rust
pub struct Composite { pub widget: CompositeWidget, pub members: &'static [(&'static str, Member)] }
pub enum CompositeWidget { Rect, Point, Corners }
```

The scalars remain the serialized truth; the composite is a **view**. Keyframing a composite writes keys on each member.

### 2.6 Versioning and migration

An effect's `version` bumps whenever a parameter's *meaning* changes — renamed, rescaled, removed, or re-based. Each bump ships a migration:

```rust
pub struct EffectMigration {
    pub id: EffectId,
    pub from: u16, pub to: u16,
    pub forward: fn(&mut EffectParams),
    pub backward: Option<fn(&mut EffectParams)>,   // None ⇒ lossy, refuse downgrade
}
```

Applied at load, in `timeline/load.rs::finalize_load`, alongside the existing orphaned-`PropertyTrack` pass. Rules:

- Migration is **pure and total** over the stored params — no I/O, no document access.
- **Round-trip tested**: `backward(forward(p)) == p` for every migration declaring `backward`.
- A project saved by a newer build carrying an **unknown** effect id loads **inert and preserved** — the effect is retained in the stack, disabled, flagged, and re-serialized unchanged. This is the `GradeOpParams::Unknown` pattern from [07 §1](07-color-grading.md), which is the one place Photonic already does forward-compat correctly, generalised. It is also [27 O-4](27-spec-audit.md#o-4--p1--cap-020--savereopen-and-backward-compatibility)'s open finding.

### 2.7 Generation

The manifest table is the source for: the runtime effect registry · `prop_registry` · inspector widget construction · MCP `list_effect_kinds` and `set_effect_param` schemas · `docs/` effect reference. All generated, all covered by the existing MCP doc-drift CI gate — which is the precedent for generation-as-truth in this repo, and the fix [27 A-10](27-spec-audit.md#a-10--p2--the-mcp-tool-count-is-stated-four-ways-and-matches-nothing) asks for on the tool catalogue.

---

## 3. Kernel binding

```rust
pub struct KernelRef {
    pub wgsl: &'static str,                    // entry point in the effect shader module
    pub cpu: fn(&mut ImageF32, &ResolvedParams),  // reference implementation
}
```

`ImageF32` is the CPU evaluator's existing linear-premultiplied `f32` RGBA buffer (`graph/eval_cpu.rs`), **not** `RasterImage`. §4 defines how a raster kernel is adapted to it.

Both paths are mandatory. The CPU function is the golden oracle and the export-determinism reference ([02 §2](02-engine.md)); an effect with no CPU path cannot be accepted, because [26 E-9](26-kdenlive-mlt-parity.md#e-9--cpugpu-evaluator-equivalence-as-a-bug-class) has already shown what an unpaired GPU path produces.

---

## 4. The raster bridge — operand contract

**This section is the load-bearing part of the document.** Getting it wrong reproduces [27 A-1](27-spec-audit.md#a-1--p0--the-live-canvas-composites-in-gamma-headless-composites-in-linear) and [27 A-3](27-spec-audit.md#a-3--p0--grade-operators-apply-transfer-functions-to-premultiplied-alpha) sixty more times.

### 4.1 The mismatch

| | Raster kernels | Video graph |
|---|---|---|
| Type | `RasterImage { pixels: Vec<u8> }` | `Rgba16Float` texture / `ImageF32` |
| Encoding | **sRGB transfer-encoded** | **linear light** |
| Alpha | **straight** | **premultiplied** |
| Range | `0..=255` per channel | unbounded float, may exceed 1.0 (HDR, D-09) |

### 4.2 The rule

Every ported kernel declares which space its maths is **defined in**, and the adapter converts around it:

```
premultiplied linear f32
  → unpremultiply                     (α > 0; α == 0 ⇒ carry RGB through unchanged)
  → [ if kernel is transfer-defined: encode via the sRGB OETF ]
  → run kernel
  → [ if encoded: decode via the sRGB EOTF ]
  → repremultiply
```

Declared per manifest:

```rust
pub enum OperandSpace { LinearStraight, TransferStraight }
```

**Which kernels are which** — the classification is not optional and not guessable:

| Space | Kernels | Why |
|---|---|---|
| `LinearStraight` | blurs, resamples, warps, geometry, `add_noise`, mosaic, median, high-pass, motion blur | Spatial averaging and sampling are only physically correct in linear light. Blurring gamma-encoded pixels is the classic halo bug |
| `TransferStraight` | `levels`, `curves`, `brightness_contrast`, `posterize`, `threshold`, `photo_filter`, `channel_mixer` | Defined against a perceptual code-value ramp; running them linear changes their meaning and breaks user expectation from the photo editor |

Exposure, white balance and anything defined in stops stay **linear** and must *not* be encoded — this mirrors [07 §3](07-color-grading.md)'s existing rule that the enc/dec pair wraps CDL/Wheels/Contrast/LUT and never Exposure/WhiteBalance.

### 4.3 Precision and range

Raster kernels are `u8`. Ported kernels operate on `f32` and **must not clamp to `0..=1`** unless the operation is definitionally bounded (threshold, posterize). Clamping is what would silently destroy HDR headroom under D-09's `LinearRec2020Hdr`. Where a ported kernel's reference implementation clamps, the port removes the clamp and the manifest records that it did.

### 4.4 Sharing the maths

The CPU path **calls the existing `raster::` function** wherever the kernel is pure and shape-compatible, exactly as `graph/ops.rs:13` already does for `blend_rgb`. Where the signature differs (`&mut RasterImage` vs `&mut ImageF32`), extract the inner per-pixel or per-kernel routine into a shared generic rather than duplicating it — a copied kernel will drift, and the photo editor's tests will not catch the drift in the video path.

### 4.5 Not portable

Interactive and seed-dependent kernels are **out of scope** as timeline effects: `healing_brush`, `spot_healing`, `content_aware_fill`, `liquify_*`, `red_eye`. They take user gestures or produce non-deterministic output and would break export determinism (SS-3).

---

## 5. Catalogue

Tiered by value-per-effort. **Tier 1 is the shipping target**; Tiers 2–3 are recorded so the manifest schema is validated against real breadth before it freezes.

### 5.1 Tier 1 — port first

| Id | Source | Params | Space | GPU |
|---|---|---|---|---|
| `blur.gaussian` | `filter::gaussian_blur` | `radius` 0..200 | Linear | separable, 2 passes |
| `blur.box` | `filter::box_blur` | `h_radius`, `v_radius` 0..200 | Linear | separable |
| `blur.motion` | `filter::motion_blur` | `angle` 0..360°, `distance` 0..500 | Linear | directional |
| `sharpen.unsharp` | `filter::unsharp_mask` | `radius`, `amount`, `threshold` | Linear | blur + combine |
| `sharpen.smart` | `advanced::smart_sharpen` | `amount`, `radius`, `threshold` | Linear | moderate |
| `color.levels` | `adjust::levels` | in/out black, white, gamma | **Transfer** | per-pixel |
| `color.curves` | `adjust::curves` | curve per channel | **Transfer** | 256-entry 1D LUT |
| `color.hue_saturation` | `adjust::hue_saturation` | `hue`, `saturation`, `lightness` | Linear | per-pixel |
| `color.vibrance` | `adjust::vibrance` | `amount` | Linear | per-pixel |
| `color.channel_mixer` | `adjust::channel_mixer` | 3×3 + constants | **Transfer** | per-pixel |
| `stylize.vignette` | `advanced::vignette` | `amount`, `feather`, `x`, `y`, `roundness` | Linear | radial |
| `stylize.grain` | `filter::add_noise` | `amount`, `size`, `monochrome` | Linear | hash noise |
| `stylize.chromatic_aberration` | `advanced::chromatic_aberration` | `amount` | Linear | 3 sampled offsets |
| `stylize.mosaic` | `filter::mosaic` | `block` | Linear | per-pixel |
| `stylize.posterize` | `adjust::posterize` | `levels` 2..255 | **Transfer** | per-pixel |
| `stylize.threshold` | `adjust::threshold` | `level`, `use_alpha`, `invert` | **Transfer** | per-pixel |
| `stylize.find_edges` | `filter::find_edges` | — | Linear | 3×3 |
| `stylize.emboss` | `filter::emboss` | — | Linear | 3×3 |
| `noise.reduce` | `advanced::reduce_noise` | `strength` | Linear | moderate |
| `noise.median` | `filter::median` | `radius` | Linear | expensive |
| `geo.perspective` | `warp::perspective` | 4 corner points (**Corners** composite) | Linear | one homography |
| `geo.pinch` / `.spherize` / `.ripple` / `.twirl` | `warp::*` | per kernel | Linear | UV remap |
| `key.luma` | new | `threshold`, `slope`, `pre_level`, `post_level` | Linear | smoothstep |
| `key.spill_suppress` | new | `amount` | Linear | per-pixel |
| `util.alpha_view` | new | `mode` ∈ Alpha, Premul, Straight | — | §7 |
| `util.unpremultiply` | new | — | — | per-pixel |
| `util.drop_shadow` | new | `color`, `radius`, `x`, `y` | Linear | alpha offset + blur |
| `util.outline` | new | `color`, `thickness` | Linear | **SDF** |

`util.outline` and rounded-corner cropping should use **signed distance fields**, not the reference's rasterised dilation: analytic antialiasing, smooth at hard angles, no thickness ceiling. The reference's own documentation apologises for its version's behaviour at corners.

Six existing kinds (`Blur`, `Sharpen`, `Glow`, `ChromaKey`, `LumaKey`, `MaskShapeGen`) are **already declared and unrendered** — they map onto this table and are closed by [26 K-0.2](26-kdenlive-mlt-parity.md#8-k-0--foundations), not added as new work.

### 5.2 Tier 2

`advanced::{surface_blur, lens_blur, clarity}` · `filter::high_pass` · `adjust::{color_balance, photo_filter, black_and_white}` · `repair::dust_and_scratches` · gradient map · HSL primaries and HSL range (secondary correction) · chroma-hold · strobe · film-damage set (grain variants, scratch lines, gate weave).

### 5.3 Tier 3 — do not build

Stylistic novelties easily community-supplied once the manifest exists, and reference services that are **anti-patterns rather than features**: a filter that only tags a frame with a blend mode (blend mode belongs on the clip node — Photonic already has this right), a filter that wraps a producer plus a transition (a node graph replaces it), and sentinel-pair masking (§7 does it properly).

---

## 6. Luma-map wipes

Closes [26 K-B7](26-kdenlive-mlt-parity.md#k-b7--luma-map-wipes). A wipe map is a greyscale image whose pixel value is **the point in the transition at which that pixel switches**; black switches first, white last.

```rust
pub enum WipeSource { BuiltIn(WipeKind), Asset(AssetId) }
pub enum WipeKind { Bar, Iris, BarnDoor, Clock }
```

```wgsl
let m = textureSample(map, samp, uv).r;
let a = clamp((t + soft - m) / max(soft, 1e-5), 0.0, 1.0);
return mix(a_tex, b_tex, a);
```

- **Generate the built-ins analytically in WGSL**, not as shipped assets. All four base patterns are closed-form; a generated map is resolution-independent and full-precision, where the reference's shipped 720×576 PGMs band visibly at 4K. Band count, serpentine alternation, mirroring and inversion are uniforms, giving the reference's ~22 named presets from four kernels.
- **Import** accepts binary `P5` PGM, 8- or 16-bit (8-bit promoted `v << 8`), so users' existing map libraries work. PNG accepted likewise.
- `softness` is **animatable** — the reference's is not, and a ramping softness is a real editorial control.
- **Rights:** built-ins are generated from Photonic's own maths. No map is copied from any GPL project's asset set — [23 §1](23-legal-open-source-implementation-routes.md#1-purpose-and-authority)'s rule that a code licence does not licence a project's assets applies directly.

---

## 7. Masking as a nested subgraph

Closes [26 K-B8](26-kdenlive-mlt-parity.md#k-b8--nested-subgraph-masking). The user-facing primitive — *apply this run of effects only inside this animated region* — is right; the reference's **implementation** is a bracketing pair of sentinel filters in a flat list, which makes ordering implicit, breaks under reordering, and is incompatible with parallel rendering.

Photonic has a real DAG, so:

```rust
pub struct MaskedGroup {
    pub mask: MaskSource,
    pub invert: bool,
    pub feather: f32,
    pub effects: Vec<ClipEffect>,     // nests
    pub op: MaskOp,                   // Over | Add | Subtract | Min | Max
}
pub enum MaskSource {
    Shape(WindowShape),               // existing GradeMask windows
    Matte(MatteRef),                  // existing photonic-matte path
    GraphNode { graph: GraphId, node: GraphNodeId },
    Path(PathRef),                    // §7.1
    Luma { source: WipeSource, threshold: f32, softness: f32 },
}
```

Compiles to: evaluate input once → evaluate the mask to an alpha texture → evaluate the effect chain over the input → composite the result over the unmodified input by mask alpha. Ordering is **structural**, nesting composes, and the whole group is one content-hashed node so caching works unchanged.

### 7.1 Roto splines reuse the vector editor

`MaskSource::Path` references Photonic's existing bezier path model with `AnimProps` on control points, edited with the existing pen and direct-select tools, tessellated by the existing tessellator. This is the clearest case in the document where the vector heritage is a decisive advantage — the reference implementations all had to invent a spline editor.

### 7.2 Alpha view

`util.alpha_view` is a **monitor view mode**, not a stack effect, sharing the present path with [26 K-B5](26-kdenlive-mlt-parity.md#k-b5--compare-effect-split-view)'s split compare. Modes: show alpha as luminance · show premultiplied RGB · show straight RGB. Judging a key against black is guesswork, and this is the cheapest fix in the document.

---

## 8. Acceptance

Every effect, before it is accepted:

1. **CPU/GPU parity** — same input, both evaluators, within tolerance. Extends [26 E-9](26-kdenlive-mlt-parity.md#e-9--cpugpu-evaluator-equivalence-as-a-bug-class)(b)'s sweep, which must iterate **every variant of every enum the manifest declares**.
2. **Partial-alpha fixture** — non-opaque input, asserting the §4.2 operand contract. `grade.rs:14-16` records that *every* existing golden fixture is opaque, which is precisely why A-3 went unnoticed; an all-opaque corpus cannot validate this work.
3. **Scale invariance** — Draft vs downsampled Full, per [26 E-6](26-kdenlive-mlt-parity.md#e-6--preview-scale-invariance-is-a-bug-class). Any effect with a pixel-denominated parameter (radius, distance, offset) must scale it; this test is what catches the ones that forget.
4. **Identity at default** — default parameters produce a pixel-identical passthrough, except where the effect is definitionally non-identity (threshold, find-edges). Cheap, and it catches sign and range errors immediately.
5. **Range validation** — out-of-range parameters are **refused**, not clamped, and surface a `CompileDiagnostic` ([27 U-2](27-spec-audit.md#5-u---under-specified-contracts) owes that type a definition).
6. **Determinism** — same inputs, byte-identical output across runs (SS-3). Noise kernels take an explicit seed from the manifest, never a clock or an address.
7. **Migration round-trip** where the manifest declares `backward`.
8. **MCP parity** — the generated schema exposes the effect and its params; goal-backward L1–L4 per [ROADMAP §10](ROADMAP.md#10-definition-of-done).

---

## 9. Sequencing

| Step | Gate |
|---|---|
| 0 | **Decide [27 A-1](27-spec-audit.md#a-1--p0--the-live-canvas-composites-in-gamma-headless-composites-in-linear) and [27 A-3](27-spec-audit.md#a-3--p0--grade-operators-apply-transfer-functions-to-premultiplied-alpha)**, and add the partial-alpha fixture. Nothing below starts first |
| 1 | Manifest schema + generation + `prop_registry` projection; **zero behaviour change**, existing 7 kinds re-expressed as manifests |
| 2 | Close [26 K-0.2](26-kdenlive-mlt-parity.md#8-k-0--foundations) — render the six declared-but-passthrough kinds *through* the new path, proving it on effects that already exist |
| 3 | Acceptance harness (§8 items 1–4) green on those six |
| 4 | Tier-1 port, family by family, each with its full §8 set |
| 5 | Luma wipes (§6), masking subgraph (§7) — each needs its own contract review before code |
| 6 | Applicability lands with [26 K-B1](26-kdenlive-mlt-parity.md#k-b1--track-and-master-effect-stacks)'s track/master stacks |

Step 2 is the load-bearing one: it proves the manifest path on **known** effects before any new maths is introduced, so a failure at step 4 is unambiguously the port and not the framework.

---

## 10. Compatibility

- **Additive serde.** `ClipEffect` gains `id: EffectId` and `version: u16`; `kind: EffectKind` is retained and deprecated for one format version, populated by projection from `id` so older builds keep loading. Migration is part of the **single consolidated v4→v5 step** inventoried in [01 §9.1](01-data-model.md#91-the-v4--v5-migration--one-step-nine-changes) — it is one of nine model changes that must land together.
- **Undo.** One user verb — add, remove, reorder, set-param — is one undo unit, unchanged. Manifest migration at load is **not** an undoable edit and must not enter the history.
- **Protected surfaces.** The existing effect stack UI, keyframe model and `AnimProps` evaluation are unchanged by this document. `EffectParams`' ordered-`Vec` representation is protected — [26 E-8](26-kdenlive-mlt-parity.md#e-8--protected-properties-that-are-already-right) records why (deterministic serialization for SS-3 and stable undo diffs).
