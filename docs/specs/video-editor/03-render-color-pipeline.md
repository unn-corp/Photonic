# 03 — Renderer Prerequisites, Video Texture Path & Color Pipeline

**Depends on:** 01-data-model.md, 02-engine.md. **Decisions:** D-09, D-10. **Feeds:** 07-color-grading.md (scopes, LUT), 11-testing-phasing.md (golden corpus, perf budgets).

Scope (per 00 §5 doc map): the P1 renderer prerequisite work gating all playback phases (D-10), the video texture path from decoded YUV to the working format, and the color-space design reconciling existing sRGB vector rendering with the new linear video path (D-09). Terminology from 01/02 is normative; `IrOp`, `EngineFrame`, `FrameGraph` etc. are as defined there.

D-09 mode overlay: sections below remain normative for `LinearRec709Sdr`, the default and compatibility path. An explicitly selected `LinearRec2020Hdr` sequence substitutes BT.2100 HLG/PQ inverse transfer, Rec.2020 matrices, working-luminance normalization (`1.0` = 203 cd/m²; default mastering peak 1,000 cd/m²), HDR scopes, and explicit SDR/export transforms from `22-dji-advanced-workflows.md` §7. Storage remains linear-premultiplied `Rgba16Float`. No clip alone changes sequence mode. Existing SDR decode/display/export math and goldens remain byte/pixel stable.

---

## 1. Current renderer state (baseline this doc changes)

<!-- spec-assert: symbol-exists crates/photonic-video/src/session.rs::EngineSession -->
<!-- SD-1 (27 §3): historically claimed `photonic-video` "does not exist yet"; the crate and its EngineSession are now real, pinned here so a regression that deletes the crate reds the gate. -->

Verified against current code, cited so P1 work has an exact starting line:

