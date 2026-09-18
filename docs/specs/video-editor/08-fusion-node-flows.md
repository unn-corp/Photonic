# 08 — Fusion Node Flows: Compositions, Catalog, Editor UI

**Depends on:** 01-data-model.md (§8 node graphs — normative data model), 02-engine.md (§2 IR + compile splice semantics — normative engine contract), 04-ui-mode-timeline.md (shell/panel ownership). **Decisions:** D-06 (node flows at both levels), D-08. **Owns:** `GraphOp` catalog, `EffectKind` registry (shared with clip-level `ClipEffect`, 01 §6.3), port-type/coercion rules, per-clip + project composition lifecycle, `NodeEditor` drawer interior (04 §4.1 delegates it here), egui-snarl integration.

This doc specs what users **author**; 02 owns how it's **evaluated**. Every `GraphOp` below states its IR lowering; the IR additions this catalog requires (§3.4) are now carried by 02 §2's `IrOp` enum directly.

---

## 1. Scope & terminology recap

Scope per 00 §5: node type catalog, per-clip + project graphs, node-editor UI, eval semantics, caching.

> **Dependency status:** `egui-snarl` is **not** in any `Cargo.toml`, and `photonic-gui` has no node-editor module. References to it below describe an intended integration, not a shipped one.

<!-- spec-assert: dep-absent egui-snarl -->
<!-- SD-13 (27 §3): `egui-snarl` is referenced as an intended integration but is not adopted; pinned so a partial addition can never leave this "not in any Cargo.toml" claim stale. -->


**Canonical terminology (used set-wide):** `NodeGraph` = the data structure (01 §8); **composition** = a per-clip graph; **project graph** = the project-level graph; "Fusion" appears only as an analogy to DaVinci Resolve's page, never as a type or feature name. The phrase "node flow" (this doc's title lineage) is retired from prose — title and filename stay for doc-map stability. **Viewer** = the node editor's composed-output inset (§6.1), distinct from the **program monitor** (04 §3), which always shows the timeline's true output.

- `NodeGraph` (01 §8): one arena entry per composition (per-clip or project), `nodes: HashMap<GraphNodeId, GraphNode>`, `edges: Vec<GraphEdge>` typed by `OutPort`/`InPort`, exactly one `Output` node, `ui: HashMap<GraphNodeId, NodePos>`.
- `GraphNode { id, op: GraphOp, params: AnimProps<EffectParams> }` — `op` carries structural/reference data (asset ids, shape kind, node arity); animatable knobs live in `params` (same `AnimProps`/`PropPath` system as clip transforms, 01 §6).
- Compile splice points (02 §2): step 3 substitutes **only the clip's source op** with its `NodeGraph` when `clip.composition` is set — `ClipIn` binds to the clip's post-trim/speed source, and the graph's `Output` feeds the remainder of the default chain (`Transform2D` from AnimProps + per-format reframe → effects → grade), the Resolve Fusion→Edit→Color model. Step 6 splices `project_graph` between the folded sequence output and the final `Output` node.
- CAP-016 is the acceptance gate: two-input merge composition on a clip must play back and export; a project-graph operator must affect final output only (not per-clip previews inside composition editing — see §5, §6.7).

---

## 2. GraphOp catalog v1

Ports column format: `name:Type`. `Image` = `Rgba16Float` linear premultiplied (matches `EngineFrame`, D-09). `Mask` = single-channel float 0–1. `Value` is defined for forward-compat only — no v1 op exposes one (§3.1).

