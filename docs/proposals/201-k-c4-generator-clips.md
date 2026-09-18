# 201 — K-C4 Generator Clips (mini-spec)

> **Status: Proposed — Band-5 mini-spec, pre-code.**
> [26 §19.1](../specs/video-editor/26-kdenlive-mlt-parity.md#191-bands) makes an accepted
> mini-spec the exit condition for every K-Band 5 item: it must name the data-model
> change, migration, undo unit, MCP surface and acceptance fixtures *before* code.
> This document discharges that for **K-C4**
> ([26 §11](../specs/video-editor/26-kdenlive-mlt-parity.md#k-c4--generator-clips)).
> No code authorization until accepted
> ([23 §14](../specs/video-editor/23-legal-open-source-implementation-routes.md#14-stopgo-checklist-before-any-code)).

**Owner ref:** 26 §11 K-C4 · **Territory:** `core-timeline` + `photonic-video-engine` · **Effort:** M

**Two things are already decided elsewhere and are honoured here without re-litigation:**

1. **Scope.** [ROADMAP §3a](../specs/video-editor/ROADMAP.md#3a-kdenlivemlt-inventory-kex) — "**K-C4's
   image-sequence half → D-6**" — and [ROADMAP §2](../specs/video-editor/ROADMAP.md#2-nle-inventory)'s
   D-6 row ("**Owns the image-sequence/stop-motion clip outright** — 26 K-C4 is generators only").
   Nothing about `AssetKind::ImageSequence`, filename-grammar discovery, stop-motion capture or
   deflicker appears below. **K-C4 is generators only.**
2. **The rights gate.** [ROADMAP §7](../specs/video-editor/ROADMAP.md#kex-gates): "**K-B7** luma-map
   wipes and **K-C4** generators: any *bundled* image or audio byte needs an `AssetRightsManifest`
   per [23 §7.2](../specs/video-editor/23-legal-open-source-implementation-routes.md#72-manifest).
   Prefer runtime synthesis from the cited standards, which avoids the gate."

**Everything in this design is runtime synthesis, and the design is arranged around that fact.**
No pattern is sampled, no image is bundled, no audio file is bundled, and — the one that is easy to
miss — **no font is bundled or read from the host**, which is why §3.5 draws digits from a stroke
table rather than through the glyph pipeline. **K-C4 therefore ships UNGATED**: it engages neither
23 §7.2's `AssetRightsManifest` nor any other ROADMAP §7 gate. §9 states that conclusion formally
and §10 records its provenance.

K-B7 (luma-map wipes) shares the same rights gate and is **not** specified here.

---

## 1. Problem and user outcome

**Today.** `ClipSource` (`crates/photonic-core/src/timeline/clip.rs:165-196`) can play an asset, a
vector document, a nested sequence, a flat colour, an adjustment layer or a title. There is no way
to put a *synthesised signal* on the timeline. `grep -rni 'generator' crates/ --include=*.rs`
returns only doc comments about 0-input graph nodes and one GUI palette label
(`crates/photonic-gui/src/panels/video/effects_browser.rs:58`, `"UTIL / GENERATORS"`, which labels
*effect* nodes, not clip sources). No `GeneratorKind`, no generator manifest, no generator IR op.

The consequence is not that a novelty is missing. It is that **Photonic cannot produce its own
reference signal.** Every question of the form "is my export's colour right / is my A/V in sync /
does my scope read what I think it reads / did this round-trip change my levels" currently requires
an external file. [26 §11 K-C4](../specs/video-editor/26-kdenlive-mlt-parity.md#k-c4--generator-clips)
makes exactly this point: colour bars and a counter are *test signals* that "make scope calibration,
export round-trip verification and A/V-sync checking possible without external files, which directly
serves [11](../specs/video-editor/11-testing-phasing.md)'s golden corpus."

**After K-C4.** A user can:

1. Open a **Generators** palette, pick **SMPTE bars** or **EBU bars**, and get a full-frame,
   standards-derived bar pattern on the timeline at the playhead — one click, one undo step.
2. Drop a **counter** (frames / timecode / seconds) or a **countdown leader** at the head of a
   sequence, optionally with the **1 kHz alignment tone** on a linked audio clip, and use it to
   verify A/V sync through an encode by eye and ear.
3. Drop a **noise** clip and get the *same pixels* on every machine, every run, forever — because
   its seed is stored in the document, not drawn from a clock or an OS RNG (§7.2).
4. Adjust a generator's parameters in the clip inspector with manifest-driven widgets — the same
   widget path effect params already use — with each edit a single undo unit.
5. Do all of the above from an agent over MCP with full parity (§6), including asking the catalogue
   what generators exist and what parameters each takes.
6. Export any of it and get **byte-identical pixels** to the interactive preview, because a
   generator is a pure function of its stored parameters and the frame ordinal (§7).

**Not** in the outcome, deliberately: PM5544 and FuBK test cards (§9.3 — a rights question the other
two patterns do not have), animatable generator parameters (§9.3), and anything at all to do with
image sequences (see the scope note above).

---

## 2. Current state in code

### 2.1 What exists and is directly usable

| # | Thing | Where | Why it matters here |
|---|---|---|---|
| 1 | `ClipSource`, `#[serde(tag = "source", rename_all = "snake_case")]`, with `Unknown(serde_json::Map)` declared last under `#[serde(untagged)]` | `photonic-core/src/timeline/clip.rs:164-196` | The extension point. A new tag is additive and an *older* build reading it lands in `Unknown` and preserves it verbatim (39 §2.2) |
| 2 | `ops::insert_clip` (validates duration > 0, non-overlap, sequence cycles) | `photonic-core/src/timeline/ops.rs:508-527` | The insert path. `add_adjustment_clip` (`ops.rs:558`) and `add_text_clip` (`ops.rs:578`) are both thin wrappers over it — the exact shape K-C4 copies |
| 3 | `TimelineCmd::InsertClip` ⇄ `RemoveClip` inversion | `photonic-core/src/timeline/commands.rs:502`, inverse at `:2272-2281` | The undo unit, already written (§5) |
| 4 | `TimelineCmd::SetClipProp { old: Box<Clip>, new: Box<Clip> }` and `ops::replace_clip_source` | `commands.rs:568-573`; `ops.rs:1140-1166` | Parameter edits are a whole-clip diff — already how `set_clip_color_label` and every other clip-property edit works |
| 5 | `IrOp::SolidColor` — a 0-input source op, evaluated as `ops::solid(cw, ch, color)` on CPU and `Passes::fill` on GPU | `graph/ir.rs:202-204`; `eval_cpu.rs:136`; `eval.rs:583-586` | Proof the 0-input source shape works end to end |
| 6 | `Effect{MaskShapeGen}` — a real **0-input procedural generator** with a CPU reference and a WGSL twin | `graph/ops.rs:608-655` (CPU); `eval.rs:1609-1625` (WGSL); parity test `eval.rs:4106-4133` | The single most useful precedent in the tree: uv from pixel centres, `dims`-driven, CPU/GPU byte-parity at 1e-3 |
| 7 | `op_size(op, cw, ch)` falls through to the canvas for any op that is not `Resize`/`Output` | `eval.rs:1180-1186` | A new generator op is canvas-sized with **no change to this function** |
| 8 | `content_hash(op, inputs, input_hashes)` = xxh3-128, and `hash_op`'s per-variant tag + resolved payload | `compile.rs:2568-2711` | Where a generator earns its cache identity (§7.3) |
| 9 | `NodeCache::lookup_or_alloc` treats a hit as valid **only if the recorded `TextureDesc` matches** | `graph/cache.rs:98-107`, test at `cache.rs:206-215` | The in-memory backstop for the canvas hazard in §2.3(a) |
| 10 | `EffectManifest` / `ParamSpec` / `ParamKind` / `Display` / `UiHint` — a versioned, data-driven catalogue with a static table | `photonic-core/src/timeline/effect_manifest.rs:95-251`, table at `:535+` | The param vocabulary K-C4 reuses wholesale (§3.3) |
| 11 | `EffectParams` — an **ordered `Vec<(PropPath, PropValue)>`, explicitly not a `HashMap`**, "a hard requirement for the export-determinism tests" | `photonic-core/src/timeline/effect_kind.rs:76-84` | The param storage K-C4 reuses, for exactly the reason its doc comment gives |
| 12 | `PcmSource` trait (`channels`/`sample_rate`/`read`) and `ClipVoice`/`Mixer::render_block` | `photonic-video/src/audio/mixer.rs:38-61` | The seam a synthesized tone plugs into (§3.6) |
| 13 | `FrameRate::frame_at(t) = t.0.div_euclid(ticks_per_frame)`, exact rational rates, `TICKS_PER_SECOND = 705_600_000` | `photonic-core/src/timeline/time.rs:13`, `:103-121` | The frame ordinal a time-varying generator is a function of (§7.1) — integer, exact, PA-8 |
| 14 | `Timecode::from_frame_index` / `format`, drop-frame included | `time.rs:180-236` | The counter's timecode string comes from the shipped, tested formatter — not a second implementation |
| 15 | `DRAFT_MAX_LONG_EDGE = 960`, `fit_long_edge`, and the scale-invariance guard | `compile.rs:225-240`; `photonic-video/tests/scale_invariance.rs` | The hard gate every geometric generator must pass (§7.4) |
| 16 | `panels/video/titles.rs:225-253` — a palette panel that builds a clip and returns one `TimelineCmd` | — | The GUI route's template |
| 17 | `list_effect_kinds` MCP tool, generated from the manifest table, with `param_spec_json` | `photonic-mcp/src/handlers/video.rs:2038-2055`; dispatch at `dispatch.rs:2444` | `list_generator_kinds` is the same function over a second table (§6) |

### 2.2 The two shipped sources that are arguably already generators

`ClipSource::SolidColor { color }` (`clip.rs:178-180`) and `ClipSource::Text { content }`
(`clip.rs:187-189`) both synthesize pixels from parameters with no media asset. The brief asks
whether K-C4 generalises or subsumes them. **Neither is subsumed. Both stay exactly as they are.**
The argument is in §3.1; the short form is that subsuming them buys nothing a user can see and
costs a format migration, an MCP wire break and a protected-surface risk on two shipped, tested,
MCP-exposed paths.

For the record, what they are:

- **`SolidColor`** is the degenerate generator, and it already lowers to a dedicated 0-input IR op
  (`compile.rs:1382-1388`), is hashed with its own tag byte 3 (`compile.rs:2617-2623`), has a
  `ClipSourceArg::SolidColor { color }` MCP arm (`photonic-mcp/src/protocol/args/video.rs:41-43`),
  and appears in the CPU/GPU parity and golden corpora. It is the shape K-C4 generalises *from*.
- **`Text`** is not a pattern generator at all — it is a **text-render path**. It lowers to
  `IrOp::TextGen` carrying a resolved `CaptionCueRun` and is composited by glyphon through the
  shared caption compositor (`compile.rs:1395-1406`; `eval.rs:664-676`). Two properties make it the
  *wrong* substrate for anything K-C4 needs, and both are load-bearing below:
  - the glyph raster comes from `FontSystem::new()`
    (`crates/photonic-render/src/caption.rs:99`), i.e. **the host's font database** — so the same
    project renders different pixels on two machines;
  - `eval_cpu` deliberately does not rasterize it: `IrOp::TextGen { .. } => Image::new(cw, ch)`
    (`eval_cpu.rs:221`), with the module doc stating captions and titles are "excluded from GPU/CPU
    byte-parity in v1" (`eval_cpu.rs:20-28`). A text-based counter would be **blank in the CPU
    golden corpus** and outside the parity gate.

### 2.3 Five things that do not exist, or exist differently from how a naive design would assume

Each one changes a concrete decision below. Stated plainly, per the brief.

**(a) The content hash does not encode the evaluation canvas.** `GpuEvaluator::evaluate(&graph,
canvas, source)` takes the canvas as a *runtime* argument (`eval.rs:465-471`), while
`IrOp::Output { w, h }` carries the full format size from compile. One `ContentHash` therefore
describes both a Draft-canvas and a Full-canvas render — the finding
[193 §2.3(a)](193-k-a1-chunked-timeline-preview-rendering.md) records. For generators this matters
*more* than for a decode, because a generator's pixels are produced from the canvas itself.
In-memory it is contained: `NodeCache` only counts a hit when the recorded `TextureDesc` matches
(`cache.rs:98`, pinned by the test at `cache.rs:206-215`). Consequence: **§7.4 forbids any generator
whose output is not a resampling of one canvas-independent ideal image**, and K-C4 adds no new
persistence of its own.

**(b) `effect_kind_tag` collapses every unknown effect id to a single byte.**
`compile.rs:2887-2902` maps the seven v1 `EffectKind` variants to tags 0–6 and everything else to
`255`, with a comment asserting that a shared tag is "a benign (rare) cache miss, never a
correctness bug". That reasoning held when `Unknown` meant "a variant from the future". It no longer
does: K-B16's bridged catalogue **lowers every bridged id as `EffectKind::Unknown(tag)`**
(`raster_bridge.rs:20-21`; `compile.rs:1224`), so two *different* bridged effects with equal
resolved params now hash identically — e.g. `color.desaturate` and `color.invert_raster`, which
share the empty `INVERT_PARAMS` list (`effect_manifest.rs:343`, and both are in `BRIDGED_IDS`,
`raster_bridge.rs:71,73`). That is a false cache **hit**, not a miss. Consequence: §3.5 gives
generators **their own IR op with their own hash arm**, and does not route them through
`IrOp::Effect`. It is also a defect in its own right — see Follow-up 1.

**(c) The existing 0-input effect is not actually lowered with arity 0.** `compile.rs:1993-1998`
says so outright: "`MaskShape` is a 0-input generator; P3 still routes the missing-input default
through it (harmless — the evaluator ignores it), pending generator-arity lowering." The arity-0
lowering is acknowledged debt on that path, and reusing it would inherit the debt. Consequence:
`IrOp::Generator` is arity-0 **by construction**, pushed with `inputs: vec![]`.

**(d) The audio feeders only accept `ClipSource::Asset`.** Both the interactive feeder
(`session.rs:2157-2172`) and the offline export feeder (`export/offline_audio.rs:80-95`) filter with
`matches!(clip.source, ClipSource::Asset { .. })`, and both hold `pcm: HashMap<ClipId,
FfmpegPcmSource>` — a **concrete** type, not `Box<dyn PcmSource>` (`session.rs:2147`,
`offline_audio.rs:73`). A synthesized tone therefore needs a small, symmetric change in two places
(§3.6), and it must be the *same* change in both or interactive and export audio diverge.

**(e) There is no `AnimTarget` that can address a clip's source.** `AnimTarget`
(`commands.rs:381-389`) covers clip transform, clip effect, grade op, track/clip audio, master bus
and audio FX. Nothing addresses `Clip.source`. Consequence: **generator parameters are static in
v1** (§3.3), which is a decision, not an omission — and the one thing that genuinely varies with
time (the counter's value, the noise field) varies with the *frame ordinal*, which needs no keyframe
machinery at all (§7.1).

**Also absent, stated plainly:** no `GeneratorId`, no generator manifest table, no `IrOp::Generator`,
no generator MCP tool, no generator GUI surface, no `PcmSource` implementation other than
`FfmpegPcmSource`, and no `EditError` variant for a track-kind mismatch (`ops.rs:34-57`).

---

## 3. Data-model change

### 3.1 Decision: one new `ClipSource` variant; `SolidColor` and `Text` are **not** subsumed

```rust
// photonic-core/src/timeline/clip.rs — a new arm on ClipSource, BEFORE Unknown
/// K-C4: a synthesized source — colour bars, counter, leader, noise, tone.
/// Pixels/samples are a pure function of `spec` and the frame ordinal; no media
/// asset, no bundled bytes (26 §11 K-C4).
Generator {
    spec: GeneratorSpec,
},
```

26 §11's Files line proposes `ClipSource::Generator(GeneratorKind)`. This spec uses a **named
struct field carrying a `GeneratorSpec`** rather than a bare enum, for one reason: a generator is an
`(id, params, seed)` triple, and a Rust enum of kinds with per-kind fields would put the parameter
schema in the type system, which is precisely what 30 §2 moved *out* of the type system for effects.
The named-field form also matches every other `ClipSource` arm's shape.

**Why `SolidColor` and `Text` stay:**

1. **`SolidColor` costs a migration and returns nothing.** Folding it in means either (i) a v6
   migration rewriting every `{"source":"solid_color","color":…}` object, or (ii) permanently
   carrying both spellings. A no-op-to-the-user migration bump also shrinks `COMPAT_WINDOW` for
   every user. What the user gains is zero: the palette can list "Solid colour" and emit a
   `ClipSource::SolidColor` clip, and nobody can tell.
2. **`SolidColor` is load-bearing in the test corpus.** `eval_cpu.rs`, `eval.rs`, `golden_frames`,
   `scale_invariance` and `job_queue.rs:336` all build fixtures from it. Retagging it churns the
   corpus for no behavioural change — the exact "widen the diff, hide the feature" failure.
3. **`Text` is a different mechanism, not a different generator.** §2.2: it is host-font-dependent
   and CPU-blank. Pulling it under a `GeneratorSpec` would imply generators may be non-deterministic,
   which is the one property this item cannot concede (§7).
4. **MCP wire compatibility.** `ClipSourceArg::SolidColor { color: String }`
   (`args/video.rs:41-43`) and `insert_text_clip` (`handlers/video.rs:1638`, documented at
   `docs/mcp-api.md:2387`) are shipped surface.

**The relationship is recorded, not implemented:** `SolidColor` is the degenerate case of a
generator and `Text` is a text-render path that happens to have no input. The `GeneratorManifest`
table is the catalogue a user browses; the palette may present "Solid colour" alongside the
generators while emitting the existing variant. Nothing in the model asserts a subtype relation,
because nothing in the code needs one.

### 3.2 The new core types

New file `crates/photonic-core/src/timeline/generator.rs`, sibling to `effect_manifest.rs`:

```rust
/// A stable, human-readable generator id, e.g. GeneratorId("bars.smpte_rp219").
/// Cow-backed for the same reason EffectId is (effect_manifest.rs:41-47): an id
/// authored by a NEWER build survives a load/save round-trip owned, rather than
/// being dropped.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GeneratorId(pub Cow<'static, str>);

/// What a ClipSource::Generator plays. Serde-additive; every field has a default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GeneratorSpec {
    pub id: GeneratorId,
    /// Ordered (path, value) pairs, exactly EffectParams' shape and for exactly
    /// its stated reason: deterministic serialization (effect_kind.rs:76-84).
    /// Paths absent here resolve to the manifest default; paths the manifest does
    /// not know are preserved verbatim and ignored at render (39 §2.2).
    #[serde(default, skip_serializing_if = "EffectParams::is_empty")]
    pub params: EffectParams,
    /// The generator's deterministic seed. Stored, never derived at render time.
    /// See §7.2 — this field is the whole reason noise is reproducible.
    #[serde(default)]
    pub seed: u64,
}
```

`EffectParams` is reused rather than cloned: it is already `#[serde(transparent)]` over an ordered
`Vec<(PropPath, PropValue)>` with `get`/`seed` helpers, and its ordering guarantee is documented as
a determinism requirement. Adding a parallel `GeneratorParams` with identical semantics would be
duplication for its own sake. (`EffectParams::is_empty` does not exist yet — one three-line
addition beside `get`, `effect_kind.rs:120`.)

### 3.3 Parameters **are** manifest-described — but in a second table, not `MANIFESTS`

The brief asks whether generators should be manifest-described like effects. **Yes** — the manifest
model is the right one, and reusing its vocabulary is most of the value:

```rust
/// One versioned generator definition. Deliberately mirrors EffectManifest
/// (effect_manifest.rs:241-251) and reuses its ParamSpec / ParamKind / Display /
/// UiHint types verbatim, so inspector widgets, MCP param schemas and generated
/// docs work with no new machinery.
#[derive(Clone, Debug, PartialEq)]
pub struct GeneratorManifest {
    pub id: GeneratorId,
    pub version: u16,
    pub name: &'static str,
    pub category: GeneratorCategory,      // TestPattern | Counter | Noise | Tone
    pub params: &'static [ParamSpec],     // effect_manifest::ParamSpec, unchanged
    /// Which track kind may host this generator (§3.4 row "Output").
    pub output: GeneratorOutput,          // Video | Audio
    /// True when the generator's pixels/samples depend on the frame ordinal.
    /// Drives §7.1: only a time-varying generator gets the ordinal folded into
    /// its resolved params — a static one must NOT, or its whole-clip cache
    /// entry becomes one entry per frame.
    pub time_varying: bool,
    /// The standard(s) the pattern is derived from, with revision. Displayed in
    /// the palette and emitted by list_generator_kinds — this is how the
    /// clean-room provenance (§10) stays visible to a reader of the product.
    pub derived_from: &'static [&'static str],
}

pub static GENERATORS: &[GeneratorManifest] = &[ /* sorted by id, test-checked */ ];
```

**Why a second table and not entries in `MANIFESTS`.** Three concrete reasons, all mechanical:

1. `EffectManifest` carries `applies: Applicability { clip, track, master, asset, reverse_safe }`
   (`effect_manifest.rs:212-218`), `arity: u8` and `space: OperandSpace`. For a clip *source* the
   first is meaningless (a source is not attached to a scope), the second is always 0, and the third
   is always "produce linear premultiplied". Putting a source in the effect table means either
   lying in three fields or widening a shipped type.
2. `MANIFESTS` is **joined to a kernel-binding table on the video side, with an exhaustiveness test
   over the join** (`effect_manifest.rs:9-18`). Adding non-effect rows breaks that join, or forces a
   "not really an effect" escape hatch inside it.
3. `MANIFESTS` feeds `prop_registry::project` and `list_effect_kinds`. A user asking "what effects
   can I add to this clip?" must not be shown "SMPTE bars".

`ParamSpec`, `ParamKind`, `Display` and `UiHint` are reused **unchanged**. Note one consequence:
K-C4 is the first user of `ParamKind::Enum` (the counter's display mode and the leader's style).
That is safe — `ParamKind::Enum(&'static [&'static str])` already exists (`effect_manifest.rs:118`)
and `PropValue::Enum(u32)` is a real variant. The module doc's caveat at `effect_manifest.rs:19-27`
is about `ParamKind::Path`, whose projection is a placeholder; K-C4 uses no `Path` param.

**Generator params are static (not animatable) in v1.** `ParamSpec.animatable` is set `false` on
every generator param. Justification, in order of weight: (i) there is no `AnimTarget` that can
address a clip source (§2.3(e)), so animation would need a new command target, a new prop-registry
projection and new keyframe UI — a materially larger item; (ii) the thing users actually want to
vary over time is the counter's number and the noise field, and both vary with the frame ordinal by
definition, with no keyframes involved (§7.1); (iii) a *static* param set means a static generator's
content hash is constant across the whole clip, so PA-1's per-node cache renders a colour-bars clip
**once** and reuses it for every frame. Animating the params would throw that away. Making a param
animatable later is additive: flip the flag, add the `AnimTarget` arm.

### 3.4 The v1 catalogue — six ids

| Id | Name | Output | Time-varying | Derived from | Key params |
|---|---|---|---|---|---|
| `bars.smpte_rp219` | SMPTE HD Colour Bars | Video | no | SMPTE RP 219-1 | `amplitude` (75 % / 100 %), `pluge` (bool) |
| `bars.ebu_3213` | EBU Colour Bars | Video | no | EBU Tech 3213 | `amplitude`, `pluge` |
| `counter` | Counter | Video | **yes** | — (Photonic-authored; ITU-R BT.1729 informs the readout convention) | `display` (frames/timecode/seconds), `direction` (up/down), `fg`, `bg`, `size` |
| `leader.countdown` | Countdown Leader | Video | **yes** | — (Photonic-authored geometry) | `seconds`, `fg`, `bg`, `sweep` (bool), `crosshair` (bool) |
| `noise` | Noise | Video | **yes** | — (Photonic-authored; §7.2's stated hash) | `density`, `monochrome`, `amount` |
| `tone` | Alignment Tone | **Audio** | yes (phase) | EBU R 68 / SMPTE RP 155 for the level references | `frequency_hz` (default 1000), `level_dbfs` (default −18) |

**PM5544 and FuBK, which 26 §11 also lists, are deliberately excluded from v1** — see §9.3 and
Follow-up 5. This is the one place this document departs from its owner item's text, and it departs
in the direction that keeps K-C4 ungated.

Notes that are part of the spec, not commentary:

- **`counter` and `leader.countdown` are separate ids, not one id with a `style` param.** Their
  parameter sets barely overlap and the manifest model is per-id. Two small manifests beat one with
  half its params inert.
- **`derived_from` is a data field, not a comment.** It is surfaced in the palette and by
  `list_generator_kinds`, so the provenance of a shipped pattern is visible to a user and to an
  agent, not only to a reader of this file.
- **The exact code values and geometry fractions of `bars.smpte_rp219` and `bars.ebu_3213` are
  transcribed from the cited standard documents by the implementer**, with the clause or table
  number recorded in a comment beside each constant, and checked against the standard by the
  provenance reviewer before merge (§10). This document deliberately does **not** restate those
  numbers: a spec that half-remembers a standard is worse than one that names it. What this document
  *does* fix is everything the standard does not cover — colour space, determinism, hashing,
  antialiasing, and the acceptance form (§8).
- **Bars are authored as Rec.709 code values and converted into the working space** by the same
  path everything else uses (linear-light Rec.709, premultiplied, `Rgba16Float` — D-09/PA-2). A bar
  generator that writes display code values straight into a linear buffer is wrong, and §8 test 3
  is the guard.

### 3.5 IR: one new op, `IrOp::Generator`

```rust
// photonic-video/src/graph/ir.rs — a new arm in the "sources" block
/// K-C4: a synthesized 0-input source. Pure function of (id, resolved params,
/// seed, frame ordinal) over the evaluation canvas. Inputs are always empty.
Generator {
    id: GeneratorTag,          // interned stable id (UnknownTag-style), NOT an enum
    params: ResolvedParams,    // includes the frame ordinal for time-varying kinds (§7.1)
    seed: u64,
},
```

**Why not `IrOp::Effect { kind: EffectKind::Unknown(id), .. }`,** which would have been free: §2.3(b)
— that path's hash collapses every unknown id to tag `255`, so `bars.smpte_rp219` and
`bars.ebu_3213` with equal params would be one cache identity producing two different images. And
§2.3(c) — that path still lowers 0-input effects as unary nodes. A generator that silently serves
another generator's pixels is the worst failure available here because it is invisible, so the op is
separate and hashes its **id string**, not a byte.

`seed` is a field on the op, not a `ResolvedParams` entry, because `PropValue::Float(f64)` cannot
carry a `u64` exactly above 2⁵³ and a seed must survive a round trip bit-for-bit.

**Seven exhaustive `IrOp` matches must gain an arm** — listed so the implementer does not discover
them one compile error at a time:

| # | Site | Arm |
|---|---|---|
| 1 | `ir.rs:133-161` `threading_for_op` | `Threading::Any` — a pure function of its params, freely parallelisable |
| 2 | `compile.rs:2583-2711` `hash_op` | New tag byte `19`; then length-prefixed id bytes (mirroring `hash_caption_cue`'s text hashing, `compile.rs:2781-2782`), `hash_resolved_params`, then `seed.to_le_bytes()` |
| 3 | `eval.rs:481+` `render_op` | One WGSL pass per generator family, uniform-only BGL, uv from `@builtin(position)` ÷ **logical** dims — `eval.rs:1616-1622` states this rule and why |
| 4 | `eval_cpu.rs:101-260` `eval_op` | The CPU reference — **the normative definition** (02 §2), which the WGSL must match |
| 5 | `source_range.rs:79-106` `source_range_for_op` | `FrameRange::identity(out)` — a generator reads no source frames |
| 6 | `eval.rs:1180-1186` `op_size` | **No change** — the `_ => (cw, ch)` fallthrough is already correct |
| 7 | `compile.rs:4069+` test-only `op_name` | `"Generator"` |

Lowering, in `build_clip_source` beside the `SolidColor` arm (`compile.rs:1382-1388`):

```rust
ClipSource::Generator { spec } => b.push(
    IrOp::Generator {
        id: GeneratorTag::intern(spec.id.as_str()),
        params: resolve_generator_params(spec, seq.frame_rate, src_time),
        seed: spec.seed,
    },
    vec![],   // arity 0 — §2.3(c)
),
```

`src_time` is the value the compiler **already computes** for every clip source (it is what
`NestedSequence` consumes at `compile.rs:1370-1381`), i.e. `source_in` plus the speed-mapped elapsed
time. Generators therefore honour trim and speed exactly like every other source, with no
special-casing — a counter starting at `source_in` counts from there, and a speed-ramped counter
repeats or skips numbers the way a speed-ramped source repeats or skips frames. Also unchanged:
`available_handle_ticks` already returns `None` ("infinite") for generator-like sources
(`compile.rs:684-690`); the new variant joins that arm, so a transition off a generator is never
handle-clipped.

An **unknown generator id** (a `.photon` written by a newer build) has no manifest, so
`resolve_generator_params` yields the params verbatim with no defaults, and the evaluator renders
transparent and emits one `CompileDiagnostic` naming the id — the same inert-and-preserved treatment
`ClipSource::Unknown` gets (`compile.rs:1408-1420`) and `EffectId` gets (30 §2.6). It is **not**
`ClipSource::Unknown`: the tag is known, so the whole spec round-trips as typed data.

### 3.6 Audio: `ToneSource: PcmSource`, and the two feeders that must change together

`tone` is the only audio generator. It lands as a `PcmSource` implementation
(`photonic-video/src/audio/tone.rs`) — no new mixer concept, no new voice type; `ClipAudio` gain,
pan and fades work on it unchanged because `ClipVoice` already takes `&mut dyn PcmSource`
(`mixer.rs:56-61`).

Two symmetric changes, which **must land in the same commit** or interactive and export audio
diverge:

| Site | Today | Change |
|---|---|---|
| `session.rs:2147` / `:2157-2172` (interactive feeder) | `pcm: HashMap<ClipId, FfmpegPcmSource>`; filter `matches!(clip.source, ClipSource::Asset { .. })` | `HashMap<ClipId, Box<dyn PcmSource>>`; filter admits a `Generator` whose manifest says `output: Audio`; construct `ToneSource` instead of spawning ffmpeg |
| `offline_audio.rs:73` / `:80-95` (export feeder) | identical shape | identical change |

A `ToneSource` needs no ffmpeg, so a generator-only sequence produces audio on a machine with no
ffmpeg at all — which also makes §8's audio tests runnable in CI without the
`locate_for_test` skip.

**The 1 kHz beep that 26 §11 attaches to the counter is a separate audio clip, not a field on the
video generator.** Reasons: the mixer's whole model is "a clip voice on an audio track", the user
must be able to mute/level/remove the tone independently, and a video generator that secretly emits
audio would be invisible to `Sequence` traversal, the meters and the loudness path. The **verb**
still inserts both at once, as one undo unit (§5).

### 3.7 What does **not** change

`Sequence`, `Track`, `TrackKind`, `MediaBin`, `MediaAsset`, `ProjectVideoSettings`, `Grade`,
`ClipAudio`, `SpeedMap` and every existing `TimelineCmd` variant. **K-C4 adds zero `TimelineCmd`
variants** (§5). No generator ever creates a `MediaAsset`: a generator is not media, it has no file,
it cannot go offline, it needs no relink and it must not appear in the media pool as a bin row.

---

## 4. Migration and format-version impact

**`CURRENT_FORMAT_VERSION` stays at 5** (`photonic-core/src/document.rs:117`). **This lands
additively inside v5.** No `v6`.

`FormatMigration` (`crates/photonic-core/src/migration.rs:48-55`) defines a step as one that
"operate[s] on the JSON tree directly (adding new fields with defaults, renaming moved fields, etc.)
before struct deserialization" on the way from N to N+1 (`migration.rs:44-47`). K-C4 has nothing for
such a step to do:

1. **The change is one new serde tag on an existing tagged enum.** No existing field is added,
   removed, retyped or given a new meaning. Every v5 document written before K-C4 loads identically
   after it, byte for byte.
2. **`GeneratorSpec`'s own fields are `#[serde(default, skip_serializing_if = …)]`**, so a
   generator authored today and re-read after the catalogue grows is unaffected, and an added
   optional field later needs no step either.
3. **The forward direction already works and is already tested.** An older build reading
   `{"source":"generator", …}` falls into `ClipSource::Unknown` (`clip.rs:190-195`, declared last
   under `#[serde(untagged)]`), which retains "the whole object — `source` tag and payload —
   verbatim and re-emits it unchanged", renders a placeholder and never guesses. That machinery is
   shipped and covered (`photonic-core/src/timeline/load.rs:382-393`, `:875+`).
4. **A version bump would be actively wrong.** It would push every existing v5 project through a
   no-op migration, make `MigrationV5ToV6` a lie about what changed, and shrink `COMPAT_WINDOW` for
   every user. All four prior Band-5 mini-specs (193–196) reached the same conclusion for the same
   reason. **Bump only when data must be reinterpreted.**

**One non-obvious obligation, which is the whole of the "migration" work beyond tests.**
`KNOWN_CLIP_SOURCE_TAGS` (`photonic-core/src/timeline/load.rs:319-326`) lists the six tags this
build understands, and `reject_corrupt_known_variants` (`load.rs:382-393`) **errors the load** when
a clip lands in `ClipSource::Unknown` while carrying a tag from that list — the guard that stops a
*malformed* known source from silently degrading into an opaque blob. `"generator"` **must be added
to that list in the same change**. Omit it and a corrupt generator payload loads as an inert Unknown
clip and is silently re-saved in that state, which is data loss with no error — the exact failure the
guard exists to prevent. §8 test 15 covers it.

The remaining migration *work* is a round-trip obligation, not a migration:

- a v5 document containing a `Generator` clip survives `to_json` → `from_json` → `finalize_load`
  unchanged, including a param path the manifest does not know and a `seed` at `u64::MAX`;
- a v5 document containing an **unknown generator id** round-trips with the id owned and intact;
- a v5 document written *before* K-C4 re-serializes with no `generator` key anywhere;
- a **malformed** `{"source":"generator", …}` object is a hard `LoadError::corrupt`, not a silent
  `Unknown` (the `KNOWN_CLIP_SOURCE_TAGS` obligation above).

New `TimelineCmd` variants would have been a separate question — there are none (§5), so the
`photon_history` sidecar is untouched and the best-effort history-load degradation
(`photon_file.rs:60-64`) is not engaged.

---

## 5. Undo unit and its exact inverse

One user verb = one undo unit. **K-C4 adds no command variant**; every verb is expressed in commands
that already ship and already invert.

| User verb | Command | Exact inverse |
|---|---|---|
| **Insert a generator** (palette click / `insert_generator_clip`) | one `TimelineCmd::InsertClip { seq, track, clip }` from `ops::insert_clip` (`ops.rs:508`) | `RemoveClip { seq, track, clip }` — the clone-and-swap already written at `commands.rs:2272-2276` |
| **Insert countdown leader *with tone*** | one `Command::Batch([InsertClip(video leader), InsertClip(audio tone)])` | reversed batch of inverses (`history/mod.rs:3173-3175`) — two `RemoveClip`s |
| **Edit a generator parameter** (inspector widget / `set_generator_param`) | one `TimelineCmd::SetClipProp { old, new }` via `ops::replace_clip_source` (`ops.rs:1140-1166`) | swap `old`/`new` — the shipped inversion |
| **Change a generator's kind** (`replace_clip_source` with a `Generator` arg) | same `SetClipProp` | same swap |
| **Re-roll a noise seed** | same `SetClipProp` (the seed is a model field) | same swap — and this is why "re-roll" is undoable at all |
| **Delete / move / trim / split a generator clip** | unchanged — the existing clip verbs | unchanged |

Three rules that follow and must be implemented as written:

1. **The paired-insert batch is safe under the mid-batch validate assert, and that was checked, not
   assumed.** `TimelineCmd::apply` debug-asserts `Sequence::validate()` after **every** command and
   `Command::Batch` applies members one at a time (`commands.rs:1748-1758`). The batch above is
   legal because each intermediate state is valid in both directions: the two clips go on
   *different* tracks (one video, one audio), so no `OverlapOrUnsorted` is transiently possible; no
   `GroupNode` is created or destroyed, so none of `UnknownGroup` / `EmptyGroup` /
   `SingletonNormalGroup` (`sequence.rs:545-561`) can transiently fire; and each `RemoveClip` on
   undo only shrinks a track. This is the failure mode
   [194 §2.4](194-k-a5-general-and-nested-clip-groups.md) exists to prevent, and it does not apply
   here — but the reason it does not apply is written down so a later change cannot quietly break it.
2. **The link between the leader and its tone is stamped on the clips *before* insertion, never by a
   follow-up command.** A fresh `LinkGroupId` (`clip.rs:145-153`) is minted and written into both
   `Clip` values, then both are inserted. This is strictly better than calling `ops::link_clips`
   (`ops.rs:1194`, which returns two `SetClipProp`s) inside the batch: fewer commands, a smaller
   history payload, and no intermediate state at all.
   **This deliberately writes a field 35 §3.3 has deprecated for one format version.** That is the
   right call for K-C4 and the reasoning is [194 §2.2](194-k-a5-general-and-nested-clip-groups.md)'s,
   not a departure from it: `link_group` is "**Deprecated for one format version**… Still the *only*
   thing wired", so a leader/tone pair that carried only a `GroupKind::AvLink` `GroupNode` today
   would move together **nowhere** — no op, no GUI, no MCP consumes the tree yet. K-C4 therefore
   matches shipped behaviour exactly and adds nothing new; when K-A5 re-points
   `link_clips`/`unlink_clip` at the tree, this call site moves with them and needs no separate
   migration because it is one `LinkGroupId` like every other.
3. **A parameter drag is one undo step, not one per frame.** Inspector sliders coalesce through the
   existing gesture rules (01 §10.0); a discrete edit (an enum change, a re-roll button) commits
   through `execute_discrete` (`history/stacks.rs:403`), the same call the existing clip-property
   edits use.

There is **no** undo unit for anything a generator renders. A generator writes nothing outside the
document; there is no cache to invert, no file to delete, and no `AnalysisCache` entry to evict.
Recorded here so a reviewer does not read the absence as a miss.

---

## 6. MCP surface

An MCP surface **is** warranted. CAP-019 parity is ROADMAP §10 point 3, 26 §5 lists PA-11 (full MCP
parity) as *not yet held*, and a GUI-only generator would widen a gap this programme is closing. It
is also the case that agents are the *primary* consumer of a test signal: "put bars and tone at the
head and export it" is an automation verb before it is a human one.

| Tool | Args | Notes |
|---|---|---|
| `list_generator_kinds` | — | **Generated** from `GENERATORS`, reusing `param_spec_json` (`handlers/video.rs:2038-2055`) exactly as `list_effect_kinds` is generated from `MANIFESTS`. Emits `id`, `name`, `category`, `output`, `time_varying`, `derived_from` and the param specs. Never hand-maintained |
| `insert_generator_clip` | `{ track_id, generator_id, params?, seed?, start_ticks\|start_tc\|start_seconds, duration_ticks, with_tone? }` | Mirrors `insert_text_clip`'s shape verbatim (`args/video.rs:508-520`; handler `handlers/video.rs:1638`), including the `resolve_tick` precedence rule and the `duration_ticks > 0` check. `with_tone: true` is only meaningful for `leader.countdown`/`counter` and produces the §5 two-clip batch |
| `set_generator_param` | `{ clip_id, path, value }` | Mirrors `set_effect_param` — a single-path setter, so an agent changing one value need not resend the whole param bag. Validates against the manifest (30 §2.7's mechanism, already implemented at `handlers/video.rs:2057+`) |
| `replace_clip_source` (existing) | `ClipSourceArg` gains `Generator { generator_id, params?, seed? }` | `ClipSourceArg` (`args/video.rs:30-45`) has arms for asset/vector/nest/solid/adjustment. Adding one is additive to a `#[serde(tag=…)]` enum; existing callers are unaffected |
| `list_clips` / `get_clip` (existing) | — | `get_clip` serializes the whole `Clip` (`handlers/video.rs:1939-1942`), so the generator spec is visible for free. `list_clips` needs no change |

Wiring follows the shipped pattern with no invention: arg structs in `protocol/args/video.rs`,
handlers beside `insert_text_clip` in `handlers/video.rs`, dispatch arms beside
`dispatch.rs:2348`, names added to the tool-name list at `handlers/video.rs:8311`, then
`schema_gen.rs` regenerated. **`docs/mcp-api.md` must be regenerated in the same change** — CI
regenerates it and fails on any diff (`.github/workflows/ci.yml:162-167`), so this is mandatory, not
optional.

A new `EditError::WrongTrackKind { track: TrackId, expected: TrackKind }` is needed (a video
generator on an audio track, or a tone on a video track). `map_edit_error` has an `other =>`
catch-all (`handlers/video.rs:269`) so this is non-breaking there; it still gets an explicit arm
whose wording matches the existing style.

**No parity exception is requested. Every generator verb is available from both surfaces.**

---

## 7. Determinism — the load-bearing section

A generator that produces different pixels for the same parameters breaks two things at once: the
content-hashed frame graph (PA-1) serves a cached texture that no longer matches its inputs, and
SS-3 export determinism stops meaning anything. This section is normative.

**The contract, in one line:** for every video generator,

```
pixel(x, y) = f(resolved_params, seed, frame_ordinal, uv(x, y), pixel_footprint)
```

with `uv(x, y) = ((x + 0.5) / w, (y + 0.5) / h)` — the pixel-centre convention `ops::mask_shape`
already uses (`ops.rs:630-633`) and the WGSL twin already mirrors (`eval.rs:1616-1622`) — and no
other input of any kind. For audio, `sample(n) = g(resolved_params, n)` with `n` the absolute
sample index from the clip's start.

### 7.1 The frame ordinal, and why only time-varying generators get one

`frame_ordinal = seq.frame_rate.frame_at(src_time)` — integer, exact, computed by the shipped
`FrameRate::frame_at` (`time.rs:119-122`, `div_euclid` over `ticks_per_frame`). No float time enters
anywhere (PA-8). The ordinal is folded into `ResolvedParams` under the reserved path
`"gen.frame"` at compile time, which is the same technique `WipeMix`'s eased `t` already uses:
"`t` is the compile-time eased mix factor, so distinct ticks yield distinct content hashes"
(`ir.rs:228-233`).

**Only generators whose manifest sets `time_varying: true` receive it.** This is not an
optimisation detail, it is a correctness-of-caching rule in both directions:

- A *static* generator (bars) that received the ordinal would produce a different hash on every
  frame, turning one cached texture into one per frame — a cache-thrash regression against PA-1 in
  the name of a feature.
- A *time-varying* generator (counter, noise) that did **not** receive it would produce one hash for
  the whole clip and the counter would freeze on its first value — a wrong-pixels bug that no
  existing test would catch.

The counter's readout string comes from `Timecode::from_frame_index` / `format`
(`time.rs:192-207`, `:180-186`), drop-frame included, and from the shipped drop-frame algorithm at
`time.rs:213-236`. No second timecode implementation.

### 7.2 The seed model

**Rules, all normative:**

1. **The seed lives in the document** (`GeneratorSpec.seed`). It is never read from a clock, an OS
   RNG, a process id, an address, or a thread id at render time. The failure this prevents is
   specific and severe: a render-time seed makes the same project produce different pixels on two
   runs, and the content hash — which sees only the params — cannot detect it, so the *cache* would
   happily serve run 1's noise for run 2's graph.
2. **The default seed at insert time is `xxh3-64(clip.id)`**, computed once and **written into the
   model**. No RNG dependency is added anywhere. Two noise clips inserted in the same session get
   different fields because their `ClipId`s differ; a duplicated clip keeps the stored value because
   it is a stored value, which is the behaviour a user expects from "duplicate".
3. **Re-roll is an explicit verb** that writes a new stored seed through `SetClipProp` (§5), so it is
   undoable. There is no implicit re-roll on load, copy, save or format change.
4. **The noise field is generated by a stateless integer hash of `(cell_x, cell_y, frame, seed)`,
   not by a sequential PRNG.** The anti-pattern is in the tree and should be read before
   implementing: `photonic_core::raster::filter::add_noise` (`filter.rs:456-489`) walks pixels in
   raster order advancing a `SplitMix64`. That is (i) resolution-dependent, (ii) unreproducible on a
   GPU, where fragments have no order, and (iii) unparallelisable on the CPU. A stateless hash has
   none of those properties.
5. **The hash is 32-bit wrapping integer arithmetic with constants recorded in the source together
   with their published origin**, so the Rust and WGSL implementations are provably the same
   function (WGSL has `u32` wrapping arithmetic and no `u64`; the stored `u64` seed enters as two
   `u32` halves). The uniform sample is `(h >> 8) as f32 * (1.0 / 16777216.0)` — a 24-bit mantissa
   value and a power-of-two divisor, so it is exact in `f32` on both sides. The chosen constants
   must come from a published, non-copyleft source (a widely documented integer-avalanche constant
   set), be recorded with that citation, and be reviewed under §10 like every other constant.
6. **The noise lattice is defined in normalized space**, `params.density` cells across the frame
   width, with bilinear interpolation between lattice values. This is what makes noise
   resolution-independent rather than an exception to §7.4. Very high densities alias, as any
   high-frequency signal does; that is inherent, documented, and not a determinism failure.
7. **The audio tone uses a fixed-point phase accumulator, not a float one, and no `libm` call.**
   Phase is a `u32` where 2³² is one cycle, advanced by a per-sample increment computed once as an
   integer; the waveform comes from a quarter-wave table with linear interpolation. Two reasons:
   `f64::sin` is a platform-provided function with no bit-identity guarantee across libm versions,
   and a float phase accumulator drifts over long durations. Both would be invisible until an
   export-comparison test failed on one machine.

**Four things a generator must never touch**, as a checklist for review: wall-clock time; any RNG
that is not the stated hash; any host resource (font database, locale, environment); and any
accumulation whose result depends on iteration order or thread count.

### 7.3 Participation in the content hash

`hash_op` gains tag byte `19` and hashes, in order: the **length-prefixed id string** (so
`bars.smpte_rp219` and `bars.ebu_3213` are distinct identities — the §2.3(b) trap), then
`hash_resolved_params` (`compile.rs:2722-2754`, which already hashes ordered `(path, value)` pairs
including the `"gen.frame"` ordinal), then `seed.to_le_bytes()`. `content_hash` then mixes in input
hashes — of which a generator has none.

Consequences, each of which should be a test (§8):

- Two generators of different kinds are never one cache entry, whatever their params.
- Changing any param, or the seed, is a new identity — so an edit invalidates naturally, through
  PA-1, with **no** new invalidation channel. Do not add one.
- A static generator has **one** identity for its whole clip; a time-varying one has a distinct
  identity per frame. Both are intended (§7.1).

**The canvas is still not in the hash** (§2.3(a)). K-C4 does not change that and does not need to:
`NodeCache` counts a hit only when the recorded `TextureDesc` matches (`cache.rs:98`), so a Draft
render and a Full render of the same generator never alias in memory. For anything **persisted**,
[193 §5.1/§5.5](193-k-a1-chunked-timeline-preview-rendering.md) already puts the render profile in
the chunk key and renders chunks only at full format size. K-C4 adds **no new refusal to K-A1's
list** — but §7.4's rule is what makes that safe, and if it were ever relaxed, K-A1's key would have
to gain the canvas.

### 7.4 Scale invariance and the Draft canvas

Every **video** generator must satisfy: *rendering at the Draft canvas equals box-downsampling the
Full render, within the tolerance the existing guard uses.* That is exactly what
`photonic-video/tests/scale_invariance.rs` asserts (32 §7 / E-6), against
`DRAFT_MAX_LONG_EDGE = 960` (`compile.rs:225`), and it is a **hard gate** under ROADMAP §10 point 7.

Two design consequences:

1. **Hard edges are analytically antialiased against the pixel footprint.** A bar boundary rendered
   as a hard step at 960 px is *not* the downsample of a hard step at 1920 px — the boundary pixel
   differs by up to a full unit. Computing edge coverage from the pixel footprint (one pixel in the
   target canvas) makes the Draft render an approximation of the area-average of the ideal pattern,
   which is what the guard measures. This is the one place the canvas legitimately enters `f`, and
   it enters only through the footprint.
2. **The counter's digits are drawn from a stroke/segment table, not from glyphs.** §2.2 gives the
   two reasons — the glyph raster comes from the host's `FontSystem::new()`
   (`caption.rs:99`), and `eval_cpu` renders `TextGen` as transparent (`eval_cpu.rs:221`), which
   would leave a glyph-based counter blank in the CPU golden corpus and outside the parity gate.
   A seven-segment digit set plus `:` and `;` separators (the drop-frame form per `Timecode.format`,
   `time.rs:180-186`) is a fixed geometry table, is antialiased by the same footprint rule, is
   identical on every machine, needs no font, and **bundles no bytes** — which is also what keeps
   §9's ungated conclusion true.

### 7.5 CPU/GPU parity

Every video generator ships a `eval_cpu` reference *and* a WGSL twin, and joins
`photonic-video/tests/cpu_gpu_parity.rs` at the 1e-3 tolerance the existing generator parity test
uses (`eval.rs:4106-4133`). The CPU reference is normative (02 §2); where they disagree, the WGSL is
wrong. Both consume the same resolved params and the same integer hash, so agreement is a property
of the design rather than a coincidence to be tuned.

---

## 8. Acceptance fixtures and tests

> **No rights-cleared content is required, no bytes are bundled, and K-C4 is *not* a gated item.**
> Every fixture below is either built programmatically in-test or synthesized by the generator under
> test. Added fixture bytes: **zero** — the 5 MB corpus budget and the combined 10 MB
> `tests/golden/` + fixtures budget (11 §1.5) are untouched, and
> [23 §7.2](../specs/video-editor/23-legal-open-source-implementation-routes.md#72-manifest)'s
> `AssetRightsManifest` gate is **not engaged**. No test needs ffmpeg, a GPU adapter, or a font.

One trap, called out because it is the obvious shortcut and it is forbidden: **the existing
`color_bars.mp4` fixture is ffmpeg's `smptebars` output** (`crates/photonic-video/tests/fixtures/README.md`).
It must **not** be used as ground truth for `bars.smpte_rp219`. Validating our bars against another
implementation's rendering is exactly the clean-room failure §10 exists to prevent, and it would
also bake that implementation's rounding into our acceptance. Ground truth is **the numeric values
in the cited standard**, transcribed independently.

| # | Test | Where | Proves |
|---|---|---|---|
| 1 | Manifest table invariants: sorted by id, ids unique, every `ParamSpec.default` discriminant matches its `ParamKind`, every `derived_from` non-empty for a standards-derived pattern | `photonic-core/src/timeline/generator.rs` unit tests (mirroring the `MANIFESTS` invariant tests) | §3.3 |
| 2 | **Bars against the standard**: for each of ≥12 named sample points (bar centres, PLUGE steps), assert the rendered linear value round-trips to the Rec.709 code value **transcribed from the cited standard clause**, with the clause named in the test | `photonic-video/tests/generators.rs` | §3.4 — the correctness of the pattern itself, independent of any other implementation |
| 3 | **Colour-space correctness**: rendering 100 % white bar → the working-space value for Rec.709 white, not the code value written raw into a linear buffer | `photonic-video/tests/generators.rs` | §3.4's colour rule / PA-2 |
| 4 | **Byte-identical repeat**: render each generator twice in one process and in two processes; assert byte equality | `photonic-video/tests/generators.rs` | §7's contract, at its coarsest |
| 5 | **Seed determinism**: same seed ⇒ identical noise; different seed ⇒ different noise; `seed: u64::MAX` round-trips; no RNG/clock symbol reachable from the generator module (source lint, patterned on the existing lint tests) | `photonic-video/tests/generators.rs` + `photonic-core` unit tests | §7.2 rules 1–4 |
| 6 | **Hash identity table**: `bars.smpte_rp219` vs `bars.ebu_3213` with identical params ⇒ **distinct** hashes; a param change ⇒ distinct; a seed change ⇒ distinct; a *static* generator at ticks t and t+1 ⇒ **identical**; a *time-varying* one ⇒ distinct | `compile.rs` unit tests beside the existing hash tests | §7.3 — including the §2.3(b) trap |
| 7 | **CPU/GPU parity** for every video generator at 1e-3, self-skipping without an adapter | `photonic-video/tests/cpu_gpu_parity.rs` | §7.5 |
| 8 | **Scale invariance**: Draft vs box-downsampled Full for bars (hard edges), leader (curves) and noise at a moderate density | `photonic-video/tests/scale_invariance.rs` | §7.4 — a hard gate |
| 9 | **Counter readout**: frame ordinal → string for non-drop and drop-frame rates, asserted against `Timecode::from_frame_index`; `source_in` offsets the count; a speed ramp repeats/skips consistently with the ordinal | `photonic-video/tests/generators.rs` | §7.1 and the `src_time` decision in §3.5 |
| 10 | **Golden frames**: one blessed CPU case per video generator at 320×180 | `tests/golden/video/generator_*/`, via `photonic-video/tests/golden_frames.rs` | Layout regression. Deliberately *secondary* to test 2 — a blessed PNG blesses whatever we rendered, including a bug; the standard's numbers do not |
| 11 | **Tone**: `ToneSource` at 1 kHz / −18 dBFS produces the expected RMS and zero-crossing count over one second at 48 kHz; phase is continuous across block boundaries; two runs are byte-identical; **no `libm` sin** | `photonic-video/src/audio/tone.rs` unit tests | §7.2 rule 7 |
| 12 | **One audio path**: the same tone clip rendered through the interactive feeder and through `offline_audio` produces identical PCM | `photonic-video/tests/audio_discontinuity.rs` or a sibling | §3.6's "must land together"; guards PA-10's intent |
| 13 | **Undo identity**: insert → one history entry, undo removes the clip; leader+tone → **one** entry, undo removes **both**; param edit → one `SetClipProp`, undo restores the prior spec exactly (`assert_undo_roundtrip`, `ops.rs:2921`) | `photonic-core/src/timeline/ops.rs` tests + `tests/timeline.rs` | §5 |
| 14 | **Debug-assert safety**: the leader+tone batch applied and inverted in a **debug** build trips no `Sequence::validate` assert | `tests/timeline.rs` (debug) | §5 rule 1 — the [194 §2.4](194-k-a5-general-and-nested-clip-groups.md) failure mode, pinned as not-applicable |
| 15 | **Serde**: v5 doc with a generator (incl. an unknown param path and `u64::MAX` seed) round-trips; a doc with an **unknown generator id** round-trips with the id intact and compiles to transparent + one diagnostic; a **malformed** `"generator"` payload is a `LoadError::corrupt`, not a silent `Unknown` (`"generator"` present in `KNOWN_CLIP_SOURCE_TAGS`); a pre-K-C4 v5 doc re-serializes with no generator key; `CURRENT_FORMAT_VERSION` still 5 | `photonic-core/tests/timeline.rs`, `tests/forward_compat.rs` | §4 |
| 16 | **Track-kind refusal**: a video generator on an audio track and a tone on a video track both return `EditError::WrongTrackKind`, with the document unchanged | `ops.rs` tests | §6 |
| 17 | **GUI route**: the generators palette inserts each catalogue entry headlessly and produces exactly one history entry | `photonic-gui/tests/video_ui_paths.rs` | ROADMAP §10 point 2 |
| 18 | **CAP-019 parity story**: MCP arm (`insert_generator_clip` → `set_generator_param`) vs GUI arm (palette → inspector), structural compare | `photonic-app/tests/acceptance_stories.rs` | ROADMAP §10 point 10 |
| 19 | **MCP**: `list_generator_kinds` lists every catalogue entry with its params; `insert_generator_clip` rejects `duration_ticks <= 0`; `replace_clip_source` accepts a `Generator` arg; `docs/mcp-api.md` regenerates clean | `handlers/video.rs` tests + the `ci.yml:162-167` gate | §6 |

`acceptance_stories.rs:30-35` already documents why solid-colour clips are used for model-level
tests ("they carry no media asset"); generator clips inherit that property and extend it to the
pixel path — which is the deeper point of this item for the test suite as a whole.

---

## 9. Risks, open questions, deliberate exclusions

### 9.1 Risks

1. **Transcribing a standard wrongly.** The single highest-probability way to ship a *plausible*
   bug: bars that look right and are numerically wrong, which then get blessed into a golden PNG and
   become the reference. Mitigations, all required: test 2 asserts values from the standard with the
   clause named; the golden PNG is explicitly secondary (test 10); every constant carries its clause
   number in a source comment; and the §10 provenance reviewer checks the constants against the
   document, not against the code.
2. **The `effect_kind_tag` collision (§2.3(b)) is a live defect that K-C4 routes around rather than
   fixes.** Two bridged effects with equal params already share a content hash today. K-C4 does not
   inherit it, but leaving it unfixed means the next contributor who adds a bridged effect walks
   into it. Follow-up 1 files it; it is a ~10-line fix (hash the `UnknownTag` string) plus a test,
   and it should not be smuggled into this item's diff.
3. **Analytic antialiasing is where CPU/GPU parity will actually break.** The edge-coverage maths
   must be written once and mirrored exactly; a `smoothstep` with a slightly different band on one
   side passes casual inspection and fails test 7 at 1e-3, or worse, passes at a loosened tolerance.
   Do not loosen the tolerance — fix the twin.
4. **The two audio feeders can drift.** §3.6's change is duplicated by construction because the two
   loops are near-copies today (`session.rs:2157-2230` vs `offline_audio.rs:80-140`). Test 12 is the
   only thing standing between "the tone plays" and "the tone plays but does not export".
5. **Catalogue creep.** Six ids is a catalogue; sixty is a product. Each new generator is a WGSL
   twin, a CPU reference, a parity test, a scale-invariance case and a golden. The manifest makes
   adding one *cheap to declare* and no cheaper to get right.

### 9.2 Open questions needing a product call (each with a recommendation)

1. **Should PM5544 and FuBK ship at all?** They are not open technical standards in the way SMPTE
   RP 219 and EBU Tech 3213 are — they are specific test-card designs originating from named
   organisations (Philips; IRT/ARD), carrying possible design-right and trademark considerations
   that a synthesized SMPTE bar pattern does not. *Recommendation: exclude from v1 (as §3.4 does),
   and treat any later inclusion as its own gated item requiring a
   [23 §3.3](../specs/video-editor/23-legal-open-source-implementation-routes.md#33-required-evidence-record)
   evidence record.* Including them on the same footing as the SMPTE/EBU patterns would convert
   K-C4 from an ungated item into a gated one, which is the opposite of what ROADMAP §7 asks for.
   **This needs a product/legal sign-off because it narrows an item's stated scope**, not because
   the engineering is hard.
2. **Should `leader.countdown` ship in v1, or only `counter`?** The leader is the largest single
   piece of geometry in the catalogue (sweep, crosshair, rings) for the smallest workflow.
   *Recommendation: ship it* — the sweep is the only generator that makes a dropped frame visible to
   the naked eye during playback, which is a real diagnostic. It is also the clean cut if the item
   runs long.
3. **Where does the Generators palette live?** *Recommendation: a `panels/video/generators.rs`
   sibling to `titles.rs`, in the same panel group*, because a generator is a source you insert (like
   a title), not an effect you attach. Explicitly **not** the effects browser, whose
   `"UTIL / GENERATORS"` heading (`effects_browser.rs:58`) means 0-input *effect nodes*.
4. **Default generator clip duration?** *Recommendation: 10 seconds for bars/noise, `seconds`
   parameter (default 5) for the leader, matching the `TitlePreset::duration_secs` precedent
   (`titles.rs:228`).* A UX call, not an engineering one.

### 9.3 Deliberately excluded

- **Image sequences / stop motion.** Owned outright by **D-6** (ROADMAP §2, §3a; 26 §11's own scope
  boundary). Not touched, not mentioned in the model, not in the catalogue.
- **PM5544 and FuBK.** §9.2 question 1.
- **Luma-map wipes (K-B7).** Shares the rights gate, is a separate item, and is not specified here.
- **Animatable generator params.** §3.3 — additive later, and it needs an `AnimTarget` arm that is
  its own decision.
- **Gradient / checkerboard / colour-wheel / SMPTE-EG-1 split-field variants.** Easy to add on the
  manifest, each with a real per-generator test cost (§9.1 risk 5). Not in v1.
- **A generator that reads the timeline** (e.g. burn in the clip's name, or a "slate" reading project
  metadata). It would make a generator a function of document state beyond its own params, which
  breaks §7's contract at the root. If a slate is wanted, it is a title clip.
- **Baking a generator to a media file.** That is a **K-C1** clip job
  ([195 §3.2](195-k-c1-clip-jobs-framework.md)'s `JobKind`), not a generator feature. K-C1 §1
  already names "K-C4 generators' bake step" as a future consumer of its framework; K-C4 provides
  the generator, K-C1 provides the job.
- **Any new `TimelineCmd` variant, any new invalidation channel, any change to `ContentHash`'s
  structure.** §5, §7.3.

---

## 10. Clean-room provenance

Per [26 §2](../specs/video-editor/26-kdenlive-mlt-parity.md#2-clean-room-and-licensing-fence) and
[23 §3.4](../specs/video-editor/23-legal-open-source-implementation-routes.md#34-clean-room-protocol):

- **What was read.** (a) Photonic's own code and specs, cited by `file:line` throughout. (b) 26 §11
  K-C4's requirement statement — that a counter, colour bars and noise are missing — itself derived
  from Kdenlive's `CC-BY-SA-4.0` user documentation as a *requirements source*, cited and never
  pasted. (c) The published standards themselves as the definition of the patterns: **SMPTE RP 219-1**
  (HD colour bars), **EBU Tech 3213** (EBU colour bars), **ITU-R BT.1729** (test patterns for
  conventional television), **EBU R 68** and **SMPTE RP 155** (audio alignment level). Standards are
  read as standards; their revision is cited beside every constant transcribed from them.
- **What was not read.** The Kdenlive source tree, the MLT / `mlt++` source tree, frei0r, FFmpeg's
  `libavfilter` sources (including `vsrc_testsrc`), and any GPL/LGPL derivative. No identifier,
  comment, constant, argument ordering, control flow or test case below derives from them. The
  implementer records the 23 §3.4 attestation for the `core-timeline` and
  `photonic-video-engine` subsystems, and an **independent provenance reviewer checks identifiers,
  comments, constants and test provenance before merge** (26 §2 point 2) — for this item that review
  explicitly includes checking each transcribed pattern constant against the cited standard clause,
  and checking the integer-hash constants against their stated published origin.
- **No implementation's output is sampled, anywhere.** §8 states this as a test-design rule, not an
  aspiration: the existing `color_bars.mp4` fixture (ffmpeg `smptebars`) is explicitly barred as
  ground truth, and acceptance is against transcribed standard values.
- **Bundled bytes: none. K-C4 is therefore NOT a legal- or fixture-gated item.** Every pattern is
  synthesized at runtime from the cited equations, the tone is synthesized from a stated frequency
  and level, the noise comes from a stated integer hash, and the counter's digits come from a
  geometry table rather than a font. No image, no audio file, no font, no LUT and no lookup table of
  third-party origin ships with this item, so
  [23 §7.2](../specs/video-editor/23-legal-open-source-implementation-routes.md#72-manifest)'s
  `AssetRightsManifest` gate is not engaged and
  [ROADMAP §7](../specs/video-editor/ROADMAP.md#kex-gates)'s K-C4 clause is satisfied by the route it
  itself recommends. This is the design's spine, not a side effect: §3.5's own-IR-op decision,
  §7.4's stroke-table digits and §8's analytic acceptance all exist partly to keep it true.
- **Photonic-ahead properties preserved** (26 §5, ROADMAP §9). **PA-1** consumed as designed — a new
  op with a complete hash arm, no new invalidation channel (§7.3). **PA-2** honoured — patterns are
  authored as standard code values and converted into the linear working space, never written raw
  (§3.4, test 3). **PA-3** — one WGSL twin per generator on the single backend, with the CPU
  reference normative. **PA-7/PA-8** — the frame ordinal is integer `frame_at` over exact rational
  rates and flicks `Tick`; no float time (§7.1). **PA-9** — `EditError::WrongTrackKind` is a typed
  error, and an unknown generator id is preserved as typed data with a diagnostic, never a silent
  fallback (§3.5). **No reference NLE limitation is ported**: the catalogue is data, not a hard-coded
  switch; generators participate in the same cache, the same colour pipeline and the same undo model
  as every other source.
- **No new dependency is contemplated or authorized.** Nothing in 26 §2's reject list, directly or
  transitively. Everything needed (`xxhash-rust`, `serde`, the shipped wgpu path, the shipped mixer)
  is already in the build. No RNG crate is added — §7.2 rule 4 removes the need for one.

---

## 11. Definition of done → ROADMAP §10, made answerable

| # | ROADMAP §10 point | Answered by |
|---|---|---|
| 1 | Core op/engine service with unit tests | `photonic-core/src/timeline/generator.rs` (catalogue + spec), `ops::add_generator_clip`, `IrOp::Generator` with CPU reference and WGSL twin; §8 tests 1–9, 11, 16 |
| 2 | GUI route, or a recorded exception | Generators palette (`panels/video/generators.rs`) + manifest-driven inspector section; §8 test 17. **No exception requested** |
| 3 | MCP tool/schema/generated docs | §6 — three new tools plus a `ClipSourceArg` arm; `list_generator_kinds` generated from the table; `docs/mcp-api.md` regenerated under `ci.yml:162-167`; §8 test 19. **No parity exception requested** |
| 4 | One user verb = one undo unit | §5 — zero new command variants, exact inverses tabulated; §8 tests 13–14 |
| 5 | Additive serde/migration round-trip | §4 — stays v5, one new serde tag; §8 test 15 |
| 6 | IR/eval/golden/sync coverage for new pixel/audio paths | The whole of §7; §8 tests 2–4, 7, 8, 10 (pixel) and 11, 12 (audio). This is the item's largest test surface and the reason §7 is normative |
| 7 | Hard gates green; trend metrics not regressed | Scale invariance (test 8) and CPU/GPU parity (test 7) are the two hard gates on this path and both are direct acceptance rows. Export determinism strengthens rather than regresses (test 4). A *static* generator adds one cached node for a whole clip, so the graph-compile budget is unaffected |
| 8 | Offline, privacy, licensing, content, product gates | §10 — no bundled bytes, no new dependency, no network, no host font, no user content of any kind touched. **Ungated** |
| 9 | No protected-surface regression | §10's PA list. `SolidColor` and `Text` are untouched (§3.1), so their golden/parity corpora are unchanged; no existing `TimelineCmd`, `IrOp` or manifest row is modified |
| 10 | Goal-backward L1–L4, incl. GUI/MCP parity | §1's six outcomes are the L4 script; §8 test 18 is the parity story; test 17 is the GUI arm |

---

## Follow-ups

Changes this document deliberately did **not** make to existing files (each needs its own change):

1. **`crates/photonic-video/src/graph/compile.rs:2887-2902`, `effect_kind_tag`** — a real defect,
   not a doc issue. Every `EffectKind::Unknown(tag)` hashes as `255`, and since K-B16 every bridged
   effect lowers as `Unknown(tag)` (`raster_bridge.rs:20-21`, `compile.rs:1224`), two bridged
   effects with equal resolved params now share a content hash. `color.desaturate` and
   `color.invert_raster` both carry the empty `INVERT_PARAMS` (`effect_manifest.rs:343`) and are both
   in `BRIDGED_IDS` (`raster_bridge.rs:71,73`), so the collision is reachable today. The function's
   comment ("a benign (rare) cache miss, never a correctness bug") describes a false *miss*; what
   actually happens is a false *hit*. Fix: hash the `UnknownTag`'s bytes for the unknown arm, plus a
   regression test asserting two bridged ids with identical params hash differently. **Filed here,
   not fixed here**, so it is not smuggled into a feature diff.
2. **[26 §11 K-C4](../specs/video-editor/26-kdenlive-mlt-parity.md#k-c4--generator-clips), Files
   line** — it proposes `ClipSource::Generator(GeneratorKind)`. §3.1/§3.3 chose a manifest-described
   `GeneratorSpec { id, params, seed }` instead, for the reasons 30 §2 gives for effects. If this
   document is accepted, 26's Files line should be amended to match, and to name the new
   `IrOp::Generator` rather than implying reuse of the `MaskShapeGen` effect path (which §2.3(b)/(c)
   argue against).
3. **[30-effect-catalogue.md](../specs/video-editor/30-effect-catalogue.md) §2** — it should record
   that `ParamSpec` / `ParamKind` / `Display` / `UiHint` are now shared with a **second** catalogue
   (`GENERATORS`), that `MANIFESTS` remains effects-only, and why the two tables are separate
   (§3.3). Without this note the next contributor will reasonably try to add a generator to
   `MANIFESTS` and break the kernel-binding join.
4. **[02-engine.md](../specs/video-editor/02-engine.md) §2's IR op list** — add `Generator`, and
   note it is the second 0-input source op after `SolidColor`.
5. **[26 §11 K-C4](../specs/video-editor/26-kdenlive-mlt-parity.md#k-c4--generator-clips), pattern
   list** — it lists "SMPTE, EBU, PM5544, FuBK". §3.4/§9.2 ship the first two and exclude the last
   two on rights grounds. If that exclusion is accepted, 26's item text should be amended to say so
   and to point at the evidence record any later inclusion would require.
6. **[ROADMAP §7](../specs/video-editor/ROADMAP.md#kex-gates)** — its K-B7/K-C4 line can record that
   **K-C4 discharged the gate by runtime synthesis** and is no longer a gate candidate, leaving K-B7
   as the only remaining holder of that row.
7. **[ROADMAP §0](../specs/video-editor/ROADMAP.md) progress table** — add a K-C4 row when the item
   lands, with its commit, per the existing convention.
8. **`crates/photonic-video/src/graph/eval_cpu.rs:20-28`** — its "excluded from GPU/CPU byte-parity"
   note covers captions and titles. Once generators land, that note should say explicitly that
   generators are **not** in the exclusion, so the boundary stays legible.
