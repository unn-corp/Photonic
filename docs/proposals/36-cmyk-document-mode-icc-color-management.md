# CMYK document mode + ICC color management (#36) — Design Proposal

> Status: design scaffold (not an implementation).

## Summary

The `Color` struct (`photonic-core/src/color.rs`) is linear sRGB only (f32 r, g, b, a). `SpotColor`
(`document.rs:359`) stores a hex value and overprint flag but has no CMYK decomposition. There is
no ICC profile concept anywhere in the codebase. This issue introduces a document color mode
switch, CMYK storage for colors, ICC profile assignment + conversion (via `lcms2`), soft-proofing
view, and correct spot-color CMYK alternates. It is a dependency for #37 (print output) and
accurate CMYK entry in #35 (color picker).

## Scope

**In scope**
- `DocumentColorMode` enum (RGB | CMYK) on `Document`; new documents default to RGB.
- `ColorValue` enum wrapping either `RgbColor` or `CmykColor` as the stored representation;
  `Color` (linear sRGB) becomes the runtime working/rendering space via conversion.
- ICC profile assignment per document (input profile + output profile); profiles loaded from
  files (`.icc`/`.icm`).
- Color conversion via `lcms2` (C binding) or `lcms2-rs` (safe Rust wrapper).
- Soft-proofing / "Proof Colors" view mode that renders through the output profile.
- `SpotColor.cmyk: Option<(f32, f32, f32, f32)>` — CMYK alternate values for a spot color.
- Export pipelines tag output with correct color space metadata.

**Out of scope**
- Multi-ink channel beyond CMYK (Pantone 5-color, hexachrome).
- On-screen color calibration (display profile is read but color management targets output, not
  display correction).
- Automatic gamut warning UI (deferred).

## Proposed approach

1. **Model** (`crates/photonic-core/src/document.rs`):
   ```rust
   pub enum DocumentColorMode { Rgb, Cmyk }

   pub struct IccProfile {
       pub name: String,
       pub path: Option<PathBuf>,   // None = built-in sRGB / Generic CMYK
       #[serde(skip)]
       pub handle: Option<Arc<lcms2::Profile>>,
   }

   // On Document:
   pub color_mode: DocumentColorMode,        // default Rgb
   pub input_profile: IccProfile,            // default sRGB IEC 61966-2.1
   pub output_profile: IccProfile,           // default Generic CMYK (for CMYK mode)
   pub soft_proof_enabled: bool,
   ```

2. **`SpotColor`** (`document.rs:359`): add `pub cmyk: Option<[f32; 4]>` (C, M, Y, K ∈ 0.0–1.0).

3. **Color conversion** (new `crates/photonic-core/src/icc.rs`):
   - Wrap `lcms2` (add as workspace dependency; `lcms2` crate).
   - `fn rgb_to_cmyk(r, g, b, output_profile) -> [f32; 4]`
   - `fn cmyk_to_rgb(c, m, y, k, output_profile) -> Color` (returns linear sRGB `Color`)
   - `fn apply_soft_proof(color: Color, output_profile, rendering_intent) -> Color`
   - A thread-local or `Arc<Mutex<>>` transform cache keyed by `(input_profile_id, output_profile_id)`.

4. **Runtime color resolution**: `Color` (linear sRGB) remains the universal *rendering* type.
   In CMYK document mode, when reading a node's fill/stroke `Color`, it is the result of
   `cmyk_to_rgb` on the stored CMYK values. The renderer never sees CMYK directly. This
   minimises blast radius: `photonic-render` is unchanged.

5. **Soft-proofing** (`crates/photonic-render/src/renderer.rs`): add a post-process pass (new
   compute shader or CPU path for correctness first) that converts the final frame through
   `apply_soft_proof` when `Document.soft_proof_enabled`. The pass replaces each pixel's sRGB
   value with the proof-corrected value. For CPU: a `wgpu::Buffer` readback → transform → upload.
   For GPU: a simple fragment shader with LUT derived from the lcms2 transform.

6. **GUI** (`crates/photonic-gui/src/app.rs`): Document Properties panel gains:
   - Color Mode dropdown (RGB / CMYK).
   - Input / Output profile file pickers.
   - "Proof Colors" toggle in View menu (sets `soft_proof_enabled`).
   Color picker (#35) CMYK tab uses real ICC-managed conversion once this lands.

7. **Export**: SVG export (`export.rs`) adds `color-profile` attribute and `<color-profile>`
   element when a custom profile is set. Raster export (`photonic-render`) attaches ICC chunk
   via image crate (PNG already supports it; JPEG via JFIF extension).

8. **MCP** (`crates/photonic-mcp/src/handlers/document.rs`): new tools
   `set_document_color_mode(mode)`, `assign_icc_profile(role, path)`.

## Affected modules

- `crates/photonic-core/src/document.rs` — `DocumentColorMode`, `IccProfile`, `SpotColor.cmyk`,
  `Document.color_mode`, `Document.input_profile`, `Document.output_profile`
- `crates/photonic-core/src/icc.rs` — new: conversion functions, lcms2 wrapper
- `crates/photonic-core/src/color.rs` — `Color` unchanged as rendering type; add doc comments
- `crates/photonic-render/src/renderer.rs` — soft-proof post-process pass
- `crates/photonic-core/src/export.rs` — ICC chunk in SVG/raster exports
- `crates/photonic-gui/src/app.rs` — Document Properties, View > Proof Colors
- `crates/photonic-mcp/src/handlers/document.rs` — `set_document_color_mode`, `assign_icc_profile`
- `Cargo.toml` (workspace) — add `lcms2` dependency

## Risks & open questions

- **lcms2 C dependency**: `lcms2` is a C library; adds a `cc`-based build step and increases
  binary size. Evaluate `lcms2-rs` (safe wrapper) vs. writing a minimal pure-Rust sRGB↔CMYK
  formula for the MVP and adding full lcms2 in a follow-on.
- **Serialization of ICC profiles**: storing the path in the `.photon` file works for local
  files; embedded profiles (binary blob in the save file) is more portable but increases file
  size. Decide embed vs. path-reference policy.
- **Rounding on mode switch**: converting a whole document's colors from RGB to CMYK is lossy.
  Warn users and allow undo; do not auto-convert on every open.
- **Soft-proof GPU path**: CPU readback per frame is a major performance hit at high resolution.
  The GPU LUT approach is correct but requires a 3D LUT texture (17×17×17 or 33×33×33).
- **SpotColor overprint**: `SpotColor.overprint` already exists (`document.rs:367`); CMYK
  alternate must be used when generating separations (#37), not for on-screen rendering.

## Acceptance criteria

- [ ] A new document can be set to CMYK mode; existing documents default to RGB.
- [ ] Colors in CMYK mode store CMYK values and display correctly converted to sRGB on screen.
- [ ] Spot colors accept a CMYK alternate value.
- [ ] Soft-proof view visibly shifts colors when an output profile differs from sRGB.
- [ ] SVG export includes an ICC color-profile element when a non-default profile is assigned.
- [ ] PNG export attaches the ICC chunk.
- [ ] CMYK round-trip: set CMYK values → read back → same values (no silent sRGB clamping).

## Effort estimate

**XL** — this is foundational infrastructure touching the model, renderer, all export pipelines,
and the GUI. lcms2 integration and soft-proofing alone are L-size work; the model + MCP changes
add another M.