| Op | In ports | Out ports | Key params | Lowers to (02 IR) | Notes |
|---|---|---|---|---|---|
| `Output` | `in:Image` | — | — | `IrOp::Output{w,h}` | Terminal; exactly one per graph (01 §8). |
| `ClipIn` | — | `out:Image` | — | binds to clip's source op post trim/speed (02 §2 step 3) | Only legal inside a **per-clip** composition; edit-time rejected in the project graph. |
| `MediaIn` | — | `out:Image` | asset ref (structural) | `IrOp::DecodeVideo`/`DecodeStill` per `AssetKind` | Audio streams on the asset are ignored here — audio stays in 09's mixer graph, never the node graph. Standalone `MediaIn` (not the host clip) samples at sequence tick directly unless followed by `TimeOffset`; used inside a composition it may instead need the clip's local time — recommend a `time_source: Local \| Sequence` param, default `Sequence`, so a second video dropped in for a keyed overlay (AS-2) advances independently of the host clip's trim. |
| `VectorIn` | — | `out:Image` | `VectorRef` (structural) | `IrOp::RasterVector{vref, doc_state, w, h}` | `w,h` from compile context (§7). |
| `SolidColor` | — | `out:Image` | `color: Color` (animatable) | `IrOp::SolidColor` | |
| `Merge` | `a:Image`(top), `b:Image`(bottom) | `out:Image` | `mode: BlendMode` (26 values, `photonic-core::layer::BlendMode`), `opacity: f32` | `IrOp::Merge{mode,opacity}` | **Binary only** — see §3.2. |
| `Transform2D` | `in:Image` | `out:Image` | position, scale, rotation, anchor | `IrOp::Transform2D` | Mirrors `ClipTransform` fields. |
| `Crop` | `in:Image` | `out:Image` | left/top/right/bottom | `IrOp::Crop` | |
| `Resize` | `in:Image` | `out:Image` | `w,h,fit: FitMode` | `IrOp::Resize` | |
| `Blur` | `in:Image` | `out:Image` | radius | `IrOp::Effect{kind: EffectKind::Blur}` | |
| `Sharpen` | `in:Image` | `out:Image` | amount, radius | `IrOp::Effect{kind: EffectKind::Sharpen}` | |
| `Glow` | `in:Image` | `out:Image` | radius, threshold, intensity, tint | `IrOp::Effect{kind: EffectKind::Glow}` | Reuses the GPU glow pass lineage already in `photonic-render::renderer::glow_renderer`. |
| `ChromaKey` | `in:Image` | `out:Image` | key_color, tolerance, edge_softness, spill_suppress | `IrOp::Effect{kind: EffectKind::ChromaKey}` | |
| `LumaKey` | `in:Image` | `out:Image` | threshold, softness, invert | `IrOp::Effect{kind: EffectKind::LumaKey}` | |
| `MaskShape` | — | `out:Mask` | shape: Ellipse\|Rect\|Polygon, position/size/rotation/feather (animatable); polygon vertices static in v1 | `IrOp::Effect{kind: EffectKind::MaskShapeGen}` (0-input generator arity, §3.4) | Vertex-level animation is a post-v1 candidate. |
| `MaskFromMatte` | `in:Image` | `out:Mask` | — (v1: none; auto subject cutout) | `IrOp::MatteExtract` (new IR node, §3.4) | Wraps `photonic-matte::remove_background` (U²-Net-p, on-device). **This is CPU inference, not a GPU shader pass** — expensive per distinct input frame. Treat as a "slow node": cache aggressively by input content hash (already the default per 02 §5), show a computing-spinner placeholder (emits fully-opaque/no-op mask meanwhile per §3.3), never run on the engine thread. |
| `Invert` | `in:Image` or `in:Mask` | matches input type | — | `IrOp::Effect{kind: EffectKind::Invert}` | Generic over Image/Mask (two thin op-kind variants sharing one node UI entry). |
| `ChannelSplit` | `in:Image` | `r:Mask, g:Mask, b:Mask, a:Mask` | — | `IrOp::ChannelSplit` (new IR node, §3.4) | `a` output is the canonical way to use alpha-as-mask (resolves the Image→Mask coercion ambiguity, §3.1). |
| `ChannelCombine` | `r:Mask, g:Mask, b:Mask, a:Mask` | `out:Image` | — | `IrOp::ChannelCombine` (new IR node, §3.4) | Missing channel inputs default to 0 (r/g/b) or 1 (a) — opaque black if fully unwired. |
| `Grade` | `in:Image` | `out:Image` | embeds a full `Grade` (07's CDL/wheels/curves/HSL/LUT stack) | `IrOp::Grade{ops}` | Same `Grade` type `Clip.grade` uses — placing it in-graph vs. on the clip are two surfaces for one mechanism. |
| `Lut` | `in:Image` | `out:Image` | LUT asset ref | `IrOp::Grade{ops: vec![GradeOp::Lut{asset}]}` | Reuses `Grade`'s IR rather than inventing a parallel path — a single-op `Grade`. |
| `Text` | — | `out:Image` | styled text (font/size/color/position — subset of `CaptionStyle` fields), static or keyframed string | `IrOp::TextGen` (new IR node, §3.4) | Basic titles/lower-thirds, not word-timed. Captions (06) are a **separate system** (`CaptionTrack`/`CaptionOverlay`, driven by transcription timing); `Text` shares the glyphon rendering pipeline but has no word-level timing model. Do not conflate: captions are track-level and always-on-top; `Text` is a compositable node a user can key, transform, and merge like any other layer. |
| `TimeOffset` | `in:Image` | `out:Image` | `offset: Tick` (animatable, but see §3.4 cost note — v1 recommends treating as a per-instance constant) | compiler duplicates the upstream subgraph at `t - offset` (§3.4) | **v1 includes only this one time op** — no ramps/trails-with-decay, no time-remap curves. Echo/trail looks are built by chaining several `TimeOffset → Merge` pairs at different offsets. |
| `Switch` | `in0..inN:Image` | `out:Image` | `selected: u32` (Enum, animatable) | none — resolved at **compile time** by rewiring the chosen input's producer directly to `Switch`'s consumers (part of 02 §2 step 7 constant-fold) | Zero eval cost; unselected branches are dead-eliminated same as disabled clips. |
| `Note` | — | — | `text: String` | none — never compiled | Pure canvas annotation; has no ports and cannot participate in edges. |

### 2.0b Transition catalog v1 (clip-level, CAP-008)

Transitions live on clips (01 §5 `Transition { kind, duration, params }`), not in graphs — cataloged here because this doc owns the effect/op registries. The engine renders a transition as a time-parameterized mix over the transition window. **Clips do not overlap** — the compositor samples the outgoing clip past its out point into its source handle ([38 §1.1](38-sequence-semantics.md#11-the-apparent-contradiction-resolved)); the earlier "clip-overlap window" wording described a model that was never built and is unsatisfiable against [01 §4](01-data-model.md)'s non-overlap invariant. Named v1 set (PM review — the catalog must be explicit so P6 doesn't ship with one transition):

| Kind | Params | Behavior over the overlap window t∈0..1 |
|---|---|---|
| `CrossDissolve` | curve (ease) | opacity mix a→b |
| `DipToBlack` | curve | a→black (t<0.5), black→b (t≥0.5) |
| `DipToColor` | color, curve | same, through the given color |
| `Wipe` | direction (L/R/U/D), softness | animated edge reveal |
| `Push` | direction | a slides out while b slides in |

Audio: the crossfade window equals the video transition window by default, using the same borrowed handles and the same clamping, with `FadeShape::EqualPower` (09 §2). Boundary declick does **not** engage across a transition — the crossfade already provides continuity ([38 §1.4](38-sequence-semantics.md#14-audio)). Additional kinds are additive registry entries post-v1.

### 2.1 Post-v1 candidates (explicitly out of scope)

`Tracker` (motion tracking — SPEC non-goal), `ParticleGen`, `Displace`/warp, 3D nodes (camera/depth/3D transform), keyframed `SpeedMap`-style time remap, per-vertex `MaskShape` polygon animation, Value-wire parameter linking / expression nodes, audio nodes inside the visual graph.

---

## 3. Port type system

### 3.1 Types + coercion

`PortType = Image | Mask | Value`. No `Audio` port type in v1 — the visual node graph and the audio mixer graph (09) are separate systems; unifying them is a post-v1 option (B) if a real use case emerges (e.g., audio-reactive params), rejected for v1 (option A, recommended) because no acceptance story needs it and it would couple 08's compile pass to 09's mixer graph for no payoff.

Coercion rules (auto-applied at the port boundary, no node needed):
- **Image → Mask**: unpremultiply, then Rec.709 luminance of RGB. (Alpha-as-mask is not this path — use `ChannelSplit`'s `a` output, which is directly `Mask`-typed.)
- **Mask → Image**: broadcast to `{r=g=b=mask, a=1}` (opaque grayscale) — mainly useful for previewing a mask by wiring it straight into `Output` or a `Merge` input during authoring.
- **Value → anything**: not implemented in v1 (§2.1) — no op currently emits a bare `Value` output, so this rule is unreachable in practice; kept in the enum for the post-v1 expression-node candidate.

Port-type mismatches are rejected **at edit time** (the wire simply refuses to connect — no invalid edge is ever representable). This is a stronger, earlier guarantee than 02 §2 step 3's "diagnostics, never black frames" — that clause covers a different failure class (a whole composition failing to satisfy the compiler, e.g. a stale/corrupt file missing its `Output`), not per-port type errors, which can't reach compile at all.

### 3.2 Merge is binary (decision)

Recommended: **binary `Merge` only**, Fusion-style — chain multiple `Merge` nodes for N-layer stacks. Rejected alternative: an N-input `Merge` — it would need a per-input mode/opacity/order list (effectively re-inventing a mini track stack inside a node), complicates the port-type table, and buys nothing a chain of binary merges doesn't already give with a clearer visual graph shape.

### 3.3 Missing-input defaults, by op family

| Family | Ops | Missing-input behavior |
|---|---|---|
| Sources (no inputs) | `MediaIn`, `VectorIn`, `ClipIn`, `SolidColor`, `MaskShape`, `Text` | N/A — always produce from params/refs. |
| Unary filter | `Transform2D`, `Crop`, `Resize`, `Blur`, `Sharpen`, `Glow`, `ChromaKey`, `LumaKey`, `Invert`, `Grade`, `Lut`, `TimeOffset` | Transparent black (premultiplied 0) if `in` unwired. |
| `MaskFromMatte` | — | Fully-opaque (all-1) mask if `in` unwired **or** while a matte computation is pending — never blocks or zeroes downstream compositing while "computing." |
| `Merge` | — | Missing `a` → passthrough `b`; missing `b` → passthrough `a`; both missing → transparent black. |
| `ChannelCombine` | — | Missing `r`/`g`/`b` → 0; missing `a` → 1 (opaque black if fully unwired, not transparent — matches how a solid color with no alpha channel is normally interpreted). |
| `Switch` | — | Missing selected input → passthrough first connected input if any, else transparent black. |
| `Output` | — | Missing `in` → the **whole composition** is treated as unsatisfied: compiler falls back to the clip's default chain (or, for the project graph, skips the splice entirely) and surfaces a diagnostic — same fallback path as a cycle or stale-file error (02 §2 step 3). |

### 3.4 IR support in 02 — resolved

02 §2's `IrOp` enum now carries everything this catalog needs; recorded here so the lowering column above is traceable:

1. **`IrOp::MatteExtract`** (in 02 §2) — wraps `photonic-matte`; CPU-inference worker-thread op, not a wgpu pass, exactly as this doc's `MaskFromMatte` row requires.
2. **`IrOp::TextGen`** (in 02 §2) — shares the glyphon text pipeline with `CaptionOverlay` but is a plain generator node, not cue-timed.
3. **`IrOp::ChannelSplit` / `IrOp::ChannelCombine`** (in 02 §2) — multi-output/multi-input; the `FrameGraph` arena already supports multi-port nodes via `OutPort`-addressed edges (01 §8).
4. **`IrOp::Effect` 0-input generator arity** (in 02 §2) — arity comes from the `EffectKind` registry entry (needed for `MaskShapeGen`), not hardcoded per-call.
5. **`TimeOffset` compile strategy** (in 02 §2, compilation step 7): the compiler duplicates the upstream subgraph re-evaluated at `t − offset`; duplicates dedup naturally via content hashing (same subgraph at same time = same hash); soft cap of 4 distinct offsets per composition (diagnostic warning, not a hard error). Cost scales with **distinct** offset values only. Still the highest-cost feature in this doc — perf-tested in §9.

---

## 4. Per-clip composition lifecycle

- **Create.** Clip context menu → "Open as Node Composition" (already named in 04 §2.6's context-menu table). Allocates a new `GraphId` in `TimelineProject.graphs`, seeds it `ClipIn → Output` (fixed v1 seed — no "seed from current chain" variant), sets `clip.composition = Some(id)`. Rejected at edit time for `ClipSource::Adjustment` clips — an Adjustment clip has no source op to substitute (07 §6.6 states the rule + test hook).
- **Source substitution, not chain bypass (normative).** Per 02 §2 step 3, setting `composition` replaces **only the clip's source op** — `clip.transform`, `clip.reframe`, `clip.effects`, and `clip.grade` all continue to apply, on top of the composition's `Output`. Nothing goes inert; identity transform / empty effects / `None` grade fold away as no-ops, so a "pure comp" costs nothing. Per-`SequenceFormat` reframe (CAP-012) works on composited clips exactly as on plain clips, because the reframe lives in the still-applied default chain. Revert is lossless because nothing was ever displaced except the source binding.
- **Open/edit.** See §6 (node editor UI + the central-panel reconciliation with 04).
- **Delete/revert.** `clip.composition = None` — a plain `SetClipProp`-shaped edit (01 §10), instantly restoring the plain source binding; `transform`/`effects`/`grade`/`reframe` were never displaced (source substitution, above). The `NodeGraph` itself is **not** deleted from the arena on revert (avoids losing work from an accidental toggle); it becomes unreferenced. A "prune unused graphs" GC pass is a reasonable low-priority maintenance action, not required for v1 (arena entries are small; 01 §9 doesn't need a special-case here).
- **Copy/paste compositions between clips (decision).** `composition` is a `GraphId` into a shared arena — naively copying the field would **alias** two clips to one graph (editing one edits both). Normative: paste **deep-clones** the `NodeGraph` into a fresh `GraphId` before pointing the target clip's `composition` at it. This is a real correctness requirement, not a nice-to-have — flagged as a test hook in §9.
- **Live playback.** No special wiring needed: any `GraphCmd` bumps `doc_generation` exactly like any other edit (01 §10, 02 §1); the engine's existing re-snapshot + hash-natural cache invalidation (02 §5) picks it up on the next compile. Timeline playback and the program monitor already "just work" once the compile splice (02 §2 step 3) exists — CAP-016's "composition's result plays back in the timeline" is a consequence of the engine architecture already specified in 02, not new plumbing.

---

## 5. Project-level graph

One `TimelineProject.project_graph: Option<GraphId>`, shared across all sequences and all `SequenceFormat`s (it is document-level, not per-sequence). Splice point is between the folded per-format composite and the final `Output` (02 §2 step 6) — confirmed **post**-format: every clip's reframe/format-specific work has already happened upstream, so the project graph always operates in the active format's final pixel dimensions, uniformly, regardless of which format is active. A project graph with its own `Resize`/`Crop` nodes tuned for one aspect ratio will need manual retuning per format — a known v1 limitation, not a blocker (per-clip reframe itself is unaffected: it lives in the clip's default chain, §4).

Typical uses: final vignette, watermark/logo overlay (`VectorIn`/`MediaIn` + `Merge`), output LUT (`Lut` node), custom letterbox/pillarbox bars beyond the automatic ones 04 §3.3 already draws (`SolidColor` + `Crop`/`Transform2D`).

`ClipIn` is invalid inside the project graph (no clip context exists there) — the node palette simply omits it when editing this graph; a corrupt file somehow containing one is diagnosed and dropped, never silently miscompiled.

Same editor UI as per-clip compositions (§6), reached via a small segmented control at the top of the node canvas: **`[ Clip: <name> | Project Graph ]`**, defaulting to whichever was last open. Opening the project graph needs no clip selection.

CAP-016's "project-graph operator affects final output only" is satisfied structurally: while editing a *clip's* composition, the viewer-pinning mechanism (§6.7) previews **upstream of** the project splice by design, so a vignette/watermark never obscures per-clip work-in-progress.

---

## 6. Node editor UI (egui-snarl)

### 6.1 Shell placement — reconciling with 04

04 §4.1 records the adjudicated split: the left-rail `DrawerGroup::NodeEditor` is palette + inspector + graph info only, and the graph canvas is a central-panel content state (04 §1.1 point 3). A full egui-snarl canvas does not fit a narrow property-drawer (the existing drawer shape is a scrollable list of collapsible property sections, `panels/mod.rs::draw_drawer` — built for that, not for 2D canvas interaction). The split, consistent with D-02 (one more central-panel content state, not a new rect):

- **Central panel**, while a composition is being edited: the node canvas occupies the majority of the existing program-monitor rect, with a resizable inset (default 70/30 split, draggable, ratio persisted like other panel prefs) — the **viewer** (§1) — showing the live composed output: either the true `Output` or whatever node is pinned (§6.7). The bottom timeline panel and its playhead/scrub controls stay live and visible throughout (you can scrub while node-editing, as in Resolve's Fusion page).
- **Left rail `NodeEditor` drawer** (04's shell): add-node search palette, the selected node's full param inspector, and graph-level info (which clip/project is open, node/edge counts). This is exactly the narrow-list shape the drawer chrome is built for, so 04's shell is reused unchanged.

Escape path: an explicit "Back to Timeline" affordance (button + `Esc`) restores the plain program-monitor central panel; the composition itself is unaffected (editing state, not document state).

### 6.2 Canvas interactions

Pan/zoom (egui-snarl native), node drag (native), wire drag with **port-type-colored** sockets that refuse an incompatible drop (§3.1 — no invalid edge is ever representable). **Keyboard fallback (a11y, 13 §16):** Tab cycles node selection, arrows nudge the selected node, Enter opens its inspector, Delete removes it — the canvas is fully operable without a pointer except wire-drag, which has the palette's "connect to selected" affordance as fallback. Add-node via the left-rail search palette (type-to-filter, grouped by family: Sources / Compositing / Filters / Keys / Masks / Color / Generators / Time / Utility) or a canvas-native right-click "Add Node" menu mirroring the same list. Box-select (marquee) + align/distribute are thin custom layers over snarl's selection primitives (same idiom as the timeline's marquee-select, 04 §2.6, for muscle-memory consistency).

### 6.3 Node body previews

Per-node thumbnail toggle, **off by default** — each visible thumbnail is one extra eval+readback per presented frame, and `MaskFromMatte`/`Glow` are expensive enough that "always render every node's thumbnail" would visibly cost frame budget (02 §8). Recommend: a small per-node pin toggle in the node header (explicit opt-in, not automatic-for-all-visible-nodes), no global cap needed since pins are a deliberate user action.

### 6.4 Param editing

Two levels, same pattern the app already uses elsewhere (inline widgets + drawer, not a third UI paradigm): 2–3 most-load-bearing params inline on the node body (e.g. `Merge` shows `mode` dropdown + `opacity` slider directly — those two are touched constantly), full param set (all of `AnimProps<EffectParams>`, keyframe add/remove, easing) in the left-rail inspector (§6.1) when the node is selected, sourced from the same `prop_registry` (01 §6.2) vector props already use.

### 6.5 Keyframe indicators

Small diamond glyph on animated-param rows in the inspector (matches the existing keyframe-curve UI convention, 01 §6/07 own the curve editor itself); a corner badge on the node body when **any** param on that node has ≥1 keyframe, so a glance at the canvas shows which nodes are animated without opening each inspector.

### 6.6 Diagnostics & cycle refusal

- **Cycle refusal** is instant, local, edit-time feedback — `graph_ops::add_edge` (mirrors `timeline/ops.rs`'s `Result<TimelineCmd, EditError>` pattern, 01 §10) returns `Result<GraphCmd, EditError::WouldCreateCycle>`; the wire drop simply snaps back with a toast. No invalid state is ever persisted (01 §8: "edge insertion cycle-checks, edit op fails, never panics").
- **Type-mismatch / compile-fallback diagnostics** (02 §2 step 3) need to carry the offending `GraphNodeId` so the editor can badge the exact node with a red exclamation glyph, not just show a generic "composition failed" toast — a requirement this doc places on 02's diagnostic type (coordination note, §9).

### 6.7 Viewer pinning

```rust
// session-only (engine/editor state), never document state
pub struct ViewNodeOverride { pub graph: GraphId, pub node: GraphNodeId }
```

Passed as an extra compile input alongside `(sequence, format, tick)`. When set and the graph being viewed matches the one currently open, the compiler reroutes the `FrameGraph`'s effective output to the pinned node's `IrNodeId` instead of the true `Output` — the same DAG is built (all shared upstream nodes stay cache-compatible), but nothing downstream of the pinned node evaluates. `EngineCmd::Export` never carries a `ViewNodeOverride` — export always compiles against the real `Output`, giving CAP-016's "export shows the composed result" guarantee independent of whatever a user happened to have pinned while authoring.

---

## 7. Evaluation semantics — user-visible rules

- **Lazy, pull-based from `Output`** (or the pinned node, §6.7): only ancestors of the evaluated output node cost anything. Disconnected scratch/experiment nodes left on the canvas are free — a normal Fusion-style workflow (keep alternates around, wire in the one you want).
- **Content-hash caching is per-node, not per-graph** (02 §5): tweaking one param re-evaluates that node and everything downstream of it; unrelated branches and everything upstream reuse their cached textures unchanged. A heavy `Blur` sitting upstream of a `Merge` whose opacity you're scrubbing feels instant, because the `Blur`'s cache entry doesn't change.
- **`TimeOffset` cost is real and visible** (§3.4): each additional *distinct* offset value in a composition is a full extra evaluation of everything upstream of that node, every frame it's on-screen. Reusing the same offset value across multiple `TimeOffset` nodes is free (identical content hash, one cached instance).
- **Resolution context**: a composition evaluates in the active `SequenceFormat`'s pixel dimensions by default (`Output.w/h` = format's `w,h`). `MediaIn`/`VectorIn` sources decode/rasterize at native resolution; nothing auto-fits — a `Resize`/`Crop`/`Transform2D` the user wires is the only way pixels change size. **Region-of-interest** (computing only the on-screen crop through the whole chain instead of full frames everywhere) is explicitly **post-v1** — v1 pays the full-frame cost at every node, consistent with 02 §8's per-frame budget table already assuming this.

---

## 8. Undo integration

Every graph edit is a `TimelineCmd::GraphEdit(GraphCmd)` (01 §10):

```rust
pub enum GraphCmd {
    AddNode { .. }, RemoveNode { .. },
    AddEdge { .. }, RemoveEdge { .. },
    SetNodeParam { old, new },          // mirrors SetClipProp's shape
    SetKeyframe { .. }, RemoveKeyframe { .. },   // reuses 01 §6's generic keyframe commands, graph-scoped
    MoveNode { old: NodePos, new: NodePos },     // editor-position edits — undoable, but see below
}
```

- `MoveNode` coalesces per drag gesture, keyed by `(GraphId, GraphNodeId)` — identical mechanism to clip-drag and keyframe-drag coalescing (01 §10). Node positions **are** undoable (unlike `Track::height_px`, which 01 §4 explicitly marks UI-only-but-persisted) — a graph's layout is part of what a user is composing, closer to canvas object placement than a panel preference.
- Creating/clearing a composition itself (`clip.composition = Some/None`) reuses the existing generic `SetClipProp { old, new }` variant (01 §10) — no new command needed.
- `MoveNode`, `SetNodeParam`, etc. delegate through `graph_ops.rs` pure functions, same "GUI and MCP call the same op" rule as `timeline/ops.rs` (01 §10, CAP-019 parity).

---

## 9. Risks & test hooks (for 11-testing-phasing.md)

| Risk | Mitigation / test hook |
|---|---|
| `TimeOffset` duplicate-subgraph compile cost (§3.4) | Perf test: composition with 4 distinct offsets against 02 §8's compile/eval budgets; diagnostic-warning threshold verified, not a hard failure. |
| Graph-compile splice correctness | Unit tests for 02 §2 step 3 (per-clip) and step 6 (project) exact insertion points, including the `Output`-missing fallback (§3.3) and cycle-refusal-never-reaches-compile invariant. |
| Type-check fixtures | Fixture graphs with each coercion rule (§3.1) and each missing-input default (§3.3) exercised; golden expected pixels. |
| Golden comp renders | CAP-016's own criterion — two-input `Merge` composition, rendered + pixel-compared, both in timeline playback and export paths. |
| Copy/paste aliasing (§4) | Regression test: paste a composition onto a second clip, edit the second, assert the first is unchanged (deep-clone, not shared `GraphId`). |
| Reframe on composited clips (§4) | Golden render of a composited clip across ≥2 `SequenceFormat`s, asserting the per-format reframe applies on top of the composition (positive case — source-substitution semantics, 02 §2 step 3). |
| `MaskFromMatte` perf | Per-frame U²-Net-p inference cost measured against the "computing" placeholder path (§3.3, §2 catalog row) — never on the engine thread; scrub-while-computing must not stall playback. |
| Diagnostic granularity | 02's compile-diagnostic type must carry `GraphNodeId` (§6.6) — a coordination requirement on 02's diagnostic type, restated there. |
| egui-snarl integration | Pin exact crate version (11 §8 dependency notes). Data model (`NodeGraph`/`GraphNode`/`GraphEdge`/`NodePos`, 01 §8) is UI-library-agnostic by construction — fallback is `egui_node_graph2` with the same data, a UI-layer-only swap, no migration. |

---

## 10. Summary of new/changed surfaces

| Surface | Change |
|---|---|
| `crates/photonic-core/src/timeline/graph_ops.rs` (new) | Pure `GraphCmd`-producing functions: `add_node`, `remove_node`, `add_edge` (cycle-checked), `remove_edge`, `set_node_param`, `move_node` — mirrors `timeline/ops.rs`. |
| `crates/photonic-core/src/timeline/effect_kind.rs` (new, or extends 01's `prop_registry.rs`) | `EffectKind` registry: `Blur, Sharpen, Glow, ChromaKey, LumaKey, Invert, MaskShapeGen`, shared by `ClipEffect` (01 §6.3) and `GraphOp`'s filter-family variants. |
| `crates/photonic-video/src/graph/ops/` | +4 new op modules: `matte_extract.rs`, `text_gen.rs`, `channel_split.rs`, `channel_combine.rs` (§3.4); compiler gains the `TimeOffset` duplicate-subgraph pass and `Switch` constant-folding. |
| `crates/photonic-gui/src/app/node_editor/` (new) | egui-snarl canvas + viewer-inset split, palette, node-body widgets, drag/box-select, diagnostic badges — central-panel content, reached from a composition-edit entry point. |
| `panels/mod.rs` `DrawerGroup::NodeEditor` interior | Palette + selected-node full inspector + graph-level info (refines 04 §4.1's delegation). |
| `session.rs` (`EngineSession`, 02) | + `view_override: Option<ViewNodeOverride>` (session state, never document state). |
