# Envelope Distort + Interactive Gradient-Mesh Tool (#46) — Design Proposal

> Status: design scaffold (not an implementation).

## Summary

The `warp_envelope` MCP tool (`photonic-mcp/src/handlers/nodes.rs:9426`) applies preset-based,
**destructive** distortion — there is no editable envelope object and no on-canvas mesh
interaction. `MeshGradient` + `MeshGradientVertex` are modelled in
`photonic-core/src/style.rs:394` and partially surfaced in the panels UI
(`photonic-gui/src/panels/mod.rs`), but no canvas hit-test or drag interaction exists.
`Tool::GradientMesh` and `Tool::EnvelopeDistort` are absent from
`photonic-gui/src/tools/mod.rs`.

This issue makes both features interactive and non-destructive.

## Scope

**In**
- Envelope Distort (non-destructive): Make-with-Warp presets (arc/bulge/wave/etc.),
  Make-with-Mesh (drag mesh control points on canvas), Edit Contents, Release/Expand.
- Gradient Mesh tool: click to place/split mesh lines on any path; drag mesh vertices
  and Bézier handles; per-vertex colour picker; Direct-Select integration.
- `Command` variants for both so undo/redo is preserved.

**Out**
- Make-with-Top-Object (lower priority, depends on clipping stack).
- Envelope Distort on text objects (font shaping complexity).
- Mesh gradients on non-path node types (groups, images).

## Proposed Approach

1. **Core model — Envelope node type**  
   Add `NodeKind::Envelope { source_id: NodeId, mesh: EnvelopeMesh }` to
   `photonic-core/src/document.rs` (or equivalent node model file).  
   `EnvelopeMesh` reuses the `rows×cols` grid shape of `MeshGradientVertex`
   (position only, no colour); deformation maps source path control points through
   bilinear/bicubic interpolation.

2. **Commands** (`photonic-core/src/history.rs` / command module)  
   `Command::ApplyEnvelope`, `Command::EditEnvelopeMesh`, `Command::ReleaseEnvelope`,
   `Command::EditMeshVertex { node_id, row, col, pos, handle }`.

3. **Render** (`photonic-render/src/renderer.rs`)  
   Envelope rendering: sample deformed path at a user-controlled resolution and
   re-tessellate. Mesh gradient rendering already routes through the GPU path —
   vertex drag changes need to invalidate the per-node tessellation cache in
   `photonic-render/src/tessellator.rs`.

4. **GUI tools** (`photonic-gui/src/tools/mod.rs`)  
   Add `Tool::EnvelopeDistort` and `Tool::GradientMesh` to the `Tool` enum.  
   Tool handlers in a new file `photonic-gui/src/tools/mesh.rs`:  
   - hit-test mesh vertices within a pick radius;  
   - drag → emit `EditMeshVertex` / `EditEnvelopeMesh` command;  
   - click on edge midpoint → insert row or column (split command).

5. **Panels** (`photonic-gui/src/panels/mod.rs`)  
   The existing `FillType::Mesh` branch (line ~5710) already toggles the fill to
   `FillKind::MeshGradient`. Extend it with "Add Row / Add Column" that now also
   produce undoable `Command`s rather than direct mutation.

6. **MCP** (`photonic-mcp/src/server.rs`, `handlers/nodes.rs`)  
   Extend `warp_envelope` to accept an optional `"mode": "non_destructive"` flag
   producing an Envelope node instead of mutating the path.

## Affected Modules

- `crates/photonic-core/src/style.rs` — `MeshGradient`, `MeshGradientVertex`
- `crates/photonic-core/src/` — document / command / history modules
- `crates/photonic-render/src/tessellator.rs` — mesh tessellation cache invalidation
- `crates/photonic-render/src/renderer.rs` — envelope deformation pass
- `crates/photonic-gui/src/tools/mod.rs` — new `Tool` variants
- `crates/photonic-gui/src/tools/mesh.rs` — new file for hit-test / drag logic
- `crates/photonic-gui/src/panels/mod.rs` — mesh panel (lines ~5679–6134)
- `crates/photonic-mcp/src/handlers/nodes.rs` — `warp_envelope` (line 9426)
- `crates/photonic-mcp/src/protocol.rs` — `WarpEnvelopeArgs` (line 1002)

## Risks & Open Questions

- Bicubic vs. bilinear deformation for envelope — bilinear is simpler but gives
  visible kinks on low-resolution meshes. Which quality floor is acceptable?
- Performance: re-tessellating on every drag event in the GPU pipeline needs profiling;
  may require throttled redraw or CPU-side preview at low resolution.
- `MeshGradientVertex` stores absolute canvas positions — dragging changes vertex
  coordinates. If the parent object is transformed, positions need to be in local space.
  Clarify coordinate space contract before implementing.
- Envelope non-destructive mode adds a new node kind; confirm serialization format
  is versioned in the `.photon` format before merging.

## Acceptance Criteria

- [ ] An object can be distorted by an editable warp preset and reverted (Release).
- [ ] A gradient mesh can be created by clicking on an object with the Mesh tool.
- [ ] Mesh vertices and Bézier handles are draggable; per-vertex colour is settable.
- [ ] All mesh edits produce undo/redo steps via `CommandHistory`.
- [ ] Export to SVG preserves mesh gradient appearance.

## Effort Estimate

**XL** — two overlapping surface areas (envelope object model + mesh tool), GPU
tessellation changes, new hit-testing in the canvas loop.
