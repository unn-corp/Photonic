# Photonic — Adobe Illustrator Feature Gap Analysis

Exhaustive list of Adobe Illustrator features not yet in Photonic, organized by category.
Current Photonic state as of 2026-03-23.

---

## 1. Drawing Tools

### Missing Shape Tools
- ~~**Flare Tool**~~ — *(implemented: `create_flare` MCP tool — procedural lens flare with halo, rays, and rings)*

### Missing Path/Drawing Tools
- **Pen Tool enhancements** — Add Anchor Point, Delete Anchor Point, Convert Anchor Point as discrete sub-tools in the GUI toolbar (individual operations available as MCP tools and path panel buttons)
- **Curvature Tool** — GUI interactive tool for drawing smooth curves point-by-point (MCP `create_curvature_path` available for smooth curve creation from point arrays)
- **Path Eraser Tool** — erase portions of a path by dragging
- **Join Tool** — draw a stroke between two open endpoints to connect them

### Missing Brush Tools
- **Paintbrush Tool** — paint strokes using calligraphic, scatter, art, bristle, or pattern brushes
- **Blob Brush Tool** — paint filled shapes that merge with touching same-color blobs
- **Bristle Brush** — simulate natural bristle behavior (density, length, stiffness, paint opacity)

### Missing Interactive Tools
- **Shaper Tool** — gesture-draw rough shapes that snap to clean vectors; Shaper Groups for non-destructive booleans
- **Eraser Tool** — drag to erase any path, creating clean anchor points at cut boundaries
- **Knife Tool** — freehand cut through filled shapes, splitting into separate closed paths

---

## 2. Selection Tools

- ~~**Global Edit**~~ — *(implemented: `select_similar` MCP tool — select all nodes matching fill color, stroke color, stroke width, kind, opacity, or tags; GUI "Select Similar Fill" button in properties panel)*

---

## 3. Transform & Modification Tools

### Missing Transform Tools
- **Free Transform Tool** — single tool for scale, rotate, skew, perspective distort, and free distort with sub-modes
- **Reshape Tool** — move anchor points while smoothly deforming the surrounding path

### Missing Width & Warp Tools
- **Width Tool** — create variable-width stroke profiles by dragging on any stroke; save named Width Profiles; asymmetric widths per side
- **Warp Tool** (vector liquify) — push path anchor points in drag direction
- ~~**Twirl Tool**~~ — *(destructive version: `twirl_path` MCP tool + GUI button; brush-based interactive version pending)*
- ~~**Pucker Tool**~~ — *(destructive version: `pucker_bloat` MCP tool + GUI buttons; brush-based interactive version pending)*
- ~~**Bloat Tool**~~ — *(destructive version: `pucker_bloat` MCP tool + GUI buttons; brush-based interactive version pending)*
- ~~**Scallop Tool**~~ — *(destructive version: `scallop_path` MCP tool + GUI button; brush-based interactive version pending)*
- ~~**Crystallize Tool**~~ — *(destructive version: `crystallize_path` MCP tool + GUI button; brush-based interactive version pending)*
- ~~**Wrinkle Tool**~~ — *(covered by `roughen_path` with detail > 0 for subdivision before displacement)*

### Missing Tool Behaviors
- **Rotate View Tool** — non-destructively rotate the canvas view for comfortable drawing at any angle (artwork unchanged)
- **Print Tiling Tool** — position the printable page area over large artwork
- ~~**Eyedropper sampling options**~~ — *(implemented: `copy_appearance` MCP tool + GUI "Copy Appearance" CollapsingHeader (visible when 2+ nodes selected) — copies fill, stroke, and/or opacity from the first selected node to all others; each attribute toggled independently via checkboxes)*

---

---

## 5. Boolean / Pathfinder Extensions

Currently have: Union, Subtract, Intersect, Exclude, Crop, Minus Back, Minus Front, Trim, Outline, Merge.

