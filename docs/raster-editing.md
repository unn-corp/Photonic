# Raster Image Editing

Photonic began life as a **vector** editor (paths, booleans, gradients, text).
This document specifies the **raster** subsystem that brings Photoshop-grade
pixel editing into the same document, the same scene graph, the same MCP tool
surface, and the same undo history — so a single `.photon` file can hold a
retouched photograph *and* vector annotation on top of it.

The north star: **a Photoshop user should be able to do their core work here**
— place a photo, run adjustments and filters, paint with brushes, make
selections, mask non-destructively, and export — and have the result be
indistinguishable from what Photoshop would produce.

---

## 1. Design principles

1. **One scene graph.** Raster content is a new `SceneNodeKind::Raster` node,
   peer to `Path`, `Group`, and `Text`. It carries the same `transform`,
   `opacity`, `blend_mode`, `visible`, `locked`, `tags`, and effects every other
   node has. Layer ordering, grouping, z-order, alignment, and clipping all work
   on raster nodes for free.

2. **CPU pixel engine, GPU display.** All pixel algorithms live in
   `photonic-core::raster` as pure, deterministic, unit-tested CPU code with no
   GPU or windowing dependency (mirrors the rest of `photonic-core`). The GPU is
   used only to *display* the resulting pixels as a textured quad.

3. **8-bit RGBA, straight alpha, sRGB.** Matches Photoshop's default 8-bit mode
   and the rest of Photonic's color model. A higher-precision (f32 linear)
   working mode is a documented future phase; every adjustment already converts
   to `f32` internally per-op, so precision is an internal upgrade, not an API
   change.

4. **Reuse, don't reinvent.** The 16 `BlendMode` variants already match the CSS
   / Photoshop compositing set 1:1 — the CPU compositor implements that exact
   math. Undo/redo reuses the existing `Command::UpdateNode { old, new }`: every
   pixel edit swaps the node's image buffer and is undoable with zero new
   history machinery.

5. **MCP-first.** Photonic is an AI-driven editor; the raster surface is exposed
   as MCP tools so Claude can retouch and composite programmatically. GUI tools
   (brush, marquee, etc.) are layered on the same core ops.

---

## 2. Photoshop capability map

What "indistinguishable from Photoshop" decomposes into, and where each piece
lives. ✅ = in this implementation, ◑ = partial, ○ = documented future phase.

### Image > Adjustments  → `raster::adjust`
| Photoshop | Status | Function |
|---|---|---|
| Brightness/Contrast | ✅ | `brightness_contrast` |
| Levels (in/out, gamma, per-channel) | ✅ | `levels` |
| Curves | ✅ | `curves` (monotonic spline over control points) |
| Exposure | ✅ | `exposure` |
| Hue/Saturation/Lightness | ✅ | `hue_saturation` |
| Color Balance | ✅ | `color_balance` (shadows/mids/highlights) |
| Vibrance | ✅ | `vibrance` |
| Black & White / Desaturate | ✅ | `desaturate`, `black_and_white` (channel mix) |
| Invert | ✅ | `invert` |
| Posterize | ✅ | `posterize` |
| Threshold | ✅ | `threshold` |
| Photo Filter | ✅ | `photo_filter` |
| Channel Mixer | ✅ | `channel_mixer` |
| Gradient Map | ✅ | `gradient_map` |
| Selective Color | ✅ | `selective_color` |
| Shadows/Highlights | ✅ | `shadows_highlights` |
| Auto Tone / Contrast / Color | ✅ | `auto_levels`, `auto_contrast` |
| Color Lookup (3D LUT) | ○ | phase 3 |

### Filter menu  → `raster::filter`
| Photoshop | Status | Function |
|---|---|---|
| Gaussian Blur | ✅ | `gaussian_blur` (separable) |
| Box / Average Blur | ✅ | `box_blur` |
| Motion Blur | ✅ | `motion_blur` |
| Sharpen / Sharpen More | ✅ | `sharpen` |
| Unsharp Mask | ✅ | `unsharp_mask` |
| Smart Sharpen | ✅ | `advanced::smart_sharpen` |
| Median / Despeckle | ✅ | `median` |
| Add Noise | ✅ | `add_noise` |
| Reduce Noise | ✅ | `advanced::reduce_noise` (bilateral) |
| Emboss | ✅ | `emboss` |
| Find Edges (Sobel) | ✅ | `find_edges` |
| Pixelate / Mosaic | ✅ | `mosaic` |
| High Pass | ✅ | `high_pass` |
| Surface Blur / Bilateral | ✅ | `advanced::surface_blur` |
| Lens Blur (bokeh) | ✅ | `advanced::lens_blur` |
| Clarity / Vignette / Chromatic Aberration | ✅ | `advanced::*` |
| Liquify (push/twirl/pucker/bloat) | ✅ | `warp::liquify_*` |
| Distort (pinch/spherize/ripple/perspective) | ✅ | `warp::*` |
| Lens Blur (depth-map) | ○ | phase 3 |

