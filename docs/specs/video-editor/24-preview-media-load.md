# 24 — Preview & Media Load Contract

**Status:** Accepted product/engine contract  
**Date:** 2026-07-19  
**Depends on:** [01-data-model.md](01-data-model.md), [02-engine.md](02-engine.md), [04-ui-mode-timeline.md](04-ui-mode-timeline.md), [05-import-export.md](05-import-export.md), [SPEC.md](SPEC.md) CAP-001 / CAP-004 / CAP-014  
**Owns:** single context-driven monitor retargeting; import readiness ladder; preview quality tiers; time-to-first-paint and scrub performance contracts; thread ownership for load/preview work.  
**Does not own:** export encode loop (02 §7), color math (03), timeline edit grammar (04/16/19), full proxy generation formats beyond policy pointer (02 §6, 05).

## 1. Product decisions (normative)

| ID | Decision |
|---|---|
| D-PM-1 | **One monitor.** Video mode uses a single central preview surface (the existing canvas → program monitor path in 04 §3). There is no permanent dual source/program layout. |
| D-PM-2 | **Context replaces content.** What the monitor shows is a pure function of focus + transport state (§3). Selection changes retarget the preview; they do not open a second viewer. |
| D-PM-3 | **Speed over dual-viewer parity.** Time-to-usable import and time-to-smooth scrub/play outrank Premiere-style dual monitors. G-10 source marks and source audition remain in scope as **session state + same-surface retarget**, not a second always-on decode path. |
| D-PM-4 | **Paint something immediately.** Any user action that expects a picture must show a cached poster, last frame, or placeholder within the budgets in §6 before exact-frame decode completes. Never flash black when a prior frame exists (04 §3.1). |
| D-PM-5 | **Draft by default.** Interactive preview (scrub, play, selection retarget) uses the **Draft** quality tier (§4). **Full** is opt-in or export-only. Export always uses originals at full quality (02 §6–7, CAP-014). |
| D-PM-6 | **Import never blocks the UI thread.** CAP-001 registration is synchronous and cheap; probe, hash, keyframe index, poster, thumbs, waveforms, and proxies are background jobs (05 §1.4 extended by §2). |

