# Raster image support: ImageNode with embed + link, plus a Links panel (#27) — Design Proposal

> Status: design scaffold (not an implementation).

## Summary

`SceneNodeKind` (node.rs line 238-242) has only `Path`, `Group`, and `Text`. There is no
image node, so raster content cannot be placed. The renderer already imports the `image`
crate (renderer.rs line 14) and wgpu handles textures — the GPU infrastructure is partly
ready. This proposal adds `SceneNodeKind::Image(ImageNode)` with embed/link storage, a
new wgpu texture pipeline, and a Links panel for managing externally linked images.

## Scope

**In:**
- `ImageNode` struct: source (embedded bytes or external path), transform (reuses
  `SceneNode.transform`), crop rect, opacity.
- Embedded mode: PNG/JPEG bytes stored as base64 in `.photon` serialization.
- Linked mode: absolute file path + last-modified timestamp; file read on load/relink.
- GPU render: a new `wgpu::RenderPipeline` (image pipeline) samples a `wgpu::Texture`
  into the MSAA framebuffer with the node transform applied.
- Non-destructive crop: `crop_rect: Option<Rect>` clips the sampled UV coordinates.
- SVG export: `<image>` with `href` as data URI (embedded) or relative file path (linked).
- MCP tools: `place_image` (embed), `place_linked_image`, `relink_image`, `embed_image`,
  `update_image_crop`.
- Links panel (GUI): list all linked images, show missing/modified status, relink, embed.

**Out:**
- Image tracing (milestone M7 dependency, separate epic).
- Pattern fills using an image (separate feature).
- Image effects / adjustments (brightness, contrast, etc.).
- PSD/PDF import (depends on this but is a separate epic).
- Multiple linked-image update on source change at runtime (detect on open, not live-watch).

## Proposed approach

1. **Model** (`photonic-core/src/node.rs`):
   ```rust
   pub enum ImageSource {
       Embedded { data: Vec<u8>, mime: String },  // "image/png" or "image/jpeg"
       Linked { path: String, last_modified: Option<u64> },
   }
   pub struct ImageNode {
       pub source: ImageSource,
       pub crop_rect: Option<(f64, f64, f64, f64)>,  // x, y, w, h in image px
       pub opacity: f64,
   }
   ```
   Add `Image(ImageNode)` to `SceneNodeKind`. Update `bounding_box()` to return crop or
   natural image size × transform.

2. **Texture cache** (`photonic-render/src/renderer.rs`):
   Add `image_textures: HashMap<NodeId, wgpu::Texture>` on `Renderer`. On scene update,
   decode changed `ImageNode` sources (using the already-imported `image` crate) and
   upload to `wgpu::TextureUsages::TEXTURE_BINDING`.

3. **Image pipeline** (`photonic-render/src/pipeline.rs`):
   New `create_image_pipeline()` with a simple UV-mapped quad vertex shader and a
   sampler + texture bind group. Respect MSAA_SAMPLES to match the existing fill pipeline.

4. **Render dispatch** (`photonic-render/src/renderer.rs`, `render()` loop line ~779):
   Add `SceneNodeKind::Image(img_node)` arm that emits a textured quad using the cached
   texture, applying the node transform and crop UV coordinates.

5. **Headless** (`photonic-render/src/headless.rs`): same arm needed in the capture path.

6. **Export** (`photonic-core/src/export.rs`):
   `export_svg` emits `<image x=... y=... width=... height=... href="data:image/png;base64,..."/>`.
   For linked images, write the relative href if the base path is known, else embed.

7. **MCP** (`photonic-mcp/src/handlers/nodes.rs`):
   `place_image` reads a local path, detects PNG/JPEG, embeds bytes.
   `place_linked_image` stores the path.
   `relink_image` updates the path; `embed_image` reads current linked file and promotes to embedded.

8. **Links panel** (`photonic-gui`): list `SceneNodeKind::Image` nodes with `Linked`
   sources; colour-code missing (file not found) vs modified (mtime changed).

## Affected modules (real paths)

- `crates/photonic-core/src/node.rs` — `ImageNode`, `ImageSource`, new `SceneNodeKind` arm
- `crates/photonic-render/src/renderer.rs` — texture cache, render dispatch
- `crates/photonic-render/src/pipeline.rs` — `create_image_pipeline()`
- `crates/photonic-render/src/headless.rs` — capture dispatch
- `crates/photonic-core/src/export.rs` — SVG `<image>` emission
- `crates/photonic-mcp/src/handlers/nodes.rs` — MCP tool handlers
- `crates/photonic-gui` — Links panel, Place Image entry point

## Risks & open questions

- **Large embedded images** balloon `.photon` file size; consider an optional external
  sidecar store or reference-counted blob storage.
- **wgpu texture format**: PNG RGBA vs JPEG RGB — need format conversion on upload.
- **Crop + transform order**: does the crop rect apply in image-local coordinates (before
  transform) or screen coordinates? Industry convention is image-local; confirm.
- **MSAA compatibility**: the image pipeline quad must resolve correctly at MSAA_SAMPLES=4
  alongside the fill pipeline; verify no blend mode conflicts.
- Open: should linked images be watched with `notify` (live reload) or checked only on
  document open? Live watch is nicer but adds complexity.

## Acceptance criteria

- [ ] PNG and JPEG can be placed (embedded), moved, scaled, cropped, and rendered on
      canvas and in headless PNG export.
- [ ] Embedded images survive save/reload (`.photon` round-trip).
- [ ] Linked images load from disk on open; missing links show a placeholder, not a crash.
- [ ] SVG export produces a valid `<image>` element with correct dimensions.
- [ ] `place_image`, `place_linked_image`, `relink_image`, `embed_image` MCP tools work.

## Effort estimate

**XL** — New GPU pipeline, texture cache, model variant, and GUI panel; image decode +
upload path requires care; crop + transform math must be correct across render paths.
