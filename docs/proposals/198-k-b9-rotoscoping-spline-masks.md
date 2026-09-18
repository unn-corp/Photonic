# 198 — K-B9 Rotoscoping Spline Masks (mini-spec)

> **Status: Proposed — K-Band 5 mini-spec, pre-code.**
> [26 §19.1](../specs/video-editor/26-kdenlive-mlt-parity.md#191-bands) makes an
> accepted mini-spec the exit condition for every K-Band 5 item: it must name the
> data-model change, migration, undo unit, MCP surface and acceptance fixtures
> *before* code. This document discharges that for **K-B9**
> ([26 §10](../specs/video-editor/26-kdenlive-mlt-parity.md#k-b9--rotoscoping-spline-masks)).
> It carries **no code authorization** ([26 §2](../specs/video-editor/26-kdenlive-mlt-parity.md#2-clean-room-and-licensing-fence)
> point 5; [23 §14](../specs/video-editor/23-legal-open-source-implementation-routes.md#14-stopgo-checklist-before-any-code)).

**Owner refs.** [26 K-B9](../specs/video-editor/26-kdenlive-mlt-parity.md#k-b9--rotoscoping-spline-masks)
contributes the requirement and the ranking. **The design authority is
[07 §4](../specs/video-editor/07-color-grading.md#4-hsl-qualifier--power-window-masks)**,
which owns `GradeMask`/`MaskRef` — 26 §1 states this explicitly for exactly this
item ("[K-B9] edits `grade.rs` … the owner doc's contract governs"). Where this
document and 07 §4.2 disagree, the disagreement is called out in
[§10](#10-risks-open-questions-and-deliberate-exclusions) and
[Follow-ups](#follow-ups), never silently resolved.

**Territory:** `panels-video` + `photonic-video-engine` (+ `core-timeline` for the
model). **Effort:** L. **Downstream:** K-B9 → K-B8
([26 §19.2](../specs/video-editor/26-kdenlive-mlt-parity.md#192-dependency-graph)).
[§6](#6-the-output-contract--what-a-roto-mask-emits) is written *for* the K-B8
consumer and is the section a K-B8 implementer should read first.

---

## 1. Problem and user outcome

**Today.** Photonic can restrict a grade to an ellipse or a rectangle
(`GradeMask::PowerWindow`, `crates/photonic-core/src/timeline/grade.rs:253`), and
that is the whole of its spatial-mask capability. There is no way to draw a shape
around a subject and follow it over time. `grep -rni 'roto' crates/ --include=*.rs`
returns **three doc comments and no code** (`grade.rs:275`,
`crates/photonic-render/src/grade.rs:338`, `:476`), all of which say the same
thing: *no roto source exists yet*. `grep -rn 'RotoShape\|RotoMask\|spline mask'
crates/` is clean (verified 2026-07-28).

This is the one gap where Photonic's vector heritage is a decisive advantage and
is currently unspent: the product already ships bezier paths, a pen tool, an
anchor/handle editor, curve fitting, path offsetting and a GPU tessellator — and
none of it is reachable from the video mode.

**After K-B9.** A user can:

1. Select a clip, open the program monitor, pick the roto tool and click out a
   closed bezier shape over the picture — the same click-to-place, click-first-
   anchor-to-close interaction the vector pen already uses.
2. Drag anchors and handles at any frame. The first edit at a new time creates a
   **shape keyframe**; the shape interpolates between keyframes with the same
   `Hold` / `Linear` / `Bezier` easing every other Photonic keyframe uses.
3. Add a second shape set to **subtract**, so a mask can have holes; feather and
   fade the whole mask with keyframable scalars.
4. Use the result as a **garbage matte on the clip** — the clip's alpha is
   multiplied by the mask, so the layer below shows through the cut — and see it
   in Draft and in Full preview and in an export, identically.
5. Point a grade op's `GradeMask::RotoMatte` at the same shape, so K-B8 and the
   colour page consume one mask object rather than two parallel ones.
6. Drive creation, inspection, keying and binding from an agent over MCP, with
   the GUI and MCP arms calling the same `ops::` functions.

**Not** in the outcome, and deliberately: tracking of any kind, motion blur, open
(stroked) shapes, edge snapping, and shape sharing between clips. Each is argued
in [§10](#10-risks-open-questions-and-deliberate-exclusions).

---

## 2. Current state in code

### 2.1 The vector/bezier stack that already exists — the reuse survey

This is the largest reuse opportunity in the item, so it is inventoried before
anything is proposed. Everything in this table **ships and is tested today**.

| Thing | Where | Reusable for roto? |
|---|---|---|
| `PathData` — SVG-string-backed wrapper over `kurbo::BezPath`, the canonical path type | `crates/photonic-core/src/path.rs:7-30` | **Yes**, as the *interchange* form (`from_bez_path`/`to_bez_path`); **no** as the stored roto form — [§3.1](#31-why-roto-is-not-animprops-over-a-pathdata) |
| Primitive constructors: `rect`, `rounded_rect`, `ellipse`, `regular_polygon`, `star` | `path.rs:33`, `:40`, `:49`, `:55`, `:74` | **Yes** — seed a roto shape from a primitive, GUI and MCP alike |
| `SceneNodeKind::Path(PathNode { path_data, fill, stroke, is_compound })` — the vector document's path node | `crates/photonic-core/src/node.rs:452`, `:461` | **No.** A roto is not a document node; it must not appear in the layers panel or the SVG/PDF exporters |
| Pen tool: click-to-place, click-first-anchor-to-close, rubber band, Escape-cancels | `crates/photonic-gui/src/app/tool_handlers.rs:1261` (`handle_pen_tool`), `:1367` (`build_pen_path`), `:1386` (`pen_over_first_anchor`) | **Interaction shape yes, code no** — it commits through `finalize_pen_node` (`:1419`) into `Document.nodes`. Also note it currently emits **polylines only** (`bez.line_to`, `:1375`); it has no handle-drag-out |
| Direct Select: anchor + handle editing, marquee over anchors, corner↔smooth convert | `crates/photonic-gui/src/app/direct_select.rs:28`, `:59`, `:1-1133` | **Interaction shape yes, code no** — it is written against `doc.nodes` and commits `Command::UpdateNode` |
| `path_anchor_points(bez) -> Vec<(usize, Point)>` and the screen/canvas hit helpers | `crates/photonic-gui/src/app/geometry.rs:210`, `:231`, `:253` | **Yes**, directly — pure geometry over a `BezPath` |
| Fill tessellation to a triangle mesh, `NonZero`/`EvenOdd` fill rules, `bezpath_to_lyon` | `crates/photonic-render/src/tessellator.rs:371-388`, `:660` | **Yes** — this is the rasterizer ([§6.1](#61-the-two-new-ir-ops)) |
| Curve fitting (kurbo `simplify_bezpath`, Levien) with accuracy + corner-angle options | `crates/photonic-core/src/ops/fit_curves.rs:1-40` | **Yes**, for a freehand-drawn roto shape → minimal bezier |
| Path offsetting via kurbo stroke expansion | `crates/photonic-core/src/ops/offset.rs:1-20` | **Deferred** — the reuse path for `expand`, which v1 excludes ([§10](#deliberately-excluded)) |
| `lyon` + `kurbo` already in the build | `crates/photonic-render/Cargo.toml`, `crates/photonic-core/Cargo.toml` | **Yes — no new dependency is contemplated** |

**Conclusion:** the geometry and rasterization halves are essentially free. The
*editing* half is not: `direct_select.rs` and `handle_pen_tool` are hard-wired to
the vector document and its `Command::UpdateNode`. K-B9 therefore reuses the
geometry helpers and reimplements the interaction against a monitor overlay,
beside the existing reframe handles (`crates/photonic-gui/src/app/reframe.rs:232`,
drawn from `monitor.rs:1145`). Extracting an editing-target trait so one tool
serves both is a follow-up, not this item ([§10](#risks)).

### 2.2 The animation stack that exists — and exactly where it stops

`AnimProps<T>` is `{ base: T, tracks: Vec<PropertyTrack> }`
(`crates/photonic-core/src/timeline/anim.rs:148`); a `PropertyTrack` is one
`PropPath` and a sorted, unique-`at` `Vec<Keyframe>` (`:107`); a `Keyframe` is
`{ at: Tick, value: PropValue, interp: Interp }` (`:92`); `Interp` is
`Hold | Linear | Bezier { out_handle, in_handle }` (`:76`) and `eval` is a pure
closed-form lane evaluator (`:199`). Times are **clip-relative** (`:192`).

Three facts bound what this machinery can carry, and all three matter:

1. **`PropValue` has five kinds — `Float`, `Vec2`, `Color`, `Bool`, `Enum`
   (`anim.rs:52`).** There is no list, path, or variable-length value. A shape of
   N control points is not a `PropValue`.
2. **The property registry is `&'static [PropEntry]` per target kind**
   (`crates/photonic-core/src/timeline/prop_registry.rs:73-114`, dispatched by
   `entries()` at `:200`). Paths are compile-time literals, including array
   components (`params.slope[0]`, `:126`). A shape whose point count changes at
   runtime cannot register `points[0..N]` there.
3. **An unresolved path is not an error — it is `orphaned: true`, retained and
   skipped** (`prop_registry.rs:249-257`, `anim.rs:110-115`, `anim.rs:201`). So a
   mis-modelled roto would not fail loudly; it would silently stop animating.

`AnimProps` is therefore exactly right for roto's **scalars** and structurally
unable to carry roto's **geometry**. [§3](#3-data-model-change) splits on that
line, and [§3.1](#31-why-roto-is-not-animprops-over-a-pathdata) argues it in full,
because 26 K-B9's Files line ("`AnimProps` on control points") reads as if the
composition were straightforward. It is not.

### 2.3 The mask seam that exists and is inert

```rust
// crates/photonic-core/src/timeline/grade.rs:253
pub enum GradeMask {
    PowerWindow { shape, center, size, rotation, softness, invert },
    RotoMatte { source: MaskRef, invert: bool },     // :263
}
// :281
pub enum MaskRef {
    Matte,                                            // photonic-matte
    GraphNode { graph: GraphId, node: GraphNodeId },  // a Mask-typed graph output
}
```

`MaskRef` is referenced in exactly two places in the whole tree — its definition
and the `timeline/mod.rs:67` re-export. **Nothing constructs it, nothing consumes
it.** `GradeOp.mask` (`grade.rs:55`) resolves through
`photonic-render/src/grade.rs:477` (`resolve_mask`), where
`GradeMask::RotoMatte { .. } => None` (`:494`) — a roto mask resolves to *full
frame*, i.e. the op applies everywhere, today.

The shape of `ResolvedMask` (`crates/photonic-render/src/grade.rs:340`) is the
constraint that decides [§6.4](#64-what-k-b9-deliberately-does-not-compile): it is
six scalars packed into shader uniforms (`grade_gpu.rs:422`, `mask_fields`), and
`IrOp::Grade` (`crates/photonic-video/src/graph/ir.rs:180`, tag `[6]` at
`compile.rs:2636`) is **unary**. A rasterized mask is a *texture*, not a uniform,
so it cannot ride `ResolvedMask` and cannot enter `IrOp::Grade` without changing a
shipped, golden-tested op's arity.

### 2.4 What is genuinely absent

- No roto model, ops, commands, GUI or MCP surface (grep above).
- No IR op that produces a mask from geometry. The closest producers are
  `Effect { kind: MaskShapeGen }` (an analytic ellipse/rect param bag,
  `prop_registry.rs:107`), `IrOp::MatteExtract` (`ir.rs:258`, U²-Net, CPU) and
  `IrOp::ChannelSplit` (`ir.rs:266`).
- **No IR op that applies a mask to an image.** There is `Merge { mode, opacity }`
  (uniform opacity only) and nothing that takes a coverage texture. This is the
  single missing primitive; [§6.1](#61-the-two-new-ir-ops) adds it.
- `MaskRef` and `GradeMask` have **no unknown-preserving variant**, unlike the
  eight enums [39 §2.2](../specs/video-editor/39-document-lifecycle.md) names.
  This is a live forward-compat hole and [§4](#4-migration-and-format-version-impact)
  is where it bites.

### 2.5 Three shipped invariants that constrain every choice below

**(a) `validate()` runs after every single command.** `TimelineCmd::apply`
debug-asserts `Sequence::validate()` for every sequence after each command
(`crates/photonic-core/src/timeline/commands.rs:1747-1757`), and `Command::Batch`
applies members one at a time. Any invariant K-B9 adds to `Sequence::validate`
(`sequence.rs:378-405`) must therefore hold at **every** command boundary in both
directions. This is why [§5](#5-undo-unit-and-exact-inverse) uses commands whose
payload is a whole shape key or a whole mask, and never a per-control-point
command batched N times.

**(b) The content hash does not encode the evaluation canvas.**
`GpuEvaluator::evaluate(&graph, canvas, source)` takes the canvas as a runtime
argument (`crates/photonic-video/src/graph/eval.rs:465`), while
`session.rs:1200`'s `preview_canvas` caps it for Draft — so one `ContentHash`
describes both a Draft and a Full render
([193 §2.3(a)](193-k-a1-chunked-timeline-preview-rendering.md)). K-B9's response
is in [§6.2](#62-the-value-contract) clause 4: roto geometry is **normalized**, so
canvas size changes sampling density and never geometry, and the op still carries
and hashes `w`/`h` exactly as `RasterVector` (`compile.rs:1823-1829`),
`Resize` and `Output` already do.

**(c) `vector_state_key` does not hash the vector document.** `compile.rs:2519`
hashes only `(vref discriminant, format size, src_time, asset uuid)`
([193 §2.3(c)](193-k-a1-chunked-timeline-preview-rendering.md)). K-B9 must **not**
route roto through `IrOp::RasterVector`; it would inherit a key that does not move
when the geometry moves. [§6.1](#61-the-two-new-ir-ops) hashes the resolved point
list directly instead.

---

## 3. Data-model change

### 3.1 Why roto is *not* `AnimProps` over a `PathData`

26 K-B9 sketches "`MaskRef::Path { .. }` over the existing path type, with
`AnimProps` on control points". Taken literally that means one `PropertyTrack` per
component per point. **It does not work, for four independent reasons**, each
verifiable in the tree:

1. **The registry cannot name the paths.** Blocks are `&'static [PropEntry]`
   (`prop_registry.rs:200-241`); a variable-length `points[i]` set has no static
   spelling. The failure mode is silent: unresolvable paths become
   `orphaned: true` and are skipped by `anim::eval` (`anim.rs:201`), so the shape
   would simply stop animating with no error.
2. **Per-lane interpolation destroys the shape.** `eval` runs each lane
   independently (`anim.rs:199-241`). One anchor on `Linear` and its neighbour on
   `Bezier` produces geometry that is in neither key — self-intersecting outlines
   between keyframes, which is precisely the artefact roto exists to avoid. Shape
   interpolation must be **whole-shape**: one time base, one easing, all points.
3. **Point count is a shape-wide structural property.** Adding a control point at
   time t must add it at *every* keyframe (or shape correspondence is undefined).
   As 3N independent lanes that is 3 track insertions × K keyframes as separate
   mutations — a `Batch` whose intermediates violate the correspondence invariant
   and trip §2.5(a)'s `debug_assert`.
4. **Cost.** A 40-point shape with 20 keys is 2,400 `Keyframe` records across 120
   `PropertyTrack`s, each serializing `{ at, value: { t, v }, interp }`. As
   whole-shape keys it is 20 records. `Command::mem_estimate`
   (`commands.rs:1631`) charges history by serialized size against a byte budget.

**The decision, stated plainly:** roto is **not** the composition of `PathData` +
`AnimProps`. It is the composition of *bezier geometry* (borrowed wholesale, §2.1)
with *`anim::Interp` and `Tick`* (borrowed wholesale) under a **whole-shape
keyframe** — plus `AnimProps` used exactly as designed for the scalars. The parts
of the animation system that are reused are reused verbatim; the part that does
not fit is not forced.

### 3.2 New module: `crates/photonic-core/src/timeline/roto.rs`

```rust
//! Rotoscoping spline masks (07 §4.2, K-B9).

/// One control point: an on-curve anchor and its two off-curve handles, all
/// ABSOLUTE and in normalized canvas coords (§6.2 clause 3), matching
/// `kurbo::PathEl::CurveTo`'s absolute control points so conversion to
/// `PathData` is a copy, not a transform.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RotoPoint {
    pub anchor: [f32; 2],
    pub in_handle: [f32; 2],
    pub out_handle: [f32; 2],
}

/// A whole-shape keyframe. `at` is CLIP-RELATIVE, like every other
/// `AnimProps` time in the model (`anim.rs:192`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RotoShapeKey {
    pub at: Tick,
    pub points: Vec<RotoPoint>,
    /// Reused verbatim from `anim::Interp` (`anim.rs:76`) — Hold / Linear /
    /// Bezier, evaluated by `anim::cubic_bezier_ease` (`anim.rs:267`).
    pub interp: Interp,
}

/// How a shape combines with the shapes before it in `RotoMask.shapes`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RotoBool {
    Add,
    Subtract,
    /// Forward-compat (39 §2.2). Renders as `Add`; tag preserved verbatim.
    #[serde(untagged)]
    Unknown(UnknownTag),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RotoShape {
    pub id: RotoShapeId,
    #[serde(default)]
    pub op: RotoBool,                       // default Add
    #[serde(default = "grade::default_true")]
    pub enabled: bool,
    /// Non-empty, sorted by `at`, unique `at`, and EVERY key carries the same
    /// `points.len() >= 3` — the correspondence invariant (§3.3).
    pub keys: Vec<RotoShapeKey>,
    /// Scalars, registry-backed and keyframable exactly like every other
    /// param block: `params.feather`, `params.opacity`.
    pub props: AnimProps<RotoShapeProps>,
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RotoShapeProps {
    /// Feather half-width in normalized FRAME-HEIGHT units (isotropic), ≥ 0.
    #[serde(default)]
    pub feather: f32,
    /// This shape's contribution weight, 0..=1.
    #[serde(default = "one_f32")]
    pub opacity: f32,
}
impl PropSet for RotoShapeProps {
    const TARGET_KIND: PropTargetKind = PropTargetKind::RotoShape;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RotoMask {
    pub id: RotoId,
    #[serde(default)]
    pub name: String,
    /// Ordered; folded per §6.2 clause 5.
    pub shapes: Vec<RotoShape>,
    #[serde(default)]
    pub invert: bool,
}
```

`RotoId` / `RotoShapeId` are minted by the existing `id_newtype!` macro
(`crates/photonic-core/src/timeline/ids.rs:14`), beside `GradeOpId` (`:80`).

`PropTargetKind::RotoShape` is added to `prop_registry.rs:58` and `entries()` at `:200`, with a two-entry
block (`params.feather` range `(0.0, 0.5)`, `params.opacity` range `(0.0, 1.0)`),
following the hand-written grade/audio blocks — **not** the effect-manifest
projection, which `prop_registry.rs`'s `projection_matches_legacy_blocks` test
constrains to `EffectKind` only.

### 3.3 The correspondence invariant, and where it is enforced

`Sequence::validate` (`sequence.rs:378`) gains `validate_rotos()` beside
`validate_transitions()` and `validate_groups()` (`:402-403`). It checks, per
`RotoShape`:

- `keys` is non-empty, strictly sorted by `at`, with unique `at`;
- every key has the same `points.len()`, and that length is `>= 3`;
- every coordinate is finite (no `NaN`/`inf` — a `NaN` anchor poisons the
  tessellator and the content hash alike).

New `ValidationError` variants: `RotoKeyOrder`, `RotoPointCountMismatch`,
`RotoDegenerateShape`, each naming `(RotoId, RotoShapeId)`.

**What it deliberately does *not* check: dangling references.** A `MaskRef::Roto`
or a `Clip.roto_matte` naming a `RotoId` that no longer exists is **legal** and
resolves *inert*, exactly as an offline LUT asset does today
(`photonic-render/src/grade.rs:463`, `None => continue`). This one decision
eliminates a whole class of ordering hazards: bind-before-create and
delete-before-unbind are both valid intermediates, so [§5](#5-undo-unit-and-exact-inverse)
never needs a plural create-and-bind command, and copy/paste of a clip can never
produce the `UnknownGroup`-shaped defect
[194 §8.1](194-k-a5-general-and-nested-clip-groups.md) records for groups.

### 3.4 Where rotos live: `Clip.rotos`

```rust
// crates/photonic-core/src/timeline/clip.rs — appended to Clip (beside `grade`, :50)
/// K-B9 roto masks owned by this clip. Keyframe times are CLIP-RELATIVE, so
/// the store must be clip-scoped. Additive; absent before K-B9.
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub rotos: Vec<RotoMask>,

/// K-B9 garbage matte: multiply this clip's alpha by the named roto.
/// A dangling id resolves inert (§3.3).
#[serde(default, skip_serializing_if = "Option::is_none")]
pub roto_matte: Option<RotoId>,
```

**Why the clip and not a sequence-level registry** (which would have mirrored
`Sequence.groups`, `sequence.rs:147`):

1. **Time base.** Roto keys are clip-relative, like every `AnimProps` in the model
   (`anim.rs:192`). A sequence-scoped store would key on sequence time, and every
   clip move or trim would desync the shape from the picture it was drawn on. That
   is a data-loss-shaped bug, not a papercut.
2. **Copy/paste and duplication come free.** `Clip` is cloned wholesale by paste
   and by `duplicate_sequence`; the roto travels with its picture and needs no
   id-remap pass.
3. **One resolution scope for both consumers.** K-B8's `MaskedGroup` lives inside
   `ClipEffect` (`clip.rs:47`), also clip-scoped, so a `RotoId` means the same
   thing to K-B8, to `GradeMask::RotoMatte` and to `roto_matte`.

The cost is that a roto cannot be shared between two clips in v1. That is an
accepted exclusion ([§10](#deliberately-excluded)); a later sequence-level
registry plus a `MaskRef::SharedRoto` variant is serde-additive and needs no
format step.

### 3.5 The `MaskRef` change, and the two unknown arms it forces

```rust
// crates/photonic-core/src/timeline/grade.rs — MaskRef (:281)
pub enum MaskRef {
    Matte,
    GraphNode { graph: GraphId, node: GraphNodeId },
    /// K-B9: a roto mask owned by the clip this grade belongs to.
    Roto { roto: RotoId },
    /// 39 §2.2: a mask source this build does not understand. Payload retained
    /// verbatim and re-emitted; resolves inert (full frame).
    #[serde(untagged)]
    Unknown(serde_json::Map<String, serde_json::Value>),
}
```

`GradeMask` (`grade.rs:253`) gains the same `Unknown` arm and becomes
`#[non_exhaustive]`. Both are **required, not optional** — see
[§4](#4-migration-and-format-version-impact).

Naming note: 26 K-B9 sketches `MaskRef::Path { .. }`. `Roto` is used instead
because `Path` collides with `PathData`/`PathNode` (`node.rs:461`) in a codebase
where "path" already means "a vector document node", and because the referent is a
roto mask, not a path. Recorded in [Follow-ups](#follow-ups).

### 3.6 Command model: two additions

```rust
// commands.rs — beside the existing TimelineCmd arms (:396)

/// Replace one roto mask wholesale (create / delete / rename / add or remove a
/// shape / add or remove a control point). Structural verbs only.
SetRoto {
    seq: SequenceId,
    track: TrackId,
    clip: ClipId,
    roto: RotoId,
    old: Option<Box<RotoMask>>,
    new: Option<Box<RotoMask>>,
},

/// Upsert or delete ONE whole shape key. The hot path: a drag of any number of
/// selected control points is one of these, never N per-point commands (§2.5a).
SetRotoShapeKey {
    seq: SequenceId,
    track: TrackId,
    clip: ClipId,
    roto: RotoId,
    shape: RotoShapeId,
    at: Tick,
    old: Option<RotoShapeKey>,
    new: Option<RotoShapeKey>,
},
```

Inverses are a mechanical `old`/`new` swap in both cases. Both get
`mem_estimate` arms (`commands.rs:1631`) and `label` arms ("Edit roto shape" /
"Edit roto"). `EditError` (`ops.rs:34`) gains `NoRoto(RotoId)` and
`NoRotoShape(RotoShapeId)`; `map_edit_error` in `handlers/video.rs` already has an
`other =>` catch-all, so both are non-breaking there and still get explicit arms.

**Why a whole-key payload rather than a point delta.** Because of §2.5(a): the
correspondence invariant (§3.3) is shape-wide, so any per-point command sequence
has an intermediate where one key's `points.len()` differs from its siblings'.
That state fails `validate_rotos` and panics the `debug_assert` at
`commands.rs:1748`. A whole key is also the natural undo granularity — the user's
verb is "the shape at this frame now looks like this", not "point 7 moved".

### 3.7 What does *not* change

`ResolvedMask` (`photonic-render/src/grade.rs:340`), `mask_fields`
(`grade_gpu.rs:422`), `IrOp::Grade`'s arity, `apply_grade` (`compile.rs:1245`),
`PowerWindow` semantics, `ProjectVideoSettings`, and every existing `IrOp`. See
[§6.4](#64-what-k-b9-deliberately-does-not-compile).

---

## 4. Migration and format-version impact

**`CURRENT_FORMAT_VERSION` stays at 5** (`crates/photonic-core/src/document.rs:117`).
K-B9 lands additively inside v5. Point by point:

1. **Every new field is `#[serde(default, skip_serializing_if = …)]`** —
   `Clip.rotos` and `Clip.roto_matte` are byte-identical in shape to how
   `effects`, `grade`, `reframe`, `composition` and `color_label` were each added
   to `Clip` (`clip.rs:43-70`). An older file loads with `rotos: []` /
   `roto_matte: None`, which is the correct and complete meaning.
2. **Nothing is reinterpreted.** `migration.rs:43-56` defines a migration as a
   function that *reinterprets existing data*. There is no existing roto data;
   there is nothing to reinterpret. A no-op `MigrationV5ToV6` would lie about what
   changed and would spend the `COMPAT_WINDOW = 1` budget (`migration.rs:16`) that
   protects every user opening a file from a slightly newer build.
3. **New `TimelineCmd` variants are not a document-format change.** They live only
   in the sibling `photon_history` key, which `load_photon` restores best-effort:
   a payload that fails to deserialize yields no history while the document still
   opens (`crates/photonic-core/src/photon_file.rs:4-21`: "A malformed history payload is
   likewise dropped, never fatal").

### 4.1 The one real forward-compat hazard, stated plainly

**Adding a variant to `MaskRef` is not forward-compatible with already-shipped v5
builds.** `MaskRef` (`grade.rs:281`) is an internally-tagged enum
(`#[serde(tag = "source")]`) with **no** `#[serde(other)]` and no untagged
fallback. A v5 document containing `"source": "roto"` makes a pre-K-B9 build fail
`MaskRef` → `GradeMask` → `GradeOp` → the whole document. `Option`+`default` on
`GradeOp.mask` (`grade.rs:54-55`) does not rescue it: serde's `default` covers an
*absent* field, not a *failed* parse.

`MaskRef` and `GradeMask` are **missing from
[39 §2.2](../specs/video-editor/39-document-lifecycle.md)'s enumeration** of
open-ended enums that must gain unknown-preserving variants (`EffectKind`/
`EffectId`, `GraphOp`, `AudioFxKind`, `TransitionKind`, `GradeOpKind`,
`MarkerAnchor`, `GroupKind`, `ClipSource`), and correspondingly absent from
`crates/photonic-core/tests/forward_compat.rs:117-147`'s six-discriminant sweep.
This is a pre-existing hole that K-B9 is the first item to walk into.

**Disposition — three parts, all in K-B9's scope:**

- **Close the hole permanently**: add the `Unknown` arms of [§3.5](#35-the-maskref-change-and-the-two-unknown-arms-it-forces),
  extend `forward_compat.rs`'s sweep to eight discriminants, and record the 39 §2.2
  list correction in [Follow-ups](#follow-ups). This makes the *next* `MaskRef`
  variant safe, which no version bump would.
- **Accept the residual**, with evidence: nothing in the shipped product can author
  a `GradeMask::RotoMatte` today — no GUI control, no MCP tool, and
  `resolve_mask` discards it (`photonic-render/src/grade.rs:494`). The only route
  is an agent hand-writing one into `set_grade`'s opaque `grade: Value`
  (`crates/photonic-mcp/src/protocol/args/video.rs:1332-1336`), where it would be
  inert. The realistic population of affected files is zero.
- **Do not bump.** A v6 would convert a serde error into a `VersionTooNew` refusal
  — a nicer message for a file nobody has — while forcing every existing v5
  project through a no-op step, shrinking the forward window for everyone, and
  leaving the underlying `MaskRef` hole open for the variant after this one. The
  bump buys a better error and costs the actual fix.

**Round-trip obligations** ([§9](#9-acceptance-fixtures-and-tests) tests T11–T13):
a v5 document with rotos survives `to_json` → `from_json` → `finalize_load`
byte-identically; a pre-K-B9 v5 document loads with `rotos: []` and re-serializes
without the key; and a document carrying an unknown `MaskRef` tag loads,
preserves it verbatim, and re-emits it unchanged.

---

## 5. Undo unit and exact inverse

Repo rule: one user verb = one undo unit; a fanned-out edit that cannot be undone
atomically must not commit partially (01 §10.0). Every row is **one** history step.

| User verb | Command | Exact inverse |
|---|---|---|
| **Draw a shape** (pen release / `create_roto`) | one `SetRoto { old: None-or-prior-mask, new }` | restore `old` (removing the mask when `old` is `None`) |
| **Drag control points at time t** (any number selected, drag release) | one `SetRotoShapeKey { old: Some(prior-key-or-None), new: Some(key) }` | swap `old`/`new` — restores the prior key, or removes the key that the drag created |
| **Delete a shape key** (Delete on the keyframe lane) | one `SetRotoShapeKey { new: None }` | re-insert `old` |
| **Change a key's easing** (`Hold`/`Linear`/`Bezier`) | one `SetRotoShapeKey` | swap |
| **Add or remove a control point** | one `SetRoto` (whole mask) — the point count changes on *every* key (§3.3), so this is structural | swap |
| **Add / delete / reorder a shape, toggle `enabled`, flip `op`, set `invert`, rename** | one `SetRoto` | swap |
| **Feather / opacity value or keyframe** | the existing `AnimProps` keyframe path — `RotoShapeProps` is an ordinary `PropSet` | existing inverse, unchanged |
| **Bind a roto** (`Clip.roto_matte`, or a grade op's `MaskRef::Roto`) | the existing clip-property / `SetGrade` command (`ops.rs:1692`) | existing inverse |
| **Draw-and-bind** (draw a roto directly onto a grade op) | `Command::Batch([SetRoto, SetGrade])` | reversed batch of inverses |

**Why draw-and-bind may be a `Batch` while a point drag may not.** Batch members
apply one at a time and each intermediate must satisfy `validate()`
(§2.5(a)). Draw-and-bind's intermediates are valid in **both** directions
precisely because §3.3 refuses to validate references: a mask with no referrer is
valid, and a referrer with no mask is valid-and-inert. A per-point command
sequence has no such property — its intermediates break the correspondence
invariant. This is the same distinction
[194 §5](194-k-a5-general-and-nested-clip-groups.md) draws between split (a legal
batch) and group move (not).

**Coalescing.** A continuous drag commits once on pointer release and is therefore
one step by the existing gesture rules. Structural verbs commit through
`history::stacks::execute_discrete` (`crates/photonic-core/src/history/stacks.rs:403`)
so a shape add never folds into an adjacent drag.

**Atomicity.** Ops validate the whole mask before returning a command; one
malformed key → `Err(EditError::…)`, no command, no document change. This is how
`ops.rs` already works and roto gets no exception.

---

## 6. The output contract — what a roto mask emits

**This section is the K-B8 interface.** It is written so
[197](197-k-b8-nested-subgraph-masking.md) can bind to it without reading the rest
of this document. `MaskRef` (`crates/photonic-core/src/timeline/grade.rs:281`) is
the existing model-level seam; `IrOp::RotoRaster` below is the graph-level one.

### 6.1 The two new IR ops

```rust
// crates/photonic-video/src/graph/ir.rs — appended to IrOp (:180)

/// K-B9: rasterize a roto mask to a coverage map. A 0-INPUT GENERATOR.
/// `shapes` is fully resolved at compile time (points interpolated at the
/// tick, scalars evaluated through `anim::eval`), so the evaluator stays
/// time-ignorant (02 §2). `w`/`h` are the compile-time format size, carried
/// and hashed exactly as `RasterVector` (:196), `Resize` and `Output` do.
RotoRaster {
    shapes: ResolvedRoto,
    w: u32,
    h: u32,
},

/// K-B9: apply a coverage map to an image. Inputs: [image, mask].
MaskApply {
    mode: MaskApplyMode,     // v1: MultiplyAlpha only
},
```

```rust
// crates/photonic-video/src/contract.rs (or graph/ir.rs)
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedRoto {
    /// In fold order; every shape already interpolated to this tick.
    pub shapes: Vec<ResolvedRotoShape>,
    pub invert: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedRotoShape {
    pub op: ResolvedRotoBool,       // Add | Subtract
    /// Flattened-ready cubic segments in normalized canvas coords.
    pub points: Vec<RotoPoint>,
    pub feather: f32,               // normalized frame-height units
    pub opacity: f32,               // 0..=1
}
```

Rasterization reuses the shipped path: `RotoPoint` list → `kurbo::BezPath` →
`bezpath_to_lyon` (`crates/photonic-render/src/tessellator.rs:660`) → lyon fill
tessellation with `FillRule::NonZero` (`:371-388`) → a single-channel render
target. Feather is a separable Gaussian over the coverage buffer. No new
dependency; no new algorithm ([§7](#7-patent-and-algorithm-review--the-mandatory-gate)).

Adding an `IrOp` variant touches four exhaustive matches, all of which must be
updated in the same change: `hash_op` (`compile.rs:2583`, whose tag bytes run 0..=18 today, so `RotoRaster`
takes **19** and `MaskApply` **20**), `graph/eval.rs`, `graph/eval_cpu.rs`, and
`graph/source_range.rs:88-107` (both new ops declare `FrameRange::identity` — a
roto reads no other frame).

### 6.2 The value contract

A `RotoRaster` node's output is a **coverage map**. Normatively:

1. **Type.** Single-channel coverage, the same value kind `IrOp::ChannelSplit`
   (`ir.rs:266`), `IrOp::MatteExtract` (`ir.rs:258`) and
   `Effect { kind: MaskShapeGen }` produce. A consumer must treat it as a mask,
   never as a colour image.
2. **Range and polarity.** `1.0` = **inside** the mask = the masked operation
   applies at full strength. `0.0` = outside = the operation does not apply.
   Values in between are partial. This matches `photonic_core::raster::Mask`
   (`crates/photonic-core/src/raster/mask.rs:4-6`: "`0` = fully masked /
   deselected, `255` = fully selected") and 07 §4.3's `lerp(in, corrected, weight)`
   convention. It **contradicts the prose in 07 §4.2** ("0=unmasked/255=fully
   masked"), whose words are inverted relative to both the crate and its own
   `weight = data[i]/255` formula — see [Follow-ups](#follow-ups).
3. **Coordinate space.** Normalized canvas coords, origin top-left, `x`/`y` in
   `[0,1]`, matching `PowerWindow`'s "normalized sequence coords" (07 §4.1) and
   `EFFECT_MASKSHAPE`'s `params.center_x` range (`prop_registry.rs:107-114`). One
   mask coordinate convention, not two. Feather is in normalized **frame-height**
   units so it is isotropic on a non-square frame.
4. **Scale invariance.** Because geometry is normalized, canvas size changes only
   sampling density. Draft and Full therefore agree by construction, with no
   per-parameter scaling — which is exactly the bug class E-6 names and which
   `crates/photonic-video/tests/scale_invariance.rs` already guards. Roto is added
   to that harness as a case ([§9](#9-acceptance-fixtures-and-tests) T7).
5. **Fold.** Shapes fold in order, starting from zero coverage:
   `c := clamp(c + s.coverage * s.opacity, 0, 1)` for `Add` and
   `c := clamp(c - s.coverage * s.opacity, 0, 1)` for `Subtract`. Disabled shapes
   contribute nothing. `RotoMask.invert` applies once, last: `c := 1 - c`.
6. **Not premultiplied and not colour-managed.** A coverage map never enters the
   linear-light transfer path (PA-2/PA-14). Consumers must not apply a transfer
   function to it.
7. **Antialiased and deterministic.** Edge coverage comes from tessellated
   geometry rendered with a fixed sample pattern — no dithering, no
   frame-counter-dependent jitter — so export determinism (03 §6) and CPU/GPU
   parity (E-9) are testable.
8. **Empty is zero, not one.** A mask with no enabled shapes, or whose shapes are
   degenerate, rasterizes to **all-zero** coverage. Under clause 2 that means "the
   masked operation applies nowhere", so a consumer emits its unmodified input.
   This is deliberately fail-**dark**: a mask that silently applies an effect to
   the whole frame is the worse failure.
9. **An unresolvable *reference* is inert, which is a different thing.** A
   `MaskRef::Roto` naming a `RotoId` that does not exist, or a `MaskRef::Unknown`,
   resolves to `None` — i.e. **no mask node is emitted at all** and the consumer
   behaves exactly as it does with `mask: None` today
   (`photonic-render/src/grade.rs:477-495`). Clause 8 is about a *valid, empty*
   mask; clause 9 is about a *missing* one. They are not the same and must not be
   conflated.
10. **Cache identity.** `ContentHash` covers the resolved point list (all
    coordinates), every scalar, the fold ops, `invert`, and `w`/`h`. Moving one
    anchor by one unit is a different hash and therefore a cache miss (PA-1
    consumed as designed). Roto is **not** routed through `IrOp::RasterVector`,
    whose state key does not hash geometry (`compile.rs:2519`, §2.5(c)).
11. **Source range.** `FrameRange::identity` — a roto reads no neighbouring
    frames, so E-1's contract is unchanged and prefetch is unaffected.

### 6.3 The binding point for K-B8 / 197

A K-B8 `MaskedGroup` names its mask source through the **existing `MaskRef`**
(`grade.rs:281`), gaining nothing new: `MaskRef::Roto { roto: RotoId }` resolves
within the owning clip's `Clip.rotos` (§3.4). At compile time, K-B8 lowers that
reference by emitting one `IrOp::RotoRaster` node and wiring it as an input to
whatever composite node K-B8 defines. Concretely, the two things 197 can rely on:

- `fn lower_mask_ref(b: &mut Builder, lc: &LowerCtx, clip: &Clip, r: &MaskRef, tick: Tick) -> Option<IrNodeId>` —
  returns the producing node, or `None` under clause 9. K-B9 implements it and
  handles `MaskRef::Roto`; the `Matte` and `GraphNode` arms stay as they are.
- The returned node's output satisfies every clause of §6.2.

**K-B9 does not define the masked composite.** The three-input "unmodified input,
modified input, mask" node is K-B8's to design (26 K-B8: "composited back over the
unmodified input by the mask's alpha"), and this document deliberately does not
pre-empt it. `MaskApply` stays the narrow two-input alpha multiplier described in
§6.1 and must not be widened into that role.

### 6.4 What K-B9 deliberately does not compile

**A grade op carrying `MaskRef::Roto` keeps resolving to `None` — full frame,
inert — exactly as it does today** (`photonic-render/src/grade.rs:494`). It is not
plumbed into `IrOp::Grade`. This is a **third** case, distinct from §6.2's clauses
8 and 9: the mask is valid *and* the reference resolves, but this consumer is not
wired yet. Q3 in [§10](#open-questions-needing-a-product-call) recommends saying
so out loud with a load-time `Warning` rather than leaving it silent.

The reason is structural, not scheduling: `ResolvedMask` is six shader-uniform
scalars (`grade.rs:340`, `grade_gpu.rs:422`) attached **per grade op**, while
`IrOp::Grade` is a unary op carrying the whole resolved stack (`ir.rs:180`). A
texture-backed mask cannot ride a uniform, and a five-op stack with five different
roto masks would need five texture inputs on one node. The correct shape — split
the masked op into its own node and composite it back by the mask — *is* K-B8's
`MaskedGroup`. Building a second, grade-only version of it inside K-B9 would ship
the same mechanism twice.

K-B9's own v1 consumer is therefore `Clip.roto_matte` → `IrOp::MaskApply`, which
is a real, standalone user outcome (cut a hole in a clip so the layer below shows
through) and requires exactly one new binary op. **Placement in the clip chain:**
after the clip's transform and before the track fold — the mask is a canvas-space
cut over what the user sees on the program monitor, matching clause 3's
coordinate space. The consequence, stated because it is real: an animated clip
transform does *not* drag the roto with it. Argued under
[§10](#open-questions-needing-a-product-call) Q1.

---

## 7. Patent and algorithm review — the mandatory gate

### 7.1 What the mandate actually says

The brief for this item cites 26 K-B9 as reading *"Effort: L — needs its own
mini-spec, an algorithm/patent review per 23 §11.1 … and owned fixtures."*
**Verified against the tree: that sentence is K-B10's, not K-B9's.** In
`docs/specs/video-editor/26-kdenlive-mlt-parity.md`, K-B9's entry (line 371) reads
only *"Effort: L. Territory: `panels-video` + `photonic-video-engine`."* and
carries **no Clean-room line at all**; the patent-review sentence is line 377,
inside K-B10 — motion tracking, a different, `product-blocked` item whose SPEC
amendment (ROADMAP §8 **S14**, "Drafted … recommendation defer … patent review
mandatory") has not been accepted.

The correction does not dissolve the gate, for two reasons:

- 26 §2 point 2 is explicit that "individual items carry an explicit **Clean-room**
  line only where the provenance risk is *specific* … the absence of that line
  never implies exemption", while 26 §7 states that every item carries the
  provenance note §2 requires. K-B9 has none; it needs one, and this section plus
  [§11](#11-clean-room-provenance) supplies it.
- [23 §11.1](../specs/video-editor/23-legal-open-source-implementation-routes.md#111-production-route)'s
  closing sentence is a **standing rule**, not a K-B10-local one: *"A permissive
  implementation license does not prove the underlying technique is free of
  third-party patent claims."* Roto is adjacent enough to tracking and matting
  that the boundary must be drawn on purpose rather than by omission.

So: the review is performed, and the fence is drawn where the two items meet.

### 7.2 Techniques K-B9 proposes, with provenance

| Technique | Where it comes from | Why it is safe |
|---|---|---|
| Cubic Bézier curves as the shape primitive | de Casteljau (1959) / Bézier (1962); ubiquitous in ISO 32000 (PDF), W3C SVG, TrueType | Foundational, ~65 years old, embodied in open standards Photonic already implements (`path.rs`, the SVG/PDF exporters) |
| Adaptive flattening of cubics to a polyline | kurbo (Levien), already a workspace dependency | Published algorithm, decades of prior art in every rasterizer; the crate is already shipped |
| Polygon scan conversion with a non-zero winding fill rule | 1970s computer-graphics literature; codified in PostScript (1985), PDF, SVG | Foundational and standardized. Photonic already ships it via `lyon` (`tessellator.rs:371-388`) |
| Antialiasing by geometric edge coverage | Standard supersampling / analytic coverage; in fixed-function GPU hardware since the 1990s | Foundational; no novel sampling scheme is proposed |
| Separable Gaussian blur for feather | Gaussian convolution; separability is a mathematical fact | Mathematics, not an invention. Photonic already ships a Gaussian kernel (`EffectKind::Blur`) |
| Point-wise interpolation of a fixed-correspondence control-point set, eased by a normalized cubic | Photonic's own `anim::Interp` + `cubic_bezier_ease` (`anim.rs:76`, `:267`), already shipped for every other keyframed property | Photonic's own code, reused. Linear interpolation of coordinate pairs carries no technique claim |
| Boolean add/subtract of coverage by clamped accumulation | Arithmetic on a coverage buffer (§6.2 clause 5) | Deliberately chosen **instead of** path-boolean geometry, which is more elaborate for no gain here |
| Curve fitting for a freehand-drawn shape | kurbo `simplify_bezpath`, already shipped and used by `ops/fit_curves.rs:11` | Photonic's own shipped code; the underlying least-squares Bézier fit is Graphics-Gems-era (1990) published work |

Every one of these is either (a) foundational and embodied in an open standard,
(b) already shipped in this repository under its existing dependency set, or (c)
plain mathematics. **No new dependency is contemplated or authorized by this
document** — `lyon` and `kurbo` are already in the build.

### 7.3 Techniques K-B9 explicitly does **not** propose

This list is normative. Pulling any of it in re-opens the gate.

- **Motion tracking of any kind** — point tracking, planar/region tracking,
  template matching, optical flow. That is **K-B10** and ROADMAP §8 **S14**, which
  is *drafted, not accepted*, carries a standing recommendation to **defer**, and
  is `product-blocked` pending its own SPEC amendment. K-B9 must not smuggle a
  tracker in as "auto-keyframe the roto". A roto shape moves because a human moved
  it, or it does not move.
- **Feature detection and description** — SIFT, SURF, ORB, corner detectors. This
  is exactly 23 §11.1's territory ("Select feature/descriptor algorithms only
  after a documented patent and quality review") and is out of scope here.
- **Edge-snapping / magnetic roto / intelligent scissors** — live-wire and
  graph-cut boundary finding. Attractive for roto, and specifically excluded: the
  best-known formulations have documented patent history, and none of it is
  needed for a manual spline tool.
- **Alpha matting and trimap refinement** — Photonic's automatic matte is
  `photonic-matte` / `MaskRef::Matte`, a separately scoped surface. K-B9 does not
  extend it and does not add a matting solver.
- **Rotoscoping motion blur from shape velocity** — excluded on cost grounds
  ([§10](#deliberately-excluded)), so the shutter-sampling question never arises.
- **Any technique read out of a rejected source tree.** See
  [§11](#11-clean-room-provenance).

### 7.4 What review is still required before code

Acceptance of this mini-spec is **not** the patent gate. Before the first line of
K-B9 code:

1. **A [23 §3.3](../specs/video-editor/23-legal-open-source-implementation-routes.md)
   evidence record** listing each technique in §7.2 with its first publication or
   standard reference, and each exclusion in §7.3 with the reason.
2. **A written freedom-to-operate note** applying 23 §11.1's standing rule
   explicitly: the review is over *techniques*, not over crate licences, and the
   fact that `lyon` and `kurbo` are MIT/Apache-2.0 is recorded as **not
   dispositive**.
3. **Sign-off by the independent reviewer** 23 §3.4 point 5 names — the same
   person who checks identifiers, comments, constants, control flow and test
   provenance before merge.
4. **A re-review trigger**, recorded in the item, if any §7.3 exclusion is later
   pulled into roto — in particular any proposal to auto-key a roto from analysis
   output, which converts K-B9 into a K-B10 consumer and inherits S14's gate.

Until items 1–3 are recorded, K-B9 is **`legal-gated`**, not backlog.

---

## 8. MCP surface

GUI/MCP parity is CAP-019 and ROADMAP §10 point 3, and 26 §5 lists full MCP parity
(PA-11) as *not yet held* — a GUI-only roto would widen a gap this programme is
closing. **An MCP surface is warranted**, with one honest caveat: an agent
free-hand *drawing* a matte around a subject is not a sensible verb, because the
agent cannot see the frame. What is sensible, and what the tools below expose, is
**creating a mask from an explicit geometry, keying it, tuning it and binding it**
— all of which are deterministic, testable, and exactly what a parity story needs.

| Tool | Args | Notes |
|---|---|---|
| `list_rotos` | `{ clip_id }` | `[{ roto_id, name, invert, shapes: [{ shape_id, op, enabled, key_times, point_count }] }]` — the read side an agent needs first |
| `create_roto` | `{ clip_id, name?, shape: RotoShapeSpec }` → `{ roto_id, shape_id }` | `RotoShapeSpec` is `{ kind: "rect" \| "ellipse" \| "polygon" \| "points", … }`, seeded through `PathData::rect` / `ellipse` / `regular_polygon` (`path.rs:33`, `:49`, `:55`) — the same constructors the GUI uses |
| `set_roto_shape_key` | `{ clip_id, roto_id, shape_id, at, points?, interp? }` | Upsert; `points: null` deletes the key. `points` is a flat array of `{ anchor, in_handle, out_handle }` in normalized coords. **Rejects** a `points.len()` that differs from the shape's other keys, with `RotoPointCountMismatch` (§3.3) |
| `set_roto_props` | `{ clip_id, roto_id, name?, invert?, shape_id?, enabled?, op?, feather?, opacity? }` | Scalars and flags; feather/opacity route through the existing `AnimProps` path |
| `bind_roto` | `{ clip_id, roto_id \| null, target: "clip_matte" \| { grade_op_id } }` | The one verb that makes a roto do something. `null` unbinds |
| `delete_roto` | `{ clip_id, roto_id }` | One `SetRoto { new: None }` |
| `get_clip` (existing) | — | Gains `rotos` and `roto_matte` for free: it serializes the whole `Clip` (`handlers/video.rs`), exactly as it already exposes `grade` |

Wiring follows the shipped pattern exactly: arg structs in
`crates/photonic-mcp/src/protocol/args/video.rs` (beside `SetGradeArgs`, `:1332`),
handlers in `handlers/video.rs` next to `set_grade` (`:5699`), dispatch arms, the
tool-name list, then `schema_gen.rs` regenerated. **CI gates the docs**:
`.github/workflows/ci.yml:164-167` regenerates `docs/mcp-api.md` and fails on any
diff, so regeneration is mandatory.

**Both arms call the same `ops::` functions.** Roto fan-out lives in
`crates/photonic-core/src/timeline/ops.rs` once; the GUI monitor tool and the MCP
handler both call it. The link-group precedent of two hand-mirrored expansions in
two crates, which [194 §6](194-k-a5-general-and-nested-clip-groups.md) documents,
must not be repeated here.

---

## 9. Acceptance fixtures and tests

**No rights-cleared content is required. K-B9 is not a fixture-gated item.** Every
test below runs against synthetic graphs, solid/adjustment clips, or the existing
corpus (`crates/photonic-video/tests/fixtures/color_bars.mp4`, already committed).
Coverage goldens are checked against **closed-form geometry** — the area of a
circle, the area of a square minus an inscribed circle — not against a reference
implementation's output, which is also the clean-room-correct way to author them
(23 §3.4 point 4). Added fixture bytes: **zero**. No `AssetRightsManifest`
(23 §7.2) is engaged.

> **The one thing that would gate this item**, and is therefore excluded: a
> "realistic rotoscoping" demo fixture containing a person. That is
> `contains_voice_or_likeness` under 23 §12's manifest and converts K-B9 into a
> gated item for no test value — a rotating synthetic shape exercises every code
> path.

| # | Test | Where | Proves |
|---|---|---|---|
| T1 | Round-trip: `RotoPoint` list → `BezPath` → `PathData` → back, bit-identical | `timeline/roto.rs` unit tests | §3.2's absolute-handle choice |
| T2 | Shape interpolation: two keys, `Linear` / `Hold` / `Bezier`; midpoint equals the closed-form blend; before-first and after-last clamp (matching `anim::eval`, `anim.rs:204-210`) | `roto.rs` unit tests | §3.1 point 2 |
| T3 | `validate_rotos` rejects unsorted keys, duplicate `at`, mismatched point counts, `< 3` points, and `NaN` coordinates | `sequence.rs` `mod tests` | §3.3 |
| T4 | `assert_undo_roundtrip` (`ops.rs:2921`) for `SetRoto` and `SetRotoShapeKey`, including the create (`old: None`) and delete (`new: None`) directions | `ops.rs` `mod tests` | §5 |
| T5 | **A 12-point drag is one history step and never trips the `validate` debug assert**; an add-control-point is one `SetRoto` and also does not trip it | `ops.rs` `mod tests`, debug build | §2.5(a) — the regression this design exists to prevent |
| T6 | Rasterizer coverage goldens: a circle's coverage sums to `πr²` within tolerance; an `Add`+`Subtract` pair yields an annulus; `invert` is exactly `1 - c`; an empty mask is all-zero (**not** all-one) | `photonic-video/tests/roto_raster.rs` | §6.2 clauses 2, 5, 8 |
| T7 | **Scale invariance**: a roto graph rendered at Full, downsampled, agrees with the same graph at Draft within tolerance — added as a case to the existing harness | `crates/photonic-video/tests/scale_invariance.rs` (extends `:105`, `:125`) | §6.2 clause 4 / E-6 |
| T8 | CPU/GPU parity: `eval_cpu` and `Evaluator` agree on a `RotoRaster → MaskApply` graph within the established tolerance | `graph/eval.rs` + `eval_cpu.rs` tests | E-9; ROADMAP §10 point 6 |
| T9 | Hash sensitivity: moving one anchor changes `ContentHash`; changing `w`/`h` changes it; an identical shape at a different tick with identical resolved geometry yields the **same** hash (so a static roto caches across frames) | `compile.rs` `mod tests` | §6.2 clause 10 / PA-1 |
| T10 | Inert vs empty: a `MaskRef::Roto` naming a missing id emits **no** mask node and the consumer is unchanged; a valid empty mask emits an all-zero one | `compile.rs` `mod tests` | §6.2 clauses 8 and 9 — the distinction most likely to be collapsed |
| T11 | Serde: a v5 doc with rotos round-trips `to_json` → `from_json` → `finalize_load` unchanged; a v5 doc without them loads `rotos: []` and re-serializes without the key; `CURRENT_FORMAT_VERSION` is still 5 | `photonic-core/tests/timeline.rs` | §4 |
| T12 | Forward-compat: an unknown `MaskRef` `source` tag and an unknown `GradeMask` `shape_kind` load, are preserved verbatim, and re-emit unchanged — the sweep at `forward_compat.rs:117-147` grows from six discriminants to eight | `photonic-core/tests/forward_compat.rs` | §4.1 |
| T13 | Negative control: a **known** `MaskRef` tag with a malformed payload is rejected, not swallowed as `Unknown` — mirroring `forward_compat.rs:234` | `photonic-core/tests/forward_compat.rs` | 39 §2.2 rule 4 |
| T14 | GUI arm: create a roto, drag points at two ticks, bind as a clip matte, undo to empty — headless | `crates/photonic-gui/tests/video_ui_paths.rs` | ROADMAP §10 point 2 |
| T15 | **CAP-019 parity story**: the MCP arm (`create_roto` → `set_roto_shape_key` → `bind_roto`) and the GUI arm produce structurally equal documents | `crates/photonic-app/tests/acceptance_stories.rs` | ROADMAP §10 point 10 |
| T16 | Export determinism: the same roto graph exported twice is byte-identical | existing export-determinism harness | 03 §6 / §6.2 clause 7 |

### Definition of done → ROADMAP §10

| # | Answered by |
|---|---|
| 1 Core op + unit tests | `timeline/roto.rs` + `ops.rs` roto ops; T1–T5 |
| 2 GUI route | Monitor roto tool (draw / edit / key), shape list, keyframe lane; T14. **No exception recorded** |
| 3 MCP tool / schema / generated docs | [§8](#8-mcp-surface); `ci.yml:164-167` gate; T15 |
| 4 One verb = one undo unit | [§5](#5-undo-unit-and-exact-inverse); T4, T5 |
| 5 Additive serde / migration round-trip | [§4](#4-migration-and-format-version-impact); T11–T13 |
| 6 IR / eval / golden / sync coverage for the new pixel path | T6–T9, T16 — this is the item's heaviest obligation, since it adds two IR ops |
| 7 Hard gates green | Scale invariance (T7) and CPU/GPU parity (T8) are both hard-gate tier per 37 §4.2. Rasterization cost is a trend metric: one extra pass per mask per frame |
| 8 Legal / content / product gates | **[§7.4](#74-what-review-is-still-required-before-code) is a live gate** — the 23 §3.3 evidence record and reviewer sign-off must exist before code. No bundled bytes, no new dependency, no likeness fixtures |
| 9 Protected surfaces | `PowerWindow` untouched; `IrOp::Grade` arity untouched (§6.4); PA-1 consumed as designed (§6.2 clause 10); PA-7/PA-8 held — `RotoShapeKey.at` is a `Tick`, never a frame number or a float |
| 10 Goal-backward L1–L4 | [§1](#1-problem-and-user-outcome)'s six outcomes are the L4 script; T14 + T15 |

---

## 10. Risks, open questions and deliberate exclusions

### Risks

1. **Modelling roto as 3N `PropertyTrack`s.** The highest-probability way to get
   this item wrong, because 26 K-B9's Files line invites it and because it *fails
   silently* — the registry orphans the paths (`prop_registry.rs:244-257`) and
   `anim::eval` skips them (`anim.rs:201`), so the shape simply never animates.
   Mitigation: [§3.1](#31-why-roto-is-not-animprops-over-a-pathdata) and T2.
2. **Per-point commands.** The second-highest, and it passes a release build:
   batched per-point mutations break the correspondence invariant mid-batch and
   panic on `commands.rs:1748`'s `debug_assert` only in tests. Mitigation:
   [§3.6](#36-command-model-two-additions) and T5.
3. **Adding an `IrOp` variant is a four-file change.** `hash_op`
   (`compile.rs:2583`), `eval.rs`, `eval_cpu.rs` and `source_range.rs:88-107` are
   all exhaustive matches. Forgetting `hash_op` is the dangerous one — it compiles
   only if a catch-all exists, and produces silent cache collisions between
   different masks. T9 exists for exactly this.
4. **The GUI editor is not free.** §2.1's survey is honest that
   `direct_select.rs` and `handle_pen_tool` cannot be reused as written. The v1
   plan is a monitor-side editor beside `reframe.rs:232`; the risk is that it
   diverges from the vector tool's feel. Mitigation: reuse
   `geometry.rs:210`'s helpers and the same hit radii, and treat "extract a shared
   editing-target trait" as an explicit follow-up rather than a v1 stretch.
5. **Mode-gated shortcuts.** [194 §8.1](194-k-a5-general-and-nested-clip-groups.md)
   defect 6 records that `handle_global_shortcuts` dispatches vector tool commands
   unconditionally. A `video.roto_pen` command id must be mode-gated from the
   first commit, or two tools fire in one frame.
6. **Feather cost.** A separable Gaussian per mask per frame is one extra pass
   pair. With several masked grade ops on several layers this compounds. Watch it
   as a trend metric; the cheap mitigation is to skip the blur passes entirely
   when `feather == 0`, which is the common case.

### Open questions needing a product call

1. **Q1 — canvas space or source space?** [§6.4](#64-what-k-b9-deliberately-does-not-compile)
   applies the matte after the clip transform, so the shape is drawn in the space
   the user sees. The consequence is that animating the clip transform does not
   drag the roto along. *Recommendation: keep canvas space.* It is one coordinate
   convention for all masks (07 §4.1), it is WYSIWYG on the program monitor, and
   the alternative — source space — makes the mask invisible in monitor
   coordinates whenever a reframe is active (CAP-012), which is worse. A
   per-mask space flag is serde-additive later if a real workflow demands it.
2. **Q2 — does the roto tool auto-key?** When a user drags a point at a tick with
   no key, does a key appear (After Effects mask behaviour) or must they press a
   key button first? *Recommendation: auto-key.* Manual roto is per-frame work by
   nature; requiring a keystroke per frame is friction with no safety benefit, and
   the undo unit is the same either way (§5 row 2). This is a UX call, not an
   engineering one.
3. **Q3 — should `MaskRef::Roto` on a grade op be *rejected* or *silently inert*
   until K-B8 lands?** §6.4 keeps today's inert behaviour. *Recommendation: inert,
   plus a load-time `Warning` diagnostic naming the grade op*, so the user is told
   rather than left wondering. A hard rejection would make a K-B8-authored file
   unloadable on a K-B9-only build, which is the failure mode §4.1 works to avoid.

### Deliberately excluded

- **Tracking, in every form.** [§7.3](#73-techniques-k-b9-explicitly-does-not-propose).
  K-B10 / S14 own it and are `product-blocked`.
- **`expand` (grow/shrink the shape).** Genuinely useful and genuinely more
  involved than feather — offsetting a closed curve needs self-intersection
  handling. `crates/photonic-core/src/ops/offset.rs` is the reuse path when it
  lands. Out of v1 so `RotoShapeProps` stays two scalars.
- **Motion blur from shape velocity.** Multiplies raster cost by the sample count
  and needs a shutter model. Serde-additive later.
- **Open (stroked) shapes.** v1 shapes are closed; a stroked roto is a different
  primitive (width, caps, joins) and would drag `photonic-core`'s stroke model
  into the video path.
- **Shape sharing between clips.** §3.4. A sequence-level registry plus a
  `MaskRef::SharedRoto` variant is additive later.
- **Path booleans on the geometry.** §6.2 clause 5 folds coverage arithmetically
  instead. Cheaper, order-independent per shape, and it makes `opacity` on a
  `Subtract` shape meaningful.
- **The masked composite node.** K-B8's, by [§6.3](#63-the-binding-point-for-k-b8--197).
- **A roto in the vector document.** A roto is timeline data on a clip, not a
  `SceneNode`; it must not reach the layers panel or the SVG/PDF exporters.

---

## 11. Clean-room provenance

Per [26 §2](../specs/video-editor/26-kdenlive-mlt-parity.md#2-clean-room-and-licensing-fence)
point 2 and §7 (K-B9's own entry carries no Clean-room line; this supplies it):

- **Sources used.** (a) Photonic's own code and specs, cited by `file:line`
  throughout; (b) 26 K-B9's requirement statement, itself derived from Kdenlive's
  `CC-BY-SA-4.0` user documentation as a *requirements source*, cited and never
  pasted; (c) open standards and foundational literature named in
  [§7.2](#72-techniques-k-b9-proposes-with-provenance) — W3C SVG and ISO 32000
  fill rules and path grammar, and the Bézier/de Casteljau construction; (d) the
  published APIs of `kurbo` and `lyon`, both already workspace dependencies.
- **Sources not used.** The Kdenlive source tree, the MLT/`mlt++` source tree,
  frei0r, and any GPL/LGPL derivative were not inspected for this item. No
  identifier, comment, constant, control flow or test case here derives from them.
  Also not inspected, and specifically relevant to roto: the source of any
  commercial NLE or compositor. The implementer records the
  [23 §3.4](../specs/video-editor/23-legal-open-source-implementation-routes.md#34-clean-room-protocol)
  attestation for the `core-timeline` and `photonic-video-engine` subsystems, and
  an independent reviewer checks provenance before merge.
- **Design origin.** Every concrete decision here derives from Photonic's own
  constraints, not from a reference product: the whole-shape keyframe comes from
  `PropValue`'s five kinds and the `&'static` registry
  (`anim.rs:52`, `prop_registry.rs:200`); the whole-key command payload comes from
  `commands.rs:1748`'s per-command `validate` assert; the inert-dangling-reference
  rule comes from the offline-LUT precedent (`photonic-render/src/grade.rs:463`);
  the normalized coordinate space comes from `PowerWindow` (07 §4.1) and E-6; the
  rasterizer is the tessellator Photonic already ships.
- **Photonic-ahead properties preserved** (26 §5, ROADMAP §9). Times are flicks
  `Tick`, never frame counts or floats (PA-8). Ranges stay half-open (PA-7).
  Failures are typed `EditError`/`ValidationError`, never strings (PA-9). Roto
  geometry is hashed into `ContentHash`, so per-node caching and hash-natural
  invalidation are consumed as designed rather than worked around (PA-1). The mask
  is a single working-format value in one backend (PA-3, E-8's single-working-
  format property). No reference NLE limitation is ported: shapes may nest
  arbitrarily via add/subtract, feather is per shape, and the mask is available to
  the clip, the grade and — via K-B8 — an arbitrary effect subtree.
- **Bundled bytes: none.** No asset ships with this item, so 23 §7.2's
  `AssetRightsManifest` gate is not engaged and K-B9 is **not** fixture-gated
  ([§9](#9-acceptance-fixtures-and-tests)).
- **No new dependency.** `lyon` (`crates/photonic-render/Cargo.toml`) and `kurbo`
  (`crates/photonic-core/Cargo.toml`) are already in the build. Nothing in 26 §2's
  reject list is contemplated, directly or transitively.
- **But the licence is not the gate.** Per 23 §11.1's standing rule, the permissive
  licences of `lyon` and `kurbo` do not clear the underlying techniques.
  [§7.4](#74-what-review-is-still-required-before-code) is the gate, and it is open.

---

## Follow-ups

Changes this document deliberately did **not** make to existing documents (it may
not edit them; each needs its own change):

1. **`26-kdenlive-mlt-parity.md`, K-B9 (line 371).** Two corrections. (a) The item
   carries **no Clean-room line**, though §7 says every item does and this one has
   specific provenance risk; it should gain one pointing at
   [§7](#7-patent-and-algorithm-review--the-mandatory-gate) of this document and at
   23 §11.1's standing rule. (b) The Files line's "`AnimProps` on control points"
   is not implementable as written —
   [§3.1](#31-why-roto-is-not-animprops-over-a-pathdata) gives four reasons. It
   should read "whole-shape keyframes reusing `anim::Interp`, with `AnimProps` for
   the per-shape scalars", so an implementer is not sent down the orphaned-track
   path. The sketched variant name `MaskRef::Path { .. }` should become
   `MaskRef::Roto { .. }` ([§3.5](#35-the-maskref-change-and-the-two-unknown-arms-it-forces)).
2. **`07-color-grading.md` §4.2.** The parenthetical "8-bit coverage,
   0=unmasked/255=fully masked in that crate's convention" **inverts the words**
   relative to both `crates/photonic-core/src/raster/mask.rs:4-6` ("`0` = fully
   masked / deselected, `255` = fully selected") and its own adjacent formula
   `weight = data[i]/255`. Suggested correction: "0 = outside the mask (the
   correction does not apply), 255 = inside (full strength)". §4.2 should also
   record that a roto tool now exists and point at `MaskRef::Roto`.
3. **`39-document-lifecycle.md` §2.2.** The enumeration of open-ended enums that
   must gain unknown-preserving variants omits **`MaskRef` and `GradeMask`**, which
   is why [§4.1](#41-the-one-real-forward-compat-hazard-stated-plainly) exists. Both
   should be added to the list, and `forward_compat.rs`'s six-discriminant sweep
   described as eight.
4. **`08-*` graph-node catalogue.** If a roto should also be reachable as a
   composition-graph source (`GraphOp::MaskRoto`), that is 08's call, not this
   document's. K-B9 deliberately does not add a `GraphOp` variant; `MaskRef::Roto`
   plus `Clip.rotos` covers the clip-scoped cases.
5. **`11-testing-phasing.md`.** The corpus section should record roto's
   closed-form coverage goldens as a category — geometry checked against
   mathematics rather than against a reference render — since that pattern will
   recur for K-B7 luma wipes.
6. **`ROADMAP.md` §0 progress table** — add a K-B9 row when the item lands, with
   its commit, per the existing convention. §8's S14 row should cross-reference
   that K-B9 ships **without** any tracking dependency, so the roto tool is not
   read as blocked behind S14.