These decisions amend the *presentation* half of G-10 in [20 §4](20-pro-workflows.md#4-g-10--source-monitor-and-true-source-marks): dual source/program panes are **not** required. Source In/Out marks, Match Frame, and Insert/Overwrite handoff remain; they operate against the single monitor’s current `PreviewTarget` (§3).

---

## 2. Import readiness ladder (CAP-001)

Extends 05 §1.4. Stages are ordered by user-visible value; later stages never gate earlier ones.

```
drop / import
    │
    ▼
L0 Register asset row          ── UI: row visible, spinner; placeable on timeline
    │
    ├──▶ L1 Content hash       ── cache identity, relink key
    ├──▶ L2 Probe              ── duration, size, fps, streams (pool metadata)
    ├──▶ L3 Poster frame       ── bin thumb + instant monitor paint
    ├──▶ L4 Keyframe index     ── fast scrub seeks (video)
    ├──▶ L5 Waveform pyramid   ── audio lane paint (audio / A+V)
    ├──▶ L6 Thumbnail strip    ── timeline strip samples (lazy / visible-range)
    └──▶ L7 Proxy (policy)     ── Draft edit path when ready (never required)
```

### 2.1 Stage contracts

| Stage | Produces | Blocks UI? | Blocks place-on-timeline? | Blocks Draft play? | Blocks fast scrub? |
|---|---|---|---|---|---|
| L0 Register | `MediaAsset` id, kind-from-ext, path | **No** | **No** | Yes (no media yet) | Yes |
| L1 Hash | `content_hash` | No | No | No | No |
| L2 Probe | `MediaProbe` | No | No | Soft (duration unknown → treat as still/unknown) | Partial |
| L3 Poster | One still in sidecar + GPU-friendly cache entry | No | No | No — monitor may show poster | No |
| L4 Keyframe index | Sidecar keyframe table | No | No | No (slow seeks OK) | **Yes for budget** until ready |
| L5 Waveform | Peak pyramid | No | No | No | No |
| L6 Thumbs strip | On-demand / visible-range samples | No | No | No | No |
| L7 Proxy | Proxy file + `MediaAsset.proxy` | No | No | No — falls back to original Draft | Improves when ready |

Warm cache (same `content_hash` in `<project>.photon.cache/`): skip straight to the highest completed stage; no re-probe/re-hash (05 §1.4).

### 2.2 Pool row status (derived, 05 §2.4)

Map stages to existing derived labels:

| Label | Condition |
|---|---|
| Importing | L0 only |
| Probing | L0 done, L2 incomplete |
| Indexing | L2 done, L4 incomplete (video) |
| Ready | L2 + L3 done (L4 optional for Ready badge; show “indexing…” sublabel until L4) |
| Proxy Building / Proxy Ready | L7 in progress / ready |
| Offline | Path unreachable |

### 2.3 CAP-001 acceptance (load)

- Multi-select import shows **all** rows at L0 before any L2 completes.
- Metadata columns fill as L2 completes without re-import.
- Clip can be inserted on the timeline after L0; playback shows placeholder until L2/L3, then upgrades.

---

## 3. Single monitor: `PreviewTarget`

### 3.1 State (session-only, not document, not undoable)

```rust
/// What the single central monitor is currently showing.
pub enum PreviewTarget {
    /// Sequence under the shared playhead (default timeline focus).
    Sequence { sequence: SequenceId },
    /// Media pool asset peek (bin selection / explicit source peek).
    Asset {
        asset: AssetId,
        /// Source-space tick; independent of sequence playhead while target is Asset.
        source_time: Tick,
    },
    /// Optional: composition graph output when node editor owns focus (08).
    Composition { clip: ClipId }, // or project graph — 08 owns id shape
}

pub struct MonitorSession {
    pub target: PreviewTarget,
    pub quality: PreviewQuality,       // §4
    pub proxy_mode: ProxyMode,         // Auto | ForceProxy | ForceOriginal (02 §6)
    /// Source marks for 3-point edit (G-10 semantics, single surface).
    pub source_in: Option<Tick>,
    pub source_out: Option<Tick>,
    pub armed_asset: Option<AssetId>,  // last asset used for source marks / I/O insert
}
```

### 3.2 Retarget rules (normative)

| Focus / event | `PreviewTarget` | Notes |
|---|---|---|
| Timeline focused, **playing** | `Sequence` | **Play wins.** Selection changes do not steal the picture until pause. |
| Timeline focused, **paused** | `Sequence` at shared playhead | Default. Sequence frame is one graph; fastest steady state. |
| Media pool row selected (timeline not playing) | `Asset` at last `source_time` or 0 | Poster first (§5), then exact source frame. |
| Explicit “peek source” (bin double-click or command) | `Asset` | Same as pool select; does not create a second pane. |
| Match Frame (G-3) | `Asset` at resolved source tick | Loads marks context; still one surface. |
| Node composition editor focused | `Composition` | When 08 UI is active. |
| Transport Play from any target | Switch to `Sequence` and play | Source peek is not a second timeline clock. |

**Shared sequence playhead** remains the single timeline clock (04).  
`Asset.source_time` is **independent session state** used only while `target == Asset` (and for source In/Out). It is not written into the document except via explicit edit ops (Insert/Overwrite using marks).

### 3.3 Source marks (G-10 without dual monitors)

- Source In/Out live on `MonitorSession`, session-only, non-undoable in v1 (ROADMAP S7).
- Mark commands apply to `armed_asset` + current asset `source_time` when target is `Asset`, or to Match Frame result.
- Insert/Overwrite consume marks + G-6 target tracks; they do not require a visible second monitor.
- UI chrome: one transport bar; when `target == Asset`, timecode shows source time and an “SOURCE” badge; when `Sequence`, shows sequence time and “SEQUENCE” (or no badge).

### 3.4 What this rejects

- Permanent dual-pane source | program layout as v1 requirement.
- Always-running second full decode pipeline “just in case.”
- Black frame on every retarget when poster or last `EngineFrame` exists.

---

## 4. Preview quality tiers

| Tier | `PreviewQuality` | Decode source | Output size (long edge) | Used for |
|---|---|---|---|---|
| **Draft** | `Draft` | Proxy if `ProxyMode` allows and proxy ready; else original | min(sequence long edge, **960**) unless sequence smaller | Scrub, play, selection retarget, default monitor |
| **Full** | `Full` | Original only (`proxy = false`) | Active `SequenceFormat` size | User toggle “Full quality”, step-frame optional, export path always Full |

Notes:

- Draft size is an **engine present/eval** parameter, not a document property.
- Graph compile for Draft may skip non-essential work only if pixel-safe for edit decisions (must not lie about cut points). Grades/effects still run; resolution is what drops.
- CAP-014: `ProxyMode::ForceOriginal` forces original even in Draft (still at Draft resolution unless Full).
- Export / MCP determinism goldens use Full + originals (02 §7, 11).

### 4.1 Proxy interaction (02 §6)

| `ProxyMode` | Draft decode file | Full / export |
|---|---|---|
| `Auto` | Proxy if ready and policy would use it; else original | Original |
| `ForceProxy` | Proxy if ready; else original + status warning | Original |
| `ForceOriginal` | Original | Original |

Proxy generation remains background (L7); never blocks L0–L3.

---

## 5. Time-to-paint path

Every monitor update follows this order. Higher steps cancel in-flight lower-priority work for the same target (latest-wins).

```
retarget or seek
    │
    ▼
P0 Hold last EngineFrame if any          ── instant (04 §3.1)
    │
    ▼
P1 Poster / sidecar still if target Asset or cold Sequence clip
    │  (L3 cache; may be wrong time — OK)
    ▼
P2 Exact frame at Draft                   ── decode + compile + eval
    │
    ▼
P3 Ring fill + prefetch                   ── scrub/play smoothness
```

### 5.1 Seek & decode (aligns 02 §3–4)

- Seeks **coalesce** (latest-wins) per engine tick.
- Seek = keyframe index entry ≤ t, decode-forward (L4). Before L4: still correct, slower; do not block UI.
- Ring defaults at Draft: **16 forward / 4 back** (02 §3).
- Cut-ahead warmup ≥ **500 ms** of timeline time before the next cut (02 §3).
- One ffmpeg sidecar per `(asset, quality_tier)` that is **hot**; LRU-evict cold sidecars. Cap concurrent video sidecars (implementation default: min(4, cores/2)).
- Selection retarget to `Asset` starts **at most one** peek decode; does not keep a second full ring unless user scrubs that asset.

### 5.2 Placeholder rules

| Condition | Monitor shows |
|---|---|
| No frame ever | Neutral surface + spinner (not pure black) |
| Offline asset | Diagonal stripe placeholder (01/05) |
| Decode error | Diagnostic placeholder; sidecar restart per 02 §3 |
| Buffering exact frame | Last frame or poster + small buffering affordance |

---

## 6. Performance budgets

Normative targets for local SSD, 1080p-class proxy or Draft, warm keyframe index unless noted. Measured in [11-testing-phasing.md](11-testing-phasing.md); numbers here are the product contract.

| Metric | Budget | Notes |
|---|---|---|
| L0 row visible after import action | **≤ 100 ms** | UI thread only |
| L2 probe complete (typical MP4) | **≤ 500 ms** p95 | Background |
| L3 poster available | **≤ 1.0 s** p95 | Background; monitor may paint earlier via P0 |
| First non-empty monitor paint after import select | **≤ 1.0 s** p95 | P0/P1 allowed |
| Exact Draft frame after seek (warm index, proxy) | **≤ 150 ms** p95 | Matches 02 §8 cold-seek-with-proxy spirit |
| Exact Draft frame (warm ring hit) | **≤ 1 frame interval** | Present only |
| Graph compile (10 tracks, 3 active) | **&lt; 0.5 ms** | 02 §8 |
| Scrub: seek coalesce under continuous drag | Drop intermediate seeks | No unbounded queue |
| Play start after pause (warm ring) | **≤ 100 ms** to first advanced frame | |
| Concurrent import of N files | L0 all N before any L2 | No serial UI stall |

Export budgets remain 02 §8; out of scope for interactive preview except “export must not freeze monitor” (export on dedicated session/workers).

---

## 7. Thread & ownership map

| Work | Owner | Thread / process | Notes |
|---|---|---|---|
| L0 register, selection, `PreviewTarget` writes | `photonic-gui` | GUI | No ffmpeg, no GPU map heavy work |
| L1 hash, L2 probe, L4 index, L5/L6, L7 proxy | `photonic-video` workers | Engine worker pool | Jobs report via `EngineStatus` |
| Seek coalesce, play state machine, graph compile schedule | Engine thread | 02 §1 | |
| ffmpeg sidecar read | Decode workers | 02 §3 | Deadlines; never block engine thread on pipe |
| Graph eval (Draft/Full) | Engine + GPU | wgpu queue shared with app | |
| Present `EngineFrame` → egui image | GUI + `photonic-render` | GUI frame | `present_engine_frame` (03/04) |
| Audio clock / mix | Audio thread + mixer worker | 02 §4, 09 | Master clock when playing |
| Export | Dedicated engine session / workers | 02 §7 | Full + originals; GUI stays live |
| MCP peek/export | Same engine services | CAP-019 | No GUI-only path |

**Hard rules:**

1. GUI never spawns ffmpeg directly — only `EngineCmd` / media jobs.
2. GUI never blocks on probe/proxy/index completion.
3. At most one “primary” interactive ring for the current `PreviewTarget`; sequence play owns the primary when playing.
4. Document mutations for import remain `TimelineCmd` / history rules (01/05); readiness stages are session/engine facts, not undo steps.

---

## 8. Engine API surface (additions / clarifications)

Aligns with 02 §1; names are contractual, exact Rust paths may match existing modules.

```rust
pub enum EngineCmd {
    // existing: Play, Pause, Seek, Step, SetLoop, SetActiveSequence,
    // SetProxyMode, Export, CancelExport, GenerateProxies, Probe, InvalidateRange, ...

    /// Retarget single monitor evaluation without requiring dual pipelines.
    SetPreviewTarget(PreviewTarget),
    SetPreviewQuality(PreviewQuality),
    /// Source-space seek when target is Asset (ignored for Sequence).
    SeekSource { asset: AssetId, time: Tick },
}

pub struct EngineStatus {
    // existing fields...
    pub preview_target: PreviewTarget,
    pub preview_quality: PreviewQuality,
    pub asset_readiness: Vec<(AssetId, AssetReadiness)>, // or query API
    pub buffering: bool,
    pub dropped_frames: u64,
}

pub struct AssetReadiness {
    pub probe: bool,
    pub poster: bool,
    pub keyframe_index: bool,
    pub proxy: ProxyStatus,
}
```

MCP (CAP-019): agents set marks and preview target via tools; they must observe the same Draft/Full and proxy rules for interactive-style peeks; export tools always Full/original.

---

## 9. UI chrome (minimal)

Owned with 04 monitor transport; specified here so speed UX is not lost:

| Control | Behavior |
|---|---|
| Transport bar | Single bar under the one monitor (04 §3.2) |
| SOURCE / SEQUENCE badge | Reflects `PreviewTarget` |
| Proxy indicator | Auto / Proxy / Original (session `ProxyMode`) |
| Quality | Draft (default) / Full toggle |
| Buffering spinner | Only when P2 in flight and no P0/P1 to show |
| Source In/Out marks | Visible on scrubber when `armed_asset` set; not a second pane |

---

## 10. Relationship to other docs

| Doc | Relationship |
|---|---|
| 02 §3–6, §8 | Decode, ring, proxy, budgets — this doc binds them to product stages and single-target preview |
| 04 §3 | Program monitor shell — this doc owns *what* is evaluated into that shell |
| 05 §1.4, §2 | Import pipeline — this doc adds L3 poster priority and readiness vs play/scrub gates |
| 15 thumbnails/waveforms | L5/L6 detail |
| 20 G-10 | Source marks + audition **without** dual-pane requirement; presentation amended by D-PM-1–3 |
| 11 testing | Must add cases for L0 latency, poster paint, Draft seek p95, play-wins retarget |

---

## 11. Implementation checklist (for agents)

1. `PreviewTarget` + retarget rules on monitor session; play-wins. **Done** (2026-07-19).  
2. Import L0→L3 priority: poster job scheduled ahead of full thumb strips. **Done** (L0–L5; L5 after L1–L4 meta send so waveforms never gate metadata). L6 strip samples remain lazy via timeline `ThumbnailCache` (spec 15; visible-range only).  
3. Draft default size + proxy Auto path wired to compile/eval. **Done** (Draft default + `ProxyMode`; L7 auto-queue when `ProjectVideoSettings.generate_proxies`).  
4. Seek coalesce + ring + cut-ahead verified against §6 budgets. **Done** — hard: `scrub_seek_coalesce_latest_wins`, `seek_coalesce_under_drag_simulation`, `warm_keyframe_index_lookup_budget`, `cut_ahead_scan_next_clip_within_lead`, `prefetch_ahead_horizon_is_contractual`, `playback::prefetch::cut_ahead_*` units; session wires `cut_ahead_targets` + at-most-one open per present. Soft: `soft_draft_seek_budget_with_warm_index` (full decode-to-frame p95 remains environment-dependent).  
5. Sidecar LRU cap; no dual always-on decoders. **Done** — `lru_evicts_coldest_unprotected_only` + session `evict_stale` via `lru_eviction_victims`; `MAX_LIVE_SOURCES=8`; cut-ahead amortizes one build/present.  
6. Source marks session fields + Insert/Overwrite consumption (G-10 residual) on single surface. **Done** — `SourceMarksSession`; focus-aware I/O; Match Frame full range; `marks_to_insert_payload_preserves_source_range`, `match_frame_style_marks_keep_remainder_out` (dual-pane still non-goal).  
7. Windows: no unix-FIFO assumption for any load/preview path (export dual-input is separate; preview is single rawvideo pipe). **Done** (export temp-file A/V path).  
8. Tests in 11 for budgets and “import N files → N rows before probe.” **Done** — `l0_register_n_stubs_before_any_probe` (N stubs, no probe/hash, &lt;100 ms); ladder L0–L5+L7 in `preview_media_load`; §6 harness in `seek_budgets`.  

---

## 12. Non-goals

- Dual permanent monitors or external reference monitor.  
- Background folder watch ingest (05 §1.3).  
- Full-quality interactive scrub as default.  
- Proxies required for correctness.  
- Changing export determinism or working color space.
