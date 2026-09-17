# [EPIC] Multiple artboards (#26) — Design Proposal

> Status: design scaffold (not an implementation).

## Summary

`Document` carries a single canvas (`width`/`height` on `Document`, line 443-444 of
`document.rs`). A `Page` struct at line 910-915 is explicitly "future use" and holds a
full nested `Document`, which is too heavy. This proposal introduces a first-class
`Artboard` list on `Document` with per-artboard rect/name/margins, migrates the existing
single canvas to a default artboard, and extends render, export, and MCP accordingly.

## Scope

**In:**
- `Artboard` struct: `id`, `name`, `x`, `y`, `width`, `height`, optional `margin_*`,
  `bleed_mm`, `slug_mm` (move current doc-level fields here).
- `Document.artboards: Vec<Artboard>` and `Document.active_artboard_id: Option<Uuid>`.
- Migration: on load of a pre-artboard `.photon`, synthesise one default artboard from
  the existing `doc.width`/`doc.height` (format_version bump).
- Global coordinate system: nodes live in document-global space; artboard is a named
  viewport rect (no per-artboard origin transform).
- Renderer: draw artboard background frames and crop/cull nodes outside each artboard
  during headless capture.
- Per-artboard export: headless render (`photonic-render/src/headless.rs`) respects an
  optional `artboard_id` parameter to render only that artboard's rect.
- MCP tools: `create_artboard`, `update_artboard`, `delete_artboard`, `list_artboards`,
  `export_artboard`.
- Artboards panel + Artboard tool (GUI, `photonic-gui`): create/resize/rename
  interactively; auto-arrange in a grid view.

**Out:**
- Per-page node isolation (Figma-style pages); the `Page` struct wrapper is not activated.
- "Export for Screens" batch multi-scale/multi-format (follow-on, depends on this).
- Inter-artboard shared object references or master pages.

## Proposed approach

1. **Model** (`photonic-core/src/document.rs`):
   - Define `pub struct Artboard { id: Uuid, name: String, x: f64, y: f64, width: f64,
     height: f64, margin_top: f64, margin_right: f64, margin_bottom: f64,
     margin_left: f64, bleed_mm: f64, slug_mm: f64 }`.
   - Add `pub artboards: Vec<Artboard>` and `pub active_artboard_id: Option<Uuid>` to
     `Document`.
   - Keep `Document.width`/`Document.height` as the overall canvas size (union of all
     artboard rects + padding) or deprecate them in favour of a computed bounding rect.
   - Bump `CURRENT_FORMAT_VERSION`; in the load path, when `artboards` is empty, inject
     one default artboard from `width`/`height`.

2. **Render** (`photonic-render/src/renderer.rs`, `headless.rs`):
   - Draw artboard outlines + fill (off-white) behind scene nodes in `render()`.
   - `headless::capture` accepts `artboard_id: Option<Uuid>` and sets the viewport to
     that artboard's rect.

3. **Export** (`photonic-core/src/export.rs`):
   - `export_svg` gains an optional `artboard_id` to clip and viewBox to that artboard.
   - `export_nodes_as_svg` unchanged.

4. **MCP** (`photonic-mcp/src/handlers/document.rs`):
   - New handlers: `create_artboard`, `update_artboard`, `delete_artboard`,
     `list_artboards`, `export_artboard` (headless capture scoped to one artboard).

5. **GUI** (`photonic-gui`):
   - Artboard tool: click-drag to create, handles to resize, double-click to rename.
   - Artboards panel: list, click to navigate/zoom, reorder, duplicate.

## Affected modules (real paths)

- `crates/photonic-core/src/document.rs` — `Artboard` struct, `Document` fields,
  migration path
- `crates/photonic-render/src/renderer.rs` — artboard background drawing
- `crates/photonic-render/src/headless.rs` — scoped capture
- `crates/photonic-core/src/export.rs` — artboard-clipped SVG
- `crates/photonic-mcp/src/handlers/document.rs` — MCP tool handlers
- `crates/photonic-gui` — Artboard tool, Artboards panel (TBD files)

## Risks & open questions

- **Canvas size semantics**: does `Document.width`/`height` become derived (union bbox) or
  still explicit? Derived is cleaner but breaks `resize_canvas` MCP tool.
- **Node-to-artboard assignment**: are nodes "owned" by an artboard, or just visually
  overlapping? Ownership enables per-artboard export culling but complicates cross-artboard
  copy-paste.
- **Migration fidelity**: single-canvas docs with guides/bleed must map correctly to the
  synthesised default artboard's margins.
- **Artboard deletion**: what happens to nodes that overlap only the deleted artboard?
  (Orphaned in global space — acceptable but needs UX clarity.)

## Acceptance criteria

- [ ] A document can hold multiple named artboards of different sizes at arbitrary positions.
- [ ] Loading an old single-canvas `.photon` file produces one default artboard matching
      the original canvas size (no visible change to existing docs).
- [ ] Headless export scoped to one artboard renders only nodes within that rect.
- [ ] MCP `create_artboard` / `list_artboards` / `export_artboard` tools work.
- [ ] SVG export with `artboard_id` sets the correct `viewBox`.

## Effort estimate

**XL** — Touches every layer (model, render, export, MCP, GUI); migration path must be
bulletproof for existing documents; artboard tool/panel is significant GUI work.