- `PhotonicRenderer` (`renderer/mod.rs:63`) drives a windowed surface, `AutoVsync` present mode (`renderer/mod.rs:234`), 4x MSAA (`const MSAA_SAMPLES: u32 = 4`, `renderer/mod.rs:46`).
- Surface format is negotiated, not hardcoded: `caps.formats.iter().find(Bgra8Unorm | Rgba8Unorm).unwrap_or(caps.formats[0])` (`renderer/mod.rs:214-227`), with an explicit comment: *"Prefer a non-sRGB linear format so egui doesn't double-gamma-correct."* This is load-bearing for §5 below — the app window surface is non-sRGB by design, unlike the scene's internal render target.
- `build_geometry` (`renderer/mod.rs:426`) re-walks the whole document and re-tessellates every fill/stroke/glow/shadow/overlay every call. The only cache is a lock-contention fallback (`cached_vertices`/`cached_indices`, `renderer/mod.rs:477-483`, used only when `document.try_lock()` fails) — not content-based dirty tracking.
- `scene_renderer.rs:18-24` and `:25-31` call `create_buffer_init` for a fresh vertex buffer and index buffer every frame. No persistent buffer field exists on `PhotonicRenderer` for the document pass.
- `SCENE_FORMAT = Rgba8UnormSrgb` (`pipeline.rs:14`), deliberately sRGB so the fixed-function blend unit decodes/blends/re-encodes in linear space for `Multiply`/`Screen`-class modes and partial-alpha src-over — this is what keeps canvas and export pixel-identical (issue #145, asserted by test `scene_format_is_srgb_for_linear_blending`, `pipeline.rs:810-818`).
- `SEPARABLE_BLEND_MODES = [Multiply, Screen, Darken, Lighten]` (`pipeline.rs:23-28`) are the only modes expressible as fixed-function `wgpu::BlendState` (`separable_blend_state`, `pipeline.rs:37`). All other modes are approximated in the live canvas per a documented follow-up (issue #17); true on-canvas isolation for backdrop-read modes is issue #226 (`renderer/mod.rs:506`).
- `COMPOSITE_SHADER` (`pipeline.rs:592`) is a WGSL full-screen isolation pass implementing all 26 blend modes: 12 W3C separable/blend-only modes plus 8 Photoshop-extra separable modes via `blend_channel` (`pipeline.rs:631-660`), 4 HSL non-separable modes (Hue/Saturation/Color/Luminosity) plus DarkerColor/LighterColor via `fs_composite`'s backdrop-dependent branch (`pipeline.rs:685-720`, helpers `pipeline.rs:661-683`). It naga-validates in CI (`pipeline.rs:924-941`, using `naga = "22"` pinned as a **dev-dependency** of `photonic-render`, `Cargo.toml:31`, specifically so WGSL parses without a GPU). `DrawSegment { mode, start, count }` (`pipeline.rs:83`) is the per-segment draw-call unit the shader would consume. `blend_mode_index` (`pipeline.rs:544`) **is called from the live canvas**: `renderer/mod.rs:602` builds `composite_pipeline` from `COMPOSITE_SHADER`, `scene_renderer.rs:384` sets it, and `blend_mode_index` is invoked at `scene_renderer.rs:225,267`. Per-*node* segment isolation (`segments_need_isolation`) remains headless-only.
- CPU compositor: `composite_document` (`compositor.rs:48`) and `composite_raster_nodes` (`headless.rs:1329`) work on **straight alpha, gamma/sRGB-encoded** `RGBA8` bytes (`compositor.rs:39`), via `blend_channel`/`blend_rgb` (`photonic-core/src/raster/blend.rs`) operating on raw 0..1 float from u8 with **no degamma step** (confirmed: no `srgb_to_linear` call anywhere in `blend.rs`). This is the opposite mechanism from the GPU path (which gets linear blending for free from the sRGB-format hardware blend unit) reaching the same visual convention — a deliberate asymmetry, not a bug, and the reason §4.3 exists.
- `HeadlessRenderer::render_rgba_with_opts` (`headless.rs:207-213`) routes per-document to CPU or GPU via the predicate `has_raster || has_pattern || has_isolated_layer || has_non_print_layer || has_stack_effects` (`headless.rs:262-302`, condition definitions + the `if`): true routes the whole doc to CPU compositing via `composite_document` (`headless.rs:334`); false takes the pure-GPU path (`headless.rs:339+`). This exact predicate (`headless.rs:262-302`) is reused unchanged in §2.5.
- Render-to-offscreen-texture **already exists** and is proven: `capture.rs:22` ("Render to an offscreen texture, read back pixels, encode as PNG"), a dedicated `fill_pipeline_1spp` (sample_count=1, `renderer/mod.rs:113`) shared by window and offscreen capture (`renderer/mod.rs:129`), consumed throughout `headless.rs` (`:89`, `:362`, `:988`). P1 does not invent texture-target rendering — it extends this path to hand back a GPU texture instead of only CPU-readback bytes.
- Rasters are never GPU-uploaded: zero `write_texture` call sites touching `RasterImage` bytes across the crate; all `create_texture` calls are render targets (MSAA, glow, capture, effects). Glyphon owns the only GPU texture atlas (`TextAtlas`, field `renderer/mod.rs:102`, constructed `renderer/mod.rs:319`).
- `crates/photonic-video` **exists** (`graph/`, `decode/`, `audio/`, `export/`, `playback/`, `media/`, `session.rs`, plus integration tests). `wgpu = "22"` is the workspace-pinned version (`Cargo.toml:workspace deps`).

---

## 2. Renderer prerequisite work (P1, D-10)

### 2.1 Shared prerequisite: revision counter + affected-node tracking

02 §1 asserts a `doc_generation: u64` counter "bumped by `CommandHistory::execute`" driving engine re-snapshot. That counter does not exist as described: `CommandHistory` has a private `revision: u64` field (`history/mod.rs:~2905`, doc'd as bumped "on every mutation") but it is only incremented in `checkpoints.rs:48` and `:63` (checkpoint/branch restore) — `CommandHistory::execute` (`history/stacks.rs:229-298`) never touches it, `undo`/`redo` don't either, and there is no `pub fn revision(&self)` accessor anywhere in `photonic-core`.

**Spec position:** extend this field, don't add a parallel one. `execute`/`undo`/`redo` each bump `self.revision` (wrapping-add, matching the existing checkpoint sites' style); add `pub fn revision(&self) -> u64`. This single counter serves two consumers: 02's `doc_generation` (engine re-snapshot trigger) and this doc's per-node tessellation cache (§2.2). One owner, no drift between them.

Second gap: nothing lets a caller ask "which nodes did this command touch." No `affected_nodes`, `touched_node`, or equivalent exists on `Command` (`history/mod.rs:1792`, `impl Command` `~2080`) or anywhere in the crate. Every command variant already carries the `NodeId`(s) it mutates (required for its own `apply`/`inverse`) — this is a mechanical, not structural, addition.

**Spec position:** add `fn affected_nodes(&self) -> SmallVec<[NodeId; 4]>` to `Command`, one match arm per existing variant returning the id(s) already stored on it. `CommandHistory::execute`/`undo`/`redo` record `(new_revision, affected_nodes)` in a small ring (last ~64 entries is enough for same-frame renderer polling; older history is covered by "unknown range → invalidate all" fallback, never a correctness issue, only a cache-hit-rate one). Expose `fn changes_since(&self, from: u64) -> ChangeSummary { revision: u64, touched: HashSet<NodeId>, overflowed: bool }` — `overflowed = true` when `from` predates the ring, signaling "invalidate everything."

### 2.2 Per-node tessellation cache

**As built (P1 S3, accepted deviation):** the cache is a **content-addressed memo at the `tessellate_*` boundary** (`renderer/tess_cache.rs`), keyed by `tess_inputs_hash` alone rather than `(NodeId, TessKind)` bundles. Rationale: this renderer resolves symbol instances and live-boolean groups to derived paths at draw time, and `Command::affected_nodes` reports the *edited* node id, not the instance/group that actually renders — id-keyed invalidation would reuse stale meshes after a master edit. Content-addressing is correct by construction (memo of a pure function), dedupes identical geometry, and makes undo/redo reversions cache hits. `revision()` still gates a whole-frame skip; `changes_since().overflowed` still clears the memo. The original id-keyed design below is retained for archaeology; the hash-input rules stand unchanged.

Original sketch: keyed by `(NodeId, TessKind)` where `TessKind ∈ {Fill, Stroke, Glow, Shadow, Overlay}` (mirrors `build_geometry`'s existing per-node draw categories, `renderer/mod.rs:426-467`). Cache entry: `CachedGeometry { vertices: Range<u32>, indices: Range<u32>, node_revision: u64, tess_inputs_hash: u64 }`.

`tess_inputs_hash` covers exactly the fields that affect vertex geometry (path points, stroke width/cap/join/dash, glow/shadow radius) — **not** color, opacity, or blend mode, which don't change vertex count/position and are applied at draw time via the existing per-node uniform path. This keeps color-only edits (very common: opacity drag, fill-color pick) from invalidating tessellation at all.

Algorithm per frame in `build_geometry`:
1. Read `history.revision()`. If unchanged since last frame, skip all diffing — reuse every cached range as-is (the common idle/pan/zoom-only case costs one integer compare).
2. Else call `history.changes_since(last_seen_revision)`. If `overflowed`, invalidate the whole cache (falls back to current full-rebuild behavior — correctness preserved, no regression risk). Otherwise, for each touched `NodeId`, re-tessellate only that node's `TessKind` entries whose `tess_inputs_hash` actually changed (still need to recompute the hash to know — cheap, no tessellation) and re-tessellate only those that differ.
3. Update `last_seen_revision`.

This is additive to §1's fallback: the lock-contention `cached_vertices`/`cached_indices` clone stays as the safety net when `try_lock` fails; the new cache operates when the lock succeeds and replaces "re-tessellate everything" with "re-tessellate the touched subset."

### 2.3 Persistent GPU vertex/index buffers

**As built (P1 S3, accepted deviation):** persistent doubling-growth buffers with **whole-buffer `queue.write_buffer` upload** per changed frame — no bump allocator/freelist/compaction. Rationale: vertices carry baked color and are reassembled in draw order into one buffer drawn by `draw_indexed` ranges; per-node stable slots would require decoupling physical from draw order (a much larger rewrite) for zero additional tessellation savings. The per-frame allocation — the actual cost §1 identified — is gone. The allocator design below is retained as the escalation path if upload bandwidth ever becomes the measured bottleneck.

Original sketch: replace `scene_renderer.rs:18-31`'s per-frame `create_buffer_init` with two growable persistent buffers (`vbuf: wgpu::Buffer`, `ibuf: wgpu::Buffer`) owned by `PhotonicRenderer`, sized via doubling growth (`usage: VERTEX | COPY_DST` / `INDEX | COPY_DST`).

Slot allocation: a bump allocator per buffer with per-node reserved byte ranges recorded in `CachedGeometry` (§2.2). Update rule:
- **In-place update** (the common case: geometry unchanged, only a re-tessellation with identical vertex/index count — e.g., stroke width edit that doesn't change point count): `queue.write_buffer(&vbuf, entry.vertices.start, &new_bytes)` at the existing offset. No reallocation.
- **Structural change** (vertex count changed — path edit, node add/remove): free the old range (mark in a per-size-class freelist), bump-allocate a new range at the tail. Freed ranges are reused by future same-or-smaller allocations; fragmentation is bounded by a **compaction pass** run when free-space ratio exceeds a threshold (e.g., 25% of buffer capacity), not every frame — compaction walks all live entries and repacks them contiguously in one `queue.write_buffer` batch, then updates every `CachedGeometry.vertices`/`.indices` range. Cheap relative to full-scene retessellation since it moves bytes, not paths.
- Whole-buffer capacity growth (doubling) triggers a full re-upload (already-tessellated CPU-side copies retained per entry until the compaction/grow completes) — rare, amortized O(1) per node over the buffer's lifetime.

Draw calls read `DrawSegment { mode, start, count }` (`pipeline.rs:83`) ranges directly against the persistent buffers instead of the ephemeral ones — no change to the draw-call shape, only to where the bytes live.

### 2.4 Wiring `COMPOSITE_SHADER`

> **Operand space is normative in [§4.5](#45-operand-spaces-for-blending-and-grading-normative)** — blending is linear, on straight alpha, and requires an sRGB render target. The live canvas does not currently satisfy that last requirement; §4.5.4 owns the fix.

`blend_mode_index` (`pipeline.rs:544`) and `COMPOSITE_SHADER` (`pipeline.rs:592`) are **wired on the live canvas** (see §2.4). This section's remaining scope is per-segment isolation; P1 wired them for the 22 non-fixed-function modes (everything outside `SEPARABLE_BLEND_MODES`): for each `DrawSegment` whose mode isn't one of `[Multiply, Screen, Darken, Lighten]`, render that segment's layer to an isolated offscreen `SCENE_FORMAT` (`Rgba8UnormSrgb`) texture, then run the full-screen `COMPOSITE_SHADER` pass sampling backdrop + isolated layer, writing `blend_mode_index` as a push constant / small uniform. This finally gives on-canvas correctness for HSL modes and backdrop-read modes (Overlay, SoftLight, ColorDodge, ColorBurn), closing issue #17's live-canvas approximation and materially retiring issue #226 (`renderer/mod.rs:506`) for the modes `COMPOSITE_SHADER` already covers.

Test hook: extend `wgsl_shaders_parse_and_validate`'s table (`pipeline.rs:924-941` pattern) — any new isolation-pass shader added here goes in the same table so CI catches WGSL syntax errors without a GPU.

### 2.5 Texture-target rendering of the vector scene (RasterVector fast path)

02 §3 states `RasterVector` renders via `HeadlessRenderer::render_rgba_with_opts` (CPU-composited, correct) in P3, "migrating to the GPU scene path when 03's texture-target work lands." Two tiers, both real, one ships in P3 and one is this P1 item:

**Tier A (P3, universal, ships first):** `render_rgba_with_opts` (`headless.rs:207-213`) already returns `(Vec<u8>, u32, u32)` — RGBA8, straight alpha, gamma-encoded, whichever internal path (CPU compositor or GPU) the routing predicate (`headless.rs:262-302`) selected. `photonic-video`'s `DecodeVideo`-sibling `RasterVector` `IrOp` (02 §2) uploads these bytes as an `Rgba8Unorm` texture (no sRGB view — raw bytes, manual conversion, see §3.2/§4.1) then runs the same conversion pass as raster/still assets (§4.2 boundary "asset → working"). Works for **every** document (raster placements, patterns, isolated layers, effect stacks included) because it's exactly what headless export already produces — zero renderer changes required, only plumbing on the `photonic-video` side.

**Tier B (this P1 item, fast path only):** when the document is pure-vector — i.e., the routing predicate (`headless.rs:262-302`, `!has_raster && !has_pattern && !has_isolated_layer && !has_non_print_layer && !has_stack_effects`) is false — skip the CPU roundtrip entirely. Render straight to the existing offscreen `SCENE_FORMAT` (`Rgba8UnormSrgb`) target (same pipeline `capture.rs`/`headless.rs` already use), then run the §3.2-style conversion pass (sRGB EOTF decode + premultiply) directly GPU-to-GPU into the `Rgba16Float` working texture, never touching the CPU. Gate this tier explicitly on the reused predicate — do not re-derive document-shape logic in `photonic-video`; import/reuse the function from `photonic-render`.

Rationale for keeping both tiers rather than jumping straight to Tier B: correctness first (Tier A is provably identical to today's headless export, since it *is* today's headless export), performance second (Tier B removes CPU readback + re-upload latency for the common pure-vector-title case — CAP-021/AS-3's dominant workload). Tier A stays as the permanent fallback for the raster/pattern/effect-stack case; it is never fully retired.

### 2.6 Golden-output safety net

**Status: delivered on this branch.** `crates/photonic-render/tests/golden_vector_equivalence.rs` implements the harness described below, against a 31-case corpus under `crates/photonic-render/tests/golden/`. Cases compare byte-for-byte by default; a case may opt into a PSNR floor by adding `tolerance_db.txt` beside its reference (used by `blend_nonseparable`, and by `text_basic` / `text_styled`, whose glyph rasterisation varies with the system fonts available per CI OS). It keeps the skip-with-message convention when no GPU adapter is present.

The pre-existing checks it supplements (`headless.rs`, e.g. `separable_blend_modes_match_reference`) are single-pixel hand-computed-expected-value assertions (`TOL: f32 = 0.03`), not image-diff regression testing.

A related design proposal — prior art, not a dependency, doc-only with no implementation — lives on branch `proposal/54-visual-regression-harness-golden-image-t` (commit `bd3c04a`) in this repository. It was previously reachable only via a personal fork remote; that fork has been deleted and every branch it held was preserved into `unn-corp/Photonic`, so the branch is now a plain `origin/` ref.

**Spec position** (the design this section gates on, now implemented):
- Fixture corpus: a checked-in set of `.photon` documents spanning node kinds, blend modes, effect stacks, raster+vector mixes — target 30-50 documents, stored under `crates/photonic-render/tests/golden/` (new dir).
- Reference images generated once from the **pre-refactor** renderer (current `main`), stored as PNG alongside each fixture.
- CI gate: render each fixture through `render_rgba_with_opts` post-refactor, compare byte-for-byte where the pipeline is meant to be exact (P1's persistent-buffer/dirty-tracking changes must not alter output at all — pure perf refactor) and via PSNR threshold (recommend ≥ 45 dB) for anything touching `COMPOSITE_SHADER` wiring (§2.4), since isolation-pass compositing may shift sub-LSB rounding vs the fixed-function path it replaces for previously-approximated modes.
- No-GPU-adapter CI runners: keep the existing skip-with-message convention (`headless.rs:1565`) rather than blocking merge — flag in 11-testing-phasing.md as a coverage gap requiring a GPU-capable CI runner for full confidence.

This is the P1 merge gate per 00 §7's top risk: "P1 lands behind golden-output comparison against current renderer."

Note: `crates/photonic-render/tests/golden/` (this section's vector-renderer-equivalence corpus, pre- vs. post-P1-refactor pixel comparison) and the repo-root `tests/golden/` (11-testing-phasing.md's video/timeline golden-frame corpus, exercising the full playback/export pipeline) are two deliberately separate systems — different scope, different lifecycle, different owning doc. Do not merge them; 11 §1.1 carries the mirror of this note.

---

## 3. Video texture path

### 3.1 YUV plane upload formats

Per 02 §3, decode produces `DecodedFrame { pts, planes }` from `ffmpeg -pix_fmt yuv420p|yuva444p ... pipe:1`. Upload:

| Source format | Plane | GPU texture | Sampling |
|---|---|---|---|
| yuv420p | Y (full res) | `R8Unorm` | nearest |
| yuv420p | Cb, Cr (half res, half res) | `R8Unorm` each | bilinear (upsample to luma res in shader) |
| yuva444p | Y, Cb, Cr, A (all full res) | `R8Unorm` each | nearest |

Three (or four, alpha-capable sources — ProRes 4444/VP9-alpha/WebM-alpha, relevant to CAP-021 round-trip of exported transparent motion graphics) separate single-channel textures, not one packed texture — avoids a manual plane-offset/stride shader and matches `queue.write_texture`'s natural row-pitch handling per plane. Range (limited 16-235/16-240 vs full 0-255) and matrix selection (BT.601 vs BT.709) come from `MediaProbe.video.color` (01 §3) populated at import via `ffprobe`; default BT.709 for HD+ sources, BT.601 for SD, per broadcast convention, when probe data is absent/ambiguous.

### 3.2 YUV→linear conversion pass

One fragment pass, one `DecodeVideo` `IrOp` (02 §2) execution, per decoded frame. Samples the 3-4 plane textures, outputs `Rgba16Float`. Steps, in order:

1. **Range expansion:** `y' = (Y_raw - offset_y) / scale_y`, `cb = (Cb_raw - offset_c) / scale_c - 0.5`, `cr = (Cr_raw - offset_c) / scale_c - 0.5`. Limited range: `offset_y=16/255, scale_y=219/255`, `offset_c=16/255, scale_c=224/255`. Full range: `offset=0, scale=1`.
2. **YUV→RGB matrix** (video-signal domain, still gamma-encoded per Rec.709 OETF convention — not yet linear):

   BT.709: `R = y' + 1.5748·cr`, `G = y' - 0.1873·cb - 0.4681·cr`, `B = y' + 1.8556·cb`
   BT.601: `R = y' + 1.402·cr`, `G = y' - 0.344136·cb - 0.714136·cr`, `B = y' + 1.772·cb`

3. **BT.709 EOTF** (decode to scene-linear — exact inverse of the OETF in §4.1, *not* approximated with the sRGB curve): `E = E'/4.5` for `E' < 0.081`, else `E = ((E' + 0.099)/1.099)^(1/0.45)`. Applied per channel.
4. Alpha (yuva444p only): straight `A_raw` sample, no transfer-function applied (alpha is linear by convention).

WGSL sketch (fragment shader, illustrative):
```wgsl
fn bt709_eotf(e: f32) -> f32 {
    return select(pow((e + 0.099) / 1.099, 1.0 / 0.45), e / 4.5, e < 0.081);
}
fn yuv_to_linear_rgb(y: f32, cb: f32, cr: f32) -> vec3<f32> {
    let r = y + 1.5748 * cr;
    let g = y - 0.1873 * cb - 0.4681 * cr;
    let b = y + 1.8556 * cb;
    return vec3<f32>(bt709_eotf(r), bt709_eotf(g), bt709_eotf(b));
}
```

Why exact BT.709 rather than the sRGB-curve approximation many engines use for convenience: CAP-015 requires accurate scopes (waveform/vectorscope/histogram). The two curves diverge by ~1-2% in the midtones; that error is invisible to the eye but visible on a vectorscope/waveform read against reference footage, which is exactly what colorists check first. §6 adds a curve-accuracy unit test.

### 3.3 Premultiply

Applied immediately after step 3/4 above, before the texture is written: `rgb_premult = rgb_linear * a`. All downstream `IrOp`s (`Transform2D`, `Effect`, `Grade`, `Merge`) operate on premultiplied linear `Rgba16Float` per D-09 — this is what makes `Merge`'s `over` compositing a single `src + dst*(1-srcA)` without a separate unpremultiply/premultiply round-trip per node.

Existing core helpers (`photonic-core::raster::filter::premultiply_planes`/`unpremultiply_planes`, `raster/geometry.rs::unpremultiply_in_place`, `raster/warp.rs::unpremultiply_in_place` — three independent copies of the same pattern) are CPU-side and gamma-space; they don't directly reuse here (this is a GPU linear-space operation), but the **duplication** they represent is worth fixing regardless. Recommend consolidating all transfer-function and premultiply logic (existing sRGB EOTF/OETF at `raster/adjust.rs:72,83`, the three premultiply copies, and this doc's new BT.709 EOTF/OETF + YUV matrices) into one `photonic-core::color` module, used by CPU raster code, the new WGSL shaders (kept numerically in sync via a parity test, §4.4), and any future host-side reference conversions. One shared module, not a fourth duplicate.

### 3.4 `Rgba16Float` working textures & texture pool

All `IrOp` outputs are `Rgba16Float`, premultiplied, and linear in the sequence working primaries: Rec.709 for `LinearRec709Sdr`, Rec.2020 for `LinearRec2020Hdr`. Outputs use sequence working resolution (preview or full, per proxy mode). Shared pooled allocator (02 §5's "Node results" cache is keyed by IR content hash — this is the allocation layer underneath that cache): an LRU-evicted pool of `Rgba16Float` textures bucketed by `(width, height)`, budget 1-2 GB matching 02 §5's stated cache budget. `graph::eval` requests a texture from the pool per node (keyed by content hash — cache hit returns the existing texture directly, no allocation), returns it to the pool (not destroyed) when its content hash ages out of the LRU. This pool is a single instance per `EngineSession`, shared across concurrent `IrOp` evaluation within one frame and across frames — the same allocator instance backs Tier B's vector-to-texture path (§2.5) so vector rasters and video frames compete for the same budget rather than each keeping a separate reserve.

Sizing math, so the 1-2 GB figure is checkable rather than asserted: one 1080p (1920×1080) `Rgba16Float` texture is 8 bytes/px × 2,073,600 px ≈ 16.6 MB; a 3-layer composite (per SS-1) with `Grade`, `Merge`, and `CaptionOverlay` intermediate nodes plus the final output realistically holds 6-8 live textures per frame at steady state, i.e. ~100-130 MB live working set at 1080p. The 1-2 GB budget is therefore headroom for: (a) 4K preview/proxy-off sessions (4x the per-texture cost), (b) the LRU keeping recently-evicted-but-likely-reused entries warm across scrub/seek rather than reallocating every frame, and (c) Tier B vector rasters at higher-than-1080p canvas sizes. Bucket granularity: round each dimension up to the next 64px multiple before hashing into a size bucket, so minor sequence-format differences (16:9 vs a custom aspect within a few px) still share pool buckets instead of fragmenting into one-off allocations.

Eviction: standard LRU by content hash, but two exceptions bypass eviction pressure — (1) the `Output` node's texture for the *currently displayed* tick is pinned (never evicted) so scrub/step never re-evaluates a frame still on screen, and (2) proxy/original swaps (`SetProxyMode`, `InvalidateRange`) evict by asset-id prefix rather than waiting for natural LRU aging, matching 02 §5's "hash-natural invalidation... except `InvalidateRange` for asset relink/proxy swap."

### 3.5 Export encode path (working format → delivery)

02 §7 states export converts `Rgba16Float` readback to encoder pix_fmt, "linear→transfer per target, tone-unmapped Rec.709 in v1," but doesn't give the conversion itself — this is the exact inverse of §3.2-§3.3, run once per exported frame on the render-loop worker thread (not the realtime engine/audio threads):

1. GPU readback of the `Output` node's `Rgba16Float` texture (linear, premultiplied) to a CPU staging buffer.
2. **Unpremultiply**: `rgb = rgb_premult / max(a, epsilon)` (same operation as the present pass, §5, but on the CPU side here since export runs off the realtime path and there's no benefit to keeping it on GPU once readback already happened).
3. **BT.709 OETF encode** (§4.1 exact formula — never the sRGB approximation, for the same CAP-015/SS-3 accuracy reason as decode): `E' = 4.5·E` for `E < 0.018`, else `E' = 1.099·E^0.45 - 0.099`.
4. **RGB→YUV matrix** (inverse of §3.2 step 2 — BT.601 or BT.709 per `ExportPreset`'s target color tag, independent of the source's probed matrix; a BT.601-sourced clip exported as a BT.709 delivery target re-matrices, it does not just relabel).
5. **Range compression**: full-range 0..1 → limited 16-235/16-240 unless the preset explicitly requests full-range output (`ExportPreset` flag, 05 owns the preset catalog).
6. Pack planes (yuv420p: chroma downsample by box filter over the 4:2:0 site positions; yuva444p: alpha plane passes through unencoded, straight, matching decode's convention in §3.2 step 4) and write to the encoder sidecar's `rawvideo` stdin pipe (02 §7).

This path is the one that must be **bit-deterministic** for SS-3's golden basis (02 §7: "same project + preset ⇒ bit-identical rawvideo stream") — steps 2-6 use the same shared `photonic-core::color` constants as decode (§3.3, §4.4) so a round-trip through decode→working→encode of an unmodified clip reproduces the source YUV bytes within the stated f32/f16 tolerance, not just "looks right."

### 3.6 Scopes-friendly readback points

07-color-grading.md's waveform/vectorscope/histogram (CAP-015) need a defined readback point, not an ad-hoc one. Spec position: scopes always read **after the `Grade` node, before `CaptionOverlay`/`Output`** — i.e., graded-but-uncomposited-with-captions. This matches colorist expectation (scopes show the graded image, not final program-with-captions) and gives a stable single tap point regardless of how many tracks fold above it. Readback is a GPU→CPU copy of the `Rgba16Float` texture at that node (compute-shader histogram per 00's doc-map note for 07, operating directly on the linear texture — no separate CPU path needed for scope computation itself, only for the tap that feeds the UI image if displayed as thumbnail).

---

## 4. Color management design

### 4.1 Transfer functions (exact)

- **sRGB EOTF/OETF** (used for: vector/raster assets entering the video graph, and the app's existing CPU compositor convention). Already implemented, reuse as-is: `raster/adjust.rs:72` (`srgb_to_linear`, threshold 0.04045, linear segment /12.92, else `((c+0.055)/1.055)^2.4`) and `:83` (`linear_to_srgb`, inverse, threshold 0.0031308).
- **BT.709 OETF** (scene-linear → video signal, used at export encode): `E' = 4.5·E` for `E < 0.018`, else `E' = 1.099·E^0.45 - 0.099`.
- **BT.709 EOTF** (video signal → scene-linear, used at decode, §3.2 step 3): exact inverse, given in §3.2.

Both curves are "gamma ~2.2-2.4 with a linear toe" and visually close, but **not interchangeable** where CAP-015 scope accuracy matters (§3.2 rationale). Spec position: sRGB curve owns the asset/vector/CPU-compositor domain; BT.709 curve owns the video-signal domain; the conversion pass at each boundary (§4.2) is what reconciles them — never conflate the two by using one curve for both domains.

### 4.2 Boundary table (normative — every color-space transition in the system)

| Boundary | From | To | Mechanism |
|---|---|---|---|
| Video decode | YUV, gamma (BT.601/709 OETF), limited/full range | Linear Rec.709, premultiplied, `Rgba16Float` | §3.2 conversion pass (matrix + BT.709 EOTF) + §3.3 premultiply |
| Vector/raster asset → video graph (Tier A) | RGBA8 straight alpha, sRGB gamma (`render_rgba_with_opts` output) | Linear Rec.709, premultiplied, `Rgba16Float` | Upload as `Rgba8Unorm` (raw bytes) → sRGB EOTF (§4.1) → premultiply |
| Vector/raster asset → video graph (Tier B, pure-vector fast path) | `SCENE_FORMAT` (`Rgba8UnormSrgb`) offscreen texture | Linear Rec.709, premultiplied, `Rgba16Float` | GPU-to-GPU conversion pass, same math as Tier A, no CPU roundtrip (§2.5) |
| Working graph → display (program monitor) | Linear Rec.709, premultiplied, `Rgba16Float` | Non-sRGB window surface (`Bgra8Unorm`/`Rgba8Unorm`) | §5 present pass: unpremultiply + sRGB OETF encode (matches egui's non-sRGB convention, not BT.709 — see §5) |
| Working graph → export | Linear Rec.709, premultiplied, `Rgba16Float` | Encoder pix_fmt (yuv420p/yuva444p etc.) | Unpremultiply → BT.709 OETF → RGB→YUV (inverse §3.2 matrix) → range compression per target |
| Pure-vector document, no video features | sRGB gamma throughout (existing) | sRGB gamma throughout (existing) | **Unchanged** — CPU compositor (`compositor.rs:48`) and GPU `SCENE_FORMAT` path both stay exactly as they are today; canvas==export guarantee (issue #145) is untouched for documents that never enter the video graph |

### 4.3 Coexistence with existing sRGB vector rendering and CPU compositor

00 §7 lists this as a top risk: "Color-space unification breaks existing canvas==export guarantee." Spec position, matching that risk's stated mitigation ("video path is additive; vector paths keep current behavior until P7 revisits"): **the video graph is a parallel, opt-in reality, not a replacement.** A document with no `timeline` (01 §2, `Document.timeline: Option<TimelineProject>`) never touches any code in this doc — `compositor.rs`, `SCENE_FORMAT`, and the existing renderer are 100% unchanged for that document (P1's tessellation/buffer work is the only cross-cutting change, and it's proven pixel-identical by §2.6's golden corpus).

For a document that *does* place a vector asset on a video timeline (CAP-006/021, AS-3): that specific `RasterVector` node goes through §2.5's Tier A or B conversion — the source-of-truth rendering (tessellation, fill, blend) is identical to non-video rendering; only what happens to the resulting pixels (feed the video graph vs. present/export directly) differs. The CPU compositor's gamma-straight-alpha convention and the GPU linear-premultiplied convention never need to directly interoperate — they meet only at the Tier A/B conversion boundary, which is explicit and tested (§6).

P7 (07-color-grading.md, per 00 §7) is where full unification (if ever needed — e.g., grading a pure-vector document) gets revisited; this doc deliberately does not attempt it, per the locked risk mitigation.

### 4.4 CPU reference path (`eval_cpu`) parity rules

02 §2 requires `eval_cpu` — an f32 CPU implementation of every `IrOp` — for golden tests and compositor-parity cases. Parity rules (normative):

1. **Identical constants.** YUV matrix coefficients, EOTF/OETF breakpoints, and range offset/scale values must be bit-identical (same literal source) between the WGSL shader (§3.2) and the Rust `eval_cpu` implementation. Enforced via the recommended `photonic-core::color` module (§3.3): both WGSL generation/embedding and `eval_cpu` import from the same Rust constants — add a test (same pattern as `wgsl_shaders_parse_and_validate`, `pipeline.rs:924-941`) asserting the WGSL source's numeric literals match the Rust constants via string search, so a constant edited in one place without the other fails CI.
2. **Same operation order.** `eval_cpu` performs range-expand → matrix → EOTF → premultiply in the exact order §3.2/§3.3 specify — reordering changes results at the LSB level even with identical constants (premultiply-then-EOTF ≠ EOTF-then-premultiply).
3. **Tolerance.** `Rgba16Float` has ~3-4 significant decimal digits; golden-test comparisons between `eval_cpu` (f32) and GPU `eval` (f16 storage, f32 compute) use absolute tolerance `1e-3` in linear-light values before re-encoding for display (tighter than the existing ad hoc `TOL: f32 = 0.03` in `headless.rs:1547`, which was sized for 8-bit sRGB quantization, not f16 linear).
4. **Raster/compositor-parity case:** when a vector/raster asset enters the video graph (§4.2 rows 2-3), `eval_cpu`'s version of that boundary conversion must match the CPU compositor's existing output (`composite_document`) exactly where they overlap (documents with no isolated-layer/pattern/effect content) — this is the concrete test that proves Tier A and Tier B (§2.5) are pixel-equivalent, not just "close."

---

### 4.5 Operand spaces for blending and grading (normative)

Two defects ([27 A-1](27-spec-audit.md#a-1--p0--the-live-canvas-composites-in-gamma-headless-composites-in-linear), [27 A-3](27-spec-audit.md#a-3--p0--grade-operators-apply-transfer-functions-to-premultiplied-alpha)) had the same root cause: **the operand space for an operation was never written down**, so each surface picked one independently. This section fixes the rule in one place. Every new operator — including every entry in [30 §5](30-effect-catalogue.md#5-catalogue)'s catalogue — cites it.

#### 4.5.1 The product position: blend in linear

Photonic blends in **linear light**, everywhere, for every compositor. W3C blend functions are defined on transfer-encoded values, so `Multiply`, `Screen`, `Overlay`, `SoftLight` and the HSL modes differ from Photoshop and CSS. **That difference is deliberate and is a product position, not a defect.**

Rationale: it is physically correct; it is already this document's stated intent for the canvas (§2.4, issue #145); it avoids two transfer-function evaluations per merge on the hottest path in the compositor; and it makes the CPU and GPU evaluators agree trivially, since `blend_rgb` is pure maths over whatever it is handed.

#### 4.5.2 Alpha

Blend functions take **straight** (non-premultiplied) colour. The premultiplied path therefore unpremultiplies, blends, and re-premultiplies. `graph/ops.rs::merge_pixel` already does exactly this and is the reference implementation:

```
cs = unpremultiply(top);  cb = unpremultiply(bottom)
Cs' = (1 - αb)·Cs + αb·B(Cb, Cs)          // W3C backdrop-blended source
co  = αs·Cs' + (1 - αs)·bottom_premul      // premultiplied source-over
```

Where `α == 0`, carry RGB through unchanged rather than dividing.

#### 4.5.3 Grade operators

Grade ops **must** unpremultiply → operate → repremultiply. [07 §3](07-color-grading.md)'s current statement that ops run "on the stored (premultiplied) RGB directly" is correct only for opaque pixels — and `grade.rs:14-16` records that *every* golden fixture is opaque, which is precisely why this went unnoticed. On partially transparent pixels (every vector title in AS-3, every keyed edge in AS-2) CDL offset, contrast pivot and LUT lookup currently operate on alpha-attenuated values, producing edge fringing and a grade that shifts as opacity is keyframed.

The existing enc/dec discipline is unchanged and orthogonal: the sRGB transfer pair wraps CDL / Wheels / Contrast / LUT **internally**, and never wraps Exposure or WhiteBalance, which are defined in linear stops.

#### 4.5.4 Render-target requirement

`COMPOSITE_SHADER` is correct **only when its render target is sRGB-encoded**, so the hardware decodes on sample and re-encodes on write and the arithmetic lands in linear. `headless.rs` pins `Rgba8UnormSrgb` and satisfies this. The **live canvas does not**: `renderer/mod.rs:71-79` selects a non-sRGB swapchain (`Bgra8Unorm`/`Rgba8Unorm`) because egui shares that surface, and `effects_renderer.rs:17` allocates isolation textures to match — so on screen the same shader blends gamma-encoded values.

**Normative fix: the document renders to an offscreen target at the headless format, and that result is presented into the egui surface.** Canvas and headless then agree *by construction* rather than by keeping two formats in sync; document rendering is decoupled from swapchain-format availability and from egui's conventions; and it matches what the video path already does, where `EngineFrame` is an offscreen texture presented into the monitor. The vector canvas rendering direct-to-swapchain is the outlier.

Rejected: an sRGB `view_formats` view of the swapchain. Cheaper, but it leaves egui and the document sharing one target with opposing colour expectations — the arrangement that caused this.

#### 4.5.5 Required fixtures

The existing corpus **cannot** observe either defect, so these gate the fix:

1. **Partial-alpha grade** — a fixture with α ∈ (0,1) through CDL, curves and a 3D LUT; asserts no fringing and that the grade is invariant under a clip-opacity change.
2. **Canvas-vs-headless parity** — the same document composited through both paths with a **non-separable** blend mode (an HSL mode or SoftLight); asserts equality within tolerance.
3. **Non-`Normal` blend across evaluators** — extends [32 §8](32-engine-contracts.md#8-cpugpu-equivalence)'s equivalence sweep.

## 5. egui overlay & monitor presentation

`EngineFrame.texture` (02 §1) is `Arc<wgpu::Texture>`, `Rgba16Float`, linear, premultiplied. Presenting it in the program-monitor panel (04-ui-mode-timeline.md owns the panel UI itself; **this section is the normative owner of the `EngineFrame`→screen handoff** — 04 conforms to the mechanism defined here, not the reverse) needs an explicit conversion pass — there is no free hardware sRGB encode available here, because the app's window surface is deliberately **non-sRGB** (`Bgra8Unorm`/`Rgba8Unorm`, `renderer/mod.rs:214-227`, chosen so egui doesn't double-gamma-correct its own already-gamma UI colors). This is the opposite situation from `SCENE_FORMAT`'s hardware-assisted trick (§1) — the present pass must do the OETF encode itself in the shader, not rely on a `*Srgb` texture format doing it implicitly.

The whole pipeline below is one function, `present_engine_frame(frame: &EngineFrame, target: &egui::TextureId) -> ()` (owned by `photonic-render`, called once per displayed frame by the GUI's monitor panel) — 04-ui-mode-timeline.md references this exact name and treats it as the sole entry point for getting an `EngineFrame` on screen; no other doc defines a competing path.

Present pass (`present_engine_frame`'s body — one full-screen fragment shader, run once per displayed frame, not per graph node):
1. Sample `EngineFrame.texture` (linear, premultiplied).
2. **Unpremultiply**: `rgb = rgb_premult / max(a, epsilon)`. Required because egui composites the monitor image against its own UI background (including a checkerboard pattern for CAP-021 alpha preview) using standard straight-alpha blending — feeding it premultiplied color would double-darken translucent/edge pixels.
3. **sRGB OETF encode** (not BT.709 — the destination is a desktop-OS-composited egui panel on an sRGB-assumption monitor, not a broadcast reference display; matches the existing app-wide convention of treating the interactive canvas as sRGB, consistent with §4.1's boundary rule that sRGB owns the display/asset domain). Recommend NOT using exact BT.709 OETF here even though the source is video — the mismatch between BT.709-mastered content shown via sRGB encode is the same ~1-2% most NLEs accept for interactive preview (Resolve/Premiere do the same); exact BT.709 stays reserved for scopes (§3.6) and export (§3.5/§4.2), where accuracy is explicitly required (CAP-015/SS-3).
4. Write to an intermediate `Rgba8Unorm` (or `Bgra8Unorm`, matching the negotiated surface format) texture.

Wiring to egui: no existing texture-in-panel plumbing exists in `photonic-gui` today — a repo-wide check found zero `egui_wgpu::Renderer::register_native_texture` call sites; the only `egui_wgpu` usage is `lightfall.rs`'s `CallbackTrait`-based paint callback for a background shader effect (not a registered texture). Spec position: use `egui_wgpu::Renderer::register_native_texture` (the standard, simpler egui integration point) to obtain an `egui::TextureId` for the post-present-pass texture each frame, displayed via `ui.image(...)` in the monitor panel — this is new plumbing this phase must build, not a reuse of an existing pattern. If per-frame texture re-registration overhead becomes measurable (unlikely at 1080p30 per 02 §8's 8ms GPU budget, but flagged for 11), a `lightfall.rs`-style direct paint callback sampling `EngineFrame.texture` in-place is the fallback, avoiding the intermediate `Rgba8Unorm` copy — deferred unless profiling shows it's needed.

---

## 6. Risks & test hooks

| Risk | Mitigation / test hook |
|---|---|
| P1 renderer rework destabilizes vector editing (00 §7 top risk) | §2.6 golden corpus (30-50 fixtures, byte-exact + PSNR≥45dB gate) blocks merge; existing test suite must also stay green |
| Persistent-buffer allocator fragmentation/complexity (§2.3) | Compaction pass at 25% free-space threshold, not per-frame; full-rebuild fallback path kept as safety net (never removed) |
| Revision counter under-wiring is a hidden cross-cutting prerequisite bug (§2.1) — both `doc_generation` (02) and the tessellation cache (§2.2) silently depend on a fix that doesn't exist yet | Single shared fix (bump on execute/undo/redo + public accessor) landed once, first, before either consumer — sequencing note for 12-agent-execution-plan.md |
| BT.709/sRGB curve conflation degrades scope accuracy (CAP-015) invisibly | Unit test: BT.709 EOTF/OETF round-trip at reference values (18% grey, 100% white, BT.709 test chart values from ITU-R BT.709-6 Table); assert against published reference within 1e-4, not visual inspection |
| Dual color convention (CPU gamma-straight-alpha vs GPU linear-premultiplied) diverges silently for documents that mix both (vector-on-timeline case) | §4.4 parity test proves Tier A (CPU-readback) and Tier B (GPU-direct) produce identical pixels for the overlapping (pure-vector) case; Tier A never removed as universal fallback |
| Conversion-pass shaders (YUV→linear, Tier B convert, present-pass encode) added without CI shader validation | All new WGSL passes added to the `wgsl_shaders_parse_and_validate` table (`pipeline.rs:924-941` pattern) — no GPU required for this gate |
| New conversion passes blow the 8ms GPU eval budget (02 §8: "Eval 1080p, 3 layers + grade + captions < 8ms GPU") | 11-testing-phasing.md adds a per-pass GPU timestamp budget line item for YUV convert, Tier B convert, and present-pass, measured independently of the existing 8ms figure |
| `photonic-core::color` consolidation (§3.3) touches raster code with existing behavior-locked tests | Land as pure refactor (function bodies unchanged, only moved + deduplicated) in the same P1 window, verified by existing raster test suite passing unchanged |
| No GPU-capable CI runner today (existing blend tests already skip without one, `headless.rs:1565`) | Flagged as a coverage gap in 11-testing-phasing.md, not silently accepted — golden-corpus comparisons and the new shader-validation tests must run somewhere with a GPU before merge sign-off |
