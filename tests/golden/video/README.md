# Video golden-frame corpus (11-testing-phasing.md §1)

CPU-reference snapshot corpus for the video frame-graph pipeline. Each case is a
small timeline (`project.photon`) plus a list of sampled ticks (`meta.toml`); the
harness compiles + evaluates it on the **`eval_cpu` reference** (02 §2 — the
canonical golden source) and diffs the result against the blessed PNGs in
`expected/cpu/`.

This is the **video/timeline** corpus. It is deliberately DISTINCT from
`crates/photonic-render/tests/golden/` (03 §2.6's P1 vector-renderer-equivalence
corpus) — different scope, lifecycle, and owning doc. Do not merge them.

## Layout

```
tests/golden/video/{case}/
  project.photon          # JSON-serialized core TimelineProject (authored by the generator)
  meta.toml               # format_index + named sample frames
  expected/cpu/frame_{n:04}.png   # blessed eval_cpu reference (canonical, 11 §1.3)
```

## Comparison metric (11 §1.2 — both layers required)

| Layer       | Metric       | Threshold (CPU-vs-CPU) |
|-------------|--------------|------------------------|
| Per-channel | max abs diff | ≤ 0.02 (linear-light)  |
| Aggregate   | PSNR         | ≥ 40 dB                |

A mismatch dumps `actual` + an abs-diff heatmap PNG to the temp dir. The GPU
evaluator's looser tier (35 dB / SSIM ≥ 0.98, 11 §1.2) is out of scope here and
lands with `graph::eval`'s headless readback path.

## Cases

`solid_color`, `merge_opaque_over`, `merge_half_opacity`, `merge_screen_blend`
(non-Normal blend via a composition), `transform2d_scaled`, `adjustment_reroot`,
`crop_resize_passthrough` (project-graph Crop→Resize), `opacity_ramp` (keyframed
opacity across three sampled ticks). Together they exercise the P3 `IrOp` set
`eval_cpu` implements: `SolidColor`, `Merge` (over + blend modes), `Transform2D`,
`Crop`/`Resize`, the multi-track fold, and Adjustment re-root.

## Regenerate / bless (11 §1.4 — reviewed diff, never automatic)

```bash
# 1. re-author project.photon + meta.toml from the builder API
cargo test -p photonic-video --test gen_video_golden -- --ignored
# 2. re-bless the reference PNGs from eval_cpu
PHOTONIC_BLESS_GOLDEN=1 cargo test -p photonic-video --test golden_frames -- --test-threads=1
# 3. review before commit — a human confirms the pixels are intended
git diff --stat tests/golden/video/
```

Compare mode (the default CI path, no GPU adapter needed) is just:

```bash
cargo test -p photonic-video --test golden_frames
```

## Budget (11 §1.5)

Commit small PNGs in-repo, no Git LFS; keep `tests/golden/` under **10 MB**
total. Frames are ≤ 160 px. Current corpus is well under budget.
