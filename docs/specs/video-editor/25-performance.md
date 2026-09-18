# 25 — Interactive Performance (Linux + Windows)

**Status:** Working contract for top-tier scrub/play  
**Date:** 2026-07-20  
**Depends on:** [02-engine.md](02-engine.md) §3–8, [24-preview-media-load.md](24-preview-media-load.md) §5–6

## 1. Targets (product)

| Path | Target |
|------|--------|
| GUI play repaint | ≤ ~120 Hz (`request_repaint_after(8ms)`) |
| Engine poll while playing | 4 ms (≤ 250 Hz wake; evaluate still frame-rate gated) |
| Decode ring (preview) | 24 forward / 6 back frames |
| Prefetch pump batch | 6 frames / tick |
| Prefetch ahead | 12 frames |
| Cut-ahead lead | 24 frames (≥ 500 ms @ 30 fps) |
| Live decode sidecars | ≤ 8 (LRU) |
| Proxy encode priority | Below-normal (Unix nice 10 / Windows `BELOW_NORMAL`) |

## 2. Build profiles

| Profile | Use |
|---------|-----|
| `cargo build --release` | Shipping: thin LTO, opt-level 3, strip debuginfo |
| `cargo build --profile release-perf` | Max speed: fat LTO, codegen-units=1 |
| `cargo build --profile dev-opt` | Local profiling: opt-level 2 + debug |

## 3. Tests

- `crates/photonic-video/tests/perf_hotpath.rs` — ring/prefetch depths, coalesce 500 cmds, compile p95, cut-ahead scan cost  
- `seek_budgets` — ring depth constants  
- Engine unit tests still green  

## 4. Platform notes

- **Linux:** engine/GUI share wgpu Vulkan or GL; proxy ffmpeg `setpriority(10)`.  
- **Windows:** same code paths; proxy ffmpeg `BELOW_NORMAL_PRIORITY_CLASS`; dual-input export uses temp file (no FIFO).  
- Prefer `WGPU_BACKEND=vulkan` when GL surface init fails under Xvfb.