### Tools (brush family)  → `raster::brush`
| Photoshop | Status | Function |
|---|---|---|
| Brush (round, hardness, flow, opacity, spacing) | ✅ | `Brush` + `stroke` |
| Pencil (hard edge) | ✅ | `Brush { hardness: 1.0 }` |
| Eraser | ✅ | `erase` |
| Clone Stamp | ✅ | `clone_stamp` |
| Healing Brush / Spot Healing | ✅ | `repair::healing_brush`, `repair::spot_healing` (frequency separation) |
| Content-Aware Fill | ✅ | `repair::content_aware_fill` (onion-peel inpaint) |
| Red-Eye / Dust & Scratches | ✅ | `repair::red_eye`, `repair::dust_and_scratches` |
| Smudge | ✅ | `smudge` |
| Dodge / Burn | ✅ | `dodge`, `burn` |
| Sponge (saturate/desaturate) | ✅ | `sponge` |
| Paint Bucket (flood fill) | ✅ | `bucket_fill` |
| Gradient tool | ✅ | `gradient_fill` |
| Blur / Sharpen tool | ◑ | localized filter via mask |

### Selections & masks  → `raster::mask`
| Photoshop | Status |
|---|---|
| Rectangular / Elliptical Marquee | ✅ `Mask::rect`, `Mask::ellipse` |
| Lasso (polygon) | ✅ `Mask::polygon` |
| Magic Wand (flood by tolerance) | ✅ `Mask::magic_wand` |
| Select by Color Range | ✅ `Mask::color_range` |
| Feather | ✅ `Mask::feather` (gaussian) |
| Grow / Contract | ✅ `Mask::grow`, `Mask::contract` |
| Invert / Add / Subtract / Intersect | ✅ `Mask` boolean ops |
| Layer mask (non-destructive) | ✅ `RasterNode.mask` |
| Vector mask | ◑ reuse clip group; raster rasterizes path → mask |
| Quick Select / Select Subject (ML) | ○ phase 3 |

### Geometry / canvas  → `raster::geometry`
| Photoshop | Status |
|---|---|
| Image Size (resample) | ✅ `resize` (bilinear/Lanczos) |
| Canvas Size | ✅ `resize_canvas` |
| Crop | ✅ `crop` |
| Rotate 90/180, Flip H/V | ✅ `rotate90`, `flip_*` |
| Free rotate / arbitrary transform | ✅ via node `transform` (sampled at composite) |
| Content-Aware Fill / Scale | ○ phase 3 |

### Layers & compositing
| Photoshop | Status |
|---|---|
| Raster layers | ✅ `Raster` node |
| 16 blend modes | ✅ `raster::blend` (matches `BlendMode`) |
| Layer opacity | ✅ node `opacity` |
| Layer masks | ✅ `RasterNode.mask` |
| Clipping masks | ✅ existing group clip |
| Adjustment layers (non-destructive) | ✅ `RasterNode.adjustment` (`AdjustmentSpec`), re-applied to the composite beneath in the export compositor; `create_adjustment_layer` MCP tool |
| Smart Objects | ○ phase 3 (vector/raster encapsulation) |
| Layer styles (drop shadow, glow…) | ◑ existing node glow effects apply |

---

## 3. Data model

### `RasterImage` (`raster/image.rs`)
```rust
pub struct RasterImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA8, straight alpha, row-major, len = w*h*4
}
```
Serialized as `{ "width", "height", "png": "<base64>" }` — PNG-compressed via
the `image` crate, base64 for JSON transport. Keeps `.photon` files text and
diffable while staying compact.

### Encoded raster import limits

All encoded raster inputs — including persisted PNG data and the GUI/MCP image
and pattern tools — pass through `RasterImage::from_encoded`. To keep a
compressed image from expanding into an unbounded pixel buffer, Photonic
rejects imports wider or taller than **16,384 px** or whose decoder allocation
budget exceeds **256 MiB**. The policy applies before Photonic materializes the
RGBA8 raster buffer.

### `Mask` (`raster/mask.rs`)
```rust
pub struct Mask { pub width: u32, pub height: u32, pub data: Vec<u8> } // 8-bit coverage
```
0 = fully masked, 255 = fully selected. Used both as a transient **selection**
and as a persisted **layer mask** (`RasterNode.mask`).

