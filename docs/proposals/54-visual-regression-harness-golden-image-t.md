# Visual Regression Harness (Golden-Image Testing for Render + Export) (#54) — Design Proposal

> Status: design scaffold (not an implementation).

## Summary

There is no automated visual correctness check. The render pipeline
(`photonic-render/src/renderer.rs`, `pipeline.rs`, `tessellator.rs`) and the export path
(`photonic-core/src/export.rs`) can silently regress as blend modes, effects, gradients,
and text features land in M2/M4. A `HeadlessRenderer` already exists in
`photonic-render/src/headless.rs` — it initialises a wgpu device, creates an offscreen
texture, and renders to a staging buffer. The missing piece is a fixture corpus and a
perceptual-diff comparison loop wired into CI.

## Scope

**In**
- A Rust test binary (or integration test) that drives `HeadlessRenderer` against a
  corpus of `.photon` fixture documents and diffs the output against committed PNG goldens.
- Fixture coverage: each blend mode, linear/radial/fluid/mesh gradients, stroke styles,
  variable-width strokes, text layout, text-on-path, patterns, image nodes, boolean
  compound paths.
- `UPDATE_GOLDENS=1` environment variable that re-renders and overwrites goldens instead
  of diffing.
- CI job that uploads diff images as artifacts on failure.
- SVG export golden snapshots (text snapshots via `insta`, since SVG is deterministic).

**Out**
- GUI interaction regression testing (no headless egui runner today).
- Performance benchmarks (separate concern from correctness).
- Cross-OS visual parity (GPU driver differences are real; goldens are Linux-only for now).

## Proposed Approach

### 1. Fixture corpus

Create `crates/photonic-render/tests/fixtures/` with one `.photon` JSON file per
feature. Naming convention: `blend_multiply.photon`, `gradient_mesh_2x2.photon`, etc.
Fixtures are minimal — a single artboard with just enough objects to exercise one
feature. Commit them; they are the ground truth inputs.

Corresponding goldens go in `crates/photonic-render/tests/goldens/` as PNG files with
the same stem. This directory is committed and diff'd by the test.

### 2. Harness binary (`crates/photonic-render/tests/visual_regression.rs`)

```rust
// Pseudocode outline — not an implementation
for each fixture in tests/fixtures/*.photon {
    let doc = Document::from_json(fixture_bytes);
    let renderer = HeadlessRenderer::new(512, 512).await; // from headless.rs
    let pixels = renderer.render(&doc, ExportOptions::default()).await;
    if UPDATE_GOLDENS {
        write_png(golden_path, &pixels);
    } else {
        let golden = read_png(golden_path);
        let diff = perceptual_diff(&pixels, &golden, tolerance: 2.0/255.0);
        assert!(diff.max_delta < threshold, "visual regression in {fixture}");
        if diff.max_delta > 0 { write_diff_png(artifact_path, &diff.image); }
    }
}
```

For perceptual diff: use the `image` crate for PNG I/O and compute per-pixel CIEDE2000
or simple max-channel-delta. A tolerance of 2/255 accommodates floating-point rounding
differences; a hard fail at >5/255 catches real regressions. The `dssim` or `lodepng`
crates are alternatives.

### 3. CI integration (`.github/workflows/ci.yml`)

Add a `visual-regression` job (Linux only, separate from `build-test`):

```yaml
visual-regression:
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - name: Install Vulkan software rasterizer
      run: sudo apt-get install -y mesa-vulkan-drivers
    - name: Run visual regression tests
      env:
        WGPU_BACKEND: vulkan   # or "gl" with llvmpipe fallback
      run: cargo test --package photonic-render --test visual_regression
    - name: Upload diff artifacts
      if: failure()
      uses: actions/upload-artifact@v4
      with:
        name: visual-diffs
        path: crates/photonic-render/tests/diffs/
```

The `HeadlessRenderer` uses `wgpu` which can fall back to `llvmpipe` (Mesa software
renderer) on Linux runners without a GPU, sufficient for pixel-level correctness checks.

### 4. `UPDATE_GOLDENS` workflow

Add a repo `workflow_dispatch` job (`update-goldens.yml`) that runs the harness with
`UPDATE_GOLDENS=1`, commits the regenerated goldens, and opens a PR. This keeps goldens
up to date when intentional visual changes land (new font renderer, anti-aliasing tweak).

### 5. SVG export snapshots

For SVG, add `insta`-based snapshot tests in `crates/photonic-core/tests/` (coordinated
with #53). SVG output is deterministic on a given platform so text snapshots are reliable
without GPU.

## Affected Modules

- `crates/photonic-render/src/headless.rs` — primary driver; may need a public
  `render_document(&Document) -> Vec<u8>` helper to simplify test setup
- `crates/photonic-render/tests/visual_regression.rs` — new test binary
- `crates/photonic-render/tests/fixtures/` — new corpus directory
- `crates/photonic-render/tests/goldens/` — new committed PNG directory
- `crates/photonic-render/Cargo.toml` — add `image`, `dssim` (or similar) as dev-deps
- `crates/photonic-core/src/export.rs` — SVG snapshot tests (see #53)
- `.github/workflows/ci.yml` — new job + Vulkan/Mesa dep install

## Risks & Open Questions

- **Driver variance**: wgpu's Vulkan/GL output can differ by 1–3 LSBs between Mesa
  versions. Goldens committed on one Mesa version may spuriously fail on runner upgrades.
  Mitigation: pin `ubuntu-latest` or use a Docker image with a fixed Mesa version; or
  loosen tolerance to 5/255.
- **`HeadlessRenderer` public API**: `headless.rs` may not expose a convenient
  `render_document()` function — it may require building a render pipeline manually.
  A small public wrapper will be needed; take care not to break MCP export paths.
- **Fixture maintenance**: each new visual feature needs a new fixture + golden update.
  Without discipline, the corpus rots. A CI check that lists fixture files with no
  matching golden (and vice versa) catches drift.
- **Build time**: rendering 30–50 fixtures sequentially may be slow. Run tests with
  `--test-threads=1` (GPU resource contention) but parallelize fixture rendering within
  the binary using `tokio::join!`.

## Acceptance Criteria

- [ ] At least one golden fixture per major rendered feature (blend modes, gradient types,
      text, stroke, image, boolean path).
- [ ] CI fails with a diff artifact when a pixel regression exceeds tolerance.
- [ ] `UPDATE_GOLDENS=1 cargo test` regenerates all goldens without manual steps.
- [ ] SVG export has text snapshot tests via `insta`.
- [ ] Tests pass on the Linux CI runner using the Mesa software rasterizer.

## Effort Estimate

**L** — harness infrastructure is M; fixture corpus authoring (one doc per feature) is
the long tail. First delivery: harness + 5 fixtures. Full corpus is incremental.
