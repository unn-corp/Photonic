# 193 — K-A1 Chunked Timeline Preview Rendering

> **Status: Proposed — Band-5 mini-spec, pre-code.**
> [26 §19.1](../specs/video-editor/26-kdenlive-mlt-parity.md#191-bands) makes an accepted
> mini-spec the exit condition for every K-Band 5 item: it must name the data-model
> change, migration, undo unit, MCP surface and acceptance fixtures *before* code.
> This document discharges that for **K-A1**
> ([26 §9](../specs/video-editor/26-kdenlive-mlt-parity.md#k-a1--chunked-timeline-preview-rendering)),
> whose full design contract is owned by
> [33-timeline-preview-render.md](../specs/video-editor/33-timeline-preview-render.md).
> No code authorization until accepted
> ([23 §14](../specs/video-editor/23-legal-open-source-implementation-routes.md#14-stopgo-checklist-before-any-code)).

**Owner ref:** [33](../specs/video-editor/33-timeline-preview-render.md) in full · 26 §9 K-A1 · 26 §19.3 row 6
**Territory:** `photonic-video-engine` → `timeline-panel` · **Effort:** L (the largest single K-Band 5 item, per 33 §8)
**Gate carried:** codec/patent/distribution record per
[23 §10.3](../specs/video-editor/23-legal-open-source-implementation-routes.md#103-patent-and-distribution-gate)
(ROADMAP §14 row for K-F5/K-A1). §9 argues this down to an *amendment* of an existing record, not a new one.

This document is subordinate to 33. Where it differs from 33 it says so explicitly
and gives the code citation that forced the difference; those deltas are collected
in **Follow-ups** as amendments 33 should absorb.

---

## 1. Problem and user outcome

A section of timeline with stacked effects, a nested sequence, a vector clip or a
1080p multi-layer composite exceeds the per-frame evaluation budget
([02 §8](../specs/video-editor/02-engine.md#8-perf-budgets-verified-in-11): 8 ms GPU for 1080p,
3 layers, grade and captions). Today the only responses Photonic offers are
**decode-cost** reductions (proxies, `ProxyMode`) and **render-cost-at-lower-resolution**
reductions (`PreviewQuality::Draft`, which caps the long edge at
`DRAFT_MAX_LONG_EDGE = 960`, `crates/photonic-video/src/graph/compile.rs:225`). Neither pays the
full-quality render cost *in advance*. When the section is genuinely expensive, the
user watches it stutter, or watches a downscaled approximation of it, and cannot
form a confident judgement about the cut.

After this item, a user can:

1. Mark one or more **preview zones** on the timeline, hit **Start Preview Render**,
   and afterwards play that region back at full sequence resolution and full
   quality in realtime, every time, regardless of how heavy the graph is.
2. Read a **status strip** under the ruler that says, per second of timeline,
   whether the region is rendered, rendering, or stale — so "will this play?" is a
   glance, not an experiment.
3. Edit something and see **only the chunks that edit actually changed** turn
   stale. Grading clip A does not invalidate a chunk covering only clip B. This is
   the differentiator; if it does not hold, the feature is merely a disk cache.
4. Press Ctrl+Z and watch stale chunks **turn valid again with no re-render**,
   because undo restores the prior hashes.
5. Export over a fully-rendered range and get a file that is **byte-identical** to
   one exported with the cache empty. A preview chunk is lossy by construction; it
   must never reach a master.
6. Drive all of the above from an agent over MCP, with full GUI/MCP parity (§7).

What the user explicitly *cannot* do afterwards, and what the UI must not imply:
this does not make **editing** faster. It makes **playback of an already-rendered
region** fast. 33 §1 is emphatic about this and so is the reference product's own
manual; the wording in the GUI is part of the deliverable, not decoration.

---

## 2. Current state in code

### 2.1 Nothing of K-A1 exists

```
grep -rn 'mod preview\|preview_chunk\|ChunkKey\|PreviewZone\|RenderPreview' crates/
```

returns **0 hits** (verified 2026-07-28 at `19f9fd5`), which is consistent with
26 §4.3's recorded grep (`timeline_preview|render_chunk|preview_chunk`, 26:106).
There is no `graph/preview` module, no chunk index, no `EngineCmd` variant, no
document field, no MCP tool and no GUI surface. Everything in §3–§8 is new.

### 2.2 What the design lands on, exactly

| # | Primitive | Where | What it gives K-A1 |
|---|---|---|---|
| 1 | `ContentHash(pub u128)` = xxh3-128 of `(op discriminant, resolved params, input hashes)` | `crates/photonic-video/src/graph/ir.rs:38`; computed by `content_hash()` at `crates/photonic-video/src/graph/compile.rs:2568-2581`, op payloads at `compile.rs:2583-2711` | The whole differentiator. The terminal `Output` node's hash is a complete description of the frame |
| 2 | `IrOp::DecodeVideo { asset, src_time, proxy }` and its hashing (`compile.rs:2591-2600` writes tag, asset uuid, src_time, **and the proxy flag**) | `ir.rs:185-189` | 33 §5's "proxy state **must** participate in the hash" is **already held**, by construction. Acceptance 6 falls out free |
| 3 | `hash_resolved_params` (`compile.rs:2722-2754`), `hash_caption_batch` (`compile.rs:2761`), `hash_resolved_grade_op` (`compile.rs:2793`), LUT tables hashed per-node (`compile.rs:2876` comment) | — | Effect params, caption karaoke colours, grade ops and resolved `.cube` tables all move the hash. Two blur radii are two identities, not a collision |
| 4 | Every present already compiles the graph: `compile_with_luts(...)` at `crates/photonic-video/src/session.rs:1112-1120` | — | 33 §4's "one hash computation (already performed) plus a lookup" is literally true. The serve check costs a `HashMap` probe |
| 5 | `NodeCache` (`crates/photonic-video/src/graph/cache.rs:71-143`) with `invalidate_matching` at `cache.rs:130` | — | The in-memory analogue; the disk chunk index is its persistent sibling, and the two must not be conflated (§4.4) |
| 6 | E-1 source-range contract: `source_range_for_op` / `graph_source_range` (`crates/photonic-video/src/graph/source_range.rs:79,112`), `SOURCE_RANGE_SOFT_CAP = 16` (`source_range.rs:69`) | — | **Bounds how far a chunk renderer may read upstream** — the thing that makes chunk boundaries safe to cut at. §5.4 |
| 7 | `combined_prefetch_lead` (`crates/photonic-video/src/playback/prefetch.rs:70-73`), wired at `session.rs:1176-1183` | — | The existing derivation of a decode window from the graph. The chunk renderer reuses it verbatim |
| 8 | `run_export_job` (`crates/photonic-video/src/export/job.rs:152`) — opens a **dedicated `EngineSession`** over a frozen snapshot (`job.rs:168-175`), forces `ProxyMode::ForceOriginal` (`job.rs:180-185`), drives it with `EngineCmd::Seek` and reads back per frame (`job.rs:243-294`), feeding `render_loop::export_frames` (`export/render_loop.rs:141`) | — | The render path K-A1 reuses. **And the reason §6.2's isolation rule must be a session flag, not an assertion in the export loop** |
| 9 | `EncoderCapabilities::probe` (`crates/photonic-video/src/export/encoder.rs:110`), `h264_encoder()` (`:148`), `VideoCodec::{H264, ProResLikeMezzanine}` (`export/presets.rs:61,65`) | — | Preflight-and-fail-closed encoder selection, and the two encoders §9 picks from so **no new codec enters the build** |
| 10 | Sidecar cache: `CACHE_DIR_SUFFIX = ".cache"` / `cache_dir_for_project` (`crates/photonic-video/src/media/keyframe_index.rs:41,48`), `proxy_cache_dir` (`media/proxy.rs:76`), `summarize_cache` categories (`media/cache_stats.rs:32-62`) | — | Where chunks live, and the K-C5 pane that must gain a `preview` category |
| 11 | `atomic_write::{staging_path, write_atomic, sweep_stale_staging}` (`crates/photonic-video/src/media/atomic_write.rs:21,32,60`) | — | [37 §2.3](../specs/video-editor/37-robustness.md)'s temp-and-rename, which 37:74 already names preview chunks as a consumer of |
| 12 | `DecodeSource` + `SourceParams { input, width, height, pix_fmt, pts_kind, keyframes }` (`crates/photonic-video/src/decode/scheduler.rs:45-77`) | — | The serve path decodes a chunk with the **existing** decoder, given a synthetic all-intra `KeyframeIndex` and `PtsKind::Cfr` |
| 13 | `TimelineCmd` + `inverse()` (`crates/photonic-core/src/timeline/commands.rs:660,666,2504-2512`), `execute_discrete` (`crates/photonic-core/src/history/stacks.rs:403`) | — | The undo machinery §6 uses; `AddMarker`/`RemoveMarker` is the exact shape zones copy |
| 14 | `ProjectVideoSettings { generate_proxies, cache_limit_mb, … }` (`crates/photonic-core/src/timeline/sequence.rs:90-103`) | — | The budget 26 §9 says preview chunks join, and where the preview profile lands |
| 15 | `draw_ruler` (`crates/photonic-gui/src/app/timeline/ruler.rs:92`) | — | Where the status strip attaches |

### 2.3 Four things that do **not** exist, or exist differently from how 33 assumes

These are the reasons this document is not a restatement of 33. Each one changes a
concrete decision below.

**(a) The content hash does not encode the evaluation canvas.**
`GpuEvaluator::evaluate(&graph, canvas, source)` takes `canvas` as a *runtime
argument* (`crates/photonic-video/src/graph/eval.rs:465-471`), and `session.rs:1200-1205`
(`preview_canvas`) sets it to the Draft-capped size while `IrOp::Output { w, h }`
still carries the full format size from `compile.rs`. So **the same
`ContentHash` describes both a Draft-canvas render and a Full-canvas render.**
A chunk keyed on the hash alone could be written at 960px and served as
full resolution. Consequence: §5.1 puts the render profile in the key and §5.5
renders chunks only at full format size.

**(b) The content hash does not encode media *bytes*, only `AssetId`.**
`compile.rs:2591-2600` hashes `asset.0.as_u128()`. `MediaAsset.content_hash:
Option<String>` (the xxh3 of file head+tail+len, `crates/photonic-core/src/timeline/media.rs:55`)
is *not* in the graph hash. In-memory this is fine — a relink explicitly evicts via
`MediaSources::invalidate_assets` (`session.rs:1599-1608`) and
`NodeCache::invalidate_matching` (`cache.rs:130`). But a **disk** chunk outlives the
session, so a user who relinks `shot_A` to different bytes, or replaces the file
under the same path, would be served a stale chunk with no signal. Consequence:
§5.2's fold mixes a media-identity salt, which is the single most important
correctness addition this document makes to 33.

**(c) `RasterVector`'s state key does not hash the vector document.**
`vector_state_key` (`compile.rs:2519-2540`) hashes only `(vref discriminant, format
size, src_time, asset uuid)`; its own doc comment says referenced-node-state hashing
"lands with the vector-animation story". Editing an embedded vector's geometry
therefore does **not** move the IR hash. `MediaSources::set_document` papers over
this in-memory by clearing the vector cache on revision change (`session.rs:1493-1500`);
a disk chunk has no such backstop. Consequence: §5.6 **refuses to chunk any tick
whose compiled graph contains `IrOp::RasterVector`**, and says so in the UI, until
that key is complete.

**(d) Export drives the same `present()` loop.**
33 §3.3's "must never reach an export … asserted in the export loop" is not
sufficient: `run_export_job` builds a real `EngineSession` (`export/job.rs:168-175`)
and pulls frames through `session.rs:1051`'s `present()`. If serving lives in
`present()`, the export session gets it too, unless serving is *off by default* at
the session level. Consequence: §6.2 makes chunk serving an opt-in `EngineCmd`,
defaulting off, which the interactive GUI session enables and nothing else does.

### 2.4 One more absent prerequisite, stated plainly

33 §8 names [32 §8](../specs/video-editor/32-engine-contracts.md) (CPU/GPU evaluator
equivalence) as an "ideally" prerequisite so that "pixel-identical" is meaningful.
It is not closed. Photonic does not today hold a claim of bit-exact output across
GPU adapters. §5.3 therefore stamps a **renderer identity** into the chunk index and
treats a mismatch as *absent*, never as *valid*. This is the difference between a
cache that is merely portable and one that is portable and correct.

---

## 3. Data-model change

Three additive fields. Chunks themselves never enter the document — they are cache
state, exactly as 33 §3.1 requires.

### 3.1 `PreviewZone` on the sequence

```rust
// crates/photonic-core/src/timeline/ids.rs — added to the id_newtype! block (ids.rs:62-89)
/// Identifies a [`PreviewZone`](crate::timeline::PreviewZone) on a sequence (K-A1).
PreviewZoneId,

// crates/photonic-core/src/timeline/sequence.rs — appended to Sequence (sequence.rs:126-160)
/// K-A1 preview zones: regions the user has asked to keep pre-rendered.
/// Additive; absent in files written before K-A1.
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub preview_zones: Vec<PreviewZone>,

/// A user-declared region to keep pre-rendered (33 §3.1). Half-open
/// `[start, start + duration)` — PA-7; `duration` is never `Option`, matching
/// `Marker` (35 §1). Zones are document state; chunks never are.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PreviewZone {
    pub id: PreviewZoneId,
    pub start: Tick,
    pub duration: Tick,
}
```

**Why `{ start, duration }` and not 33 §3.1's `{ start, end }`.** Every range in the
model is half-open start+duration — `Clip`, `Marker.duration` (`sequence.rs:836-838`,
"Never `Option` (35 §1)"), the loop range. PA-7 records half-open ranges as a
protected property specifically because the reference's inclusive-`out`-plus-`length`
pair is a permanent off-by-one hazard. Introducing an `end` field here would be a
one-field regression against a protected surface. This is a **delta from 33 §3.1**;
Follow-up 1.

**Why zones are per-sequence and not per-project.** Chunks are keyed by
`(sequence, format)` in 33 §3.1 already, and PA-6 (per-sequence formats) means a
zone at 00:10 in sequence A has no meaning in sequence B.

**Invariants**, enforced in `ops.rs` at construction, not by the GUI:
non-zero duration; frame-aligned start and duration on the sequence rate
(`FrameRate::snap`); zones **merged on overlap** so the list is a disjoint,
sorted cover. Merging is why "Add Preview Zone" over an existing zone is a
`SetPreviewZones` command rather than an `AddPreviewZone` (§6.1).

### 3.2 The preview profile on `ProjectVideoSettings`

```rust
// crates/photonic-core/src/timeline/sequence.rs — appended to ProjectVideoSettings (sequence.rs:90-103)
/// K-A1 preview-chunk render profile. `None` = the built-in default
/// (`IntraH264`, quality 12, `PreviewScale::Full`). Additive.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub preview_profile: Option<PreviewProfile>,

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PreviewProfile {
    pub codec: PreviewCodec,
    /// Encoder quality knob, profile-interpreted; lower is better for H.264 CRF.
    pub quality: u8,
    pub scale: PreviewScale,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PreviewCodec { IntraH264, IntraMezzanine }

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PreviewScale { Full, Half }
```

It lives beside `cache_limit_mb` (`sequence.rs:96`) because 26 §9 puts the chunk
budget there and because the profile is a project-level rendering choice, like
`generate_proxies` (`sequence.rs:92`). **`PreviewCodec::Lossless` from 33 §3.3 is
dropped** — §9 argues why (it would introduce a third encoder to the distribution
surface for a preview-only path; `IntraMezzanine` maps to `prores_ks`, which
already ships at `export/presets.rs:65,313`). Delta from 33 §3.3; Follow-up 2.

### 3.3 Nothing else

No clip field, no track field, no asset field. `ChunkKey`, `ChunkIndex`,
`PreviewProfileId` and the render/serve machinery are **engine types in
`crates/photonic-video/src/graph/preview/`**, never serialized into the `.photon`.
The chunk index is a sidecar JSON file (§5.7), which is cache, not document.

---

## 4. Migration

**`CURRENT_FORMAT_VERSION` stays at 5. This does not need a v6.**

`crates/photonic-core/src/document.rs:117` pins the current version at 5, and
`crates/photonic-core/src/migration.rs` defines a `Migration` as a function that
*reinterprets existing data* on the way from N to N+1. K-A1 reinterprets nothing:

- `preview_zones` is `#[serde(default, skip_serializing_if = "Vec::is_empty")]` and
  `preview_profile` is `#[serde(default, skip_serializing_if = "Option::is_none")]`
  — byte-identical in shape to how `markers`, `groups`, `rating` and `tags` were
  each added (`sequence.rs:142`, `media.rs:68-73`), all inside v5, none with a bump.
- An old file loads with `preview_zones: vec![]` and `preview_profile: None`, which
  is the correct and complete meaning: "no zones declared, use the built-in profile".
- A K-A1 file opened by an older build omits both keys when empty/`None`, and when
  present they are preserved by [39 §2.2](../specs/video-editor/39-document-lifecycle.md)'s
  unknown-preserving machinery (landed; `crates/photonic-core/tests/forward_compat.rs`).
- **No chunk data is in the document**, so there is no cache-format compatibility
  question at the document layer at all.

A version bump would be actively wrong: it would push every v5 project through a
no-op migration and would make a `MigrationV5ToV6` a lie about what changed. **Bump
only when data must be reinterpreted.**

There *is* a second, independent versioned artefact: the **chunk-index sidecar**
(§5.7). It carries its own `index_version: u32`, starting at 1. Its compatibility
rule is the opposite of the document's, and deliberately so: **an index whose
version, renderer identity or profile digest is not recognised is discarded
wholesale, not migrated.** A cache is rebuildable by definition (CAP-014's rule for
proxies, `media/proxy.rs:11-15`); spending migration code on it would be a category
error, and a half-understood cache is a wrong-pixels bug.

Required migration work is therefore one round-trip test, not a migration (§8 test 11).

---

## 5. The chunk model — key, fold, eviction, partial edits

This section is what 26 §19.3 asks for: making "the content hash makes invalidation
exact rather than heuristic" concrete enough to implement.

### 5.1 The key

```rust
// crates/photonic-video/src/graph/preview/key.rs
pub struct ChunkKey {
    pub sequence: SequenceId,
    pub format: u16,             // active SequenceFormat index — per-format chunks (PA-6)
    pub profile: ProfileDigest,  // xxh3-64 of (PreviewCodec, quality, PreviewScale)
    pub start: Tick,             // chunk-aligned, floor from Tick::ZERO
    pub fold: ChunkFold,         // 128-bit, §5.2
}
```

`format` and `profile` are **selectors**, not invalidators: changing either picks a
different chunk set, so switching format and back, or trying a profile and
reverting, costs nothing (33 acceptance 5, extended to profiles). `fold` is the
validity predicate. `start` plus the index's per-chunk `tick_count` gives the span.

**Delta from 33 §3.1:** 33's `ChunkKey` has no `profile` field. Without it, changing
the profile silently reuses chunks encoded under the old one, and — because of
§2.3(a) — a `PreviewScale::Half` chunk would be served as full resolution.
Follow-up 3.

**Chunk length: one second of sequence time**, frame-aligned, floor-aligned from
`Tick::ZERO` so boundaries are stable under trim and insert. 33 §3.1's reasoning
holds and is strengthened by PA-8: with `TICKS_PER_SECOND = 705_600_000` and exact
rational `FrameRate`, one second is an exact integer number of ticks at every
supported rate, and `ticks_per_frame()` divides it exactly (`ir.rs:375-383` proves
this for 30000/1001). A fixed *frame* count would not be rate-independent. The final
chunk of a sequence may be short; its `tick_count` is recorded so a 24-tick fold is
never confused with a 30-tick one.

### 5.2 The fold — how a chunk key is derived

For each tick `t` in the chunk, the engine already has, from the present-path
compile it performs anyway (`session.rs:1112-1120`):

```
out_hash(t) = compiled.graph.nodes[compiled.graph.output].content_hash
```

The fold is an **order-sensitive** xxh3-128 over the sequence of
`(ordinal, out_hash)` pairs, plus a **media-identity salt** and a **renderer
identity**:

```
ChunkFold = xxh3_128(
    b"photonic.preview.chunk.v1",
    render_identity_digest,                     // §5.3
    tick_count as u32,
    for i in 0..tick_count:  (i as u32, out_hash(start + i*tpf).0),
    media_salt                                  // §5.2.1
)
```

Three properties, each load-bearing:

1. **Any tick change alters the fold.** Folding rather than storing per-tick hashes
   is exact for validity and compact — 33 §3.1's argument, kept. 128 bits, never
   truncated: a fold collision presents a stale frame, which is the worst failure
   mode available here because it is invisible.
2. **Order sensitivity matters.** Two frames swapped by an edit (a clip reversal, a
   reorder) must change the fold. An XOR or sum fold would not catch it.
3. **`tick_count` is folded in**, so a short trailing chunk can never match a full one.

**5.2.1 The media salt — the correction §2.3(b) forces.** For every distinct
`AssetId` referenced by any `IrOp::DecodeVideo`/`DecodeStill` node in the chunk's
graphs, the salt mixes, in `AssetId`-sorted order:

```
(asset uuid, MediaAsset.content_hash.unwrap_or(""), resolved decode path, proxy flag)
```

`MediaAsset.content_hash` is `media.rs:55` (xxh3 of head+tail+len — "the relink
identity", its own doc comment). The resolved decode path comes from
`media::resolve_decode_input` (`media/mod.rs:34`), which is what actually gets
opened. **If any referenced asset has `content_hash: None`** — an unprobed asset —
the chunk is **not written**, and the reason is surfaced (`§10` diagnostic
`PreviewChunkSkipped`). Refusing to cache an unidentifiable input is the only
honest option; guessing produces a cache that is wrong exactly when media changes
underneath it, which is the common case this feature would otherwise create.

This salt is the reason a chunk survives a project move (33 §2 consequence 3) *and*
does not survive a relink to different bytes.

### 5.3 Renderer identity

```rust
pub struct RenderIdentity {
    pub engine_semver: &'static str,      // photonic-video crate version
    pub backend: wgpu::Backend,
    pub adapter_name: String,
    pub adapter_driver: String,
    pub shader_digest: u64,               // xxh3-64 over the WGSL sources compiled into the build
}
```

Its digest is folded into every chunk (§5.2) and stamped in the index header. A
chunk whose identity does not match the running build is **treated as absent** —
shown as *not rendered*, not as *stale* — and is eligible for immediate deletion.

Why absent rather than stale: "stale" means "the document changed", which is a
statement about the user's edits and is what the red strip communicates. A chunk
rendered on a different GPU is not stale; it is unverifiable. Conflating them would
teach users that red means "you changed something", then lie to them the first time
they open a project on a laptop. 32 §8's CPU/GPU equivalence contract is not closed
(§2.4), so bit-exactness across adapters is not a property we can claim; when 32 §8
lands with an adapter-independence guarantee, `adapter_name`/`adapter_driver` can be
dropped from the digest in a follow-up and every existing chunk simply re-renders once.

### 5.4 Chunk independence — why cutting at a boundary is safe (E-1)

A chunk renderer must know how far outside its own span it is allowed to read. E-1
answers this and it is already implemented:

- `source_range_for_op` (`graph/source_range.rs:79-108`) declares, per op, the
  upstream tick range needed for an output tick. Today every op is identity except
  `Deinterlace`, which declares `[out−1, out+1]` (`source_range.rs:84-86`).
- `graph_source_range` (`source_range.rs:112-118`) unions them for a whole graph.
- `SOURCE_RANGE_SOFT_CAP = 16` (`source_range.rs:69`) bounds the expansion; past it
  the compiler is required to diagnose rather than expand unboundedly.

So for chunk `[c, c+1s)`, the decode window the renderer must warm is
`[c − L, c + 1s + L)` where `L = max over ticks of lead_from_source_range(...)`
(`prefetch.rs:63-67`), and `L` is provably `≤ SOURCE_RANGE_SOFT_CAP` ticks. The
chunk renderer computes it with the **same** `combined_prefetch_lead` the
interactive path uses (`prefetch.rs:70-73`, wired at `session.rs:1176-1183`), so
there is one derivation of "how far ahead may I read", not two.

**This is the property that makes chunk boundaries free.** A frame's value never
depends on an arbitrary amount of history; it depends on a declared, bounded window.
The reference engine cannot state this, which is why its chunking is a heuristic
over time ranges. Photonic's is a consequence of a contract that already shipped.

### 5.5 Rendering: what a chunk actually contains

- Always at **full `SequenceFormat` size** for `PreviewScale::Full`, or exactly
  half each dimension (rounded down to even) for `PreviewScale::Half`. Never at the
  Draft canvas — see §2.3(a). The scale is in the key, so the two never mix.
- Always `Quality { proxy }` matching the *user's current proxy intent*, and the
  proxy flag is inside the per-node hash already (`compile.rs:2599`), so
  proxy-rendered and original-rendered chunks are automatically distinct identities
  (33 acceptance 6, free).
- Rendered by a **dedicated `EngineSession`** over a frozen `Arc<TimelineProject>`
  snapshot, in the shape `run_export_job` already proves (`export/job.rs:168-175`),
  driven with `EngineCmd::Seek` per tick and read back with `read_texture_rgba16f`
  (`export/job.rs:266`). One render path, not two — 33 §4's "same compile, same
  eval, same convert, different sink".
- The sink is `render_loop::export_frames` (`export/render_loop.rs:141`) with a
  `ResolvedExport` whose `out_path` is the chunk's staging path and whose
  `colorimetry` is `Colorimetry::BT709_LIMITED`, matching `export/job.rs:230`.
- **The chunk-render session has chunk serving OFF** (§6.2), so a preview render can
  never read its own or another chunk's output. Without this the feature could
  laminate lossy generations on top of each other.
- Written through `atomic_write::staging_path` + rename
  (`media/atomic_write.rs:21`), so a crash leaves a `.tmp`, never a truncated file
  that looks finished ([37 §2.3](../specs/video-editor/37-robustness.md), 37:74-76).
  Startup sweeps with `sweep_stale_staging` (`atomic_write.rs:60`).

### 5.6 What is refused, at render time, with a reason

A chunk is **not written** when any of these hold for any tick it covers. Each is a
known incompleteness in the hash, and serving a chunk over an incomplete hash is the
one unrecoverable bug in this design:

| Refusal | Because |
|---|---|
| The graph contains `IrOp::RasterVector` | §2.3(c): `vector_state_key` (`compile.rs:2519-2540`) does not hash the vector document's contents, so a vector edit does not move the hash |
| Any referenced asset has `content_hash: None` | §5.2.1: the input is unidentifiable |
| The compiled frame carries a `CompileDiagnostic` at error severity | Caching a frame the compiler already complained about caches the complaint's consequences |
| `EncoderCapabilities::probe` (`encoder.rs:110`) does not report the profile's encoder | §9: preflight and fail closed, never infer at runtime |

Refusals surface as the strip's "cannot render" state plus one coalesced diagnostic
(§10) — never as a silent green, and never as a silent red the user cannot act on.
Refusal is per chunk, so a zone containing one vector clip still pre-renders the rest.

### 5.7 Storage and the index

Files: `<project>.photon.cache/preview/<sequence>/<format>/<start>-<fold>.<ext>`,
joining proxies, posters, keyframe indices, pts indices, waveforms and thumbnails
under the existing sidecar layout (`media/keyframe_index.rs:41-53`,
`media/proxy.rs:76-80`). For an unsaved project, the OS-temp fallback
`proxy_cache_dir(None)` shape is reused — a chunk is never required for correctness,
exactly as CAP-014 says of proxies (`media/proxy.rs:11-15`).

Index: `<project>.photon.cache/preview/index.json`, one file per project, carrying
`index_version`, the `RenderIdentity`, and per chunk `{ key, tick_count, bytes,
written_unix, last_served_unix }`. It is written atomically and is **rebuildable by
directory scan** — the filename encodes the whole key — so a corrupt index is
recoverable without losing chunks (37's row 6: "corrupt a cached proxy/preview
chunk: detected, deleted, regenerated, no user-visible failure").

### 5.8 Eviction — when a chunk is dropped

Four distinct paths, deliberately not merged:

1. **Explicit.** `EngineCmd::ClearPreview { sequence, range }`, "Remove Preview
   Zone" *with* its "delete rendered chunks?" affordance defaulted to **no**, and
   the K-C5 cache pane's per-category purge. Explicit deletion is immediate.
2. **Staleness does not delete.** A chunk whose fold no longer matches is marked
   stale and shown red; its **file is retained**, because undo may restore validity
   (33 §5, and 33 §2's consequence 2 — the whole point). It becomes the *first*
   eviction candidate, not a deletion.
3. **Budget LRU** against `ProjectVideoSettings::cache_limit_mb`
   (`sequence.rs:96`; `None` = unbounded). Victim order, most-evictable first:
   *(a)* identity/index-version mismatch, *(b)* stale **and** outside every live
   zone, *(c)* valid and outside every live zone, *(d)* stale and inside a zone,
   *(e)* valid and inside a zone — within each class, oldest `last_served_unix`
   first. Never evict a chunk currently open in the serve decoder or being written.
   The bias against evicting inside a live zone is 33 §3.2's requirement made
   orderable.
4. **Thrash honesty.** If, over a rolling window, bytes evicted from class (e) exceed
   bytes written, the strip reports **cache pressure** and the render job **stops**
   rather than looping. 33 §6's "say so rather than silently thrashing", made
   into a stop condition instead of a label: a job that cannot make progress should
   not keep spending GPU and disk to prove it.

### 5.9 A partial-chunk edit — worked

**A chunk is atomic.** There is no sub-chunk patching in v1.

Sequence at 30 fps, `ticks_per_frame = 23_520_000`. Chunk 7 covers ticks
`[4_939_200_000, 5_644_800_000)` = seconds 7.0–8.0 = frames 210–239. The user
trims clip B, whose head sits at frame 224, one frame later.

1. Frames 210–223 compile to identical graphs; their `out_hash` values are unchanged.
2. Frames 224–239 compile with a different `src_time` on B's `DecodeVideo`
   (`ir.rs:185-189`), so 16 of the 30 hashes change.
3. The fold over all 30 changes ⇒ chunk 7's recorded `fold` no longer matches ⇒
   chunk 7 goes **red**. Its file stays on disk (§5.8 rule 2).
4. Chunks 0–6 and 8–N are untouched: their ticks' hashes are bit-identical, so their
   folds match and they stay **green**. This is exact invalidation — the edit
   invalidated precisely one second of timeline.
5. Re-rendering chunk 7 re-renders all 30 frames, including the 14 that did not
   change. That waste is bounded by the chunk length, by construction.
6. Ctrl+Z restores B's trim ⇒ frames 224–239 recompile to the original hashes ⇒
   the fold matches the retained file ⇒ chunk 7 is **green again, with no
   re-render**. 33 acceptance 3, as a consequence rather than a feature.

**Why not patch sub-ranges.** Storing per-tick validity would need either per-frame
files (an index entry and a filesystem inode per frame — at 30 fps, 1800 files per
minute) or a seekable rewrite of an encoded container. The bookkeeping is larger
than the thing it saves, and the failure modes (a partially-rewritten chunk that
decodes) are exactly the invisible-wrong-pixels class this design is trying to
eliminate. One second of re-render is the right trade. Revisit only if measurement
says otherwise.

### 5.10 What invalidation costs

Nothing extra per present. `session.rs:1112` already compiles; the output node's
hash is read from the compiled graph. Chunk validity is re-checked **lazily on
lookup**, not eagerly on `doc_generation` change (33 §5) — but the strip needs a
whole-zone answer to paint. So: on a `doc_revision` change (`session.rs:1013`'s
`poll_snapshot` is the trigger), the preview module re-folds **only the chunks whose
zone intersects the changed range**, on the background worker, publishing results
into the status snapshot. Re-folding a chunk costs 30 compiles, which is the same
work the present loop does in one second of playback, and it is off the engine thread.

---

## 6. Undo unit

**One user verb = one undo unit. Zone edits are document verbs; render, cancel and
clear are cache verbs and produce none.**

### 6.1 The document verbs

| User verb | Command | Exact inverse |
|---|---|---|
| Add Preview Zone | `TimelineCmd::SetPreviewZones { seq, old: Vec<PreviewZone>, new: Vec<PreviewZone> }` | `SetPreviewZones { seq, old: new, new: old }` |
| Remove Preview Zone | same | same |
| Remove All Preview Zones | same, with `new: vec![]` | same |
| Adjust a zone's range (drag its edge) | same, coalesced across the drag gesture | same |

**One command shape, not four.** The reason is §3.1's merge-on-overlap invariant:
adding a zone that touches an existing one *edits* that one, so an
`AddPreviewZone { zone }` / `RemovePreviewZone { zone }` pair in the
`AddMarker`/`RemoveMarker` shape (`commands.rs:660-671,2504-2512`) could not express
its own inverse without capturing the pre-state anyway. Capturing `old` and `new`
vectors is the honest encoding, and it makes `inverse()` a swap — trivially correct,
which is what `crates/photonic-core/tests/timeline.rs`'s undo-identity sweep checks.
The vector is tiny (a zone is 24 bytes; a project with 50 zones is 1.2 KB per
history entry).

**"Remove All" is one undo unit, not N.** This differs from K-C1's rule that a batch
of N background completions is N units ([195 §5](195-k-c1-clip-jobs-framework.md)),
and the distinction is real: K-C1's completions arrive minutes apart from
independent jobs, whereas "Remove All" is a single synchronous verb over known
state. One verb, one unit.

**Edge-drag coalescing** uses the existing gesture machinery, the same as a clip
trim drag; the background render/refold path uses `execute_discrete`
(`crates/photonic-core/src/history/stacks.rs:403`) if it ever needs to commit — it does
not in v1, because nothing about a chunk is document state.

### 6.2 The cache verbs, and the export-isolation rule

**Start Preview Render · Stop · Clear Rendered Chunks · Toggle Preview Playback**
produce **no undo unit at all**, and this is the honest answer rather than an
omission. None of them changes the document. "Undoing" a running render would mean
killing a job, which is *Stop* — a different affordance with its own button, exactly
the argument [195 §5](195-k-c1-clip-jobs-framework.md) makes for jobs. Undoing a
*Clear* would mean re-rendering, which is *Start*. ROADMAP §10 point 4 is satisfied
for these verbs the way it is for `set_loop_range` (`dispatch.rs:2549`, whose schema
already says "Session state only — no undo step", `schema_gen.rs:5755`).

`EngineCmd` gains four variants (`session.rs:183-223`):

```rust
/// K-A1: render preview chunks covering `range` for `sequence` on the
/// background worker. Chunk-aligned outward. Idempotent — already-valid
/// chunks are skipped.
RenderPreview { sequence: SequenceId, range: (Tick, Tick) },
/// Stop the in-flight preview render. Partial chunks are discarded, never indexed.
CancelPreview { sequence: SequenceId },
/// Delete indexed chunks. `None` = the whole sequence.
ClearPreview { sequence: SequenceId, range: Option<(Tick, Tick)> },
/// K-A1: enable serving from preview chunks in this session's present path.
/// **Default false.** The interactive GUI session sets it true; export and
/// chunk-render sessions never do.
SetPreviewChunkServing(bool),
```

`SetPreviewChunkServing` is the §2.3(d) correction. 33 §3.3 asks for the
export-isolation rule to be "asserted in the export loop, not merely documented";
because `run_export_job` drives the same `present()` (`export/job.rs:168-175` →
`session.rs:1051`), an assertion downstream of the render is the wrong place. Making
serving opt-in per session means an export **cannot** be given a chunk, because
nothing in the export path sends the command. The assertion in the export loop
stays as well — belt and braces on a rule whose violation silently degrades a
master — but it is now the second line of defence, not the first. Delta from 33 §3.3;
Follow-up 4.

**Serving, mechanically.** In `present()`, after `compile_with_luts`
(`session.rs:1112-1120`) and before `evaluator.evaluate` (`session.rs:1149-1151`):
if serving is enabled, take the compiled graph's output `content_hash`, look up the
covering chunk, and on a fold-consistent hit decode the frame from the chunk file
through a `ChunkSources` decoder pool — a sibling of `MediaSources` (`session.rs:1394`)
built on the existing `DecodeSource`/`SourceParams` (`decode/scheduler.rs:45-77`)
with a synthetic all-intra `KeyframeIndex` (every frame a keyframe) and
`PtsKind::Cfr(seq.frame_rate)`. On a miss, evaluate normally. **A chunk is an
optimisation and never a correctness dependency** (33 §4).

Serving deliberately happens **outside the IR**: no `IrOp::DecodeChunk` is added, no
hash is perturbed, and PA-1's frame graph is consumed as designed rather than widened.

**Audio is never chunked** (33 §4). The mixer, meters and `graph_latency_samples`
stay live. Chunk serving does not touch `start_playing`/`spawn_audio_feeder`
(`session.rs:1207-1245`) at all.

**Priority.** A running preview render **suspends while the interactive session is
playing** and resumes when it pauses, and its encoder child runs at background
priority via the same `lower_background_priority` the proxy path uses
(`media/proxy.rs`). Suspending is chosen over GPU-level prioritisation because
Photonic has one `GpuContext` and no scheduler that can express "this queue submit
is less important"; claiming priority we cannot enforce would fail acceptance 8 in a
way no test would catch. Playback wins, absolutely.

---

## 7. MCP surface

GUI/MCP parity holds **completely**. There is no exception to record. Every verb is
mechanical, has no filesystem-path argument the user supplies, and is exactly the
kind of long-running work an agent should be able to start and poll.

| Tool | Args | Notes |
|---|---|---|
| `add_preview_zone` | `{ sequence_id?, start, end }` → `{ zone_id, zones }` | One undo unit. Each bound follows the **ticks > tc > seconds precedence** convention that `set_loop_range` established (`schema_gen.rs:5755`). Overlap merges, so the response returns the resulting zone list |
| `remove_preview_zone` | `{ sequence_id?, zone_id }` | One undo unit |
| `clear_preview_zones` | `{ sequence_id? }` | One undo unit ("Remove All") |
| `list_preview_zones` | `{ sequence_id? }` → `[{ zone_id, start, duration, chunks: { rendered, stale, missing, refused } }]` | Read-only; the agent-visible form of the status strip |
| `render_preview` | `{ sequence_id?, range? }` → `{ started, chunks_queued }` | `EngineCmd::RenderPreview`. `range` omitted = every zone. **No undo step** |
| `cancel_preview_render` | `{ sequence_id? }` | `EngineCmd::CancelPreview`. **No undo step** |
| `clear_preview_render` | `{ sequence_id?, range? }` | `EngineCmd::ClearPreview` — deletes chunks, keeps zones. **No undo step** |
| `get_preview_status` | `{ sequence_id? }` → `{ profile, chunk_seconds, total_bytes, budget_mb, pressure, per_chunk: [{ start, state }] }` | The pollable progress surface. `state ∈ {rendered, rendering, stale, missing, refused, unverifiable}` |
| `set_preview_profile` | `{ codec, quality, scale }` | Writes `ProjectVideoSettings::preview_profile`. **One undo unit** — it is a document field. Fails closed when `EncoderCapabilities::probe` lacks the encoder (§9) |

Naming follows the shipped video-tool conventions (`set_proxy_mode`,
`attach_proxy`, `set_loop_range` — `dispatch.rs:2555,2597,2549`); `sequence_id`
optional-defaults-to-active follows `export_sequence` (`dispatch.rs:2622`).

Every failing result carries the full `Diagnostic` in its data payload per
[36 §5](../specs/video-editor/36-error-model.md), so an agent receives
`code`/`subject`/`consequence`, not prose.

**K-H obligation** (26 §16): these tools land **with** the GUI verbs, in the same
change, and `docs/mcp-api.md` regenerates under the existing doc-drift gate.

---

## 8. Acceptance fixtures and tests

**No rights-cleared content is required. K-A1 is not a content- or fixture-gated
item.** Every fixture already exists in
`crates/photonic-video/tests/fixtures/` (README documents the corpus at ~2.5 MiB
against a 5 MB budget): `color_bars.mp4` (4 s, 320×180, 30 fps, ~5 KiB),
`counter.mp4` (10 s, 300 frames, frame-number burn-in, GOP 60) with
`frame_truth.json`, `title_asset.photon`, and
`channel_swap_rgb_to_gbr.cube` for a LUT-in-the-hash case. **Added fixture bytes:
zero.** 23 §7.2's `AssetRightsManifest` gate is not engaged.

ffmpeg-dependent tests use the established skip-with-message convention
(`ffmpeg_locate::locate_for_test`, the `tools_or_skip!` macro at
`crates/photonic-video/tests/export_synthetic.rs:35-49`); GPU tests use
the adapter-skip convention (`graph/cache.rs:159-172`).

| # | Test | Where | Proves |
|---|---|---|---|
| 1 | **Correctness** — render chunks over a 2 s range of `counter.mp4` + a Blur effect, then serve; compare each served frame against a live evaluation of the same tick. Max-abs difference in the display-referred encode domain within the profile tolerance (§9) | `crates/photonic-video/tests/preview_chunks.rs` | 33 acceptance 1 — the test that justifies the feature |
| 2 | **Exact invalidation** — two clips A and B on one track; render; apply a `Grade` to A only; assert chunks covering only B stay valid and chunks covering A go stale, by fold comparison | `photonic-video/src/graph/preview/` unit tests (pure — no GPU, no ffmpeg) | 33 acceptance 2, the differentiator |
| 3 | **Undo restores validity** — render, edit, undo; assert the fold matches the retained file and **no re-render is queued** (assert on the job queue, not on wall time) | `photonic-video/tests/preview_chunks.rs` | §5.9 step 6, 33 acceptance 3 |
| 4 | **Export isolation** — export a range twice, once with a full chunk set present and once with the cache purged; assert byte-identical outputs. Plus a unit assertion that a session created by `run_export_job` has `chunk_serving == false` | `photonic-video/tests/export_synthetic.rs` (beside the existing SS-3 determinism cases) | §6.2, 33 acceptance 4, SS-3 |
| 5 | **Format keying** — switch `active_format` and back; assert zero re-renders and the same chunk files | preview unit tests | 33 acceptance 5 / PA-6 |
| 6 | **Proxy participation** — render under `ProxyMode::ForceProxy`, then request under `ForceOriginal`; assert a miss. Asserted directly on `content_hash` for `IrOp::DecodeVideo` with `proxy: true` vs `false` | `photonic-video/src/graph/compile.rs` unit test + preview unit test | 33 acceptance 6. Already true at `compile.rs:2599`; the test pins it |
| 7 | **Media-identity salt** — same `AssetId`, different `MediaAsset.content_hash`; assert the fold differs and the chunk is not served. Plus: `content_hash: None` ⇒ chunk refused, not written | preview unit tests | §5.2.1 — the §2.3(b) hole |
| 8 | **Vector refusal** — a sequence containing an embedded-vector clip (`title_asset.photon`); assert the chunk is refused with `PreviewChunkSkipped` and the rest of the zone still renders | `photonic-video/tests/preview_chunks.rs` | §5.6 — the §2.3(c) hole |
| 9 | **Renderer identity** — hand-write an index whose `shader_digest` differs; assert every chunk reports `unverifiable` (missing), never `rendered` | preview unit tests | §5.3 |
| 10 | **Budget and eviction order** — 40 synthetic index entries across the five victim classes with `cache_limit_mb = 1`; assert the eviction order is exactly (a)…(e) and that no in-zone valid chunk is dropped while an out-of-zone one remains | preview unit tests (pure) | §5.8, 33 acceptance 7 |
| 11 | **Serde** — a v5 doc with `preview_zones` + `preview_profile` round-trips; a v5 doc without them loads empty/`None` and re-serializes without the keys; `CURRENT_FORMAT_VERSION` is still 5; unknown-field preservation still holds | `photonic-core/tests/timeline.rs`, `tests/forward_compat.rs` | §4 |
| 12 | **Undo identity** — Add/Remove/Remove-All/edge-drag each produce exactly one `SetPreviewZones`; `inverse()` restores the exact prior `Vec`; the zone-merge invariant holds after undo and redo | `photonic-core/tests/timeline.rs` | §6.1 |
| 13 | **Cancellation leaves no partial chunk** — cancel mid-render; assert neither the staging path nor the final path exists and the index has no entry | `photonic-video/tests/preview_chunks.rs` | 33 acceptance 9, 37 §2.3 |
| 14 | **Corrupt chunk** — truncate a chunk file; assert it is detected, deleted, reported as missing, and re-rendered with no user-visible failure | `photonic-video/tests/preview_chunks.rs` | [37 §5](../specs/video-editor/37-robustness.md) row 6 |
| 15 | **Priority** — with a preview job queued, start playback and assert (a) the job reports suspended and (b) `EngineStatus::dropped` over a fixed frame budget is not worse than the same playback with no job. Marked a **trend metric**, not a hard gate | `photonic-video/tests/playback_soak.rs` | 33 acceptance 8, and ROADMAP §10 point 7's two-tier rule |
| 16 | **Audio unaffected** — mixer output and `master_level` are bit-identical with and without chunk serving over the same range | `photonic-video/tests/ss3_sync_drift.rs` | 33 acceptance 10 |
| 17 | **MCP end-to-end** — `add_preview_zone` → `render_preview` → `get_preview_status` → `cancel_preview_render` → `clear_preview_zones`; assert one undo step for the zone verbs and zero for the render verbs | `photonic-mcp/src/handlers/video.rs` tests, beside the existing job tests | §7 |
| 18 | **GUI path** — the strip renders, the commands are reachable and rebindable, and the hit-target/keyboard-gate lints stay green | `photonic-gui/tests/video_ui_paths.rs`, `hit_target_lint.rs`, `keyboard_gate_lint.rs` | §11 / ROADMAP §10 point 2 |

Note for the implementer, in the shape [195 §8](195-k-c1-clip-jobs-framework.md)
flags: `crates/photonic-core/tests/diag_catalogue.rs` holds a deliberately frozen
`EXPECTED_WIRE_CODES` list. The three new codes (§10) must be added in the same
change or the gate trips — which is the gate working.

**Test 1's tolerance must be a number in the source, not a sentiment.** Initial
budgets, to be tightened by measurement and never loosened: `IntraH264` ≤ 2/255
max-abs per channel in the encoded Rec.709 limited-range domain; `IntraMezzanine`
≤ 1/255. The comparison happens in the encode domain, not linear, because the
round-trip is linear → `working_pixel_to_yuv_codes` (`export/convert.rs:108`) →
encode → decode → `YuvConverter` back to linear, and stating a tolerance in linear
light would be stating it in the wrong space for a display-referred codec.

---

## 9. The codec decision and the 23 §10.3 gate

26 §7 requires a per-item clean-room note; ROADMAP §14 additionally puts **K-A1's
preview-chunk codecs** under 23 §10.3's patent-and-distribution record, alongside
K-F5's hardware encoders. Addressed directly:

**Decision: preview chunks use only encoders Photonic already ships.**

| Profile | Encoder | Already in the build? |
|---|---|---|
| `IntraH264` (**default**) | `EncoderCapabilities::h264_encoder()` (`export/encoder.rs:148`) at `-g 1`, CRF 12, `yuv420p` | Yes — `VideoCodec::H264` (`presets.rs:61`) ships in three export presets, and `media/proxy.rs` already generates all-intra H.264 proxies (`proxy.rs:1-15`, `-g 1`, baseline) |
| `IntraMezzanine` | `prores_ks` | Yes — `VideoCodec::ProResLikeMezzanine` (`presets.rs:65`, preset at `presets.rs:313`) |

All-intra is not a stylistic choice: seeking *within* a chunk must be free, which is
the same reason the proxy path already uses `-g 1` (`proxy.rs:1-8`) and the same
reason §6.2's serve decoder can use a synthetic all-keyframe `KeyframeIndex`.

**Consequences for the gate, stated so a legal reviewer can act on them:**

1. **No new codec, container, encoder binary or pixel-format combination enters the
   distribution surface.** The 23 §10.3 record is therefore an **amendment** of the
   existing H.264 and ProRes-like rows — adding the preview-chunk
   codec/container/pix_fmt combination and the `-g 1` intra configuration — not a
   new freedom-to-operate analysis. That is the entire reason `PreviewCodec::Lossless`
   from 33 §3.3 is dropped: FFV1/UT Video would be a third encoder, requiring a new
   row, a new configuration record and a new SBOM entry, to serve a preview-only
   path that `IntraMezzanine` already covers visually.
2. **Trademark wording** for ProRes stays as 23 §10.3 requires: the enum is named
   `IntraMezzanine`, the UI string is "Mezzanine (visually lossless)", and the
   product never claims ProRes compatibility or certification.
3. **Availability is preflighted and fails closed, never inferred.**
   `EncoderCapabilities::probe` (`encoder.rs:110`) runs before any chunk render and
   before `set_preview_profile` accepts a profile; a missing encoder is
   `ExportEncoderUnavailable`-class refusal, exactly as 23 §10.1 requires and as
   ROADMAP §14 restates for this item.
4. **No 10-bit or HDR chunk path in v1.** D-13 owns that route; a preview chunk in a
   10-bit pipeline is a D-13 follow-up, not a K-A1 scope item.

**Net: K-A1 carries the 23 §10.3 obligation, and this decision reduces it to a
one-paragraph amendment.** It remains a real gate — the amendment must be recorded
before release — but it does not block implementation.

---

## 10. Diagnostics

Three new codes in a new `Preview` family (`crates/photonic-core/src/diag.rs:142-163`
documents ten families; this makes eleven — the same widening
[195 §11](195-k-c1-clip-jobs-framework.md) risk 2 flags, and the same two-file
change plus doc amendment):

| Code | Raised when | `subject` | Remedy |
|---|---|---|---|
| `PreviewChunkSkipped` | §5.6 refusal (vector, unhashed media, compile error, missing encoder) | `Subject::Sequence` (`diag.rs:83`) + the chunk start in `detail` | `Remedy::None` (`diag.rs:137`) for vector/media; `Remedy::OpenSettings` (`diag.rs:131`) at the profile picker for a missing encoder |
| `PreviewChunkCorrupt` | A chunk fails to decode or its length disagrees with the index | `Subject::Sequence` | `Remedy::Retry` (`diag.rs:133`) — deleted and re-rendered automatically (test 14) |
| `PreviewCachePressure` | §5.8 rule 4's stop condition | `Subject::Sequence` | `Remedy::OpenSettings` — raise `cache_limit_mb`, shrink the zone, or use `PreviewScale::Half` |

All coalesced on `(code, subject)` through `DiagnosticLog`, so a 200-chunk zone with
one vector clip produces one toast, not 200. Technical detail (chunk start, refusal
reason, encoder stderr tail) goes in `detail`, never in `message`
([36 §4.2](../specs/video-editor/36-error-model.md)).

---

## 11. UI

33 §6 owns the design; two things need saying here because they are gated by tests.

**The strip is a strip of tinted bars, not filled rows.** 33 §6's red/yellow/green
table meets a real constraint: `DESIGN.md:170-171` says `error`/`warning` are "used
only for text/icon tint, never as a fill", and the Components section says of status
rows "do not fill entire rows with status color". A chunk strip is not a row — it is
a **data-coding graphic**, the same category DESIGN.md already carved out for
node-editor port sockets ("functional data-coding (like status tints), not chrome
accent — documented so it isn't flagged as an accent-rule violation",
`DESIGN.md:199`). So: a 4px-tall lane immediately below the ruler
(`crates/photonic-gui/src/app/timeline/ruler.rs:92`), aligned to the same time axis,
each chunk a discrete segment tinted with an existing token — `error` (missing),
`warning` (rendering), `success` (rendered), `secondary` (refused/unverifiable),
nothing (outside a zone) — and this exemption is recorded in DESIGN.md's
data-coding paragraph in the same change. **No new colour token is proposed**, which
matters because `crates/photonic-gui/tests/design_contrast.rs` asserts every
`colors:` token appears as a foreground in the `contrast` block or in a named
`exempt:` row, so a new token is a three-place change and a WCAG obligation.
Reusing `success`/`warning`/`error` costs one contrast row for the new
graphic-role pairing (3:1 threshold) and nothing else.

**Wording.** The command is **Start Preview Render**, its tooltip says "speeds up
*playback* of rendered regions; it does not speed up editing", and the panel never
uses the word "render" unqualified where it could be confused with export. 33 §1
makes this a requirement because users of the reference product misunderstand it
persistently.

Commands land in `crates/photonic-gui/src/commands.rs` and are rebindable
(K-G-class shortcut machinery), routed through `app/command_center.rs`, and mirrored
in MCP per §7.

---

## 12. Risks, open questions, and deliberate exclusions

### Deliberately out of scope

- **Sub-chunk patching.** §5.9 argues it; the bookkeeping exceeds the saving and
  introduces a partially-valid-file failure mode.
- **Audio chunking.** 33 §4. Audio is cheap relative to video and the mixer must
  stay live for meters, ducking and `graph_latency_samples` to mean anything.
- **Chunks for `PreviewTarget::Asset`** (the source-monitor peek path,
  `session.rs:1088-1107`). A single-decode graph is not the cost problem.
- **Automatic idle rendering.** v1 renders only on explicit request. An "auto-render
  when idle" preference is a follow-up whose real question is a power/thermal policy,
  not a caching one.
- **Sharing chunks across machines or over a network.** §5.3's renderer identity
  makes cross-machine reuse unverifiable today; the feature would need 32 §8 closed
  first and is a separate item.
- **Rendering chunks for a *nested* sequence independently of its parent.** The
  parent's hash already folds the nested subgraph, so the parent's chunks cover it.
  Separate nested-sequence chunks would be a second cache with a second invalidation
  story for no additional user outcome.
- **10-bit / HDR chunks.** §9 item 4; D-13 owns it.

### Risks

1. **The hash is not yet complete for every op.** §2.3(c) is the known case
   (`RasterVector`), handled by refusal. The risk is a *future* op landing with an
   incomplete hash and silently becoming cacheable. Mitigation: a
   `chunkable(op) -> bool` function that is **exhaustively matched** over `IrOp`
   with no wildcard arm, so adding a variant to `ir.rs:180-283` fails to compile
   until someone decides. This is the same discipline `threading_for_op`
   (`ir.rs:133-162`) and `source_range_for_op` (`source_range.rs:79-107`) already use
   — both are exhaustive today, and that is not an accident.
2. **Fold collision presents a stale frame.** 128 bits, never truncated, order- and
   count-sensitive, salted with a domain-separation string. This is the residual and
   it is accepted at the same level the `NodeCache` already accepts it
   (`ir.rs:36-38`: "collisions are a non-concern at cache scale").
3. **Cache growth surprises the user.** A minute of 1080p all-intra H.264 at CRF 12
   is order-of-hundreds of MB. `cache_limit_mb` defaults to `None` = unbounded
   (`sequence.rs:96,116`). Mitigation: K-A1 ships with the K-C5 cache pane gaining a
   `preview` category (`media/cache_stats.rs:38-62` currently lists proxies, posters,
   keyframes, waveforms, other) and the render dialog showing an **estimated size
   before starting**, computed from zone duration × measured bytes/second. Do not
   change the `None` default in this item — that is a product call, and silently
   capping an existing project's cache would be a surprise of a different kind.
4. **Suspend-on-play makes rendering feel slow** to a user who plays constantly.
   Accepted: playback correctness beats render throughput, and the strip shows
   *why* progress stopped. Revisit only with a real GPU priority mechanism.
5. **Scope creep into "render the whole timeline".** A zone is user intent. An
   implicit "everything is a zone" default would make the cache unbounded by
   construction and turn every edit into a background render storm. Reviewers should
   reject it.

### Open questions needing a product call

1. **Should removing a preview zone delete its rendered chunks?**
   *Recommendation: no, by default, with a checkbox in the confirm dialog.* Zones are
   cheap to re-add and chunks are expensive to re-render; deleting on zone removal
   makes an undoable verb destroy non-undoable work. The counter-argument — cache
   sprawl — is answered by §5.8's LRU, which evicts out-of-zone chunks first anyway.
2. **Should `PreviewScale::Half` be the default rather than `Full`?**
   *Recommendation: `Full`.* 33 §3.3 says full resolution, and half-scale re-creates
   exactly the ambiguity PA-15 warns about — a user who cannot tell whether what they
   are watching is the real composite. Half remains available for people with small
   disks, one click away, and it is in the key so switching is free.
3. **Should the strip live under the ruler, or in the ruler?**
   *Recommendation: a separate 4px lane immediately below*, because `draw_ruler`
   (`ruler.rs:92`) already owns marker diamonds, labels and the playhead, and adding
   a fourth semantic layer to that one function is how a 300-line draw function
   becomes a 500-line one. This is a UX call with an engineering consequence, so it
   is recorded rather than assumed.
4. **Does "Start Preview Render" render every zone, or only the zone under the
   playhead?** *Recommendation: every zone, in playhead-outward order*, so the region
   the user is looking at goes green first while the rest fills in. The alternative
   needs a second verb; the ordering gives the same felt behaviour with one.

---

## 13. Clean-room provenance

Per [26 §2](../specs/video-editor/26-kdenlive-mlt-parity.md#2-clean-room-and-licensing-fence)
item 2 and 26 §7's per-item requirement:

- **What was read.** Kdenlive's user-facing documentation (`docs.kdenlive.org`,
  `CC-BY-SA-4.0`) for the *existence and shape* of timeline preview rendering: that
  it exists, that it uses fixed-size chunks, that it shows a red/yellow/green status
  strip above the tracks, that multiple non-contiguous preview zones are supported,
  that editing over a rendered chunk reverts it, and — explicitly — that its manual
  states this speeds up playback and not editing. That is a **requirements source**
  under 26 §2 item 1: cited, never pasted. FFmpeg's published CLI documentation for
  intra-only encoding options; FFmpeg is invoked across a process boundary as an
  external program, the model Photonic already ships (`export/encoder.rs`,
  `media/proxy.rs`), introducing no linkage question.
- **What was not read.** The Kdenlive source tree, the MLT source tree, and any
  GPL/LGPL derivative. No symbol, constant, chunk size, file-naming scheme, control
  flow or test was taken from either. In particular the reference's 25-frame chunk
  size is **not** adopted — §5.1 uses one second of sequence time for a stated,
  independent reason (PA-8's exact rational rates make a fixed frame count
  rate-dependent, which would be a regression against a protected property). The
  implementer records the
  [23 §3.4](../specs/video-editor/23-legal-open-source-implementation-routes.md#34-clean-room-protocol)
  attestation for this subsystem, and an independent provenance reviewer checks
  identifiers, comments, constants and test provenance before merge.
- **Where the design actually comes from.** The invalidation model is derived
  entirely from Photonic's own `ContentHash` (`graph/ir.rs:38`,
  `graph/compile.rs:2568`) and its own E-1 source-range contract
  (`graph/source_range.rs`), neither of which has an analogue in the reference
  engine — 26 PA-1 records that MLT has no graph object and therefore cannot answer
  "what does frame N depend on". The storage layout, atomic-write discipline,
  cancellation shape and LRU come from Photonic's own shipped proxy/poster/keyframe
  sidecar machinery (`media/proxy.rs`, `media/keyframe_index.rs`,
  `media/atomic_write.rs`, `playback/prefetch.rs:116-132`). The render path is
  Photonic's own `run_export_job` (`export/job.rs:152`) with a different sink.
- **Bundled bytes: none.** No asset ships with this item, so 23 §7.2's
  `AssetRightsManifest` gate is not engaged and K-A1 is **not** a
  legal-or-fixture-blocked item.
- **No new dependency.** Nothing in 26 §2's reject list, directly or transitively.
  Everything needed (`xxhash-rust`, `serde_json`, the existing ffmpeg boundary,
  `wgpu`) is already in the build.
- **One real gate remains:** §9's 23 §10.3 codec/patent/distribution record, reduced
  by the encoder decision to an amendment of existing rows.

---

## 14. Definition of done → ROADMAP §10

| # | ROADMAP §10 point | Answered by |
|---|---|---|
| 1 | Core op/engine service with unit tests | `photonic-video/src/graph/preview/` (key, fold, index, eviction — all pure and testable without GPU or ffmpeg) plus the render/serve integration; §8 tests 1–10, 13, 14 |
| 2 | GUI route, or a recorded exception | Chunk status strip below the ruler + Start/Stop/Clear/Add Zone/Remove Zone/Remove All in `commands.rs`, rebindable. **Recorded exception: none** |
| 3 | MCP tool/schema/generated docs | §7's nine tools; `docs/mcp-api.md` regenerated under the drift gate. **Recorded exception: none** — parity is complete |
| 4 | One user verb = one undo unit | §6.1: zone verbs produce exactly one `SetPreviewZones` with a swap inverse; §6.2: render/cancel/clear are session verbs with no undo step, on the `set_loop_range` precedent. Test 12 |
| 5 | Additive serde/migration round-trip | §4: stays v5; `preview_zones` and `preview_profile` additive. Test 11 |
| 6 | IR/eval/golden/sync coverage for new pixel/audio paths | **No new pixel path** — chunks reuse `compile` → `eval` → `convert` → `encoder` unchanged, and serving reuses `DecodeSource` + `YuvConverter`. The new *surface* is the round-trip, covered by test 1's tolerance golden and test 16's audio invariance |
| 7 | Hard gates green; trend metrics not regressed | Hard: export determinism (test 4), cache invariants (tests 2, 5–10), serde (11), undo (12). Trend: test 15's playback-under-render, explicitly a trend metric per ROADMAP §10 point 7's two-tier rule and the recorded soak sensitivity of this machine |
| 8 | Offline, privacy, licensing, content, product gates | §13: no bundled bytes, no new dependency, no network. §9: the one real gate is the 23 §10.3 amendment. Chunks contain rendered frames of the user's own media and never leave the sidecar cache |
| 9 | No protected-surface regression | PA-1 **consumed as designed** — no `IrOp` added, no hash perturbed, serving lives outside the IR (§6.2). PA-7 protected by choosing `{start, duration}` over 33's `{start, end}` (§3.1). PA-8 protected by a one-second rather than 25-frame chunk (§5.1). PA-6 consumed via per-format keying (§5.1). PA-15 protected by refusing to blur the Draft/proxy/preview-render distinction (§2.3(a), §5.5) |
| 10 | Goal-backward L1–L4, incl. GUI/MCP parity | §1's six outcomes are the L4 script; §7's parity is complete with no exception |

---

## Follow-ups (other documents that need a change — **not** made here)

1. **[33 §3.1](../specs/video-editor/33-timeline-preview-render.md)** —
   `PreviewZone { start, end }` should become `{ id, start, duration }`. PA-7 records
   half-open start+duration as a protected property, and every range in the shipped
   model (`Clip`, `Marker.duration` at `sequence.rs:836`) already uses it. §3.1 above.
2. **33 §3.3** — drop `PreviewCodec::Lossless`; the two-profile set
   (`IntraH264`, `IntraMezzanine`) maps onto encoders already in the build and keeps
   the 23 §10.3 record an amendment rather than a new analysis. §9 above.
3. **33 §3.1** — `ChunkKey` needs a `profile` component and the index needs a
   `RenderIdentity`. Without the first, changing the profile silently reuses
   old-profile chunks and a `Half` chunk can be served as full resolution (because
   the content hash excludes the eval canvas, `eval.rs:465-471`); without the second,
   a project opened on another GPU serves unverifiable pixels. §5.1, §5.3.
4. **33 §3.3 / §4** — the export-isolation rule should be stated as *"chunk serving
   is opt-in per session and off by default"*, not as an assertion in the export
   loop. `run_export_job` opens a real `EngineSession` and pulls through the same
   `present()` (`export/job.rs:168-175`), so a downstream assertion is the wrong
   layer. §6.2.
5. **33 §5** — add the two hash-incompleteness refusals as normative: `RasterVector`
   (because `vector_state_key` does not hash the vector document,
   `compile.rs:2519-2540`) and `MediaAsset.content_hash: None`. 33 §5 currently says
   proxy state must participate in the hash — it already does
   (`compile.rs:2599`); the *media bytes* do not, and that is the gap that matters
   for a disk-persistent cache. §2.3(b), §5.2.1, §5.6.
6. **26 §9 K-A1** names `EngineCmd::{RenderPreviewRange, ClearPreview}` while 33 §4
   names `{RenderPreview, CancelPreview, ClearPreview}`. 33 is the owner doc and its
   naming is adopted here, plus `SetPreviewChunkServing`; 26's Files row should be
   updated to match so a future implementer does not add both.
7. **[DESIGN.md](../../DESIGN.md)** — the data-coding paragraph (`DESIGN.md:199`,
   node-editor port sockets) should gain the timeline chunk strip as a second named
   instance of "functional data-coding, not chrome accent", so the strip is not
   flagged against the "status colours are tint, never fill" rule at
   `DESIGN.md:170-171`. The `contrast` block gains graphic-role rows for
   `success`/`warning`/`error` against the timeline ruler background. No new token.
8. **[36-error-model.md](../specs/video-editor/36-error-model.md)** §3.2's family
   table needs a `Preview` row (`PreviewChunkSkipped`, `PreviewChunkCorrupt`,
   `PreviewCachePressure`), and `diag.rs:140`'s "the ten error families" doc comment
   becomes eleven — the same amendment [195](195-k-c1-clip-jobs-framework.md)'s
   follow-up 2 raises for `Job`. If both land, it is twelve; whichever lands second
   must re-read the comment rather than assume.
9. **[02-engine.md](../specs/video-editor/02-engine.md)** §1's crate module map
   should list `photonic-video/src/graph/preview/`, and §5's cache table should show
   the disk chunk cache as a sibling of the node-result cache with a different
   lifetime and a different key.
10. **[K-C5](../specs/video-editor/26-kdenlive-mlt-parity.md#k-c5--project-archiving-and-cache-management)**
    — `media/cache_stats.rs:38-62`'s category list must gain `preview`; 26 §11 K-C5
    already anticipates this ("…and K-A1's preview chunks"), so this is landing the
    text that exists rather than new scope.
11. **ROADMAP.md** §2/§14 — record that K-A1's 23 §10.3 obligation is discharged as
    an amendment to the existing H.264 / ProRes-like rows once §9's decision is
    accepted, so the item does not read as blocked on a fresh codec analysis.
