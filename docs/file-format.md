# .photon File Format

`.photon` files are plain JSON, human-readable and version-control friendly. This document describes the schema.

---

## Top-level structure

```jsonc
{
  "id": "550e8400-...",          // document UUID
  "name": "My Design",
  "width": 1123.0,              // logical pixels at 96 dpi
  "height": 794.0,
  "layer_order": ["<uuid>", ...],  // render order, bottom → top
  "layers": { "<layer-uuid>": Layer, ... },
  "nodes":  { "<node-uuid>":  SceneNode, ... }
}
```

Default canvas size is A4 landscape at 96 dpi (1123 × 794).

---

## Layer

```jsonc
{
  "id": "<uuid>",
  "name": "Background",
  "visible": true,          // omitted when true
  "locked": false,          // omitted when false
  "opacity": 1.0,           // omitted when 1.0
  "blend_mode": "normal",   // omitted when "normal"
  "node_ids": ["<uuid>", ...]  // draw order within layer, bottom → top
}
```

---

## SceneNode

```jsonc
{
  "id": "<uuid>",
  "name": "body",
  "layer_id": "<layer-uuid>",
  "kind": { ... },           // see below
  "transform": [1,0,0,1,0,0],  // omitted when identity
  "opacity": 1.0,            // omitted when 1.0
  "visible": true,           // omitted when true
  "locked": false,           // omitted when false
  "blend_mode": "normal",    // omitted when "normal"
  "tags": ["body"]           // omitted when empty
}
```

### kind: Path

```jsonc
{
  "type": "path",
  "path_data": "M 0 0 L 100 0 L 50 80 Z",
  "fill": Fill,
  "stroke": Stroke,
  "is_compound": false    // omitted when false
}
```

### kind: Group

```jsonc
{
  "type": "group",
  "children": ["<uuid>", ...],  // draw order, bottom → top
  "clip_children": false         // omitted when false
}
```

### kind: Text

```jsonc
{
  "type": "text",
  "content": "Hello",
  "font_family": "sans-serif",
  "font_size": 48.0,
  "font_weight": "regular",
  "fill": Fill
}
```

---

## Fill

```jsonc
{
  "kind": FillKind,
  "opacity": 1.0,    // omitted when 1.0
  "enabled": true    // omitted when true
}
```

### FillKind: None

```jsonc
{ "type": "none" }
```

### FillKind: Solid

```jsonc
{ "type": "solid", "color": "#3b82f6" }
```

Colors are lowercase hex strings: `#rrggbb` or `#rrggbbaa`.

### FillKind: Gradient

```jsonc
{
  "type": "gradient",
  "kind": "linear",       // or "radial"
  "stops": [
    { "offset": 0.0, "color": "#ff0000" },
    { "offset": 1.0, "color": "#0000ff" }
  ],
  // linear: [x0, y0, x1, y1] in node-local coordinates
  // radial: [cx, cy, fx, fy, r]
  "coords": [0.0, 0.0, 100.0, 0.0]
}
```

### FillKind: FluidGradient

IDW (inverse distance weighting) gradient evaluated from free-placed control points.

```jsonc
{
  "type": "fluid_gradient",
  "points": [
    { "x": 10.0, "y": 10.0, "color": "#ff0000" },
    { "x": 90.0, "y": 90.0, "color": "#0000ff" }
  ],
  "power": 2.0    // IDW exponent; higher = harder transitions
}
```

### FillKind: MeshGradient

Bilinear interpolation over a rows × cols grid of color vertices.

```jsonc
{
  "type": "mesh_gradient",
  "rows": 2,
  "cols": 2,
  "vertices": [
    { "x": 0.0,   "y": 0.0,   "color": "#ff0000" },
    { "x": 100.0, "y": 0.0,   "color": "#00ff00" },
    { "x": 0.0,   "y": 100.0, "color": "#0000ff" },
    { "x": 100.0, "y": 100.0, "color": "#ffff00" }
  ]
}
```

Vertices are row-major: row 0 left→right, then row 1 left→right, etc.

---

## Stroke

```jsonc
{
  "color": "#000000",
  "width": 2.0,
  "line_cap": "butt",      // "butt" | "round" | "square"
  "line_join": "miter",    // "miter" | "round" | "bevel"
  "dash_array": [],        // omitted when empty
  "miter_limit": 4.0,      // omitted when default
  "enabled": true          // omitted when true
}
```

---

## Transform

Stored as a 6-element array representing a 2-D affine matrix `[a, b, c, d, e, f]`:

```
| a  c  e |
| b  d  f |
| 0  0  1 |
```

Identity `[1, 0, 0, 1, 0, 0]` is omitted from serialization.

**Common transforms**

| Operation | Array |
|---|---|
| Translate (tx, ty) | `[1, 0, 0, 1, tx, ty]` |
| Scale (sx, sy) | `[sx, 0, 0, sy, 0, 0]` |
| Rotate θ | `[cos θ, sin θ, -sin θ, cos θ, 0, 0]` |

---

## Blend modes

One of: `"normal"`, `"multiply"`, `"screen"`, `"overlay"`, `"darken"`, `"lighten"`, `"color_dodge"`, `"color_burn"`, `"hard_light"`, `"soft_light"`, `"difference"`, `"exclusion"`, `"hue"`, `"saturation"`, `"color"`, `"luminosity"`

---

## Coordinate system

- Origin `(0, 0)` is the **top-left** of the canvas.
- **X increases right**, **Y increases down**.
- Units are logical pixels at 96 dpi.
- Node transforms are applied in local space before layer ordering.

---

## Minimal example

```json
{
  "id": "11111111-0000-0000-0000-000000000000",
  "name": "Example",
  "width": 400.0,
  "height": 300.0,
  "layer_order": ["aaaa0000-0000-0000-0000-000000000000"],
  "layers": {
    "aaaa0000-0000-0000-0000-000000000000": {
      "id": "aaaa0000-0000-0000-0000-000000000000",
      "name": "Layer 1",
      "node_ids": ["bbbb0000-0000-0000-0000-000000000000"]
    }
  },
  "nodes": {
    "bbbb0000-0000-0000-0000-000000000000": {
      "id": "bbbb0000-0000-0000-0000-000000000000",
      "name": "rect",
      "layer_id": "aaaa0000-0000-0000-0000-000000000000",
      "kind": {
        "type": "path",
        "path_data": "M 50 50 L 350 50 L 350 250 L 50 250 Z",
        "fill": { "kind": { "type": "solid", "color": "#3b82f6" } },
        "stroke": { "color": "#000000", "width": 1.0 }
      }
    }
  }
}
```
