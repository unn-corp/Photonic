# 15 — Clip Thumbnails & Audio Waveforms (parity: NLE gap 10)

> **Status: Delivered.** This file records the shipped thumbnail/waveform contract. Live regression protection and remaining backlog are tracked in [ROADMAP.md](ROADMAP.md).

**Why:** the single biggest visual-parity gap vs Premiere/CapCut/Resolve. Clips are flat colored rectangles; reference NLEs render frame thumbnails along video clips and a min/max waveform along audio clips, so a user reads their timeline at a glance. Seam already flagged at `crates/photonic-gui/src/app/timeline/clips.rs:132-133`.

**Scope:** render thumbnails on video clips and waveforms on audio clips in the timeline, backed by background-generated, sidecar-cached data. No blocking on the render thread.

## 1. Data sources (already exist — reuse, don't rebuild)

- **Audio waveform:** `photonic_video::audio::waveform` builds a min/max+RMS multi-resolution peak pyramid and caches it in the project sidecar dir (`<project>.photon.cache/`, keyed by asset content hash). `WaveformPyramid::{build_pyramid, save_to_dir, load_from_dir}` exist. The timeline just needs to *load* the pyramid for an audio clip's asset and pick the pyramid level whose bucket ≈ 1–2 px at current zoom.
- **Video thumbnails:** `photonic_video::decode::DecodeSource` + the keyframe index decode frames; the `TexturePool` / `photonic_render::video` upload path exists. Add a thumbnail cache (low-res JPEG/PNG per sampled source-time) in the sidecar dir keyed by `(asset_hash, source_tick_bucket)`.

## 2. New: thumbnail cache (`photonic-video`)

`photonic-video/src/media/thumbnails.rs` (new):
- `ThumbnailCache` — LRU in-memory (bounded, e.g. 256 entries) over decoded low-res RGBA thumbnails, backed by disk in the sidecar dir. Key: `(AssetId, source_tick rounded to a coarse bucket, target_px height ~64)`.
- `request(asset, source_tick, px) -> Option<ThumbHandle>` — returns a cached thumbnail immediately if present; otherwise enqueues a background decode job (reuse the decode worker/sidecar, one frame, scaled to ~114×64) and returns `None` this frame. Never blocks.
- Background worker decodes via a short-lived `DecodeSource` seek+one-frame (the keyframe index makes this cheap) → downscale → store to LRU + disk.
- Expose thumbnails to the GUI as either raw RGBA the GUI uploads to an egui texture, or (simpler) pre-registered `egui::TextureId`s via the shared engine device. Pick the raw-RGBA route to keep `photonic-video` egui-free; the GUI registers textures.

## 3. GUI rendering (`photonic-gui/src/app/timeline/clips.rs`)

At the flagged seam (~clips.rs:132), per visible clip:
- **Video clip:** sample thumbnails across the clip's width at a fixed spacing (e.g. one every ~120 px). For each sample x, map to source tick (`source_in + (x−clip_start)*speed`), call `ThumbnailCache::request`; draw the returned thumbnail into that horizontal slice of the clip rect. Missing thumbnails draw the current flat fill (graceful). Bound the number of thumbnail texture registrations per frame (budget ~ visible-clip-count × slices, cap and log if exceeded — no silent truncation).
- **Audio clip:** load the asset's `WaveformPyramid`; select the level by zoom; draw a filled min/max envelope (two-tone with RMS overlay per DESIGN.md) inside the clip rect behind the name label. If the pyramid isn't built yet, request it in the background (via the engine/session) and draw the flat fill meanwhile.
- Thumbnails/waveforms respect clip trim (`source_in`) and speed.

## 4. Performance & threading

- Generation is ALWAYS background (decode workers / a waveform build thread); the draw path only reads caches. A cache miss draws the flat fill, never stalls.
- Reuse the sidecar cache dir + content-hash keys (01 §9) so thumbnails/waveforms survive project reopen and are rebuildable.
- Add a per-frame registration budget (config const) to bound egui texture churn; the fallback flat-fill absorbs the rest.

## 5. Tests

- `ThumbnailCache` unit: request-miss-enqueues-then-hit (with a fake decode source), LRU eviction, disk round-trip.
- Waveform-level selection: bucket-width → level mapping is monotone; known-ramp pyramid values (reuse audio's existing waveform tests).
- Golden (optional): a timeline-panel render test with a stub thumbnail/waveform source producing a non-flat clip body (GUI logic factored into a pure `clip_thumbnail_slices(clip, view)` fn that's unit-tested for slice positions/source-tick mapping).

## 6. Phasing

Ship audio waveforms first (data already fully exists — lowest risk, high impact), then video thumbnails (needs the new decode-backed cache). Each is an independent commit.