### `RasterNode` (`node.rs`)
```rust
pub struct RasterNode {
    pub image: RasterImage,
    pub mask: Option<Mask>,      // non-destructive layer mask
    pub source_uri: Option<String>, // original file, for relink/re-export
}
```
Added as `SceneNodeKind::Raster(RasterNode)`. The image occupies local rect
`[0,0,width,height]`; the node `transform` places/scales/rotates it in document
space, exactly like a path's geometry. `local_bounds` returns that rect so
align/distribute/bounding-box tools work unchanged.

### File format
`CURRENT_FORMAT_VERSION` → 2. The new enum variant is additive: v1 files never
contain raster nodes, so they load unchanged; the v1→v2 migration is a no-op
version bump. `COMPAT_WINDOW = 1` lets v2 files open leniently on a v1 build
(raster nodes dropped) rather than hard-failing.

---

## 4. Compositing

`raster::blend::composite(base, top, x, y, opacity, mode, mask)` alpha-composites
`top` onto `base` (straight-alpha "source-over") with one of the 16 blend modes,
a global opacity, and an optional coverage mask. Separable modes (Multiply,
Screen, Overlay, …) run per channel; non-separable modes (Hue, Saturation,
Color, Luminosity) operate on the RGB triple in HSY space — the same formulas
the SVG/CSS compositing spec defines, so on-screen, exported, and Photoshop
results agree.

**Export** (`photonic-render::headless`): the vector stack is GPU-rendered as
today; raster nodes are CPU-composited into the readback buffer in draw order.
v1 limitation: raster and vector planes composite as two ordered groups
(rasters below the vector plane composite under it, those above composite over);
fully interleaved per-node z-ordering between raster and vector is phase 2 (a
unified CPU rasterizer over the existing lyon meshes). For raster-first
documents — the Photoshop use case — this is exact.

**Display** (`photonic-render::renderer`): a textured-quad pipeline uploads each
`RasterImage` as a `wgpu::Texture` and draws it as two triangles transformed by
the node matrix and camera, with the node's blend/opacity. Textures are cached
by node id + content hash so painting re-uploads only the edited node.

---

## 5. MCP surface (`handlers/raster.rs`)

All mutating tools lock the document, build a `Command` (`AddNode` to place,
`UpdateNode` to edit), and run it through `CommandHistory` for undo.

| Tool | Action |
|---|---|
| `place_image` | decode PNG/JPEG/WebP (path or base64) → new `Raster` node |
| `create_raster_layer` | blank transparent raster node at a size |
| `rasterize_node` | bake a vector/text node into pixels |
| `apply_adjustment` | `{ node_id, kind, params, selection? }` → `raster::adjust` |
| `apply_filter` | `{ node_id, kind, params, selection? }` → `raster::filter` |
| `brush_stroke` | `{ node_id, points[], brush, color }` → paint |
| `bucket_fill` / `gradient_fill` | fill ops |
| `make_selection` | build a `Mask` (rect/ellipse/polygon/wand/color range) |
| `apply_layer_mask` / `clear_mask` | attach/detach `RasterNode.mask` |
| `transform_image` | crop / resize / resize_canvas / rotate / flip |
| `get_raster_info` | dims, has-mask, histogram |

Each adjustment/filter takes an optional `selection` mask so edits are confined
to a selection, exactly like Photoshop.

---

## 6. Phasing

- **Phase 1 (this change):** core CPU engine (image, blend, adjust, filter,
  mask, brush, geometry) fully unit-tested; `Raster` node + serialization +
  format v2; full MCP surface; export compositing; GUI texture display + a
  brush/marquee tool.
- **Phase 2:** non-destructive adjustment layers with live recompositing;
  unified CPU rasterizer for exact raster/vector z-interleaving; f32/linear
  working mode; brush dynamics (pressure, scatter, dual brush, custom tips).
- **Phase 3:** content-aware fill/scale, healing, Liquify, surface/lens blur,
  3D-LUT color lookup, ML-assisted selection (Select Subject), Smart Objects.

---

## 7. Crate impact

| Crate | Change |
|---|---|
| `photonic-core` | new `raster/` module; `Raster` node kind; `image`+`base64` deps; format v2 |
| `photonic-render` | textured-quad pipeline; raster compositing in headless export |
| `photonic-mcp` | `handlers/raster.rs`; dispatch + `tool_list` entries |
| `photonic-gui` | `Raster`/brush/marquee tools; texture upload in viewport |
</invoke>
