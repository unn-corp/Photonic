# 33 — Chunked Timeline Preview Rendering

**Status:** Draft — implementation contract; no code authorization
**Date:** 2026-07-20
**Audience:** engine maintainers, timeline panel owner

**Depends on:** [02-engine.md](02-engine.md) (`ContentHash`, `NodeCache`, `EngineCmd`), [24-preview-media-load.md](24-preview-media-load.md) (Draft/Full tiers, time-to-paint), [25-performance.md](25-performance.md), [26-kdenlive-mlt-parity.md](26-kdenlive-mlt-parity.md), [32-engine-contracts.md](32-engine-contracts.md).

**Owns:** [26 K-A1](26-kdenlive-mlt-parity.md#k-a1--chunked-timeline-preview-rendering) in full.

---

## 1. What this is, and what it is not

Pre-render marked regions of the timeline to disk so **playback** of heavy sections is always realtime. Heavy means stacked effects, nested sequences, vector rasterization, or high-resolution composites — anywhere the per-frame evaluation budget ([02 §8](02-engine.md#8-perf-budgets-verified-in-11): 8 ms GPU for 1080p, 3 layers, grade and captions) is exceeded.

**It does not speed up editing.** The reference's manual is explicit about this and users still misunderstand it, so the UI must not imply otherwise. It speeds up *playback of already-rendered regions*. An edit invalidates what it touches.

This is distinct from proxies (decode cost) and from Draft preview scaling (render cost at reduced resolution). Preview rendering is **full-quality render cost, paid once, in advance**. All three are orthogonal and all three should exist — Photonic already has the other two ([26 PA-15](26-kdenlive-mlt-parity.md#5-photonic-ahead-register-pa---do-not-port-backwards)).

---

## 2. Why Photonic's version is structurally better

The reference must invalidate by **time range**, because MLT has no dependency information — there is no graph object, only a transient per-frame stack of callbacks, so it cannot answer "what does frame N depend on". It therefore over-invalidates and cannot tell whether an edit actually changed a given frame.

Photonic's `IrNode` already carries `ContentHash(u128) = hash(op, resolved params, input hashes)`, and the sequence output hash for a tick is a **complete** description of what that frame will be. So:

> **A chunk is valid iff, for every tick it covers, the sequence-output content hash equals the hash recorded when the chunk was rendered.**

Three consequences fall out for free:

1. **Exact invalidation.** An edit that provably cannot change a frame does not invalidate it. Colour-grading clip A does not invalidate a chunk covering only clip B.
2. **Undo restores validity.** Undoing an edit restores the prior hashes, so the chunk is valid again. The reference implements "smart preview undo/redo" as a special case; here it is a consequence.
3. **Chunks survive project moves.** Hash-keyed storage in the existing sidecar cache, like proxies, posters and waveforms.

---

## 3. Model

### 3.1 Zones and chunks

```rust
pub struct PreviewZone { pub start: Tick, pub end: Tick }   // user intent, in the document
```

Zones are user-declared regions to keep rendered. Multiple, non-contiguous, undoable. They are **document state**; chunks are **cache state** and never enter the document.

```rust
pub struct ChunkKey {
    pub sequence: SequenceId,
    pub format: usize,          // active SequenceFormat index — per-format chunks
    pub start: Tick,            // chunk-aligned
    pub hash: ContentHash,      // fold of per-tick sequence-output hashes
}
```

**Chunk length:** one second of sequence time, frame-aligned, floor-aligned from zero so chunk boundaries are stable under trim. The reference uses 25 frames; one second is the same idea expressed rate-independently, which matters because Photonic supports exact rational rates.

**The hash is a fold over the chunk's ticks.** Storing per-tick hashes would be exact but large; folding is exact for validity (any tick change alters the fold) and compact. A fold collision would present a stale frame — use the existing 128-bit hash and do not truncate.

### 3.2 Storage

`<project>.photon.cache/preview/<sequence>/<format>/<start>-<hash>.<ext>`, joining proxies, posters, keyframe indices, waveforms and thumbnails under the existing sidecar layout and its `cache_limit_mb` budget. Eviction is LRU across the whole cache, biased against evicting chunks inside a live zone.

### 3.3 Codec

A **preview profile**, distinct from any export preset:

```rust
pub struct PreviewProfile { pub codec: PreviewCodec, pub quality: u8, pub scale: PreviewScale }
pub enum PreviewCodec { IntraH264, IntraProResLike, Lossless }
pub enum PreviewScale { Full, Half }
```

Default: all-intra H.264 at high quality, full resolution — all-intra because seeking within a chunk must be free, which is the same reason `media/proxy.rs` already generates `-g 1`.

**Hard rule: chunk output must never reach an export.** Export always re-evaluates from originals ([02 §6](02-engine.md)). A preview chunk is lossy by construction and rendering from it would silently degrade a master. This must be asserted in the export loop, not merely documented.

---

## 4. Engine integration

```rust
EngineCmd::RenderPreview { sequence: SequenceId, range: (Tick, Tick) }
EngineCmd::CancelPreview { sequence: SequenceId }
EngineCmd::ClearPreview  { sequence: SequenceId, range: Option<(Tick, Tick)> }
```

Rendering is a **background job** on the existing worker pool, reusing the export render loop rather than adding a second render path — same compile, same eval, same convert, different sink. It shares [26 K-F1](26-kdenlive-mlt-parity.md#k-f1--gui-render-queue)'s job queue and is inspectable there.

**Serving.** In the present path, before graph evaluation: compute the sequence-output hash for the tick, look up the covering chunk, and if the recorded hash matches, decode from the chunk instead of evaluating. Cost is one hash computation (already performed) plus a lookup. On a miss, evaluate normally — a chunk is an optimisation and never a correctness dependency.

**Audio is never chunked.** It is cheap relative to video and mixing must stay live for the mixer and meters to work. The reference does the same.

**Priority.** Preview rendering runs at lower priority than interactive playback and must not compete for decode or GPU with the active playhead. If the user plays into an unrendered region, playback wins.

---

## 5. Invalidation

Hash-natural, per §2. Concretely:

- On `doc_generation` change, the affected sequence's chunk index is re-checked lazily — on next lookup, not eagerly.
- A chunk whose recorded hash no longer matches is marked stale and shown red; its file is not deleted immediately (undo may restore validity) but is eligible for LRU eviction.
- Changing the active `SequenceFormat` selects a different chunk set rather than invalidating — per-format keying makes multi-format work ([26 K-F3](26-kdenlive-mlt-parity.md#k-f3--multi-format-render)) cheap here too.
- Relinking an asset or swapping a proxy changes the source hash and therefore the dependent chunks. Proxy state **must** participate in the hash, or a chunk rendered from a proxy could be served as full quality.

---

## 6. UI

A **chunk status strip** immediately below the ruler in the timeline panel, aligned to the same time axis:

| State | Colour | Meaning |
|---|---|---|
| Not rendered | red | Inside a zone, no valid chunk |
| Rendering | yellow | Job in flight |
| Rendered | green | Valid chunk |
| Outside zone | no marking | Not requested |

Commands: **Add Preview Zone** (from the timeline in/out zone) · **Remove Preview Zone** · **Remove All** · **Start Preview Render** · **Stop**. Bound in `commands.rs` and rebindable, mirrored in MCP per [26 K-H](26-kdenlive-mlt-parity.md#16-k-h--mcp-trail).

Colours come from `DESIGN.md` tokens. If a "rendering/pending" state colour is not already in the token set it is a **DESIGN.md addition**, declared as such rather than invented locally ([13 §0](13-ux-components.md)'s rule).

The strip must show cache pressure honestly: if chunks are being evicted faster than they are rendered, say so rather than silently thrashing.

---

## 7. Acceptance

1. **Correctness** — a served chunk is pixel-identical to live evaluation of the same tick, within the preview codec's declared tolerance. This is the test that justifies the feature.
2. **Exact invalidation** — editing clip A does **not** invalidate a chunk covering only clip B. The differentiator against the reference; if this fails, the feature is merely a cache.
3. **Undo restores validity** — render, edit, undo: the chunk is green again with no re-render.
4. **Export isolation** — a full-quality export over a fully-rendered range is byte-identical to one with no chunks present (SS-3).
5. **Format keying** — switching `SequenceFormat` and back does not re-render.
6. **Proxy participation** — a chunk rendered under `ForceProxy` is not served under `ForceOriginal`.
7. **Budget** — chunk storage respects `cache_limit_mb`; eviction prefers chunks outside live zones.
8. **Priority** — playback into an unrendered region is not degraded by a running preview job.
9. **Cancellation** — stopping mid-render leaves no partial chunk in the index.
10. **Audio** — unaffected; mixer and meters live throughout.

---

## 8. Sequencing and effort

**Effort: L**, and this is the largest single item in [26 K-Band 5](26-kdenlive-mlt-parity.md#19-priority-and-dependencies).

| Step | Work |
|---|---|
| 1 | Chunk key, hash fold, index, sidecar storage — no rendering yet, verifiable in isolation |
| 2 | Render path reusing the export loop; job queue integration |
| 3 | Serve path in the present loop, behind a flag; acceptance tests 1 and 4 |
| 4 | Zones as document state + undo; UI strip |
| 5 | Invalidation, eviction, budget; tests 2, 3, 7 |
| 6 | MCP parity |

**Prerequisites.** [26 K-0.2](26-kdenlive-mlt-parity.md#8-k-0--foundations) (effects actually render — chunking passthrough effects is pointless) and ideally [32 §8](32-engine-contracts.md) (CPU/GPU equivalence, so "pixel-identical" is a meaningful claim). Not blocked by [26 K-0.1](26-kdenlive-mlt-parity.md#8-k-0--foundations), since this uses the render loop directly rather than `EngineCmd::Export`.
