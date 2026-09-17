# Architecture

Photonic is a five-crate Cargo workspace. Each crate has a single responsibility and a clear dependency direction:

```
photonic-app
    ├── photonic-gui
    │       └── photonic-render
    │               └── photonic-core
    └── photonic-mcp
            └── photonic-core
```

`photonic-core` has no internal dependencies. `photonic-render` and `photonic-mcp` each depend only on core. `photonic-gui` depends on render. `photonic-app` ties everything together.

---

## photonic-core

Business logic and all persistent data types.

### Document model

```
Document
 ├── layer_order: Vec<LayerId>        (render order, bottom → top)
 ├── layers: HashMap<LayerId, Layer>
 └── nodes: HashMap<NodeId, SceneNode>
```

Every `SceneNode` carries:

| Field | Type | Notes |
|---|---|---|
| `id` | `Uuid` | |
| `name` | `String` | |
| `layer_id` | `LayerId` | owning layer |
| `kind` | `SceneNodeKind` | `Path`, `Group`, `Text`, or `Raster` |
| `transform` | `Transform` | 2-D affine, identity by default |
| `opacity` | `f32` | 0.0–1.0 |
| `visible` | `bool` | |
| `locked` | `bool` | UI-only protection |
| `blend_mode` | `BlendMode` | 15 CSS composite modes |
| `tags` | `Vec<String>` | semantic labels for AI queries |

**`SceneNodeKind` variants**

- `Path(PathNode)` — a `PathData` (SVG path string wrapped around a `kurbo::BezPath`) plus `Fill`, `Stroke`, and `is_compound` flag.
- `Group(GroupNode)` — ordered `children: Vec<NodeId>` and optional `clip_children` flag.
- `Text(TextNode)` — content string and basic typography properties.
- `Raster(RasterNode)` — a `RasterImage` (RGBA8 pixel buffer) plus an optional non-destructive layer `mask` and `source_uri`. The Photoshop-grade raster engine lives in `photonic-core::raster` (see [raster-editing.md](raster-editing.md)).

### Transform

A 2-D affine matrix stored as `[a, b, c, d, e, f]`:

```
| a  c  e |
| b  d  f |
| 0  0  1 |
```

Key methods: `translate`, `scale`, `rotate`, `scale_around`, `rotate_around`, `then` (composition), `apply` (point transform), `to_kurbo`.

### Fill system

```
Fill
 ├── kind: FillKind
 ├── opacity: f32
 └── enabled: bool

FillKind
 ├── None
 ├── Solid(Color)
 ├── Gradient        — linear or radial, stop list, coordinate pair
 ├── FluidGradient   — IDW interpolation from free-placed control points
 └── MeshGradient    — rows×cols bilinear grid
```

`Stroke` carries color, width, line cap/join, dash array, and miter limit.

### Command history

Every mutation is wrapped in a `Command` enum value and pushed onto `CommandHistory`:

```
Command
 ├── AddNode / RemoveNode / UpdateNode
 ├── AddLayer / RemoveLayer / ReorderLayers / SetActiveLayer
 ├── ReorderNode
 ├── GroupNodes / UngroupNodes
 └── Batch(Vec<Command>)
```

`CommandHistory` maintains separate undo and redo stacks (default max 200 steps). Named snapshots (`create_checkpoint` / `restore_checkpoint`) store full document clones.

### Boolean path operations (`ops/boolean.rs`)

`boolean_op(a: &PathData, b: &PathData, op: BoolOp) -> Result<PathData>`

Flattens both paths to `geo::MultiPolygon` using Lyon's flattening, applies the set operation, then converts the result back to a `kurbo::BezPath`. Operations: `Union`, `Intersect`, `Subtract`, `Exclude`.

### Export (`export.rs`)

`export_svg(doc)` walks the document in draw order and emits SVG, writing gradient definitions into `<defs>`. Transforms serialize as `matrix(a,b,c,d,e,f)`. PNG/JPEG export routes through `HeadlessRenderer`.

---

## photonic-render

GPU rendering using wgpu. No business logic — takes a `Document` reference and draws it.

### PhotonicRenderer

Owns the wgpu surface, device, queue, and render pipelines. On each frame:

1. Acquire surface texture.
2. If the document changed, tessellate all visible path nodes (fill + stroke) into a combined vertex buffer. Result is cached — reused on lock contention.
3. Record the fill render pass (camera uniform, vertex buffer, MSAA resolve).
4. Record the egui overlay pass.
5. Submit and present.

MSAA is 4× for clean vector edges. Alpha blending uses standard over-compositing.

### HeadlessRenderer

Off-screen GPU renderer with no window surface. Used for PNG/JPEG/ICO export. `render_to_buffer(doc, width, height)` returns raw RGBA bytes.

### Tessellator

Converts `PathData` → lyon triangle meshes:

- **Fill**: `FillTessellator` at tolerance 0.1
- **Stroke**: `StrokeTessellator` with configurable line caps, joins, miter limits

### CanvasView (2-D camera)

```
pan_x, pan_y: f64   // screen-space offset
zoom: f64           // scale factor
width, height: u32  // viewport pixel dimensions
```

Methods: `pan`, `zoom`, `fit_to_rect`, `screen_to_document`, `document_to_screen`.

### WGSL shader pipeline

