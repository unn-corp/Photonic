# 197 — K-B8 Nested-Subgraph Masking

> **Status: Proposed — Band-5 mini-spec, pre-code.**
> [26 §19.1](../specs/video-editor/26-kdenlive-mlt-parity.md#191-bands) makes an accepted
> mini-spec the exit condition for every K-Band 5 item: it must name the data-model
> change, migration, undo unit, MCP surface and acceptance fixtures *before* code.
> This document discharges that for **K-B8**
> ([26 §10](../specs/video-editor/26-kdenlive-mlt-parity.md#k-b8--nested-subgraph-masking)).
> No code authorization until accepted
> ([23 §14](../specs/video-editor/23-legal-open-source-implementation-routes.md#14-stopgo-checklist-before-any-code)).

**Owner ref:** 26 §10 K-B8 · design sketch in [30 §7](../specs/video-editor/30-effect-catalogue.md#7-masking-as-a-nested-subgraph) · **Territory:** `core-timeline` + `photonic-video-engine` · **Effort:** L

**Position in the dependency graph** (26 §19.2, lines 848–853): `E-1 → K-B8` and
`K-B9 → K-B8`. E-1 **is landed** — `source_range_for_op` / `graph_source_range` /
`SOURCE_RANGE_SOFT_CAP` (`crates/photonic-video/src/graph/source_range.rs:69,79,112`),
consumed by prefetch at `crates/photonic-video/src/session.rs:1180-1183`. K-B9 (roto
splines) is being specified in parallel at
[198](198-k-b9-rotoscoping-spline-masks.md). **This document does not assume a spline
implementation.** §6 defines the *mask-source interface* a source must satisfy; 198
must satisfy it, and §6 is the checklist a reviewer holds 198 to.

Where this differs from 30 §7's sketch it says so and gives the code citation that
forced the difference; those deltas are collected in **Follow-ups**.

---

## 1. Problem and user outcome

The user-facing primitive — *"apply this run of effects only inside this animated
region"* — does not exist in Photonic in any form. 26 §10 K-B8 is right that the
requirement is good and that the reference implementation's bracketing-sentinel-pair
approach is the wrong shape for a DAG. What 26 does not say, and what §2 establishes,
is sharper: **Photonic today has no working way to apply anything to part of a frame
except a static elliptical power window inside a grade op.** Every other mask in the
catalogue is either inert, or unreachable, or cannot be animated.

After this item, a user can:

1. In the node editor, select a run of nodes, choose **Wrap in Mask**, wire a mask
   into the new node's `mask` port, and see those nodes' effect applied **only where
   the mask is**, composited back over the untouched input.
2. Keyframe the mask. Today no mask anywhere in Photonic is animatable (§2.2 item 4);
   after this, a mask that is a graph node animates through the same `AnimProps`
   machinery as every other node param.
3. **Nest** mask groups and get containment for free: an inner group can never change
   a pixel its enclosing group leaves alone. §7.4 proves this is a consequence of the
   composite's algebra, not a policy that has to be enforced.
4. Point a **grade op** at a graph mask — `GradeMask::RotoMatte { source:
   MaskRef::GraphNode { .. } }` — and have it do something. That enum has shipped
   since v5 and resolves to `None` (full frame) today
   (`crates/photonic-render/src/grade.rs:494`). K-B8 is what makes it real.
5. Use a clip's own alpha as a mask via `ChannelSplit`, which is currently a
   passthrough on both evaluators (§2.2 item 2).
6. Drive all of it over MCP with existing tools — §8 shows the surface is almost
   entirely free, because `add_graph_node` and `set_grade` already take free-form
   payloads.

What the user **cannot** do after this item, and the UI must not imply: mask a run of
effects from the flat clip effect-stack inspector without entering a composition. §10
argues that exclusion and recommends the follow-up that closes it.

---

## 2. Current state in code

Verified against `8a33f32` on 2026-07-28.

### 2.1 What exists and works

| # | Primitive | Where | What K-B8 gets from it |
|---|---|---|---|
| 1 | `NodeGraph` / `GraphNode` / `GraphEdge` / `GraphOp`, one arena on `TimelineProject.graphs`, per-clip compositions and the project graph | `crates/photonic-core/src/timeline/graph.rs:44-49,122-187,222-261` | The nesting substrate. 26 K-B8's "Photonic has a real DAG" is literally true and already serialized |
| 2 | Intra-graph cycle refusal at edit time — `would_create_cycle` (`graph.rs:347`), enforced in `graph_ops::add_edge` (`crates/photonic-core/src/timeline/graph_ops.rs:53-70`), returning `EditError::WouldCreateCycle` | — | No invalid edge is representable *within* one graph. §7.5 records what this does **not** cover |
| 3 | Multi-input lowering, proven by `Merge`: `input_source(graph, node, InPort::A/B)` and per-port defaults (`crates/photonic-video/src/graph/compile.rs:1729-1735,1840-1868`) | — | The three-input `MaskComposite` lowering is the same shape, one port wider |
| 4 | `Builder::push` deduplicates by content hash before allocating an `IrNodeId` (`compile.rs:443-460`) | — | A mask branch and a base branch sharing one upstream source lower to **one** decode node, not two. Masking costs one extra pass, not a second decode |
| 5 | `ContentHash` = xxh3-128 of `(op, resolved params, input hashes)` (`crates/photonic-video/src/graph/ir.rs:34-38`, `compile.rs:2568-2581`), with `hash_resolved_params` walking the ordered param `Vec` (`compile.rs:2722-2754`) | — | Mask caching is free and exact — §7.2 |
| 6 | E-1: `source_range_for_op` is a **wildcard-free** match over `IrOp` (`source_range.rs:79-107`); `graph_source_range` unions it (`:112-118`); `SOURCE_RANGE_SOFT_CAP = 16` (`:69`); consumed by `combined_prefetch_lead` (`session.rs:1180-1183`) | — | A new `IrOp` variant **fails to compile** until someone declares its range. §7.1 |
| 7 | E-4: `threading_for_op`, also wildcard-free (`ir.rs:133-162`) | — | Same forcing function for the threading declaration |
| 8 | `Effect{MaskShapeGen}` is a **real** 0-input ellipse-matte generator on **both** evaluators, with a GPU/CPU parity test — CPU `ops::mask_shape` (`crates/photonic-video/src/graph/ops.rs:608-654`), WGSL twin (`crates/photonic-video/src/graph/eval.rs:1609-1625`, dispatch at `:1100-1118`), parity at `eval.rs:4106-4130` | — | One working mask source exists on day one, so §6's interface has a live implementor before 198 lands |
| 9 | The mask-as-texture convention already ships: `ops::mask_shape` writes `[a,a,a,a]` (`ops.rs:651`) and the WGSL returns `vec4<f32>(a,a,a,a)` (`eval.rs:1622-1624`) | — | §3.3 writes down a convention rather than inventing one |
| 10 | `IrOp::WipeMix` is documented as "a premultiplied lerp between the two layers" (`ir.rs:228-238`), CPU `ops::wipe`, WGSL twin | — | `MaskComposite`'s maths is the same lerp with a texture weight instead of a swept edge. The kernel has a working sibling to copy |
| 11 | `GradeMask::PowerWindow` is real end-to-end: `ResolvedMask::weight` on CPU (`crates/photonic-render/src/grade.rs:337-374`) and `window_weight` in every grade shader (`crates/photonic-render/src/grade_gpu.rs:240,302,395`) | — | The one working mask in the product, and the counter-example §3.4 argues from |
| 12 | Node-editor port typing exists as data: `PortType::{Image, Mask}` and `op_ports` (`crates/photonic-gui/src/panels/video/node_editor.rs:65-76,95-146`) — `MaskShape` declares `out:Mask`, `MaskFromMatte` `in:Image → out:Mask`, `ChannelSplit` four `Mask` outputs | — | The UI vocabulary for a `mask` port already exists. §9 test 12 covers the gap at item 2.2/6 |
| 13 | `GraphCmd` covers every graph edit with an exact inverse, including `AddNode { edges }` / `RemoveNode { edges }` carrying the incident edge set (`crates/photonic-core/src/timeline/commands.rs:189-236,957-1006,1010-1031`; capture at `graph_ops.rs:28-51`) | — | §5: K-B8 adds **no new command variant** |
| 14 | `GraphOp::Unknown(serde_json::Map)` — verbatim-preserving forward-compat variant that lowers to passthrough of its primary input (`graph.rs:181-186`, `compile.rs:2016-2029`), covered by `crates/photonic-core/tests/forward_compat.rs` | — | §4: this is why K-B8's home in `GraphOp` is what keeps it additive **and** lossless in an older build |

### 2.2 What is inert, unreachable, or absent — stated plainly

This is the honest list, and each row changes a decision below.

1. **There is no op anywhere that composites one image over another through a mask.**
   `grep -rn 'MaskedGroup\|MaskApply\|mask_apply\|MaskComposite\|mask_group' crates/`
   returns **zero hits** (the only hit in the tree is 30 §7's Rust sketch at
   `docs/specs/video-editor/30-effect-catalogue.md:287`). `IrOp::Merge` is binary and
   has no mask input (`ir.rs:222-227`; 08 §3.2 "Binary only").

2. **`ChannelSplit` and `ChannelCombine` are passthrough on both evaluators.**
   CPU: `IrOp::ChannelSplit { .. } => in0()`, `IrOp::ChannelCombine => in0()`
   (`crates/photonic-video/src/graph/eval_cpu.rs:215-216`). GPU: neither has an arm,
   so both fall into the blit-passthrough `_` at `eval.rs:1149-1153`. The two
   evaluators agree — both are wrong. The compiler additionally cannot route the four
   out-ports and always emits alpha (`compile.rs:1932-1943`).

3. **`MaskFromMatte` → `IrOp::MatteExtract` is inert on both paths** — CPU `=> in0()`
   (`eval_cpu.rs:214`, "P8 U²-Net inference"), GPU falls into the same blit `_` arm.

4. **No mask anywhere in Photonic can be animated.** `GradeOp` carries
   `params: AnimProps<GradeOpParams>` **and** a separate, un-animated
   `mask: Option<GradeMask>` (`crates/photonic-core/src/timeline/grade.rs:44-56`);
   `resolve_mask` takes no `tick` argument at all
   (`crates/photonic-render/src/grade.rs:477`); and `prop_registry` declares **no**
   `mask.*` property path in any grade block
   (`crates/photonic-core/src/timeline/prop_registry.rs:116-156`). 26 K-B8's word
   *animated* is unmet today by construction.

5. **`GradeMask::RotoMatte` resolves to `None` — full frame.**
   `crates/photonic-render/src/grade.rs:494`, with the doc comment at `:475-476`
   saying so. `MaskRef` (`grade.rs:273-286`) is, today, dead data: nothing in
   `photonic-video` reads it (`grep -rn 'MaskRef' crates/photonic-video/` is clean).

6. **The node editor never refuses an incompatible wire.** `port_type`
   (`node_editor.rs:678`) is referenced only from the wire-colouring code
   (`:1089,:1105`). 08 §3.1's "refuse an incompatible drop — no invalid edge is ever
   representable" is **not implemented**; only the intra-graph cycle check is.

7. **`GraphOp::MaskShape`'s `MaskShapeKind` never reaches the kernel.** The shape is
   structural in the enum (`graph.rs:78-88,154-156`) but is not carried into
   `ResolvedParams`, so "every mask-shape node renders as an ellipse for now"
   (`ops.rs:612-614`). A rectangle window is unreachable in the graph.

8. **`Invert` does not invert a mask.** `ops::invert` inverts straight RGB and leaves
   alpha untouched (`ops.rs:201-220`). On the `[a,a,a,a]` mask convention that yields
   `[0,0,0,a]` — coverage unchanged. 08 §2's "generic over Image/Mask (two thin
   op-kind variants)" has one of the two variants.

### 2.3 Four facts that change a decision below

**(a) The content hash does not encode the eval canvas — but the *cache* does.**
`GpuEvaluator::evaluate(&graph, canvas, source)` takes `canvas` as a runtime argument
(`eval.rs:465-471`), so one `ContentHash` describes both a Draft and a Full render.
For K-B8 this is **contained**: `NodeCache::lookup_or_alloc` treats an entry as valid
only when `self.rendered.get(&hash) == Some(&desc)` (`crates/photonic-video/src/graph/cache.rs:98`),
so a mask rendered at the Draft canvas is never served at Full — it re-renders.
Consequence: **K-B8 persists nothing to disk**, and §10 records "do not persist a mask
texture" as a standing rule so this containment is not silently lost later. This is
consistent with [193 §2.3(a)](193-k-a1-chunked-timeline-preview-rendering.md), which
raises the same fact for a cache that *does* outlive the session.

**(b) Working textures are pool-bucketed; masks must therefore be told their logical
size.** The pool allocates at `TextureDesc::bucket()` — dimensions rounded up to a
multiple of 64 (`ir.rs:49-56`, `crates/photonic-video/src/pool.rs:131-138,222-228`) —
and passes carry `logical_w/logical_h` separately. The `MaskShapeGen` pass already
gets this right and documents why (`eval.rs:1617-1621`: "A generator must derive its uv
from `@builtin(position)` and the LOGICAL dims … not from the quad's bucket-spanning
uv, or the ellipse would be placed against the 64px bucket"). Consequence: §6's
interface makes logical-canvas coordinates a hard requirement on every mask source,
and §9 test 5 pins it at a non-multiple-of-64 canvas.

**(c) The GPU grade path does *not* get this right, and the parity sweep cannot see
it.** `apply_grade_op_gpu` sizes its target from `input.width()/height()`
(`grade_gpu.rs:593-595`) — the **bucket** dims, because `eval.rs:1139-1142` hands it
`&src.texture` from the pool — and every masked grade shader evaluates
`window_weight(in.uv.x, in.uv.y, …)` (`grade_gpu.rs:240,302,395`) against a full-quad
uv (`grade_gpu.rs:44-58`). The CPU reference runs at the logical image size
(`eval_cpu.rs`'s `Image` is never bucketed). So a `PowerWindow` lands in a different
place on the two evaluators — at 1920×1080 the bucket is 1920×1088, a ~0.74% vertical
offset. `tests/cpu_gpu_parity.rs` cannot catch it: its grade rows use `mask: None` and
its canvas is `(8, 8)` (`crates/photonic-video/tests/cpu_gpu_parity.rs:52`), which
buckets to 64×64. This is an **E-9-class divergence in shipped code**, adjacent to but
not caused by K-B8. §9 test 9 adds the row that exposes it; the fix is Follow-up 4,
because it is a grade-path bug and folding it into K-B8 would hide it.

**(d) `GraphOp` has an unknown-preserving variant; `Vec<ClipEffect>` does not.**
`GraphOp::Unknown(serde_json::Map)` retains the whole object verbatim and re-emits it
(`graph.rs:181-186`), which is what makes §4 additive with no data loss. `ClipEffect`
is a plain struct with no unknown-field capture, and the effect stacks are
`Vec<ClipEffect>` in four places (`clip.rs:620-644`; stacks resolved by
`ops::effect_stack`, `crates/photonic-core/src/timeline/ops.rs:1490-1508`). A new
element *shape* in those vectors is not something an older build can preserve. This is
the decisive argument in §3.1 and §4.

---

## 3. Data-model change

### 3.1 The decision: K-B8 lands in the node graph, not in the flat effect stack

26 K-B8's Files row offers two homes: "`core/src/timeline/effect_kind.rs` (`ClipEffect`
becomes a small tree, or a `MaskedGroup` variant)". **Neither is taken.** K-B8 lands as
one new `GraphOp` variant. Three reasons, in order of weight:

1. **Forward compatibility is decided by this choice, not by the format version.**
   §2.3(d): a `GraphOp` this build does not know is preserved verbatim and lowers to
   passthrough. A `Vec<ClipEffect>` element this build does not know is a
   deserialization failure or a silent drop on save — and 39 §2.3's "warn before the
   first save" machinery only fires on a *higher* `format_version`, which §4 argues
   K-B8 must not claim. Putting the group in the graph makes an older build render it
   inert and re-save it losslessly, which is exactly 39 §2.2's contract.
2. **It is what 26 asks for.** "Photonic has a real DAG. Model it as a **nested
   subgraph node** … Ordering becomes structural, and nesting composes." A tree
   embedded in a flat ordered list is the reference implementation's implicit-ordering
   problem restated one level down, not solved.
3. **Nesting is free.** §7.4 shows containment falls out of the composite's algebra
   when nesting is topology. In a list-of-trees it would have to be an enforced
   invariant with its own validation and its own failure mode.

The cost is honest and stated in §1: masking a run of effects requires a composition.
§10 open question 1 recommends the follow-up that adds the inspector sugar, and names
the one thing that blocks it today (`GraphOp` cannot carry a manifest `EffectId`, so
37 of the 44 catalogue effects have no node form).

### 3.2 The one new variant

```rust
// crates/photonic-core/src/timeline/graph.rs — appended to GraphOp (graph.rs:122-187),
// declared BEFORE the `#[serde(untagged)] Unknown` arm so serde tries it first.

/// K-B8: composite a processed branch back over its unmodified input, weighted
/// by a Mask input. Inputs: `base:Image` (0), `over:Image` (1), `mask:Mask` (2).
/// Animatable knobs (`params.opacity`, `params.invert`) live in
/// [`GraphNode::params`], per this module's structural-vs-animatable rule
/// (graph.rs:119-121). No structural payload — the mask is a wire, not a field.
MaskComposite,
```

Everything else it needs already exists:

| Concern | Carrier | Why not a struct field |
|---|---|---|
| Which mask | The edge into `InPort(2)` | A `GraphId`/`GraphNodeId` field would duplicate the edge and could disagree with it. 30 §7's `MaskSource::GraphNode { graph, node }` is a reference *into* the same arena the edge already addresses |
| Opacity | `params.opacity: Float` in `AnimProps<GraphNodeParams>` (`graph.rs:227,240-249`) | Animatable knobs live in params — the module's own documented rule |
| Invert | `params.invert: Bool` | Needed because `Invert` does not invert a mask (§2.2 item 8). `PropValue::Bool` is already a registry kind (`effect_kind.rs:112`) and is already hashed (`compile.rs:2744-2747`) |
| Nesting | Graph topology | §7.4 |
| Mask combination (union/intersect/subtract) | **Not in K-B8** — §6.3 assigns it to 198 | A single group takes exactly one mask wire; arithmetic only becomes necessary when one source produces several shapes |

The IR twin:

```rust
// crates/photonic-video/src/graph/ir.rs — appended to IrOp (ir.rs:179-283)
/// K-B8 masked-group composite. Inputs: [base, over, mask]. Premultiplied
/// linear lerp — the same algebra as `WipeMix` (ir.rs:228-238) with a texture
/// weight instead of a swept edge:
///     w   = clamp(mask.a, 0, 1); if invert { w = 1 - w }; w *= opacity
///     out = base + (over - base) * w        // all four channels, premultiplied
/// `w == 0` ⇒ `out == base` exactly; `w == 1, opacity == 1` ⇒ `out == over`.
MaskComposite { opacity: f32, invert: bool },
```

`opacity` and `invert` are compile-time-resolved scalars, so distinct keyframed values
are distinct content hashes with no extra machinery, exactly as `WipeMix.t` is
(`ir.rs:232-233`).

**Port defaults — every incomplete wiring is a no-op.** This is the rule, not a
convenience:

| Port | Unwired ⇒ | Consequence |
|---|---|---|
| 0 `base` | the existing missing-input default (`project_default_or_transparent`, `compile.rs:2066-2087`) | Same as every other unary op |
| 1 `over` | **falls back to `base`** | The op is identity. Not transparent — an unwired `over` would *erase* the picture inside the mask, which is a wrong-pixels failure |
| 2 `mask` | **the node is elided**: lowering returns `base`'s `IrNodeId` directly | A broken group costs nothing at eval **and produces a content hash bit-identical to the ungrouped chain**. §9 test 3 pins that identity |

Eliding rather than emitting `MaskComposite{w=0}` is deliberate: it means "mask
missing" and "no mask group" are the same cache entry, so wiring and unwiring a mask
does not thrash the node cache.

### 3.3 The Mask representation, written down

**A Mask is an `Rgba16Float` working texture with the coverage value replicated into
all four channels: `[c, c, c, c]`, `c ∈ [0, 1]`.** This is not new — it is what
`ops::mask_shape` writes (`ops.rs:651`) and what the WGSL twin returns
(`eval.rs:1622-1624`). K-B8 makes it normative because a second consumer now depends on it.

- `MaskComposite` reads `mask.a`. On the replicated convention `.a == .r`, so a source
  that only fills alpha still works; a source that only fills RGB does not, and §6
  requires replication.
- The convention is self-consistently premultiplied: `[c,c,c,c]` is premultiplied
  white at coverage `c`, so a mask can be blitted, transformed, blurred and resized by
  the existing passes without a special case. That property is why §6 can require
  feathering to be an ordinary `Blur` in the mask branch rather than a knob on every
  source.
- 08 §3.1's `Mask = single-channel float 0–1` is the *type*; this is its
  representation in the single working format the engine keeps (PA "single working
  format in the graph interior", 26 §17 E-8).

### 3.4 Does `GradeMask`'s current shape suffice? — explicit answer

**`GradeMask` does not change. It also does not suffice as K-B8's mask type, and the
reason is structural rather than cosmetic.**

- **It cannot animate.** The window geometry (`center`, `size`, `rotation`,
  `softness`) lives in the enum payload (`grade.rs:253-264`) with no `AnimProps`
  carrier, `resolve_mask` has no `tick` parameter (`photonic-render/src/grade.rs:477`),
  and `prop_registry` declares no `mask.*` path (§2.2 item 4). 26 K-B8 requires an
  *animated* region. A type that structurally cannot be keyframed cannot be the
  carrier.
- **It resolves to a shader uniform, not to pixels.** `ResolvedMask`
  (`photonic-render/src/grade.rs:337-347`) is six floats plus two flags, consumed as a
  closed-form `weight(x, y)` inside each grade shader. That is the right design for a
  grade op and the wrong one for a group, whose mask must be an arbitrary texture. It
  is precisely why `RotoMatte` had to resolve to `None` (`grade.rs:494`) — there was
  no texture path for it to resolve *into*.

**`MaskRef` is the right seam and is reused unchanged.** `MaskRef::GraphNode { graph,
node }` (`grade.rs:285`) already means "a `Mask`-typed output of a node in a
composition graph", which is exactly what `MaskComposite`'s port consumes. K-B8 makes
that seam real in one place (§6.4) and leaves the enum's shape alone; 198 extends it
**additively** with its own variant, which serde's `#[serde(tag = "source")]` tagging
(`grade.rs:279-286`) accepts with no migration.

So: **no new mask data type in K-B8.** The mask is a wire in the graph; `MaskRef` is
how a *grade op* names one.

### 3.5 Three inert things become real

Each is a prerequisite for a usable mask vocabulary, each is small, and each is a
behaviour change to an existing node that a reviewer should see named rather than
discover:

1. **`IrOp::ChannelSplit { channel }` gets a kernel on both paths** — broadcast the
   selected channel to `[c,c,c,c]`. Replaces the CPU passthrough at `eval_cpu.rs:215`
   and adds a GPU arm before the blit `_` at `eval.rs:1149`. Without this there is no
   Image→Mask route at all, and "use this clip's alpha as a mask" is unreachable. The
   compiler's inability to route the four out-ports independently (`compile.rs:1932-1943`)
   is **not** fixed here — alpha stays the emitted channel; multi-out-port lowering is
   excluded (§10).
2. **`GraphOp::MaskShape`'s `MaskShapeKind` reaches the kernel** as
   `params.shape: PropValue::Enum` (`0 = ellipse, 1 = rect`), so a rectangle window
   works and, because `hash_resolved_params` hashes `Enum` (`compile.rs:2748-2751`),
   the two shapes are distinct cache identities. `Polygon` stays unreachable (its
   vertices are structural and static, `graph.rs:84-87`) — excluded, §10.
3. **`GradeMask::RotoMatte` stops resolving to `None`** — §6.4.

`ChannelCombine`, `MatteExtract` and the `Invert`-over-`Mask` variant stay inert; §10
records why and §6 records what they must do when they land.

---

## 4. Migration and format-version impact

**`CURRENT_FORMAT_VERSION` stays at 5. This does not need a v6, and taking one would
be wrong.**

`crates/photonic-core/src/document.rs:117` pins the current version at 5;
`crates/photonic-core/src/migration.rs` defines a `Migration` as a function that
*reinterprets existing data* on the way from N to N+1. K-B8 reinterprets nothing:

- The only serialized change is **one additional `GraphOp` variant**. A v5 file
  containing no `mask_composite` node is byte-identical before and after, and the
  `#[serde(tag = "op")]` tagging (`graph.rs:122-123`) means the new tag simply joins
  the set serde tries.
- `GraphNode.params` is an open `EffectParams` map (`graph.rs:240-249`); adding
  `params.opacity` / `params.invert` entries needs no schema change, exactly as every
  other node's params work.
- **An older build opens a K-B8 file losslessly.** The `mask_composite` object falls
  into `GraphOp::Unknown(serde_json::Map)` (`graph.rs:181-186`), which retains the
  whole object verbatim, re-emits it unchanged on save, diagnoses once per load, and
  lowers to passthrough of its primary input (`compile.rs:2016-2029`). Passthrough of
  `InPort(0)` is `base` — which is **exactly the correct inert degradation**: the older
  build shows the unmasked source rather than the masked result, and never applies the
  group's effects to the whole frame. This is not luck; it is why §3.1 chose this home,
  and it is covered by the existing harness at
  `crates/photonic-core/tests/forward_compat.rs`.
- `COMPAT_WINDOW = 1` (`migration.rs:16`) — bumping to v6 would spend the entire
  window and push every v5 project through a migration that changes nothing.

[193 §4](193-k-a1-chunked-timeline-preview-rendering.md), [194 §4](194-k-a5-general-and-nested-clip-groups.md),
[195 §4](195-k-c1-clip-jobs-framework.md) and [196 §5](196-x-2-opentimelineio-interchange.md)
all reach the same conclusion by the same rule. **Bump only when data must be
reinterpreted.** Required migration work is therefore one round-trip test, not a
migration (§9 test 11).

---

## 5. Undo unit

**K-B8 adds no new `Command` or `TimelineCmd` variant.** Every verb maps onto a shipped
graph command whose inverse is already implemented and already covered by the
undo-identity sweep in `crates/photonic-core/tests/timeline.rs`.

| User verb | Command | Exact inverse |
|---|---|---|
| Add a Mask Composite node from the palette | `GraphCmd::AddNode { graph, node, pos, edges }` (`commands.rs:190-196`) | `GraphCmd::RemoveNode { graph, node, pos: Some(pos), edges }` (`commands.rs:1015-1020`) — restores the node, its position **and its incident edges**, because `AddNode` carries them |
| **Wrap in Mask** (the headline verb, §5.1) | `Command::Batch` of `[AddNode{MaskComposite, edges: new}, RemoveEdge ×k]` | `Command::Batch` of the members' inverses in reverse order — `[AddEdge ×k, RemoveNode]` |
| Wire a mask into the `mask` port | `GraphCmd::AddEdge { graph, edge }` (`commands.rs:204-207`) | `RemoveEdge { graph, edge }` (`commands.rs:1036-1039`) |
| Unwire it | `RemoveEdge` | `AddEdge` |
| Drag the opacity slider | `GraphCmd::SetNodeParam { graph, node, old, new }` (`commands.rs:212-217`) | the same command with `old`/`new` swapped (`commands.rs:1040-1050`); the gesture coalesces into one entry |
| Keyframe the opacity | `GraphCmd::SetKeyframe { graph, node, path, old, new }` (`commands.rs:218-224`) | `SetKeyframe` restoring `old`, or `RemoveKeyframe` when `old` is `None` |
| Delete the mask source node | `GraphCmd::RemoveNode` (via `graph_ops::remove_node`, `graph_ops.rs:28-51`) | `AddNode` with the captured incident edge set |
| Point a grade op at a graph mask | `TimelineCmd::SetGrade { owner, old, new }` (`commands.rs:632-635`) | the swap (`commands.rs:2476-2479`) |

### 5.1 Why "Wrap in Mask" may be a `Batch`, spelled out

The prompt's standing hazard is real: `TimelineCmd::apply` debug-asserts
`Sequence::validate()` after **every** command (`commands.rs:1748-1758`) and
`Command::Batch` applies members one at a time, so a plural edit expressed as N
singular commands can transiently break an invariant and panic in debug. It is safe
here, and the reason is specific rather than general:

`Sequence::validate` inspects **only** clip duration/ordering/overlap, transitions and
the group tree (`crates/photonic-core/src/timeline/sequence.rs:378-405,414-430,433-470`).
It does not look at `TimelineProject.graphs` at all — graphs live on the project, not
on a sequence. **No member of a Wrap-in-Mask batch can move any quantity
`Sequence::validate` reads**, so every intermediate state is trivially valid.
`Command::Batch` is one undo step by definition (`crates/photonic-core/src/history/mod.rs:2241-2242`)
and `HistoryEntryKind::of` folds its members for classification
(`crates/photonic-core/src/history/tree.rs:38-48`), so K-G5's history surface labels it
correctly.

**Corollary, and this is normative:** §7.5 forbids teaching `Sequence::validate` about
mask references. Doing so would make "delete the mask node, then clear the referrer" a
two-command sequence with an invalid middle — which is precisely the panic this
paragraph exists to avoid.

**No verb in K-B8 produces zero undo units and none produces more than one.** There is
no background completion, no cache verb and no session-only state, so the
`execute_discrete` pattern [195 §5](195-k-c1-clip-jobs-framework.md) needs does not
arise here.

---

## 6. The mask-source interface — what 198 must satisfy

`MaskComposite` consumes a wire, so "what is a mask source?" is a contract on the
producing node, not a type. Any node — `MaskShape`, `ChannelSplit`, a future roto op,
a future luma-wipe op — may drive a `mask` port **iff** it satisfies all seven clauses.
198 must satisfy them; so must K-B7's luma maps, D-12's tracked regions and anything
else that later claims to produce a mask.

### 6.1 The seven clauses

1. **Type.** Its lowering terminates in an `IrOp` whose output is a Mask under §3.3 —
   coverage replicated into all four channels, `[0, 1]`, premultiplied-consistent. The
   node's `op_ports` row (`node_editor.rs:95-146`) declares `PortType::Mask` on that
   out-port.
2. **Hash completeness — the load-bearing clause.** *Every* byte that can change the
   mask's pixels must reach `hash_op` (`compile.rs:2583+`). Spline control points,
   their keyframe-resolved values at the evaluated tick, per-point feather, the shape
   count, and their order all count. The cautionary example is in the tree:
   `vector_state_key` hashes only `(vref discriminant, format size, src_time, asset
   uuid)` (`compile.rs:2519-2540`) and its own doc comment defers referenced-node-state
   hashing — which is why [193 §5.6](193-k-a1-chunked-timeline-preview-rendering.md)
   has to *refuse* to cache any frame containing `RasterVector`. A roto op with an
   incomplete hash would serve a stale mask from `NodeCache` after every shape edit,
   and the failure is invisible. **A mask source that cannot hash its geometry must not
   ship.**
3. **Source range.** It declares its upstream tick requirement in
   `source_range_for_op` (`source_range.rs:79-107`). The match is wildcard-free, so a
   new variant fails to compile until someone decides — use that. A non-temporal mask
   declares `FrameRange::identity(out)`. A tracked mask that reads neighbours declares
   the span and **must stay inside `SOURCE_RANGE_SOFT_CAP = 16`** (`source_range.rs:69`),
   because `graph_source_range` unions across the whole graph (`:112-118`) and the
   result becomes the decode window for *every* branch (§7.1).
4. **Threading.** It declares in `threading_for_op` (`ir.rs:133-162`), also
   wildcard-free. A pure geometric mask is `Threading::Any`; anything holding
   per-instance state is `PerInstance`; anything temporal is `Serial`.
5. **Scale invariance (E-6).** Coordinates are canvas-normalized, or any
   pixel-denominated parameter scales with the canvas. 26 §17 E-6's Photonic-delta row
   names masks explicitly as carrying this hazard (26:737). The kernel must derive uv
   from `@builtin(position)` and the **logical** dims, never from the quad's
   bucket-spanning uv — §2.3(b); `eval.rs:1617-1621` is the worked precedent. It
   registers a row in `crates/photonic-video/tests/scale_invariance.rs`.
6. **CPU/GPU parity (E-9).** A CPU kernel in `graph/ops.rs`, a WGSL twin in
   `graph/eval.rs`, and a row in `crates/photonic-video/tests/cpu_gpu_parity.rs` — with
   the enum it introduces matched **without a wildcard**, so the next variant fails to
   compile rather than shipping unverified. That file's header states this is the whole
   point of the harness.
7. **Bounded failure.** An unresolvable source yields **no mask**, and the group elides
   to `base` (§3.2). It never yields a full-frame mask. §6.2 argues this against the
   spec text that says otherwise.

### 6.2 The failure default is 0, not 1 — a delta from 08 §3.3

08 §3.3 says `MaskFromMatte` emits a "fully-opaque (all-1) mask if `in` unwired **or**
while a matte computation is pending — never blocks or zeroes downstream compositing
while 'computing'." For a *standalone* mask node feeding an author's own wiring that is
defensible. For a **group** it is not: an all-1 mask means "apply the whole effect run
to the entire frame", so a roto file that failed to load, or a matte still inferring,
silently produces a heavily-graded full frame that looks deliberate.

**K-B8's rule: a missing, pending or refused mask makes the group inert.** Output
equals the unmodified input, plus one coded `CompileDiagnostic`. This matches Photonic's
existing failure vocabulary everywhere else — an unknown `GraphOp` is passthrough and
"never guessed" (`compile.rs:2016-2029`), an unknown effect loads `inert` and is skipped
exactly like a disabled one (`clip.rs:638-642`, `compile.rs:1214`), a missing
composition falls back to the plain source with a diagnostic
(`compile.rs:1573-1577`), an offline LUT resolves to identity and "never a black frame"
(`compile.rs:1258-1260`). Nothing happening is visible; something happening everywhere
is not. Follow-up 3 amends 08 §3.3 to scope its all-1 rule to the standalone node.

### 6.3 What K-B8 does **not** ask 198 for

- **Mask arithmetic** (union / intersect / subtract across several shapes). A group
  takes exactly one mask wire and nesting composes (§7.4), so K-B8 never needs it. 198
  will, because a roto tool has many splines. If 198 introduces it, it must be nodes or
  an op with the same seven clauses, and it **must not** change `MaskComposite`.
  Note for whoever builds it: it cannot be done by wiring two masks into the existing
  `Merge`. `blend_rgb` unpremultiplies both operands, so two `[a,a,a,a]` masks under
  `Multiply` composite to `a + b(1−a)` — a union of coverage, not a product
  (`ops.rs:662-700` — `blend_rgb` at `:698`).
- **A feather knob per source.** Feathering is a `Blur` node in the mask branch, which
  works today on both evaluators and is animatable. This is why §3.2 drops 30 §7's
  group-level `feather`, and why §3.3's replicated-RGBA convention matters — an
  ordinary image pass operates on a mask correctly.
- **An invert knob per source.** `MaskComposite` owns `params.invert` (§3.2), because
  `Invert` does not invert coverage (§2.2 item 8). One inverter, at the consumer.

### 6.4 `GradeMask::RotoMatte` becomes real — the `MaskRef` seam

`MaskRef::GraphNode { graph, node }` is the seam the assignment names, and K-B8 makes
it the *first* real consumer of `MaskRef`. The rule:

> A `GradeOp` whose `mask` is `RotoMatte { source: MaskRef::GraphNode { graph, node },
> invert }` lowers as: split the grade stack around that op; emit
> `Grade{[…before]}` → `X`; emit `Grade{[that op, mask: None]}` over `X` → `Y`; emit
> `MaskComposite { opacity: 1.0, invert }` with inputs `[X, Y, <lowered mask node>]`
> → `Z`; continue with `Grade{[…after]}` over `Z`.

That is semantically identical to a per-pixel weight — which is what a `PowerWindow`
already is, gating one op's contribution — and it reuses the one new op rather than
inventing a texture path inside the grade shaders. `apply_grade`
(`compile.rs:1245-1252`) currently emits one `IrOp::Grade` for the whole stack; it
gains a split at each `RotoMatte`-masked op. Concrete consequences:

- `resolve_mask` (`photonic-render/src/grade.rs:477`) keeps returning `None` for
  `RotoMatte` — correct, because the mask no longer lives inside the grade shader. The
  line that changes is the *compiler's*, not the resolver's.
- `MaskRef::Matte` still resolves to nothing (`MatteExtract` is inert, §2.2 item 3) and
  therefore makes the op inert per §6.2, with the diagnostic naming the reason.
- A `MaskRef::GraphNode` naming a missing graph or node is inert + diagnosed, §7.5.
- 198's new `MaskRef` variant slots into the same lowering with no further change,
  provided it satisfies §6.1.

**Scope honesty:** this is the largest single piece of compiler surgery in the item and
the one a reviewer could reasonably cut. If it is cut, `MaskRef` stays dead and 07
§4.2's stretch stays unshipped indefinitely; K-B8's other five outcomes are unaffected.
The recommendation is to keep it — it is the only thing that makes `MaskRef`'s five-year
placeholder mean something, and it costs no new op.

---

## 7. Evaluation semantics

### 7.1 Source range: the mask is evaluated at the same tick, in the same graph

The mask branch is lowered by the same `lower_node` at the **same output tick** as the
base and over branches (`compile.rs:1737-1756`). It is not a separate evaluation
domain, and `MaskComposite` introduces no time shift of its own:

```rust
// crates/photonic-video/src/graph/source_range.rs:89-106 — joined to the identity arm
IrOp::MaskComposite { .. } => FrameRange::identity(out),
```

Three consequences worth stating because they are the ones people get wrong:

1. **A temporal mask declares its own range and prefetch picks it up for free.** If
   198's roto op reads `[out−1, out+1]`, `graph_source_range` unions it
   (`source_range.rs:112-118`) and `combined_prefetch_lead` widens the decode window
   (`session.rs:1180-1183`). K-B8 writes no prefetch code.
2. **A user who wants a *delayed* mask wires `TimeOffset` into the mask branch.** That
   already works: `GraphOp::TimeOffset` re-lowers its upstream subgraph at `t − offset`
   with content-hash dedup and a soft cap on distinct offsets
   (`compile.rs:1950-1975`). `MaskComposite` must not grow an offset field — one
   mechanism, per E-1's whole argument.
3. **E-1's union is graph-wide, not per-branch.** `graph_source_range` unions every
   node's range for the whole frame, so a lookahead-hungry mask widens the decode
   window for the base branch too. That is conservative and safe (it is a decode
   window, not a correctness input) but it is coarser than a masked graph would like.
   Recorded as Follow-up 6 against 32 §1 rather than fixed here.

### 7.2 Caching: masks are ordinary nodes, and that is the point

A mask node is an `IrNode` like any other. `Builder::push` computes its
`ContentHash` from `(op, resolved params, input hashes)` and dedups before allocating
(`compile.rs:443-460`); `NodeCache` keys the rendered texture on
`(ContentHash, TextureDesc)` (`cache.rs:89-105`). Therefore:

- **One mask driving three groups is one node and one texture** — the dedup at
  `compile.rs:451-453` collapses identical subgraphs across the whole frame.
- **A mask and the frame it masks share their upstream decode.** The base branch and a
  `ChannelSplit`-derived mask branch both hang off the same `DecodeVideo` node, which
  dedups to one decode. Masking costs one extra pass, not a second decode.
- **Undo restores cache validity with no re-render**, because undoing a param edit
  restores the prior resolved params and therefore the prior hash — the same property
  [193 §5.9](193-k-a1-chunked-timeline-preview-rendering.md) relies on.
- **The Draft/Full hazard is contained by the cache, not by the hash** — §2.3(a). The
  standing rule that follows: **K-B8 persists no mask texture to disk.** If a later
  item wants to, it inherits 193 §5.1–§5.3's key/fold/renderer-identity discipline and
  must add the canvas to the key; it does not get to reuse the bare `ContentHash`.

### 7.3 CPU/GPU parity (E-9)

`MaskComposite` gets both halves in the same change, non-negotiably:

- CPU: `ops::mask_composite(base, over, mask, opacity, invert) -> Image`, modelled on
  `ops::wipe`'s premultiplied lerp.
- GPU: a `Passes::mask_composite` pipeline — three textures, a sampler, and a
  `(opacity, invert)` uniform — dispatched from `render_op` (`eval.rs:534-1153`) ahead
  of the blit `_` arm at `:1149`. `logical_w/logical_h` are passed, per §2.3(b).
- A wildcard-free row in `crates/photonic-video/tests/cpu_gpu_parity.rs` sweeping
  `invert ∈ {false, true}` × `opacity ∈ {0, 0.5, 1}` × mask ∈ {all-0, all-1, ramp}.

Same for `ChannelSplit`'s new kernel (all four `Channel` variants) and `MaskShape`'s
rect/ellipse enum. That is three new parity rows, and the harness's existing discipline
— "a NEW variant added without a parity case fails to COMPILE" — is what keeps them
honest.

§2.3(c)'s pre-existing masked-grade divergence is **exposed** by §9 test 9 and **fixed**
by Follow-up 4. K-B8 must not fix it silently inside its own change; a wrong-pixels bug
that has been shipping deserves its own commit and its own test.

### 7.4 Nesting composes, and containment is a theorem

Nesting is topology: an inner group's output feeds an outer group's `over` port.

```
        ┌────────────────────── base ──────────────────────┐
  in ───┼──→ [FX₁] ──→ [inner MaskComposite]──→ [FX₂] ──→ over
        │                     ↑ m_inner                    │
        └──────────────────────────────────→ [outer MaskComposite] ──→ out
                                              ↑ m_outer
```

Let `b` be the base and `w = clamp(m, 0, 1)` (post-invert, times opacity). Then
`inner_out = b′ + (over′ − b′)·w_inner` and
`out = b + (outer_over − b)·w_outer`. Since `outer_over` differs from `b` only on
pixels the inner chain changed, and the inner chain changes nothing where
`w_inner == 0`:

> **Containment.** `out(p) ≠ b(p)` implies `w_outer(p) > 0`. A nested group can never
> change a pixel its enclosing group leaves alone.

This holds for *any* depth, needs no enforcement, and is testable as an assertion over
a random mask pair (§9 test 4). It is the concrete cash value of 26 K-B8's "ordering
becomes structural, and nesting composes", and it is why §3.2 carries no
`MaskCombine`/`MaskOp` enum: 30 §7's `Over | Add | Subtract | Min | Max` would break
containment for three of its five values (a group could paint outside its parent) and
would make "what does this group affect?" unanswerable from the tree. Delta from 30 §7;
Follow-up 1.

**Depth is not artificially capped.** Cycle refusal already bounds the graph
(`graph.rs:347`, `graph_ops.rs:53-70`), each level is one extra pass, and the existing
per-frame compile budget is the honest limit. A fixed nesting cap would be a number
with no derivation.

### 7.5 When a mask subgraph references a node that is later deleted

Three distinct cases, deliberately answered differently:

**(a) The mask node is deleted while wired to a `MaskComposite` port.**
`GraphCmd::RemoveNode` retains only edges not incident to the node
(`commands.rs:969-972`), so the `mask` port becomes unwired, and §3.2's default elides
the group to `base`. The group goes inert; nothing else changes. Undo restores the node
*and* its incident edges, because `graph_ops::remove_node` captured them
(`graph_ops.rs:38-51`). **This already works and needs no code.**

**(b) A `MaskRef::GraphNode { graph, node }` names a graph or node that no longer
exists.** The reference is *not* in the graph's edge list — it is a field inside a
`Grade`, in a different container — so nothing cleans it up. The rule:

> Lowering emits a coded `CompileDiagnostic` (`MaskSourceMissing`, `Warning`) naming
> the graph and node, and the masked grade op renders **inert** (§6.2). The document is
> **not** repaired: the dangling `MaskRef` is left exactly as written.

Not repairing it is the decision, and the reason is undo. Deleting the referenced node
is itself an undoable command; if lowering (or a load-time fixup) rewrote the referrer,
undo would restore the node but not the reference, and the user's grade would come back
unmasked with nothing to point at. Silently editing a document as a side effect of
compiling it is the class of behaviour `lower_composition` already refuses — a missing
composition graph diagnoses and falls back to the plain source rather than clearing
`clip.composition` (`compile.rs:1573-1577`).

**(c) Should `RemoveNode` refuse when a `MaskRef` points at the node?** **No.** The
GUI warns before the delete ("1 grade op references this node") and the delete
proceeds. Two reasons: refusing an edit because of a reference elsewhere in the
document is how an editor starts feeling haunted, and — decisively — the refusal would
have to live somewhere that can see both containers, which means
`Sequence::validate()`. It must not go there: `validate` runs as a debug assertion after
**every** command (`commands.rs:1748-1758`), so "delete the node, then clear the
referrer" would be two commands with a panicking intermediate. §5.1 records the same
constraint from the other direction. `Sequence::validate` learns nothing about masks.

**(d) Cross-graph cycles — a new guard is required.** `MaskRef::GraphNode` may name a
node in a *different* `NodeGraph`. `would_create_cycle` (`graph.rs:347`) walks only
`self.edges`, so it is blind to graph-A-masks-via-graph-B-masks-via-graph-A, and
`LowerCtx` (`compile.rs:1697-1713`) carries a memo keyed on `(GraphNodeId, tick)` plus a
`cycle: HashSet<SequenceId>` that guards **nested sequences only**. Left alone, a
cross-graph mask cycle is an infinite recursion in the compiler. K-B8 therefore adds a
`graph_cycle: HashSet<GraphId>` to the lowering path: entering a graph as a mask source
inserts its id and refuses (inert + `MaskSourceCycle` diagnostic) if it is already
present. Cheap, and it must land in the same change as §6.4, not after it.

**(e) The referenced graph's `ClipIn`.** A graph lowered as a mask source is lowered
with `clip` bound to the **referencing** clip, so a `ClipIn` inside it resolves to that
clip's source — the only reading that has a meaning, and the same binding
`lower_composition` performs (`compile.rs:1590-1602`). In the project graph, `clip` is
`None` and `ClipIn` is already diagnosed and dropped (`compile.rs:1792-1798`).

---

## 8. MCP surface

**GUI/MCP parity holds completely, and almost all of it is already there.** No new tool
is required for the node-level primitive, because the shipped graph tools take
free-form payloads:

| Tool | Status | K-B8 use |
|---|---|---|
| `add_graph_node` | **Existing**, `dispatch.rs:2781`, handler `crates/photonic-mcp/src/handlers/video.rs:6145-6175` | `{"graph_id": …, "op": {"op": "mask_composite"}}`. `op` deserializes straight into `GraphOp` (`video.rs:6147`), and its schema is an open `{"type":"object"}` (`crates/photonic-mcp/src/schema_gen.rs:6198-6206`) — **no schema change** |
| `add_graph_edge` / `remove_graph_edge` | **Existing**, `dispatch.rs:2793,2799` | Wire `base` / `over` / `mask` by `(node, port)` |
| `set_graph_node_param` | **Existing**, `dispatch.rs:2805` | `params.opacity`, `params.invert` |
| `set_keyframe` / `remove_keyframe` | **Existing** | Animate the mask and the opacity |
| `set_grade` | **Existing**, `video.rs:5699-5727` | The whole `Grade` is accepted as free-form JSON (`video.rs:5701-5711`), so `{"mask":{"shape_kind":"roto_matte","source":{"source":"graph_node","graph":…,"node":…},"invert":false}}` already round-trips. **No schema change** |
| `create_clip_composition` | **Existing** | Gets a clip into a graph so the above applies |
| `remove_graph_node` | **Existing**, `dispatch.rs:2787` | §7.5(a) |

Two changes are required, and they are the two that touch `docs/mcp-api.md`:

1. **`add_graph_node`'s description string** (`schema_gen.rs:6197`) enumerates example
   ops; `mask_composite` joins the list, with its three ports named. A description-only
   edit still regenerates `docs/mcp-api.md`, and CI diffs it
   (`.github/workflows/ci.yml:162-167`).
2. **One new tool: `wrap_in_mask`** — `{ graph_id, node_ids[], mask_node_id? }` →
   `{ mask_composite_node_id }`. This is the MCP mirror of §5's headline GUI verb.
   Without it the agent can reach the capability only by replaying the batch by hand,
   which is three or four tools and *four undo steps* — a parity break on ROADMAP §10
   point 4, not just an ergonomic one. One tool, one undo unit, matching the GUI.

Every failing result carries the full `Diagnostic` in its data payload per
[36 §5](../specs/video-editor/36-error-model.md), so an agent that wires an Image into a
`mask` port gets `code`/`subject`/`consequence`, not prose.

**K-H obligation** (26 §16): `wrap_in_mask` lands **with** the GUI verb in the same
change, and `docs/mcp-api.md` regenerates under the existing drift gate.

---

## 9. Acceptance fixtures and tests

**No rights-cleared content is required. K-B8 is not a content-, fixture- or
legal-gated item.** Every test below is either pure (no GPU, no ffmpeg) or runs on
synthetic solid/ramp textures built in-test. The existing corpus at
`crates/photonic-video/tests/fixtures/` is not touched and **no fixture byte is added**,
so 23 §7.2's `AssetRightsManifest` gate is not engaged.

GPU tests use the established adapter-skip convention
(`GpuContext::request_blocking() → None ⇒ eprintln + return`,
`crates/photonic-video/tests/scale_invariance.rs:18-26`).

| # | Test | Where | Proves |
|---|---|---|---|
| 1 | **Composite algebra** — `w = 0 ⇒ out == base` bit-exactly; `w = 1, opacity = 1 ⇒ out == over` bit-exactly; a ramp mask lerps monotonically; `invert` mirrors the ramp | `graph/ops.rs` unit tests (pure) | §3.2's contract, including the two exactness claims the whole feature rests on |
| 2 | **CPU/GPU parity** — `MaskComposite` over `invert × opacity{0, 0.5, 1} × mask{0, 1, ramp}`, max-abs ≤ 1e-3; plus all four `ChannelSplit` channels and both `MaskShape` shapes, each matched **without a wildcard** | `crates/photonic-video/tests/cpu_gpu_parity.rs` | §7.3 / E-9(b) |
| 3 | **Broken group is bit-identical to no group** — compile a chain with an unwired-`mask` `MaskComposite` and the same chain without the node; assert the output node's `ContentHash` is **equal** | `graph/compile.rs` unit test (pure) | §3.2's elision rule and its cache consequence |
| 4 | **Containment under nesting** — random 64×64 masks `m_outer`, `m_inner`; assert every pixel where `out ≠ base` has `w_outer > 0`, at depths 1, 2 and 3 | `graph/ops.rs` unit tests (pure) | §7.4's theorem |
| 5 | **Scale invariance (E-6)** — Draft vs downsampled Full for a `MaskShape → MaskComposite` chain at a canvas that is **not** a multiple of 64 (e.g. 1000×562, bucket 1024×576), so the §2.3(b) bucket hazard is actually exercised | `crates/photonic-video/tests/scale_invariance.rs` | §2.3(b), §6.1 clause 5 |
| 6 | **Source range and threading** — `source_range_for_op(MaskComposite, t) == identity(t)`; `threading_for_op(MaskComposite) == Threading::Any`; a graph with a synthetic temporal mask widens `graph_source_range` and stays under `SOURCE_RANGE_SOFT_CAP` | `graph/source_range.rs` unit tests (pure) | §7.1 / E-1 |
| 7 | **Hash sensitivity** — `opacity`, `invert`, the mask input's hash, and the `MaskShape` shape enum each move the output `ContentHash`; two groups sharing one mask subgraph dedup to **one** `IrNode` | `graph/compile.rs` unit tests (pure) | §7.2 and §6.1 clause 2's discipline |
| 8 | **Grade mask via `MaskRef::GraphNode`** — a two-op grade whose second op carries `RotoMatte{GraphNode}` compiles to `Grade → Grade → MaskComposite → Grade`, and the masked op's contribution is zero where the mask is zero | `graph/compile.rs` + `tests/golden_frames.rs` | §6.4 — the first live use of `MaskRef` |
| 9 | **Masked `PowerWindow` parity at a bucket-crossing canvas** — a `Cdl` op with a `PowerWindow` at a 1000×562 canvas, CPU vs GPU. **Expected to FAIL against today's code** (§2.3(c)); it lands `#[ignore]`d with the Follow-up 4 reference in its attribute, and is un-ignored by that fix | `crates/photonic-video/tests/cpu_gpu_parity.rs` | §2.3(c) — makes a shipping wrong-pixels bug visible instead of latent |
| 10 | **Deletion behaviour** — (a) delete a wired mask node ⇒ group inert, undo restores node + edges; (b) `MaskRef` naming a missing node ⇒ inert + `MaskSourceMissing`, and the document is **byte-identical** after the compile (no silent repair); (c) a cross-graph mask cycle ⇒ inert + `MaskSourceCycle`, and the compile **terminates** | `photonic-core/tests/timeline.rs` + `graph/compile.rs` unit tests | §7.5 (a)/(b)/(d) |
| 11 | **Serde and forward compat** — a v5 doc with a `mask_composite` node round-trips; `CURRENT_FORMAT_VERSION` is still 5; a hand-written doc whose op tag is `mask_composite` loads as `GraphOp::Unknown` in a build with the variant removed, is re-emitted verbatim, and lowers to passthrough of `InPort(0)` | `photonic-core/tests/timeline.rs`, `crates/photonic-core/tests/forward_compat.rs` | §4 |
| 12 | **Port typing** — `op_ports(MaskComposite)` declares `[Image, Image, Mask]` in and `[Image]` out; wiring an `Image` out-port into `InPort(2)` is **refused** by the edit op | `photonic-gui/tests/video_ui_paths.rs` + `graph_ops` unit test | §2.2 item 6 — the one place K-B8 closes 08 §3.1's unimplemented rule |
| 13 | **Undo identity** — Wrap-in-Mask produces exactly **one** history entry; its inverse restores the exact prior node set, edge set and `ui` map; redo reproduces it; an opacity drag coalesces to one entry | `photonic-core/tests/timeline.rs` | §5, §5.1 |
| 14 | **MCP end-to-end** — `create_clip_composition` → `add_graph_node{op:mask_composite}` → `add_graph_edge` ×3 → `set_graph_node_param` → `wrap_in_mask`; assert one undo step per verb and that `docs/mcp-api.md` regenerates clean | `photonic-mcp/src/handlers/video.rs` tests, beside the existing graph tests | §8 |
| 15 | **Export determinism (SS-3)** — export a range containing a nested mask group twice; assert byte-identical output | `crates/photonic-video/tests/export_synthetic.rs` | ROADMAP §10 point 7's hard-gate tier |

Note for the implementer, in the shape [193 §8](193-k-a1-chunked-timeline-preview-rendering.md)
and [195 §8](195-k-c1-clip-jobs-framework.md) both flag:
`crates/photonic-core/tests/diag_catalogue.rs` holds a deliberately frozen
`EXPECTED_WIRE_CODES` list. If the two new codes surface as `Diagnostic`s rather than
compile-only `CompileDiagnostic`s, they must be added there in the same change or the
gate trips — which is the gate working.

---

## 10. Risks, open questions, and deliberate exclusions

### Deliberately out of scope

- **Masking from the flat clip effect-stack inspector.** §3.1 argues the model choice;
  open question 1 recommends the follow-up. The blocker is concrete and worth naming so
  nobody rediscovers it: `GraphOp` has variants for 7 effect kinds
  (`compile.rs:2096-2110`) while the manifest catalogue has 44 entries (`MANIFESTS`, `effect_manifest.rs:535+`), all declared
  `Applicability::CLIP_ONLY` (`crates/photonic-core/src/timeline/effect_manifest.rs:212-225,543+`);
  K-B16's bridged ids reach the compiler only as `EffectKind::Unknown(tag)` inside
  `apply_stack` (`compile.rs:1223-1233`). Promoting a stack into a graph therefore needs
  a `GraphOp::Effect { id: EffectId }` first — real, small, and not K-B8's.
- **`ChannelCombine`'s four-input wiring** (Mask→Image). The reverse direction; K-B8
  needs Image→Mask. Stays passthrough with its existing comment.
- **`ChannelSplit`'s per-out-port routing.** The alpha out-port is what a mask needs;
  routing r/g/b independently is multi-output lowering, which `compile.rs:1932-1943`
  correctly scopes to its own change.
- **`MaskFromMatte` / `MatteExtract`.** Needs `photonic-matte` inference (08 §2's
  "slow node"). It satisfies §6's interface when it lands; until then a
  `MaskRef::Matte`-masked grade op is inert and diagnosed.
- **`MaskShapeKind::Polygon`.** Its vertices are structural and static
  (`graph.rs:84-87`); animated polygon geometry is K-B9's problem, not a second
  half-implementation here.
- **Mask arithmetic** — §6.3, assigned to 198.
- **Persisting a mask texture to disk.** §7.2's standing rule.
- **A nesting-depth cap** — §7.4.
- **Audio.** There is no `Audio` port type in the visual graph (08 §3.1) and K-B8 does
  not introduce one.

### Risks

1. **§6.4's grade-stack split is the riskiest change in the item.** `apply_grade` is
   called at all four effect scopes (`compile.rs:1205-1252`) and any error in the split
   mis-orders a grade. Mitigation: the split is purely structural (same ops, same
   order, different node boundaries), and test 8 asserts the emitted shape as well as
   the pixels. If it slips, cut it — §6.4 records what is lost.
2. **A future mask source ships with an incomplete hash.** This is the invisible
   failure: a stale mask served from `NodeCache` after a shape edit. `vector_state_key`
   (`compile.rs:2519-2540`) is the live example of how it happens. Mitigation: §6.1
   clause 2 is a review gate on 198, and test 7's pattern is the template every source
   copies.
3. **`Sequence::validate` grows a mask rule.** It must not (§5.1, §7.5(c)). A reviewer
   should reject any change that adds mask or graph reachability to `validate`, because
   the debug-assert-after-every-command loop turns it into a panic on a legitimate
   two-command edit.
4. **Scope creep into a compositing application.** `MaskComposite` plus nesting is one
   node away from tracked, arithmetic, garbage-matted roto. `MaskComposite` carries
   two scalars and three ports; reviewers should reject widening it.
5. **§2.3(c)'s divergence is pre-existing and its fix is out of band.** The risk is that
   test 9 lands `#[ignore]`d and stays ignored. Mitigation: Follow-up 4 is filed against
   an owner doc, and the ignore attribute carries the reference.

### Open questions needing a product call

1. **Should the flat effect-stack "Mask these effects…" sugar ship in the same wave?**
   *Recommendation: no — ship it immediately after, once `GraphOp::Effect { id }`
   exists.* Splitting it keeps K-B8's model change to one enum variant and lets the
   sugar be judged as the UX decision it is. The counter-argument is real: until it
   ships, the headline verb lives in the node editor, and users who never open it will
   not find masking. That is a discoverability cost, not a capability gap, and it is
   one release long.
2. **When a mask group is created by Wrap-in-Mask, should the `mask` port start unwired
   (group inert) or pre-wired to a fresh centred `MaskShape` ellipse?**
   *Recommendation: pre-wired to a centred ellipse at 50% size.* An inert node that
   does nothing until you find the second step is the shape of feature nobody uses;
   a visible ellipse makes the mechanism obvious in one click and is one `RemoveEdge`
   away from unwired. This is a UX call with no engineering consequence either way.
3. **Should the node editor refuse an incompatible wire generally, or only on `mask`
   ports?** *Recommendation: only `mask` ports in K-B8, general refusal as a follow-up.*
   08 §3.1 asks for the general rule and §2.2 item 6 shows it was never built; turning
   it on everywhere at once would refuse edges in existing documents that currently
   load, which is a compatibility question deserving its own change. The `mask` port is
   new, so nothing existing can break.
4. **Should `params.invert` be animatable, or a static bool?** *Recommendation:
   animatable, because it is free.* `PropValue::Bool` already flows through
   `AnimProps`, `resolve_effect_params` and `hash_resolved_params`
   (`compile.rs:2744-2747`); refusing to animate it would be extra code. A step-function
   invert at a keyframe is a legitimate edit.

---

## 11. Clean-room provenance

Per [26 §2](../specs/video-editor/26-kdenlive-mlt-parity.md#2-clean-room-and-licensing-fence)
item 2 and 26 §7's per-item requirement:

- **What was read.** Kdenlive's user-facing documentation (`docs.kdenlive.org`,
  `CC-BY-SA-4.0`) for the *existence and shape* of the user workflow — that a
  "mask apply" capability exists, that it scopes a run of effects to a region, and that
  the region can be animated. That is a **requirements source** under 26 §2 item 1:
  cited, never pasted. MLT's published *filter documentation* was consulted only for
  the fact that its equivalent is a `mask_start`/`mask_apply` bracketing pair in a flat
  list — a statement about the published interface, which is what 26 K-B8 already
  records and what this design deliberately does not follow.
- **What was not read.** The Kdenlive source tree, the MLT source tree, and any
  GPL/LGPL derivative. No symbol, constant, parameter name, ordering, control flow or
  test was taken from either. In particular **no bracketing-sentinel mechanism is
  adopted** — §3.1 and §7.4 replace it with graph topology for stated, independent
  reasons (forward-compat via `GraphOp::Unknown`, and containment as an algebraic
  consequence), neither of which has an analogue in the reference. The implementer
  records the
  [23 §3.4](../specs/video-editor/23-legal-open-source-implementation-routes.md#34-clean-room-protocol)
  attestation for this subsystem, and an independent provenance reviewer checks
  identifiers, comments, constants and test provenance before merge.
- **Where the design actually comes from.** Entirely from Photonic's own shipped code:
  the DAG and its cycle refusal (`timeline/graph.rs`), the content-hashed IR and its
  dedup (`graph/ir.rs`, `graph/compile.rs`), E-1's source-range contract
  (`graph/source_range.rs`), E-4's threading declaration (`ir.rs:133-162`), the
  premultiplied-lerp algebra already shipped as `WipeMix` (`ir.rs:228-238`), the
  replicated-RGBA mask convention already shipped as `MaskShapeGen` (`ops.rs:651`,
  `eval.rs:1622-1624`), and the inert-never-guessed failure vocabulary
  (`compile.rs:1573-1577,2016-2029`, `clip.rs:638-642`). 26 §17 E-8 records that MLT has
  no graph object and therefore no way to express any of this; the design is a
  consequence of a property Photonic already holds.
- **Bundled bytes: none.** No asset ships with this item, so 23 §7.2's
  `AssetRightsManifest` gate is not engaged and K-B8 is **not** a legal- or
  fixture-gated item.
- **No new dependency.** Nothing in 26 §2's reject list, directly or transitively.
  Everything needed (`wgpu`, `xxhash-rust`, `serde`, `glam`) is already in the build.
- **No codec, container or patent surface is touched**, so 23 §10.3's
  patent-and-distribution record is not engaged.

---

## 12. Definition of done → ROADMAP §10

| # | [ROADMAP §10](../specs/video-editor/ROADMAP.md#10-definition-of-done) point | Answered by |
|---|---|---|
| 1 | Core op/engine service with unit tests | `IrOp::MaskComposite` + `ops::mask_composite` + the GPU pass + the `ChannelSplit`/`MaskShape` kernels; §9 tests 1, 3, 4, 6, 7 are pure (no GPU, no ffmpeg) |
| 2 | GUI route, or a recorded exception | Node-editor palette entry + **Wrap in Mask** on a node selection, rebindable through `commands.rs`. **Recorded exception: none.** The inspector sugar is a follow-up, not an exception — the capability has a GUI route today (§10 open question 1) |
| 3 | MCP tool/schema/generated docs | §8: one new tool (`wrap_in_mask`) plus one description edit; everything else rides existing free-form tools. `docs/mcp-api.md` regenerates under the drift gate (`.github/workflows/ci.yml:162-167`). **Recorded exception: none** |
| 4 | One user verb = one undo unit | §5: every verb maps to a shipped `GraphCmd` with an existing exact inverse; Wrap-in-Mask is one `Command::Batch` = one history entry (`history/mod.rs:2241-2242`), safe for the reason spelled out in §5.1. Test 13 |
| 5 | Additive serde/migration round-trip | §4: stays v5; one additive `GraphOp` variant; an older build preserves it verbatim and renders it inert. Test 11 |
| 6 | IR/eval/golden/sync coverage for new pixel paths | Three new kernels, each with a CPU reference, a WGSL twin and a wildcard-free parity row (§7.3); goldens via test 8; export determinism via test 15 |
| 7 | Hard gates green; trend metrics not regressed | Hard: parity (2, 9), scale-invariance (5), cache/hash invariants (3, 7), serde (11), undo (12, 13), export determinism (15). Trend: one extra full-screen pass per group is a measurable but bounded eval cost; no soak test is a gate here, per the recorded soak sensitivity of this machine |
| 8 | Offline, privacy, licensing, content, product gates | §11: no bundled bytes, no new dependency, no network, no codec surface. Nothing leaves the process |
| 9 | No protected-surface regression | PA-1 (content-hashed graph) **consumed as designed** — one new node, hashed by the existing machinery, no hash perturbed (§7.2). PA "single working format" preserved by §3.3's replicated-RGBA mask. E-1 and E-4 consumed via their wildcard-free matches (§7.1). E-9 strengthened by three new parity rows **and** by test 9 exposing a pre-existing divergence (§2.3(c)). E-6 strengthened by test 5's non-bucket-aligned canvas |
| 10 | Goal-backward L1–L4, incl. GUI/MCP parity | §1's six outcomes are the L4 script; §8's parity is complete with no exception |

---

## Follow-ups (other documents and code that need a change — **not** made here)

1. **[30 §7](../specs/video-editor/30-effect-catalogue.md#7-masking-as-a-nested-subgraph)**
   — the `MaskedGroup` sketch should be reconciled with §3 above. Specifically:
   `MaskSource::Shape(WindowShape)` cannot carry geometry (`WindowShape` is
   `Ellipse | Rectangle` and nothing else, `grade.rs:266-271`); `MatteRef`, `PathRef`
   and `WipeSource` do not exist in the tree (`MaskRef::Matte` does, `grade.rs:283`);
   `invert` and `feather` duplicate knobs that belong at the consumer and in the mask
   branch respectively (§6.3); and `MaskOp { Over | Add | Subtract | Min | Max }` should
   be dropped, because three of its five values break §7.4's containment property and a
   single group takes exactly one mask wire. The corrected shape is "one
   `GraphOp::MaskComposite`, mask is a wire".
2. **[26 §10 K-B8](../specs/video-editor/26-kdenlive-mlt-parity.md#k-b8--nested-subgraph-masking)**
   — its Files row names `core/src/timeline/effect_kind.rs` (`ClipEffect` becomes a
   tree). §3.1 rejects that home on forward-compatibility grounds; the row should name
   `core/src/timeline/graph.rs`, `graph/ir.rs`, `graph/compile.rs`, `graph/eval.rs`,
   `graph/eval_cpu.rs`, `graph/source_range.rs`.
3. **[08 §3.3](../specs/video-editor/08-fusion-node-flows.md)** — the missing-input
   default table's all-1 rule for `MaskFromMatte` should be scoped to the standalone
   node. A mask consumed by a group defaults to **0** (group inert), per §6.2. 08 §2's
   `Invert` row should also record that the Mask-typed variant is not implemented
   (`ops.rs:201-220` leaves alpha untouched), and 08 §3.1's "refuse an incompatible
   drop" should be marked unimplemented outside the `mask` port (`node_editor.rs:678` is
   colouring-only).
4. **`crates/photonic-render/src/grade_gpu.rs` — a live CPU/GPU divergence.**
   `apply_grade_op_gpu` sizes from `input.width()/height()` (`:593-595`), which is the
   **pool-bucketed** texture handed to it by `eval.rs:1139-1142`, and the masked shaders
   evaluate `window_weight` against a full-quad uv (`:240,:302,:395`). The CPU reference
   runs at logical size. A `PowerWindow` therefore lands in a different place on the two
   evaluators whenever the canvas is not a multiple of 64. The fix is to pass logical
   dims into the grade passes, as `eval.rs:1617-1621` already does for `MaskShapeGen`.
   This is 26 E-9's bug class in shipped code; §9 test 9 is the failing test to
   un-ignore. **Not fixed in K-B8, deliberately** — it predates this item and deserves
   its own commit.
5. **[07 §4.2](../specs/video-editor/07-color-grading.md)** — "`RotoMatte` becomes real
   once a roto tool exists (post-v1, no phase currently assigned)" is superseded: §6.4
   makes it real via `MaskRef::GraphNode` **before** a roto tool exists, because the
   graph can already produce a mask. The section should record the lowering rule and
   that `resolve_mask` intentionally keeps returning `None` for `RotoMatte` since the
   mask no longer lives in the grade shader.
6. **[32 §1](../specs/video-editor/32-engine-contracts.md)** — E-1's
   `graph_source_range` unions across the whole graph
   (`source_range.rs:112-118`), so it cannot express "the mask branch needs `t−1` but
   the base does not", and prefetch over-warms. Conservative and safe, but the contract
   should record the coarseness so a later per-branch refinement is a known option
   rather than a surprise. §7.1 item 3.
7. **[02 §2](../specs/video-editor/02-engine.md)** — the IR op table gains
   `MaskComposite` (3 inputs, 2 resolved scalars), and §5's cache section should note
   that a mask node is an ordinary cached node with no special lifetime.
8. **[36 §3.2](../specs/video-editor/36-error-model.md)** — if `MaskSourceMissing` /
   `MaskSourceCycle` surface beyond `CompileDiagnostic`, the family table needs a row
   and `diag.rs:140`'s "the ten error families" comment needs incrementing. Both
   [193 Follow-up 8](193-k-a1-chunked-timeline-preview-rendering.md) and
   [195 Follow-up 2](195-k-c1-clip-jobs-framework.md) raise the same amendment for
   `Preview` and `Job`; whichever lands last must **re-read** the comment rather than
   assume a count.
9. **[198 — K-B9](198-k-b9-rotoscoping-spline-masks.md)** — §6.1's seven clauses are
   the interface 198 must satisfy, and §6.3 records what K-B8 explicitly does *not* ask
   it for (mask arithmetic, per-source feather, per-source invert). If 198 proposes a
   different mask representation than §3.3's replicated RGBA, or a mask-combination
   design that changes `MaskComposite`, the two documents must be reconciled before
   either is coded.
10. **`GraphOp::Effect { id: EffectId }`** — not proposed here, but named because it is
    the single blocker for §10 open question 1 and for putting K-B16's 44-entry
    catalogue into the node editor at all. Currently `graph_op_effect_kind`
    (`compile.rs:2096-2110`) covers 7 kinds while `apply_stack` reaches the rest only
    through `EffectKind::Unknown(tag)` (`compile.rs:1223-1233`). Worth its own small
    item.