**Missing modes:**
- **Live Pathfinder / Compound Shape mode** — non-destructive boolean that remains editable (Alt-click in Illustrator's Shape Modes)
- **Pathfinder as Live Effect** — apply any pathfinder operation as a non-destructive appearance effect

---

## 6. Text & Typography

### Missing Text Tools
- ~~**Area Type Tool**~~ — *(implemented: `set_text_area` MCP tool + GUI "Set as Area Boundary" button when text + path selected; `clear_text_area` MCP + GUI "Clear Area" button)*
- ~~**Type on a Path Tool**~~ — *(implemented: `set_text_path` MCP tool + GUI "Set as Path Spine" button when text + path both selected; `clear_text_path` MCP tool + GUI "Clear Path" button; offset configurable)*
- ~~**Vertical Type Tool**~~ — *(implemented: `set_text_direction { node_id, vertical: true/false }` MCP tool + GUI toggle button in Text Operations panel)*
- **Touch Type Tool** — individually move, scale, and rotate individual characters non-destructively within a text object

### Missing Text Features
- ~~**Text Threading**~~ — *(implemented: `link_text_frames { from_id, to_id }` + `unlink_text_frames { node_id }` MCP tools + GUI "Text Frame Threading" panel in text node properties — stores next_frame/prev_frame chain on TextNode; GUI shows chain state and Link/Unlink buttons when another text node is co-selected)*
- **Text Wrap** — wrap text around objects/shapes automatically
- ~~**Variable Fonts support**~~ *(partial: `set_font_weight` (100–900) and `set_font_style` (normal/italic/oblique) MCP tools + B/I toggle buttons in GUI Text Operations panel; Width/Slant axes and live sliders pending)*
- ~~**OpenType features**~~ — *(implemented: `set_opentype_features { node_id, features, mode }` + `get_opentype_features` MCP tools + GUI "OpenType Features" panel with checkboxes for 12 common feature tags (liga, calt, frac, smcp, sups, subs, ordn, swsh, dlig, onum, tnum, zero); stored as Vec<String> on TextNode)*
- **Glyphs Panel** — browse and insert any glyph from the active font, filtered by category
- ~~**Character Styles**~~ — *(implemented: `create_character_style` (capture from node or explicit attrs), `apply_character_style`, `list_character_styles`, `delete_character_style` MCP tools; GUI: Character Styles panel in text node properties with Apply/Delete buttons)*
- ~~**Paragraph Styles**~~ — *(implemented: `create_paragraph_style`, `apply_paragraph_style`, `list_paragraph_styles`, `delete_paragraph_style` MCP tools; GUI: Paragraph Styles panel in text node properties)*
- ~~**Text Decoration (underline/strikethrough/overline)**~~ — *(implemented: `set_text_decoration { node_id, decoration }` MCP tool + GUI U/S/O toggle buttons in Text Operations panel — text_decoration field on TextNode; decoration values: underline, line-through, overline, none)*
- ~~**Tabs Panel**~~ — *(implemented: `set_tab_stops { node_id, stops }` + `clear_tab_stops { node_id }` MCP tools + GUI "Tab Stops" CollapsingHeader in text node properties — positions stored as `Vec<f64>` on TextNode; GUI shows current stops, Add-stop DragValue, and Clear All button)*
- ~~**Paragraph options (spacing + indent)**~~ — *(implemented: `set_paragraph_options { node_id, spacing_before?, spacing_after?, indent? }` MCP tool + GUI DragValues in Text panel — paragraph_spacing_before, paragraph_spacing_after, text_indent fields on TextNode; negative indent supported for hanging indents)*
- Hyphenation, hanging punctuation, Adobe Every-line vs Single-line composer
- **Spell check** — in-document spelling verification
- **SVG Color Fonts** — support for emoji/color fonts

---

## 7. Color Tools & Panels

### Missing Color Features
- ~~**Spot Color support**~~ — *(implemented: `define_spot_color`, `list_spot_colors`, `apply_spot_color`, `delete_spot_color` MCP tools + Spot Colors panel in GUI — named inks with overprint flag, applied as solid fills)*
- ~~**Global Color swatches**~~ — *(implemented: `add_color_swatch`, `list_color_swatches`, `apply_color_swatch`, `update_color_swatch` (with propagation), `delete_color_swatch` MCP tools + Color Swatches panel in GUI properties)*
- ~~**Swatch Libraries**~~ — *(implemented: `load_swatch_library` MCP tool + Color Swatches panel dropdown — loads web, material, pastels, earth_tones, neon, or grayscale preset palettes; skips duplicates)*
- **Color Picker dialog** — full-featured HSB / RGB / CMYK / hex picker (currently only basic solid fill input)

### Missing Gradient Features
- ~~**Freeform Gradient**~~ — *(implemented as Fluid Gradient — IDW interpolation from free-placed control points)*
- **Gradient Tool on-canvas controls** — drag to reposition gradient handles, angle, and stops directly on the object
- **Multiple gradient stops** — currently limited; need arbitrary stop counts and midpoint control
- ~~**Save gradients as swatches**~~ — *(implemented: `save_gradient_swatch`, `list_gradient_swatches`, `apply_gradient_swatch`, `delete_gradient_swatch` MCP tools + Gradient Swatches panel in GUI)*

### Missing Pattern Features
- **Pattern Options Panel** — full pattern creation mode: Grid, Brick by Row, Brick by Column, Hex by Column, Hex by Row tile types; configurable tile size, overlap, spacing; live preview
- **Pattern as stroke fill** — apply patterns to stroke paths (not just object fills)
- **Save patterns to Swatches**

---

## 8. Stroke Enhancements

- ~~**Variable Width Profiles**~~ — *(implemented: `define_width_profile`, `list_width_profiles`, `apply_width_profile`, `delete_width_profile` MCP tools + GUI Width Profiles panel — named per-path width envelopes stored at doc level; apply sets stroke.width to profile average)*

---

## 9. Effects System (Non-Destructive Appearance)

### Missing Architecture
- **Appearance Panel** — stack multiple fills, strokes, and effects on a single object; effects are non-destructive and remain editable; reorder/remove individual fills/strokes independently
- ~~**Graphic Styles Panel**~~ — *(implemented: `define_graphic_style`, `list_graphic_styles`, `apply_graphic_style`, `delete_graphic_style` MCP tools + GUI "Graphic Styles" panel — save fill+stroke+opacity as named presets, apply to any path/text node)*

### Missing Illustrator Effects (Vector, Non-Destructive)
**3D**
- Extrude & Bevel — add depth and bevel to flat art with lighting controls
- Revolve — rotate a profile around a vertical axis to create a 3D solid
- Inflate — balloon/puff effect
- Rotate — display 2D object rotated in 3D space
- Materials Panel — apply textures/materials to 3D objects; map artwork onto surfaces
- Ray Tracing Render — high-quality realistic rendering of 3D within Illustrator

**Convert to Shape**
- Rectangle, Rounded Rectangle, Ellipse (convert any object's appearance to that shape non-destructively)

**Distort & Transform (as live effects)**
- Free Distort, ~~Pucker & Bloat~~, ~~Roughen~~, ~~Transform (with copies)~~, Tweak, Twist, ~~Zig Zag~~ *(destructive versions available as MCP tools + GUI buttons; live effect versions pending appearance stack)*

**Stylize (vector)**
- ~~Drop Shadow~~ *(destructive: `add_drop_shadow` MCP tool + GUI button; live effect pending appearance stack)*, Feather, ~~Inner Glow~~, ~~Outer Glow~~, ~~Round Corners~~ *(destructive: `round_corners` MCP tool + GUI button; live effect pending appearance stack)*, Scribble

~~**Warp Effects (15 presets)**~~ — all 15 presets implemented as destructive `warp_envelope` MCP tool: arc, arc_lower, arc_upper, arch, bulge, shell_lower, shell_upper, flag, wave, fish, rise, fisheye, inflate, squeeze, twist *(live effect versions pending appearance stack)*

**SVG Filters** — apply SVG-based filter effects; editable filter code

### Missing Raster Effects (Photoshop Effects applied at raster resolution)
- Gaussian Blur, Radial Blur, Smart Blur
- Artistic filters (Dry Brush, Watercolor, Cutout, etc.)
- Texture filters (Grain, Craquelure, Texturizer, etc.)
- Sketch filters (Halftone, Stamp, Charcoal, etc.)
- Effect Gallery — browse and stack multiple raster effects with live preview

---

## 10. Layers & Organization Enhancements

- **Sublayers** — hierarchical nesting within layers (more than one level deep)
- **Paste on All Artboards** — paste to the same position on every artboard simultaneously


---

## 11. Artboards

- **Multiple artboards** — Photonic has one canvas; need multi-artboard document support (up to 1000)
- **Artboards Panel** — list, reorder, rename, duplicate, navigate between artboards
- **Different artboard sizes** within the same document
- **Artboard Tool** — add, resize, move, delete artboards interactively
- **Rearrange artboards** — auto-layout all artboards in a grid
- **Export per artboard** — export each artboard individually with its own settings
- **Export for Screens** — multi-artboard, multi-scale, multi-format batch export in one operation

---

## 12. Symbols

- ~~**Symbols Panel**~~ — *(implemented: `define_symbol`, `list_symbols`, `place_symbol`, `break_link_to_symbol`, `delete_symbol` MCP tools + Symbols panel GUI)*
- ~~**Break Link to Symbol**~~ — *(implemented: `break_link_to_symbol` MCP tool + "Break Link to Symbol" GUI button)*
- ~~**Dynamic Symbols**~~ — *(implemented: `set_symbol_override { node_id, fill_hex?, stroke_hex? }` + `clear_symbol_overrides` MCP tools + GUI "Symbol Override" panel (when symbol instance selected) — per-instance fill/stroke color overrides stored on SceneNode as `symbol_fill_override`/`symbol_stroke_override`)*
- ~~**Symbol Libraries**~~ — *(implemented: `load_symbol_library { library_name }` MCP tool + GUI "Load Library…" CollapsingHeader in Symbols panel — loads "arrows" (6 shapes), "shapes" (diamond, hexagon, star, cross, checkmark), or "ui" (checkbox, radio, close, menu, plus) preset symbols as hidden off-canvas master nodes; skips already-defined names)*
- **Symbolism Tools** (spray-based):
  - ~~**Symbol Sprayer**~~ — *(implemented: `spray_symbol_instances` MCP tool + GUI "Symbol Sprayer" CollapsingHeader in Symbols panel — scatters N instances of a symbol around a center point using golden-angle spiral distribution)*
  - Symbol Shifter, Scruncher, Sizer, Spinner, Stainer, Screener, Styler

---

## 13. Brushes

- **Calligraphic Brushes** — elliptical tip with angle, roundness, size (fixed/random/pressure)
- **Scatter Brushes** — distribute object copies along a path (size, spacing, scatter, rotation)
- **Art Brushes** — stretch artwork along the entire length of a path
- **Pattern Brushes** — tile artwork along a path with separate tiles for corners, caps, and straight sections
- **Bristle Brushes** — natural bristle simulation
- **Brush Options** — modify any brush; option to apply changes to existing stroked paths
- **Expand Appearance** — convert brush strokes to regular editable paths
- **Brushes Panel** with brush libraries

---

## 14. Blend Tool / Object Blending

- ~~**Blend between objects**~~ — *(basic version: `blend_objects` MCP tool + GUI button — specified steps with shape/color/opacity interpolation; requires same element count)*
- ~~**Blend Options**~~ — *(implemented: `blend_objects` extended with `smooth_color: true` (auto-steps from color distance) and `spacing` (Specified Distance mode). GUI: "Blend (Smooth Color)" and "Blend (32 px spacing)" buttons in Blend panel)*
- ~~**Blend Spine**~~ — *(implemented: `set_blend_spine` / `clear_blend_spine` MCP tools + GUI "Blend Spine" CollapsingHeader in Group Operations — assigns a custom path node as the spine for a blend group)*
- ~~**Reverse Spine**~~ — *(implemented: `reverse_blend_spine` MCP tool + GUI "Reverse Spine" button in Blend Spine panel — reverses the direction of the assigned spine path, inverting blend interpolation order)*
- ~~**Reverse Front to Back**~~ — *(implemented: `reverse_node_order` MCP tool + GUI "Reverse Order" button — reverses children order in any group; single undoable step)*
- ~~**Expand blend**~~ — *(implemented: `expand_blend` MCP tool + GUI "Expand Blend" button in Group Operations panel — dissolves the group wrapper and places all children as standalone nodes at the parent layer position)*

---

## 15. Envelope Distort

- **Make with Warp** — apply any warp preset as an editable envelope mesh
- **Make with Mesh** — overlay a mesh grid; drag mesh points to distort the object
- **Make with Top Object** — use any shape as a distortion container
- **Edit Contents** — enter envelope to edit original artwork inside
- **Envelope Options** — fidelity, distort linear gradients, distort pattern fills

---

## 16. Live Paint

- **Live Paint Bucket** — paint fill colors into any region formed by intersecting paths (not only closed shapes); paint "edges" (stroke segments) between regions
- **Live Paint Selection Tool** — select individual faces and edges within a live paint group
- **Gap detection** — control whether small gaps are treated as region boundaries
- **Expand** — convert live paint group to standard filled paths

---

## 17. Gradient Mesh

- **Mesh Tool** — click on any object to add mesh points and lines; each point accepts an independent color
- **Create Gradient Mesh** — auto-generate mesh from an object at specified row/column count
- **Mesh point editing** — Direct Selection on mesh points; Bézier handles control color transition smoothness
- Enables photorealistic shading within a single vector object

---

## 18. Perspective Grid

- **Perspective Grid Tool** — enable one-point, two-point, or three-point perspective grids
- **Perspective Selection Tool** — select and move objects in perspective space maintaining foreshortening
- Draw objects directly on perspective planes
- Attach existing artwork to a perspective plane
- Move horizon line, ground level, vanishing points
- Save perspective grid presets

---

## 19. Image Tracing

- **Image Trace Panel** — convert placed raster images to editable vector paths
- Presets: High Fidelity Photo, Low Fidelity Photo, 3/6/16 Colors, Grayscale, Black and White Logo, Sketched Art, Silhouettes, Line Art, Technical Drawing
- Controls: mode (Color/Grayscale/B&W), palette, color count, threshold, noise reduction, path fidelity, corner detection, fills/strokes toggle, snap curves to lines, ignore white
- **Expand** — convert trace result to editable paths
- AI-enhanced tracing (fewer anchor points, faster)

---

## 20. Graph / Data Visualization Tools

- ~~**Column Graph Tool**~~ — *(implemented: `create_bar_chart` with vertical mode)*
- ~~**Stacked Column Graph**~~ — *(implemented: `create_stacked_bar_chart` with `horizontal: false`)*
- ~~**Bar Graph**~~ — *(implemented: `create_bar_chart` with horizontal mode)*
- ~~**Stacked Bar Graph**~~ — *(implemented: `create_stacked_bar_chart` with `horizontal: true`)*
- ~~**Line Graph**~~ — *(implemented: `create_line_chart` with smooth/area options)*
- **Area Graph** *(partial: `create_line_chart` with `fill_area: true`)*
- ~~**Scatter Graph**~~ — *(implemented: `create_scatter_plot` MCP tool)*
- ~~**Pie Graph**~~ — *(implemented: `create_pie_chart` with donut option)*
- ~~**Radar Graph**~~ — *(implemented: `create_radar_chart` MCP tool + GUI demo button — configurable axes, multi-series, grid rings, fill area)*
- **Graph Data Window** — enter or paste data (CSV/Excel compatible)
- **Graph Design** — replace bars/markers with custom artwork (pictographs)
- **Graph Type Options** — configure axes, tick marks, labels, legend placement, drop shadow

---

## 21. Automation

### Missing Automation Features
- ~~**Actions Panel**~~ — *(implemented: `define_action`, `list_actions`, `play_action`, `delete_action` MCP tools + GUI Actions panel — define named sequences of MCP tool calls; replay with optional node ID substitutions; stops at first error with step report)*
- **Batch Processing** — run any action on an entire folder of files (File > Automate > Batch)
- **Script support** — JavaScript (ExtendScript), AppleScript, VBScript; File > Scripts menu
- ~~**Script Event Manager**~~ — *(implemented: `register_event_trigger { event, action_name }`, `list_event_triggers`, `remove_event_trigger { event, action_name? }` MCP tools + GUI "Event Triggers" panel — maps document lifecycle events (on_open, on_save, on_node_create, on_selection_change) to named action sets; stored in event_triggers Vec on document model)*
- ~~**Variables Panel**~~ — *(implemented: `define_variable`, `list_variables`, `set_variable_value`, `delete_variable`, `apply_variables` MCP tools + `bind_text_variable`/`unbind_text_variable`; GUI: Variables panel + Variable Binding panel per text node)*
- ~~**Asset Export Panel**~~ — *(implemented: `tag_node_for_export` MCP tool + `export_tagged_assets` — per-node export specs with name, format, and scale multipliers; GUI: Asset Export collapsing panel in properties showing tag status with Tag/Remove buttons)*

---

## 22. View & Navigation

- ~~**Navigator Panel**~~ — *(implemented: `get_canvas_overview` MCP tool returning all visible node bounding boxes + fill colors; GUI Navigator collapsing panel in properties showing miniature document thumbnail with selected node highlighted)*
- **Rotate View** — non-destructively rotate the canvas angle for comfortable drawing (separate from Rotate View Tool above — this is the panel/shortcut mechanism)
- **Pixel Preview** — preview how vector art rasterizes at screen resolution
- **Overprint Preview** — simulate overprinting ink behavior for print production
- **Proof Colors / Soft Proofing** — preview output for a specific color profile

---

## 23. Document & Workspace Features

### Missing Document Features
- **CMYK color mode** — full CMYK document mode with proper color management
- **Color Management / ICC profiles** — assign, convert, and proof using ICC color profiles
- ~~**Bleed and slug settings**~~ — *(implemented: `set_document_bleed { bleed_mm, slug_mm }` + `get_document_bleed` MCP tools + GUI "Print Settings" collapsing panel — stores bleed and slug values in mm on the document model, persisted in .photon files)*
- **Print dialog** — media size, orientation, crop marks/bleed, color management, separations output

### Missing File Format Support
**Import:**
- PDF (multi-page, with layer preservation option)
- EPS
- DXF / DWG (CAD format)
- Photoshop PSD (with layer preservation)
- TIFF, BMP, WMF/EMF

**Export:**
- ~~**TIFF**~~ — *(implemented: `export_raster { format: "tiff" }` MCP tool extension + GUI Export dialog "TIFF" option — lossless RGBA TIFF output via the image crate; full width/height/background controls)*
- PDF (vector)
- EPS
- DXF/DWG

**Linked images:**
- **Place Linked** — reference external image files; auto-update when source changes
- **Links Panel** — manage linked files; relink, update, embed, reveal on disk
- **Embed images** — store raster image data inside the document

### Missing Workspace Features
- ~~**Custom workspaces**~~ — *(implemented: `save_workspace`, `load_workspace`, `list_workspaces`, `delete_workspace` MCP tools + GUI Workspaces panel — named presets storing panel search-filter queries; load instantly switches panel layout via prop_search)*
- **Custom tool panels** — create compact panels containing only preferred tools
- **Contextual Taskbar** — floating bar with context-relevant actions for current selection/tool

---

## 24. Alignment & Snapping Enhancements

- **Snap to Glyph** — snap to typographic baselines, cap height, x-height, and descender
- ~~**Smart Guide distance labels**~~ — *(implemented: `measure_distances` MCP tool + GUI Distances panel (visible when 2+ nodes selected) — reports edge-to-edge horizontal/vertical gaps and center-to-center distance for all node pairs)*

---

## 25. Clipping Masks

- ~~**Clipping Mask**~~ — *(implemented: `make_clipping_mask` MCP tool — topmost child of group becomes clip path; GUI: Make/Release buttons in Clipping Mask panel for Group nodes)*
- ~~**Release Clipping Mask**~~ — *(implemented: `release_clipping_mask` MCP tool + GUI button — clears clip path, all children revert to normal)*
- **Edit Contents / Edit Mask** — independently reposition the clip path and the masked artwork
- **Draw Inside mode** — draw directly inside a selected object without needing a separate mask step

---

## 27. Mockup Tool

- Place vector artwork onto photographs of real-world objects (T-shirts, mugs, phones, packaging)
- Automatically adjusts artwork to follow surface contours and perspective
- Live editing — modify the source vector artwork and the mockup updates in real time

---

---

## 29. AI / Generative Features

- **Text to Vector Graphic** — generate fully editable vector scenes, subjects, or icons from a text prompt (Adobe Firefly / partner models)
- **Text to Pattern** — generate editable repeating vector patterns from text prompts
- **Generative Shape Fill** — fill any selected shape with AI-generated vector artwork from a text prompt
- **Generative Recolor** — recolor existing artwork using a text prompt (e.g., "sunset palette")
- **Generative Expand** — expand artboard; AI generates new content matching existing style to fill the new area
- **Auto Select** — AI-powered selection that identifies and selects semantically similar objects
- **Retype / Font Identification** — identify fonts from placed images; suggest matching Adobe Fonts; generate custom variable fonts

---

## 30. Miscellaneous Missing Features

- **Transform Panel** — reference point selector (9-point) *(X/Y/W/H/rotation editable DragValues already added to properties panel)*
- ~~**Isolation Mode**~~ — *(implemented: `enter_isolation_mode` / `exit_isolation_mode` MCP tools + GUI double-click)*
- ~~**Find/Replace for objects**~~ — *(implemented: `find_replace_style`, `find_nodes`, `select_same` MCP tools)*
- ~~**Magic Wand tolerance**~~ — *(implemented: `magic_wand_select` with configurable tolerance)*
- **Crop Image** — non-destructively crop a placed image to a defined boundary
- ~~**Flatten Transparency**~~ — *(implemented: `flatten_transparency` MCP tool + GUI "Flatten Transparency" panel button — bakes opacity into color alpha values)*
- **Expand Appearance** — convert any appearance (effects, multiple fills/strokes) to editable flat paths
- **Outline Object** — create an outline version of any object (related to Expand)
- ~~**Offset multiple objects**~~ — *(implemented: `offset_path` MCP tool + GUI "Expand (+2 px)" / "Contract (−2 px)" buttons in path properties panel)*
- **Package Document** — collect all linked files and fonts into a single folder for sharing

---

*Generated 2026-03-23, updated 2026-04-01. Cross-referenced against Photonic feature set (150 MCP tools, 10+ GUI tools, 6 fill types, 16 blend modes) and Adobe Illustrator 2025/2026 feature set.*

---

# Photonic — Bespoke Feature Ideas

Features that go *beyond* Adobe Illustrator. Illustrator is the floor, not the ceiling.
Grounded in community pain points, competitive gaps, and Photonic's unique AI-first architecture.

---

## Guiding Principles

1. **Every operation is AI-addressable** — Photonic's MCP server is a first-class citizen, not a bolt-on. Features should be designed so an AI agent can drive them as naturally as a human can.
2. **No legacy baggage** — We have no 30-year-old code to protect. Features that Illustrator can't do because of backward compatibility are our opportunity.
3. **The create → screenshot → observe → adjust loop** is the core differentiator. Features should accelerate or deepen this loop.
4. **Non-destructive by default** — Nothing destroys original data unless explicitly requested.
5. **Structured data, not just pixels** — Vector art is structured. Photonic should expose and leverage that structure more deeply than any tool before it.

---

## Category A — Parametric & Constraint-Based Design

The #1 unmet need across all vector tools. CAD software solved this decades ago. Illustration tools still haven't.

### A1. Live Property Constraints
Bind any numeric property of one node to a formula referencing another node.

```
rect_b.width = rect_a.width * 2
circle.cx = rect_a.x + rect_a.width / 2
gap_line.length = parent.width - padding * 2
```

- Constraints are stored on the document model, evaluated reactively on every frame
- Visual indicator on constrained properties (lock icon + formula preview on hover)
- Cycle detection with clear error messaging
- MCP tool: `set_constraint(node_id, property, expression)` / `list_constraints()` / `remove_constraint()`

### ~~A2. Parametric Shapes via Equation~~
~~Define a closed path by a parametric equation. The shape recalculates if variables change.~~

~~Examples:~~
~~- Lissajous curves: `x = A·sin(a·t + δ)`, `y = B·sin(b·t)`~~
~~- Superellipses: `|x/a|^n + |y/b|^n = 1`~~
~~- Rose curves: `r = cos(k·θ)`~~
~~- Any custom expression the user types~~

~~- Variables exposed as sliders in the Properties panel~~
~~- Auto-updates path data when sliders change~~
~~- Exportable as static SVG or as a parameterised SVG with CSS variables~~

*(implemented: `create_parametric_shape` MCP tool — Lissajous, Superellipse, Rose, Hypotrochoid, Epicycloid; GUI buttons in Data Visualization panel)*

### A3. Reactive Layout Engine
An opt-in layout mode for groups that behaves like CSS Flexbox/Grid — but for illustration.

- ~~**Flex Group**: children auto-distribute along an axis with configurable gap, alignment, and wrapping~~ *(implemented: `apply_flex_layout` MCP tool + GUI buttons in Group Operations panel — row/column direction, gap, cross-axis alignment)*
- ~~**Grid Group**: children snap to a defined row/column grid with configurable gutter~~ *(implemented: `apply_grid_layout` MCP tool + GUI "Grid (3 cols)" / "Grid (4 cols)" buttons in Group Operations panel)*
- ~~**Stack Group**~~ — *(implemented: `apply_stack_layout { group_id, align_h?, align_v? }` MCP tool + GUI "Stack (center)" button in Group Operations panel — repositions all children to the same anchor within the union bounding box; supports left/center/right × top/center/bottom alignment)*
- Resizing the group container reflows children automatically
- This isn't just for UI design — it makes repeated illustration elements (icon sheets, logo variations, badge layouts) dramatically faster

### ~~A4. Document Grammar / Design Rules~~
~~Define rules that the document must satisfy. Violations are flagged non-disruptively.~~

*(implemented: `define_grammar_rule`, `list_grammar_rules`, `check_grammar`, `delete_grammar_rule` MCP tools + GUI "Document Grammar" panel — five rule types: palette_includes, max_colors, min_text_size, required_layer, max_node_count; stored on document model; check_grammar returns per-rule pass/fail with descriptions)*

---

## Category B — AI-Native Workflow (The Real Differentiator)

Photonic has a built-in AI loop that no other vector tool has. These features exploit it fully.

### ~~B1. Prompt History on Objects~~
*(implemented: `set_node_prompt` MCP tool (append/prepend/replace modes) + `get_node_prompts` MCP tool. `prompt_history: Vec<String>` stored on every `SceneNode`, persisted in `.photon` files, skipped on export. GUI: "Origin (Prompt History)" collapsing section in Properties panel shows entries when history is non-empty.)*

- Remaining: re-run-with-edits UI (requires generative integration)

### ~~B5. Composition Advisor~~
*(implemented: `analyze_composition` MCP tool — checks quadrant balance, canvas density, object overlap, color contrast pairs, palette size, and off-canvas objects; returns structured JSON findings. GUI: "Composition Analysis" collapsing panel with "Analyze Canvas" button and findings list.)*

### B4. Variation Generator
Select any object or group and generate N visual variations of it.

- AI produces variations by: recoloring, reshaping (more angular / more organic), scaling proportions, adding/removing detail
- Variations appear as new nodes in a temporary "Variations" layer for review
- Accept one, accept all, or discard — non-destructive
- MCP tool: `generate_variations(node_id, count, intent: "more geometric" | "more organic" | ...)`

### B5. Composition Advisor
AI analyses the full canvas and identifies compositional weaknesses.

- Balance: "The visual weight is heavily top-left. Consider moving X or adding weight to Y."
- Contrast: "The background and foreground colors have a contrast ratio of 1.8:1 — too low for readability."
- Rhythm: "Elements A, B, C are distributed unevenly. Apply even spacing?"
- White space: "This region has no visual elements and may benefit from a subtle background texture or focal point."
- Purely advisory — produces a structured report, takes no action unless confirmed

### B6. Intent-Preserving Edit
When editing a node that was AI-generated, the system offers to re-run the original prompt with the edit baked in, rather than treating it as a raw path edit.

- "You're moving the anchor point of 'sun icon'. Regenerate with 'sun icon, slightly asymmetric rays' instead?"
- Preserves semantic intent vs. raw geometry editing
- Opt-in per edit

### B7. Multi-Agent Collaboration
Multiple MCP clients (multiple Claude sessions, or Claude + a specialised agent) can connect to the same document simultaneously.

- Document mutations are serialised through the existing actor model (no deadlocks)
- Each agent has a named "presence" visible in the Layers panel (coloured cursor indicator, like Figma multiplayer)
- Agents can "lock" a selection to prevent conflicts
- SSE events broadcast all mutations to all connected clients in real time
- Enables workflows like: "Agent A generates the composition, Agent B optimises the SVG output, Agent C checks brand compliance — all concurrently"

---

## Category C — Structured SVG & Developer Handoff

The community's #2 pain point with Illustrator: it destroys SVG quality. Photonic can own the "clean SVG" space.

### ~~C5. Multi-Target Export Profiles~~
~~Define named export profiles, each with its own format, scale, color space, and overrides.~~

*(implemented: `add_export_profile`, `list_export_profiles`, `remove_export_profile`, `run_export_profile` MCP tools — named export configurations stored in `.photon` file; supports SVG, PNG, JPEG, WebP formats with per-profile width/height/precision/semantic_ids settings; GUI: Export Profiles panel shows stored profiles with Remove buttons)*

---

## Category D — Document History & Versioning

Illustrator has undo. Photonic can have something closer to Git.

### ~~D2. Named Branches~~
*(implemented: `branch_create`, `branch_list`, `branch_switch`, `branch_delete` MCP tools + GUI Branches panel with Save/Switch/Delete. Branches are in-memory session state (like checkpoints). Merge strategy and disk persistence are future work.)*

### D3. Timeline Scrubber
Visual scrub through the full edit history (not just undo/redo).

- ~~Horizontal timeline in the History panel showing every recorded operation~~ *(implemented: `list_history` MCP tool + GUI "Edit History" collapsing panel with ⟳ refresh button — returns last N undo stack entries with descriptions, newest first)*
- ~~Click to jump to any step in history~~ *(implemented: `jump_to_history` MCP tool + GUI "Jump to step" DragValue slider in Edit History panel)*
- Drag the scrubber to preview any point in history without committing to it
- Checkpoints appear as named markers on the timeline
- Branching points appear as fork indicators

---

## Category E — Physics & Algorithmic Layout

No mainstream vector tool touches this. It's a genuine white space.

### ~~E1. Physics-Based Distribution~~
~~Distribute selected objects using a physics simulation.~~

~~- **Gravity mode**: objects fall toward a defined attractor point and settle~~
- **Repulsion mode**: ~~objects push each other apart until equilibrium (no overlaps, natural spacing)~~ *(implemented: `distribute_no_overlap` MCP tool + GUI "Distribute (No Overlap)" button — iterative pairwise bounding-box repulsion)*
~~- **Collision settle**: drop objects and let them stack/settle by gravity with collision detection~~
~~- **Magnetic snap**: objects snap to implicit grid lines defined by their neighbours~~
~~- Configurable: iterations, damping, gravity strength, collision margin~~
~~- "Freeze" the result to convert the simulation output back to static positions~~

### ~~E2. Rhythm & Repetition Detection~~
~~Analyse the document and identify visual rhythms, then let the user enforce or extend them.~~

~~- "These 4 icons appear to be evenly spaced at 32px. Make the 5th match?"~~
~~- "These circles decrease in size by 20% each time. Extend the pattern?"~~
~~- "This group repeats every 120° — you have 3 of 4 rotations. Add the missing one?"~~
~~- Purely suggestive — one-click accept or dismiss~~

*(implemented: `detect_rhythms` MCP tool + "Detect Rhythms" button in Composition Analysis GUI panel — detects horizontal/vertical spacing, uniform widths, size progressions, and rotational symmetry; returns structured JSON patterns with descriptions)*

### E3. Generative Fill-In (Spatial)
Select an empty region and ask Photonic to generate vector content that fits the surrounding visual context.

- AI reads the nearby shapes, color palette, and style language
- Proposes content in the MCP screenshot→observe loop
- Strictly additive — fills the void without modifying existing content
- MCP tool: `fill_region(bounding_box, intent: "decorative background" | "matching icon" | ...)`

---

## Category F — Mirror, Symmetry & Pattern Tools

Illustrator has patterns but no live symmetry drawing.

### F1. Live Symmetry Drawing
A drawing mode where strokes are mirrored in real time across configurable axes.

- ~~**Bilateral static mirror**~~ — *(implemented: `mirror_copy` MCP tool + GUI "Mirror H Copy" / "Mirror V Copy" buttons — duplicates and flips selected nodes across bounding-box center; copies are independent paths)*
- **Bilateral** (live linked clone mode) — editing the original updates the reflected copy in real time
- **Quad** (4-way: H + V mirror) — for mandalas, UI icons, tiles
- ~~**Radial**~~ (N-way rotational symmetry, configurable N) — *(implemented: `rotate_copies` MCP tool + GUI "Radial Copies" DragValue + Apply button — creates N evenly-spaced rotational copies around a node's bounding-box center)*
- **Custom axis** — define any line as the mirror axis, including diagonal
- "Flatten symmetry" bakes all copies into independent paths

### F2. Parametric Pattern Designer
A pattern editor that exposes pattern variables as sliders.

- Define a tile with named variables: `gap = 8`, `dot_radius = 4`, `rotation = 45`
- Sliders in the Properties panel update the pattern in real time across the entire fill
- Patterns can reference color tokens (so a brand color change updates all pattern fills)
- MCP tool: `set_pattern_variable(pattern_id, variable, value)` — AI can explore pattern variations

### ~~F3. Truchet Tile Generator~~
~~A specialised pattern tool for Truchet-style algorithmic tilings.~~

~~- User provides one or more tile designs~~
~~- Photonic auto-generates all valid rotations and mirrors~~
~~- Tiles placed in a grid with configurable rules (random, alternating, rule-based)~~
~~- Fully editable vector output~~
~~- Niche but beloved by generative art and textile designers — a real differentiator~~

*(implemented: `create_truchet_tiling` MCP tool — arcs, diagonals, triangles styles; configurable seed, tile size, colors; GUI buttons "Truchet Arcs" and "Truchet Triangles" in Data Visualization panel)*

---

## Category G — Motion & Animation Foundation

No vector illustration tool has good animation. Photonic can lay the groundwork.

### G1. Property Keyframes
Animate any numeric node property between named checkpoints.

- Define "Frame A" and "Frame B" checkpoints
- Mark specific properties as "animated between frames"
- Photonic interpolates (linear, ease-in-out, bezier curve) between the values
- Preview in-app with a play button

### G2. Lottie / CSS Animation Export
Export animated documents as:

- **Lottie JSON** — for web and mobile (React Native, iOS, Android)
- **CSS @keyframes** — for web use without a Lottie library
- **APNG / WebP animated** — for environments that can't run Lottie
- MCP tool: `export_animation(format: "lottie" | "css" | "apng", fps: 24, duration_ms: 2000)`

### G3. Motion Path
Attach an object to a path and animate its position along it.

- Object follows the path's tangent (auto-orient)
- Configurable easing per segment
- Integrates with Lottie export — Lottie supports motion paths natively

---

## Category H — Collaboration & Sharing

### H1. Shareable Document URLs
Export a read-only web viewer URL for any saved document.

- Recipients can inspect nodes, copy colors, download SVG assets — no Photonic install required
- Replaces the need for Figma-style "dev mode" for sharing with developers
- Self-hostable viewer (WASM-compiled renderer or pre-rendered SVG with metadata overlay)

---

## Category I — Precision & Technical Illustration

### ~~I1. Dimension Annotations~~
~~Snap-to-edge measurement annotations that auto-update when objects move.~~

~~- Click two points / edges → a live dimension line appears showing the distance~~
- Dimensions update automatically when the measured objects are moved or resized *(positions cached at creation time; not live-linked)*
~~- Styled as technical drawing annotations (configurable arrowhead style, text format)~~
~~- Toggle visibility (visible in editor, stripped from export)~~
~~- MCP tool: `add_dimension(from_node, to_node, axis: "x" | "y" | "diagonal")`~~

*(implemented: `add_dimension { from_node_id, to_node_id, axis?, label_offset? }` + `list_dimensions` + `remove_dimension { id }` MCP tools + GUI "Dimension Annotations" panel (visible when 2 nodes selected or dimensions exist) with H/V/Diagonal add buttons and per-entry remove buttons; rendered as orange tick-mark lines with distance labels as canvas overlay when guides are visible; stripped from exports)*

### I2. Construction Geometry
Non-printing helper geometry for precision drawing — like CAD construction lines.

- ~~**Infinite construction lines (horizontal, vertical, at any angle, through any point)**~~ — *(implemented: `add_construction_line` MCP tool and GUI panel — angled infinite reference lines through any point)*
- Tangent lines snapping to curves
- Perpendicular bisectors
- Construction circles (through 3 points, inscribed in triangle, etc.)
- Automatically excluded from all export formats
- Toggle visibility layer: `View > Show Construction Geometry`

### I3. Geometric Constraints (CAD-lite)
Snap objects to geometric relationships and enforce them:

- **Tangent**: this curve is always tangent to that line
- **Concentric**: these two circles always share a center
- **Parallel**: this segment is always parallel to that segment
- **Equal length**: these two paths are always the same length
- **Perpendicular**: these two segments are always 90°

Less full CAD, more "smart snapping with memory." Constraints are stored on the document and re-enforced after any edit.

---

## Category J — Quality-of-Life Improvements Illustrator Has Never Fixed

These are small but would make professionals choose Photonic immediately.

### J1. Stable, Versioned SVG Output
- SVG output format is explicitly versioned (`photonic-svg-v1`)
- Format only changes between major Photonic versions, with a migration flag
- Users can lock to a specific output format version per export profile
- Illustrator's silent format changes have destroyed pipelines for thousands of users — Photonic promises never to do this

### J2. Artboard Margins
- ~~**Define top/right/bottom/left margins per artboard**~~ — *(implemented: `set_artboard_margins { top?, right?, bottom?, left? }` + `get_artboard_margins` MCP tools + GUI "Artboard Margins" panel — values persist on document model; visual blue margin rectangle overlay on canvas when guides are visible)*
- Snap-to-margin as a first-class snapping mode
- ~~"Fit content to margins" button in artboard settings~~ *(implemented: `fit_to_margins` MCP tool + GUI "Fit to Margins" button in the Artboard Margins panel — scales and centers selected (or all) nodes to fill the safe area; uniform aspect-ratio mode by default)*

### J3. Non-Destructive Boolean Operations (All of them)
- Every boolean operation (union, subtract, intersect, exclude, divide, trim, merge, crop) has a non-destructive live-effect mode
- Original paths remain editable; result is re-computed on every edit
- "Flatten" to bake into permanent paths only when explicitly requested
- Photonic's document model should store boolean operations as composable operations, not just the result geometry

### ~~J4. Per-Object Undo History~~
~~- "Undo for this object only" — reverse the last N changes to a specific node without affecting the rest of the document~~
~~- Particularly valuable when an AI agent has been making many changes: `undo_node(node_id, steps: 3)` in the MCP~~

*(implemented: `undo_node` MCP tool + GUI "↩ Revert Last Edit" / "↩↩ Revert 3 Edits" buttons in Properties panel — scans undo stack for node-specific UpdateNode commands, applies the N-th pre-mutation snapshot as a new undoable command)*

### ~~J5. Persistent Smart Guides~~
*(implemented as `pin_object_guides` MCP tool + GUI "Pin Guides" button — creates permanent ruler guides at the bounding-box edges and center of selected nodes. Deduplicates within 0.5 px. Configurable edge set: all/edges/center or comma-separated subset. Guides are standard ruler guides and persist in the document file.)*

- Remaining: live-linked mode (guides update when object moves) — not yet implemented


---

## Prioritization Notes

### Build first (highest leverage, Photonic-unique):
1. **B7 Multi-Agent Collaboration** — core to MCP value proposition, no competitor has it
2. **C2 Copy as SVG Code** — immediate pain-point win; pulls Illustrator refugees
3. **J3 Non-Destructive Booleans** — most requested missing feature industry-wide
4. **A1 Live Property Constraints** — foundational for parametric everything
5. **F1 Live Symmetry Drawing** — high visual payoff, beloved by logo/icon designers

### Build second (medium effort, high delight):
6. **D2 Named Branches** — differentiates from Illustrator and Figma simultaneously
7. **C5 Multi-Target Export Profiles** — replaces a painful multi-step workflow
8. **E1 Physics-Based Distribution** — no competitor has it; gets shared on social
9. **B1 Prompt History on Objects** — AI-native, no other tool conceives of this

### Long-term (high complexity, high ceiling):
11. A3 Reactive Layout Engine
12. A4 Document Grammar
13. G1/G2 Animation + Lottie Export
14. I2/I3 Construction Geometry + Geometric Constraints
15. H1 Shareable Document URLs

---

*Bespoke section generated 2026-03-23. Based on: Adobe Illustrator community pain points (UserVoice, Adobe Community forums), competitive analysis (Figma, Affinity Designer, Inkscape, Sketch), and architectural opportunities unique to Photonic's MCP-first design.*