The fill pipeline is a simple 2-D shader. Vertex stage transforms position by a column-major 4×4 camera matrix (NDC mapping). Fragment stage outputs vertex color (pre-multiplied alpha).

---

## photonic-gui

Immediate-mode GUI (egui) running on top of the wgpu surface.

### PhotonicApp state

Holds:
- Active `Tool` enum value
- Currently selected node ID
- Drag origin + offset for move operations
- Polygon/star parameter cache
- Active fill/stroke color for new shapes
- Pen path accumulation (anchor points collected until path is closed)
- Shape Builder selection state

### Tools

```
Tool
 ├── Select          — click to select, drag to move
 ├── DirectSelect    — edit bezier anchors and handles
 ├── Pan             — drag viewport
 ├── Rectangle
 ├── Ellipse
 ├── Polygon
 ├── Star
 ├── Pen             — click to place anchors; Enter/double-click to close
 └── ShapeBuilder    — drag across shapes; boolean ops from context menu
```

### Panels

Each panel function returns an `Option<PanelAction>`:

```
PanelAction
 ├── ReorderNode(NodeId, ZOrderOp)
 ├── BooleanOp(NodeId, NodeId, BoolOp)
 ├── RestoreCheckpoint(CheckpointId)
 ├── UpdateNodeFill(NodeId, Fill)
 └── UpdateNodeStroke(NodeId, Stroke)
```

`PhotonicApp::update()` dispatches `PanelAction` values to `CommandHistory`.

---

## photonic-mcp

JSON-RPC 2.0 server (HTTP POST) built on axum. All handler functions are async and take a shared `AppState`.

### AppState

```rust
struct AppState {
    document: Arc<Mutex<Document>>,
    history:  Arc<Mutex<CommandHistory>>,
    capture_tx: Sender<ScreenshotRequest>,  // to render thread
    config: McpServerConfig,
}
```

### Request/response envelope

```json
// Request
{ "jsonrpc": "2.0", "id": 1, "method": "create_shape", "params": { ... } }

// Success
{ "jsonrpc": "2.0", "id": 1, "result": { ... } }

// Error
{ "jsonrpc": "2.0", "id": 1, "error": { "code": -32602, "message": "..." } }
```

### Handler modules

| Module | Responsibility |
|---|---|
| `handlers/nodes.rs` | Create, update, delete, group, reorder, boolean ops, paths, text, effects |
| `handlers/document.rs` | Document state, checkpoints, symbols, swatches, styles, variables, width profiles |
| `handlers/layers.rs` | Layer create/delete/reorder/merge, visibility, blend mode |
| `handlers/canvas.rs` | Screenshot, fit/center, artboard margins, bleed, canvas resize |
| `handlers/transforms.rs` | Apply transform, align, distribute, mirror/scatter/rotate copies |
| `handlers/annotations.rs` | Non-printing annotations and dimensions |
| `handlers/audit.rs` | Audit-log query and export |
| `handlers/clipboard.rs` | Copy/paste nodes, clipboard history |
| `handlers/color_guide.rs` | Color-harmony guide and palette suggestions |
| `handlers/raster.rs` | Pixel editing: place/create raster layers, adjustments, filters, brush stroke, bucket/gradient fill, image transform, layer masks, raster info (selection-confined; routed through `Command::UpdateNode` for undo) |

The full tool surface is **auto-generated** — see [mcp-api.md](mcp-api.md),
regenerated from `server::tool_list()` via
`cargo run -p photonic-mcp --bin dump_tools | python3 tools/gen-mcp-docs.py`.

---

## photonic-app

Binary entry point. Reads CLI arguments and selects a mode:

| Mode | Command | Behaviour |
|---|---|---|
| GUI (default) | `photonic [file.photon]` | Full window + egui + wgpu |
| MCP server | `photonic mcp [--port N]` | Headless tokio HTTP server |
| Lua REPL | `photonic repl` | Interactive scripting |
| MCP proxy | `photonic proxy` | CLI client to remote MCP |

In GUI mode the MCP server runs on a separate tokio task sharing `Arc<Mutex<Document>>` with the render thread. Screenshots are requested via a oneshot channel from the MCP task to the render thread.

Logging goes to `%APPDATA%\Photonic\photonic.log` via a synchronous file appender. A blank A4 artboard (1123 × 794 px at 96 dpi) is created when no file is provided.

---

## Concurrency model

```
main thread  ─────────  winit event loop + wgpu rendering
                               │
                  Arc<Mutex<Document>>
                  Arc<Mutex<CommandHistory>>
                               │
tokio thread ─────────  axum HTTP + MCP handlers
```

Screenshot requests travel via a `tokio::sync::oneshot` channel: the MCP handler sends a request; the render thread captures the next frame and sends back PNG bytes.

---

## Error handling conventions

| Layer | Convention |
|---|---|
| `photonic-core` | `Result<T, String>` |
| `photonic-app` | `anyhow::Result<T>` |
| MCP JSON-RPC | Structured `{ code, message }` error objects |

---

## Serialization strategy

- Human-readable JSON everywhere (`.photon` files are plain text).
- Serde `skip_serializing_if` prunes defaults: identity transforms, `opacity = 1.0`, `visible = true`, empty tag lists.
- Path data is stored as an SVG path string and parsed back to `kurbo::BezPath` on demand.
