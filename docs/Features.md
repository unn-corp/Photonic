# Photonic — Current Features

Complete inventory of features implemented as of 2026-03-23.

---

## MCP Tools (277 Tools)

### Shape Creation
| Tool | Description |
|---|---|
| `create_shape` | Rectangle, Rounded Rect (configurable `corner_radius`), Ellipse, **Arc** (`arc_start_angle`, `arc_end_angle`, `arc_open`), Polygon, Star, Line with fill/stroke. GUI: Arc tool in Shapes toolbar — drag to set bounding box, options panel controls sweep angles and open/closed mode. |
| `create_path` | SVG path data with style support |
| `create_spiral` | Archimedean spiral with `outer_radius`, `inner_radius`, `turns`, `segments_per_turn`; fill/stroke support |
| `create_grid` | Rectangular grid from bounding box with `cols` and `rows` cell divisions; single multi-subpath node. GUI: Grid tool in Shapes toolbar — drag to set bounds, options panel controls row/column count. |
| `create_polar_grid` | Polar (radial) grid with concentric rings and radial spokes; `rings`, `sectors`, `inner_radius` params. GUI: Polar Grid tool — drag defines outer bounds, options panel controls rings/sectors/inner ratio. |
| `create_flare` | Procedural lens flare: semi-transparent halo circle, radiating ray triangles, and concentric stroke rings — all grouped. Configurable ray count, ring count, halo color, ray opacity. |
| `create_curvature_path` | Create a smooth cubic bezier curve through a series of `[x, y]` points using Catmull-Rom interpolation. Auto-computes handles — no manual control points needed. Optional `closed` flag for smooth closed shapes. |
| `create_text` | Text nodes with font family, size, weight, alignment, fill/stroke, line_height, letter_spacing |
| `build_shape_from_points` | Polygon from a point array with custom connection order |

### Node Editing
| Tool | Description |
|---|---|
| `update_node` | Modify fill, stroke (incl. `dash_array` up to 3 pairs, `dash_offset`, `arrowhead_start`/`arrowhead_end`: `none`\|`filled_arrow`\|`open_arrow`), opacity, visibility, lock state, blend mode, tags, text content, inner/outer glow, gaussian glow |
| `delete_nodes` | Remove multiple nodes at once |
| `duplicate_nodes` | Deep-clone nodes with configurable x/y offset per copy |
| `create_array` | Grid or radial pattern repetition (grid: rows/cols with stride; radial: count around center) |

### Grouping & Hierarchy
| Tool | Description |
|---|---|
| `group_nodes` | Wrap multiple nodes into a group |
| `ungroup_nodes` | Dissolve a group back to its parent layer |
| `reorder_node` | Z-order control: send_to_back, bring_to_front, send_backward, bring_forward, move_above, move_below |

### Boolean Operations
| Tool | Description |
|---|---|
| `boolean_operation` | Union, Subtract, Intersect, Exclude — with `keep_originals` option |
| `add_anchor_points` | Insert a midpoint anchor on every path segment; optional `passes` (1–8); non-path nodes skipped |
| `delete_anchor_point` | Remove specific anchor points from a path node by zero-based BezPath element indices. Single undoable step. GUI: Direct Select tool + Delete key. |
| `snap_to_pixel` | Round the position (translation) of one or more nodes to integer coordinates for pixel-perfect alignment. GUI: "Snap to Pixel" panel with button when a node is selected. Single undo step. |
| *(GUI only)* | **Align to Artboard** — align one or more selected nodes to the document canvas: left, center horizontal, right, top, center vertical, bottom. Works on single nodes (unlike Align which requires 2+). GUI: "Align to Artboard" panel. Single undo step. |
| *(GUI only)* | **Object Hide/Show** — eye icon toggle per node in the properties panel; hides/shows the node without affecting layer visibility. MCP: `update_node` with `visible` field. Single undo step. |
| *(GUI only)* | **Editable X/Y position** — node X and Y coordinates in the properties panel are now DragValue inputs (drag or type to move); replaces read-only labels. Single undo step per change. |
| `pathfinder_divide` | Split two overlapping path nodes at every intersection edge into distinct colored face nodes (A-only, overlap, B-only). Both originals are removed; up to three face nodes created. MCP requires `[back_id, front_id]`. GUI: "Divide" button in Pathfinder panel when 2 nodes selected. |
| `divide_objects_below` | Use a path as a cutting edge to split all overlapping objects beneath it in z-order. Each overlapping object is replaced by face nodes (inside/outside the cutter); non-overlapping objects are untouched; cutter is removed. GUI: "Divide Objects Below" button in Path Operations panel. |
| *(GUI only)* | **Arrow-key nudge** — arrow keys move selected nodes by a configurable distance (default 1 px; Shift×10). Distance set in Edit → Behavior → "Arrow nudge (px)". Single undo step per keypress. |
| *(GUI only)* | **Cursor coordinate overlay** — live X/Y readout in document coordinates shown as a semi-transparent overlay at the bottom-left of the canvas whenever the cursor is inside it. Adapts to dark/light mode. |
| `recolor_artwork` | Map every unique solid fill in selected nodes to the nearest color in a target palette (Euclidean RGB distance). Gradient fills are skipped. `palette` accepts hex strings. GUI: "Recolor" panel with hex palette input field. Single undo step. |
| `distribute_on_path` | Place evenly arc-length-spaced copies of source nodes along a guide path. `path_node_id` is the guide; `node_ids` are the objects to clone; `count` sets total copies; `align_to_path` rotates each copy to face the path tangent. GUI: "Distribute on Path" panel when 2+ nodes selected. Single undo step. |
| `simplify_path` | Reduce anchor-point count using Ramer-Douglas-Peucker. Configurable `tolerance`; `dry_run` mode previews before/after point counts without applying. Reports percentage reduction. Undoable. |
| `outline_stroke` | Convert the stroke on each selected path node into a new filled closed path tracing the stroke outline. New node inherits stroke color/opacity as its solid fill; original node's stroke is disabled. Batch multi-node support; single undo step. Also available on the radial wheel for path nodes. |
| `reverse_path_direction` | Reverse the winding direction of one or more path nodes; preserves cubic/quadratic Bézier curve fidelity; non-path nodes skipped |
| `average_anchor_points` | Reposition all on-curve anchors to their mean position. `axis`: `"horizontal"` (equalise X), `"vertical"` (equalise Y), `"both"` (centroid, default). Control handles shift with their anchor to preserve local curve shape. Non-path nodes skipped. |
| `offset_path` | Create a parallel copy of one or more paths expanded outward (positive distance) or contracted inward (negative distance). Configurable corner join style (miter/round/bevel). `create_copy: true` (default) adds a new node above the original; `false` replaces in place. Non-path nodes skipped; single undo step. GUI: "Expand (+2 px)" / "Contract (−2 px)" buttons in path properties panel. |
| `split_into_grid` | Divide a path node's bounding box into a rows×cols grid of rectangle nodes. Each cell inherits the source node's fill, stroke, opacity, and blend mode. Optional `gutter_x`/`gutter_y` gutters between cells. Source is deleted by default (`keep_original: false`). Single undo step. |
| `join_paths` | Close open subpaths (1 node) or merge two path nodes into one by connecting their nearest open endpoints with a straight line (2 nodes). Single undo step. Also available in the Path Operations panel ("Close Path") and Boolean Operations panel ("Join Paths") and radial wheel. |
| `pathfinder_crop` | Clip all selected path nodes to the boundary of the frontmost selected node (highest z-order). Each back node is replaced by `path ∩ frontmost_path`; the frontmost is removed. All transforms are baked before the intersection. GUI: "Pathfinder" section in the properties panel (2+ nodes selected). Single undo step. |
| `pathfinder_minus_back` | Subtract all back nodes from the frontmost selected path (Illustrator's Minus Back). Each back node's baked path is subtracted in sequence from the frontmost node; back nodes are removed. Frontmost node's style is preserved. GUI: "Minus Back" button in the Pathfinder panel (2+ nodes selected). Single undo step. |
| `pathfinder_minus_front` | Subtract the frontmost selected path from every back node (Illustrator's Minus Front). The frontmost node punches a hole in each back node (`back − front`); the frontmost is removed. Each back node's style is preserved. GUI: "Minus Front" button in the Pathfinder panel (2+ nodes selected). Single undo step. |
| `pathfinder_trim` | Remove hidden areas from every selected path node (Illustrator's Trim). Nodes processed back-to-front; each node's path = `its_path − union(all_paths_above)`. Strokes disabled on all results; fills preserved; no nodes removed. GUI: "Trim" button in the Pathfinder panel (2+ nodes selected). Single undo step. |
| `pathfinder_outline` | Convert selected filled path nodes to stroked outlines (Illustrator's Outline). Solid fill color moves to stroke; fill set to none. Gradient fills fall back to black. Existing stroke width preserved (default 1 pt). Path data unchanged; non-path nodes skipped. GUI: "Outline" button in the Pathfinder panel. Single undo step. |
| `pathfinder_merge` | Trim all selected nodes of hidden areas, then merge (union) any nodes sharing the same solid fill color into a single shape (Illustrator's Merge). Non-solid fills each produce a separate result node. Original nodes replaced. Strokes disabled on all results. GUI: "Merge" button in the Pathfinder panel (2+ nodes selected). Single undo step. |
| *(GUI only)* | **W/H resize DragValues** — Width and Height of the selected path node are now editable DragValues in the properties panel (world-space AABB dimensions). Dragging or typing a new value scales the node around its top-left anchor. Reuses the `set_node_size` scale-around logic. Single undo step. |
| *(GUI only)* | **Outline Mode** — toggle (Ctrl+Y or Canvas settings checkbox) that covers the GPU-rendered canvas with a flat background and redraws all visible path nodes as 1 px wireframe strokes. Useful for checking shape structure and overlaps without color distraction. |
| *(GUI only)* | **Copy / Paste / Paste in Place** — Ctrl+C copies selected nodes to an in-process GUI clipboard; Ctrl+V pastes with +10px offset; Ctrl+Shift+V pastes at exact original coordinates (Paste in Place). System clipboard images paste as raster layers, while SVG/HTML vector clipboard data is imported as editable nodes. Each paste is a single undoable step. MCP surface: `paste_from_history` with `offset_x=0`. |
| *(GUI only)* | **Rotation DragValue** — rotation angle in degrees added to the properties panel (below X/Y). Extracts current rotation from the transform matrix, rotates around the node's world-space bounding-box center by the delta. Single undo step. |
| `convert_anchor_points` | Convert all cubic bezier anchor points in selected path nodes to `smooth` (junction handles made collinear through each interior anchor, preserving outgoing magnitude) or `corner` (handles retracted to anchor points, producing straight-line segments / cusps). Non-path nodes skipped. Single undoable step. GUI: "To Smooth" / "To Corner" buttons in the Path Operations panel. |
| `magic_wand_select` | Click at a canvas coordinate `(canvas_x, canvas_y)` to select the topmost node at that point, then expand the selection to all nodes sharing the specified `attribute` (`fill_color`, `stroke_color`, `stroke_weight`, `opacity`, `blend_mode`, `object_type`) within `tolerance`. GUI: Magic Wand tool in Path Editing toolbar — click any object to select all matching. Attribute and tolerance configurable in the options panel. |
| `create_freehand_path` | Create a polyline path from an ordered list of canvas-space `[x, y]` points. Equivalent to the Pencil tool drag. GUI: Pencil tool in Path Editing toolbar — drag to draw; points are collected every ~5 canvas units and a `LineTo` path is created on mouse release. |
| `enter_isolation_mode` / `exit_isolation_mode` | Enter Isolation Mode for a group: selects all its direct children and restricts click-selection to those children. Exit clears the selection and restores normal editing. GUI: double-click a group in the Select tool to enter; Escape or double-click outside to exit. Status bar shows "Isolation: [name]" breadcrumb. |
| `select_inside_group` | Replace the selection with the direct children of the specified group node. Equivalent to Alt+click in Illustrator's Group Selection tool. GUI: Alt+click on a group in the Select tool selects the topmost child node instead of the group. |
| `get_recent_colors` | Return the list of recently used solid fill and stroke colors (up to 20), ordered most-recently-used first. GUI: swatches row shown below the Fill panel whenever recent colors exist — click any swatch to apply that color as the selected node's fill. Colors are recorded automatically on every fill/stroke change. |
| `lasso_select` | Provide a `points` polygon (array of `[x, y]` canvas-space pairs) to select all nodes whose AABB centroid falls inside the polygon (ray-casting). `centroid_mode: true` (default) tests the center; `additive: true` adds to the existing selection instead of replacing it. GUI: Lasso tool in Path Editing toolbar — drag freehand on canvas to enclose objects; release selects enclosed nodes. Shift+drag adds to selection. |
| `scissors_cut` | Cut a path node at the canvas point nearest to `(canvas_x, canvas_y)`, splitting it into two open path nodes that inherit the original's style, transform, opacity, and blend mode. Canvas point is transformed to node-local space before the split. Original node is removed; two named `(1/2)` / `(2/2)` nodes are created. Single undoable batch step. GUI: Scissors tool in Path Editing section of toolbar — crosshair cursor, blue dot indicator snaps to nearest path within 20 px, click to split. |
| `add_guide` / `remove_guide` / `list_guides` / `clear_guides` | Ruler guide management — add horizontal or vertical reference lines at a precise document-unit position; optional custom RGBA color; locked guides cannot be deleted. `clear_guides` removes all unlocked guides. All mutations use `SetGuides` command for single undo. GUI: guides rendered as cyan overlay lines on the canvas (toggle with Ctrl+; or the "Show Guides" checkbox in Canvas settings). "Clear All Guides" button shows live count. |

### Transforms & Alignment
| Tool | Description |
|---|---|
| `apply_transform` | Translate, Rotate (with origin), Scale (with origin), Reflect (H/V), Matrix (6-element affine), Shear (shear_x/shear_y with optional origin) |
| `align_nodes` | Align by edges/centers: left, center_horizontal, right, top, center_vertical, bottom; distribute evenly or with an exact `spacing` pixel gap; `anchor` modes: `selection` (default), `canvas`, `key_object` (align all nodes to a specific node's bounding box without moving it) |
| `set_node_size` | Exact pixel dimensions with anchor point and aspect ratio lock |
| `layout_nodes` | 2D spatial arrangement with flex and grid modes |
| `measure_nodes` | World-space bounding box, center, pairwise distance and angle |

### Styling
| Tool | Description |
|---|---|
| `style_transfer` | Copy fill, stroke, opacity, blend mode from source to targets |
| `find_replace_style` | Batch find/replace fill color, stroke color, stroke width, and font family across document or selection; dry-run preview mode; color/width tolerance |
| `find_replace_text` | Search and replace text node content using plain strings or regular expressions; optional case-sensitivity, node scoping, and dry-run preview; returns structured diff of old/new content |
| `make_compound_path` | Combine two or more path nodes into a single compound path; overlapping subpaths create holes via the even-odd fill rule; bottommost node's fill/stroke/position are kept; all transforms baked; GUI: "Make Compound Path" button in Compound Path panel (2+ nodes selected); single undo step |
| `release_compound_path` | Release a compound path back into individual subpath nodes; each subpath inherits the compound path's fill and stroke; GUI: "Release Compound Path" button in Compound Path panel (compound node selected); single undo step |
| `adjust_colors` | Shift RGBA channel values across selected path nodes; each `delta_r/g/b/a` is added to the channel value and clamped to [0,1]; handles solid fills, gradient stops, fluid/mesh gradient points, and stroke colors; optional `node_ids` subset; GUI: "Adjust Colors" panel with R/G/B/A sliders and Apply/Reset buttons; single undo step |
| `invert_colors` | Invert all RGB color values (1 − value, alpha preserved) on selected path nodes; handles solid fills, gradient stops, fluid/mesh gradient points; optional `node_ids` subset; single undo step |
| `convert_to_grayscale` | Convert fill and stroke colors to perceptual grayscale (ITU-R BT.601: 0.299R + 0.587G + 0.114B) on selected path nodes; handles solid fills, gradient stops, fluid/mesh gradient points; optional `node_ids` subset; single undo step |
| `blend_colors` | Distribute fill colors linearly across 2+ path nodes; first and last keep their solid fill colors, intermediate nodes receive interpolated values; optional `direction` axis (`horizontal`/`vertical`/`depth`) auto-sorts before blending; single undo step |
| `color_guide` | Generate a color harmony palette from a base color (explicit hex or from first selected node's solid fill); supports complementary, analogous, triadic, split_complementary, tetradic, monochromatic rules; returns swatch array with hex, r, g, b, a per color |

### Querying
| Tool | Description |
|---|---|
| `find_nodes` | Query by tag, name, type, layer, visibility, world-space region |
| `get_node` | Retrieve a single node by ID or name |
| `get_document_info` | Compact document summary: canvas size, layer list with node counts, node totals by kind (path/text/group), unique font names, unique solid fill colors. GUI: shown in Properties panel when nothing is selected. |
| `get_document_state` | Full document tree; optional path_data inclusion and summary mode |
| `inspect_node` | Computed geometry/structure metrics: path perimeter, area, centroid, anchor count; group descendant count, unique colors, total complexity; text line/char count and font properties |
| `auto_name_nodes` | Rename nodes with descriptive names derived from content and geometry: text nodes use their content, groups show child count, paths derive a colour + shape description (e.g. "blue medium square"). `scope`: `selection`\|`document`; `overwrite`: also rename non-generic names; `dry_run`: preview without applying |
| `check_style_continuity` | Analyse style consistency across the document or a node subset. Flags outliers — nodes whose fill color, stroke width, opacity, or font family deviate from the dominant values. Configurable `checks` (fill/stroke/opacity/font), `node_ids` subset, and `outlier_threshold`. Read-only; returns a structured JSON report. |
| `clean_up` | Batch-remove degenerate content: stray points (paths with no drawing segments), unpainted objects (no visible fill or stroke), and empty text nodes. All three checks are on by default and can be toggled independently. `dry_run: true` previews what would be removed without making any changes. |
| `select_same` | Select all document nodes sharing a specific attribute with a reference node: `fill_color`, `stroke_color`, `stroke_weight`, `opacity`, `blend_mode`, or `object_type`. Configurable numeric/color tolerance. Updates the active selection and returns matching IDs. |
| `select_by_kind` | Select all nodes of a specified object type: `path`, `text`, `group`, or `same_layer` (all nodes on the active layer). Optional `additive` flag extends the current selection. GUI surface: "Select all…" buttons in the properties panel when nothing is selected. |
| `smooth_path` | Smooth jagged or polygonal paths using Chaikin's corner-cutting algorithm. `factor` (0–0.5) controls rounding strength; `iterations` (1–8) sets the number of passes. Applies to specified node IDs or the current selection. GUI surface: Smooth tool (click or drag over a path to smooth it). |
| `zig_zag_path` | Apply zig-zag (sharp corners) or smooth wave (bezier) distortion to path segments. Configurable `size` (amplitude), `ridges_per_segment`, and `smooth` flag. GUI: "Zig Zag" button in Path Operations panel. Single undo step. |
| `pucker_bloat` | Radially distort path points from a center. Positive `strength` = bloat (expand outward), negative = pucker (contract inward). Center defaults to path centroid. GUI: "Pucker" / "Bloat" buttons in Path Operations panel. Single undo step. |
| `roughen_path` | Randomly displace path anchor and control points for hand-drawn/organic effects. Configurable `size` (max displacement), `detail` (subdivision passes), and `seed` (reproducible). GUI: "Roughen" button in Path Operations panel. Single undo step. |
| `twirl_path` | Spiral-rotate path points around a center with linear falloff — points near center rotate more, creating a vortex effect. Configurable `angle` (degrees) and center. GUI: "Twirl" button in Path Operations panel. Single undo step. |
| `tag_nodes` | Batch add/remove tags on nodes. Tags enable semantic queries via find_nodes. |
| `sample_color_at` | Eyedropper: sample fill/stroke color of the topmost node at a canvas coordinate. Returns hex colors and node info. |
| `set_active_layer` | Set the active layer for new node creation. By UUID or name. |
| `delete_layer` | Delete a layer. Nodes moved to first remaining layer by default, or deleted with delete_nodes=true. |
| `move_to_layer` | Move nodes to a different layer. Appended to top of target layer z-order. Undoable. |
| `add_dimension_line` | Technical dimension annotation — extension lines + arrowhead dimension line + distance label, all grouped. For technical/architectural illustrations. |
| `reorder_layers` | Change layer stacking order. Provide complete order array (bottom to top). Undoable. |
| `set_selection` | Set active selection to specific node IDs. Replace or additive mode. |
| `get_selection` | Return current selection — node IDs, names, kinds, visibility, lock state. Read-only. |
| `flatten_group` | Recursively ungroup all nested groups into flat nodes. Simplifies complex SVG imports. |
| `center_on_canvas` | Center nodes on canvas without scaling. Supports H-only or V-only centering. |
| `remove_fill` | Remove fill from selected nodes (set to transparent). |
| `remove_stroke` | Remove stroke from selected nodes (set to none). |
| `fit_to_canvas` | Scale and center all artwork to fit within canvas bounds. Uniform scale (never up), configurable padding. |
| `create_scatter_plot` | X/Y scatter plot from data point pairs. Auto-scaled, configurable dot radius and color. Single compound path for efficiency. |
| `scatter_copies` | Randomly scatter copies within a rectangular area. Random position, rotation, and scale variation. Deterministic seed. For confetti, foliage, particles. |
| `create_line_chart` | Data-driven line chart with smooth Catmull-Rom curves. Multi-series overlay, optional area fill. Auto-scaled axes. |
| `create_bar_chart` | Data-driven bar chart. Vertical or horizontal, configurable gap/colors/labels. Bars grouped. Tableau-10 defaults. |
| `create_pie_chart` | Procedural pie/donut chart from data values. Proportional slices, Tableau-10 palette, optional inner radius for donut style. Slices grouped. |
| `create_radar_chart` | Radar (spider) chart from multi-dimensional data. Configurable axes, multi-series overlapping polygons, semi-transparent fill, concentric grid rings, Tableau-10 palette. GUI: "Radar Chart (demo)" button in Data Visualization panel. |
| `create_stacked_bar_chart` | Stacked bar/column chart from multiple data series. Segments stacked per position showing part-to-whole relationships. Vertical (column) or horizontal (bar) mode. Tableau-10 palette. GUI: "Stacked Column (demo)" button in Data Visualization panel. |
| `create_parametric_shape` | Closed path from parametric equations: Lissajous, Superellipse, Rose, Hypotrochoid, Epicycloid. Configurable frequency, exponent, petals, inner/pen ratio, sample density. GUI: "Lissajous", "Superellipse", "Rose Curve" demo buttons in Data Visualization panel. |
| `create_truchet_tiling` | Procedural Truchet tiling over a rectangular area. Three styles: "arcs" (quarter-circle arc pairs), "diagonals" (random diagonal lines), "triangles" (filled triangles). Configurable tile size, seed, color, background, stroke width. All tiles grouped. GUI: "Truchet Arcs" and "Truchet Triangles" demo buttons in Data Visualization panel. |
| `distribute_no_overlap` | Push selected nodes apart using iterative pairwise bounding-box repulsion until no boxes overlap. Configurable padding gap and iteration cap. Uses current selection if `node_ids` is empty. Single undo step. GUI: "Distribute (No Overlap)" button in Distribute No Overlap panel (visible when 2+ nodes selected). |
| `noise_deform` | Apply two-octave sinusoidal displacement to all path anchor and control points, producing smooth wave-like organic deformation. Configurable amplitude, frequency, phase seed, and axis (both/x/y). GUI: "Wave Deform" and "Swell" buttons in path distortion panel. |
| `mirror_copy` | Duplicate selected nodes and flip each copy across its own bounding-box center, producing mirrored twins in the same layer. Supports horizontal (left-right) and vertical (top-bottom) axes. GUI: "Mirror H Copy" and "Mirror V Copy" buttons next to Flip controls. |
| `add_export_profile` | Save a named export configuration (format, dimensions, SVG precision) to the document for one-command re-export. Replaces existing profile with same name. |
| `list_export_profiles` | List all named export profiles stored in the current document. |
| `remove_export_profile` | Delete a named export profile from the document. |
| `run_export_profile` | Execute a named export profile and return the export data (SVG markup or base64 raster). GUI: Export Profiles panel shows stored profiles with Remove buttons. |
| `pin_object_guides` | Create persistent ruler guides at the bounding-box edges and center of selected nodes. Deduplicates within 0.5 px. Configurable edge set (all/edges/center or custom). GUI: "Pin Guides" button in properties panel. |
| `reverse_node_order` | Reverse the front-to-back stacking order of children within selected group nodes. Topmost child becomes bottommost. Single undoable step. GUI: "Reverse Order" button in group properties (shown when 2+ children). |
| `set_node_prompt` | Record an AI prompt on a node's prompt_history field (append/prepend/replace modes). Enables creative provenance tracking — an agent can later read *why* a node looks the way it does. |
| `get_node_prompts` | Return the full chronological prompt history for a node. GUI: "Origin (Prompt History)" collapsing section in Properties panel (shown when history is non-empty). |
| `get_document_template` | Capture the current document as a reusable template — canvas size, layers, guides, and export profiles preserved; all node content stripped. GUI: "Copy Template JSON" button in Export Profiles panel. |
| `apply_document_template` | Apply a captured template to the current document non-destructively — merges canvas size, guides, export profiles, and new layers (by name). Existing nodes are never removed. |
| `select_similar` | Select all nodes whose visual attributes match the reference node(s). Criteria: fill_color, stroke_color, stroke_width, kind, opacity, tags (comma-separated). Configurable tolerance. GUI: "Select Similar Fill" button in properties panel. |
| `create_paragraph_style` | Save a named paragraph style (alignment, line height, letter spacing, font family/size) to the document. Capture from a source text node or specify explicitly. |
| `list_paragraph_styles` | List all named paragraph styles saved in the document. |
| `apply_paragraph_style` | Apply a named paragraph style to one or more text nodes; only defined attributes are changed. GUI: Paragraph Styles panel in text node properties. |
| `delete_paragraph_style` | Delete a named paragraph style from the document. |
| `create_character_style` | Save a named character style (font, size, weight, fill, letter spacing, line height) to the document. Capture from a source text node or specify attributes directly. |
| `list_character_styles` | List all named character styles saved in the document. |
| `apply_character_style` | Apply a named character style to one or more text nodes; only defined attributes are changed. GUI: Character Styles panel in text node properties with per-style Apply/Delete buttons. |
| `delete_character_style` | Delete a named character style from the document. |
| `add_color_swatch` | Add (or update) a named color swatch in the document palette. Hex color stored with the swatch name. |
| `list_color_swatches` | List all named color swatches in the document palette. |
| `apply_color_swatch` | Apply a named swatch's color to the fill of one or more nodes. GUI: Color Swatches panel with Apply/Delete per swatch row. |
| `update_color_swatch` | Rename or recolor an existing swatch; optionally propagates the color change to all nodes using the old color. |
| `delete_color_swatch` | Remove a named swatch from the document palette (does not alter existing node fills). |
| `define_spot_color` | Define (or update) a named spot color — a named ink with an optional `overprint` flag for print-production workflows. Stored in document. |
| `list_spot_colors` | List all named spot colors in the document with hex values and overprint flags. |
| `apply_spot_color` | Apply a named spot color as a solid fill to one or more nodes. GUI: Spot Colors panel with Apply/Delete per row. Single undo step. |
| `delete_spot_color` | Remove a named spot color from the document. Does not alter existing node fills. |
| `save_gradient_swatch` | Save the gradient fill of a node as a named reusable swatch. Updates existing swatch with same name. GUI: "Save Gradient Swatch" button in Gradient Swatches panel (visible when gradient-filled node selected). |
| `list_gradient_swatches` | List all named gradient swatches stored in the document. |
| `apply_gradient_swatch` | Apply a named gradient swatch's fill to one or more nodes. GUI: "Apply" button per swatch row in Gradient Swatches panel. Single undo step. |
| `delete_gradient_swatch` | Remove a named gradient swatch from the document. GUI: "Del" button per swatch row. Does not alter existing node fills. |
| `analyze_composition` | Analyze the visual composition of the document — checks quadrant balance, canvas density, object overlaps, near-duplicate fill colors, palette size, and off-canvas objects. Returns structured JSON findings with severity (ok/info/warning). GUI: "Composition Analysis" panel with "Analyze Canvas" button and findings list. Read-only. |
| `branch_create` | Save the current document state as a named branch (session-scoped). Overwrites if name already exists. GUI: "Branches" panel with branch name input + Save button. |
| `branch_list` | List all named document branches saved in the current session. GUI: Branches panel shows all names. |
| `branch_switch` | Restore the document to a previously saved named branch. Clears undo/redo history. GUI: "Switch" button per branch in Branches panel. |
| `branch_delete` | Delete a named branch. Live document is not affected. GUI: "✕" button per branch in Branches panel. |
| `make_clipping_mask` | Create a clipping mask on a group node — the topmost child becomes the clip path for all other children. Undo-safe. GUI: Make/Release buttons in Clipping Mask panel (Group nodes only). |
| `release_clipping_mask` | Release the clipping mask from a group node; all children revert to normal visible objects. Undo-safe. |
| `set_text_path` | Place a text node along a path spine (Type on a Path). Text flows along the curve at a configurable start offset. Undo-safe. GUI: "Set as Path Spine" button when text + path are both selected. |
| `clear_text_path` | Remove the path spine from a text node, reverting to normal positioned text. Undo-safe. GUI: "Clear Path" button in Type on a Path panel. |
| `set_text_direction` | Set text layout to horizontal (default) or vertical (top-to-bottom). Undo-safe. GUI: toggle button in Text Operations panel. |
| `set_text_area` | Flow a text node inside a closed path boundary (Area Type). The area_path_id references the containing shape. Undo-safe. GUI: Set as Area Boundary button when text + path selected. |
| `clear_text_area` | Remove the area boundary from a text node, reverting to normal point text. Undo-safe. GUI: Clear Area button in Area Type panel. |
| `define_variable` | Define (or update) a named document variable (key-value string pair) for data-driven design. GUI: Variables panel with delete buttons and Apply All. |
| `list_variables` | List all named document variables and their current values. |
| `set_variable_value` | Update the string value of an existing document variable. |
| `delete_variable` | Remove a named document variable from the document. |
| `apply_variables` | Batch-replace text content in all bound text nodes with their variable's current value. Undo-safe (single batch). GUI: Apply All Variables button. |
| `bind_text_variable` | Bind a text node to a document variable so apply_variables updates its content. GUI: Variable Binding panel in text node properties. |
| `unbind_text_variable` | Remove the variable binding from a text node. Undo-safe. GUI: Unbind button in Variable Binding panel. |
| `define_symbol` | Define a node as a named reusable symbol master. Stores the node ID as the master reference. GUI: "Define as Symbol…" button in Symbols panel. |
| `list_symbols` | List all defined symbols with their names, IDs, and master node IDs. |
| `place_symbol` | Place an instance of a named symbol at a specified (x, y) position. Creates a clone with `symbol_ref` set. Undo-safe. GUI: "Place" button in Symbols panel. |
| `break_link_to_symbol` | Remove the `symbol_ref` from a node, making it an independent editable copy. Undo-safe. GUI: "Break Link to Symbol" button in Symbols panel. |
| `delete_symbol` | Remove a symbol definition from the document registry. Existing instances retain their `symbol_ref` UUID. GUI: "Del" button in Symbols panel. |
| `set_font_style` | Set the font style (normal, italic, or oblique) on a text node. Also fixes font weight rendering which was stored but previously ignored. Undo-safe. GUI: I toggle button in Text Operations panel. |
| `set_font_weight` | Set the font weight (100–900) on a text node. Clamped to valid range. Undo-safe. GUI: B toggle button in Text Operations panel. |
| `get_canvas_overview` | Return a compact spatial map of all visible nodes: bounding box, layer, kind, and fill color for each node, plus overall canvas bounds. GUI: Navigator collapsing panel in properties with miniature document thumbnail. |
| `tag_node_for_export` | Tag a node for batch asset export with a name, format (svg/png/jpeg/webp), and scale multipliers. Pass empty name to remove tag. GUI: Asset Export panel in node properties. |
| `export_tagged_assets` | Export all tagged nodes as assets. SVG returned inline; raster assets return metadata for export_raster. Optional filter by asset name. |
| `point_on_path` | Sample points along a path at specified fractions (0–1). Returns (x, y) coordinates and tangent angle. Critical for AI-assisted precise positioning along curves. |
| `create_speech_bubble` | Speech bubble shape — rounded rectangle with configurable triangular tail. Defaults to white fill + black stroke. |
| `set_visibility` | Show/hide nodes. Omit `visible` to toggle. Hidden nodes not rendered but preserved in document. |
| `set_locked` | Lock/unlock nodes. Locked nodes cannot be GUI-selected or modified. Omit `locked` to toggle. |
| `select_all` | Select all nodes, optionally filtered by layer. |
| `deselect_all` | Clear the selection. |
| `set_blend_mode` | Batch-set blend mode on multiple nodes. All 16 modes supported. |
| `set_opacity` | Batch-set opacity on multiple nodes at once. More efficient than individual update_node calls. |
| `randomize_colors` | Assign random colors from a palette (or auto-generated) to selected nodes. Configurable fill/stroke targeting and seed. |
| `duplicate_layer` | Duplicate a layer with all its nodes. Deep-clones every node with new IDs. Single undoable batch. |
| `swap_fill_stroke` | Swap fill and stroke colors on selected nodes. Fill becomes stroke, stroke becomes fill. Works with paths and text. |
| `resize_canvas` | Resize the document canvas dimensions. Does not scale artwork. Undoable command. |
| `create_heart` | Heart shape using smooth cubic bezier curves. Defaults to red fill. Configurable size. |
| `create_gear` | Procedural gear/cog shape with configurable teeth count, inner/outer radius, and center hole. Compound path with even-odd fill. |
| `flip_nodes` | Flip/mirror nodes horizontally or vertically around bounding box center. Paths flipped geometrically; text/groups via transform. GUI: Flip H/V buttons. |
| `create_cross` | Cross/plus shape primitive. 12-point polygon with configurable size, thickness, and rotation. Set rotation=45 for X shape. |
| `measure_path` | Measure a path's total arc length, anchor/segment count, bounding box, and open/closed status. Read-only. |
| `measure_distance` | Measure distance between two points or nodes. Returns distance, delta X/Y, and angle. Targets: [x,y] coordinates or node UUID/name. |
| `create_arrow_shape` | Block arrow/chevron shape with triangular head and rectangular shaft. Configurable length, head/shaft proportions, and direction angle. |
| `create_donut` | Ring/annulus shape with inner and outer radius. Supports full rings and partial arc segments for pie chart slices. Even-odd fill rule for proper hole rendering. |
| `create_sunburst` | Radial sunburst with alternating filled wedges. Configurable ray count, inner/outer radius, and color. Classic retro/vintage effect. |
| `create_wave_pattern` | Generate decorative sine wave patterns with configurable wavelength, amplitude, line count, and area. Useful for water, topographic, and abstract effects. |
| `hatch_fill` | Fill path shapes with parallel hatching lines clipped to path boundary. Configurable spacing, angle, optional cross-hatch angle, stroke width, and color. |
| `stipple_fill` | Fill path shapes with randomly placed dots using rejection sampling. Configurable dot count, radius, color, and seed. Creates a separate stipple path on the same layer. |
| `add_drop_shadow` | Add a drop shadow behind nodes — creates an offset, recolored copy with configurable offset, color, and opacity. Works with paths, text, and groups. GUI: "Drop Shadow" button. |
| `transform_copies` | Create N copies with cumulative translate/rotate/scale/opacity offsets. Perfect for radial patterns, step-and-repeat, spiral scaling, and fade trails. |
| `round_corners` | Round sharp corners with smooth quadratic arc fillets. Radius auto-clamped to half shortest segment. GUI: "Round Corners" button in Path Operations panel. Single undo step. |
| `warp_envelope` | Apply named envelope warp distortions: all 15 Illustrator presets (arc, arc_lower, arc_upper, arch, bulge, wave, flag, squeeze, inflate, fisheye, shell_lower, shell_upper, fish, rise, twist). Configurable `bend`, `distort_h`, `distort_v`. GUI: Arc/Wave/Bulge/Flag buttons in Path Operations panel. Single undo step. |
| `crystallize_path` | Add sharp outward spike detail to path segments, creating star/crystal/frost edges. Configurable `size` (spike height) and `count` (spikes per segment). GUI: "Crystallize" button in Path Operations panel. Single undo step. |
| `scallop_path` | Replace path segments with smooth inward-curving scallop arcs. Configurable `depth` and `count` (arcs per segment). GUI: "Scallop" button in Path Operations panel. Single undo step. |
| `blend_objects` | Generate intermediate path nodes that interpolate shape geometry, fill color, opacity, and position between two source paths. Three step modes: `steps` (fixed count), `smooth_color: true` (auto-compute from color distance), `spacing` (Specified Distance — steps from pixel gap). GUI: "Blend (5 steps)" / "Blend (Smooth Color)" / "Blend (32 px spacing)" buttons when 2 nodes selected. |
| `set_blend_spine` | Assign a path node as the custom blend spine for a group node. The spine guides interpolation between blend objects. Stores `blend_spine_id` on `GroupNode`. GUI: "Blend Spine" CollapsingHeader in Group Operations panel with UUID input + Set/Clear buttons. |
| `clear_blend_spine` | Remove the blend spine assignment from a group node, reverting to default straight-line interpolation. |
| `reverse_blend_spine` | Reverse the direction of the blend spine path in a group node, inverting the start-to-end interpolation order. GUI: "Reverse Spine" button in Blend Spine panel when spine is assigned. |
| `expand_blend` | Expand a blend group into individual discrete objects — dissolves the group and places all children as standalone nodes at the parent layer. Equivalent to Illustrator's Object > Blend > Expand. GUI: "Expand Blend" button in Group Operations panel. |
| `save_workspace` | Save the current properties-panel search filter query as a named workspace preset. Overwrites on name conflict. GUI: name input + Save button in Workspaces panel. |
| `load_workspace` | Load a saved workspace by name; returns its search_query for the GUI to apply as a panel filter. Read-only. GUI: per-workspace Load button in Workspaces panel. |
| `list_workspaces` | List all saved workspace presets with their search queries. Read-only. |
| `delete_workspace` | Delete a named workspace preset from the document. GUI: ✕ button per workspace row. |
| `load_symbol_library` | Load a built-in symbol library into the document. Libraries: "arrows" (6 directional arrows), "shapes" (diamond, hexagon, pentagon, star-5pt, cross, checkmark), "ui" (checkbox-empty, checkbox-checked, radio-empty, close-x, menu-lines, plus-icon). Master nodes placed off-canvas and hidden. GUI: "Load Library…" CollapsingHeader in Symbols panel with Arrows/Shapes/UI Icons buttons. |
| `spray_symbol_instances` | Scatter N instances of a named symbol around a center point using golden-angle spiral distribution (1–200 instances, configurable spread radius). GUI: "Symbol Sprayer" CollapsingHeader in Symbols panel. |
| `set_symbol_override` | Set per-instance fill and/or stroke color overrides on a symbol instance node (Dynamic Symbol). Overrides stored as hex strings on `symbol_fill_override`/`symbol_stroke_override` fields. GUI: "Symbol Override" panel when a symbol instance is selected. |
| `clear_symbol_overrides` | Clear all per-instance color overrides from a symbol instance, reverting to master defaults. GUI: "Clear Override" button in Symbol Override panel. |
| `flatten_transparency` | Bake node opacity and fill opacity into color alpha values, then set opacity to 1.0 for print-ready output. Works on paths and text. GUI: "Flatten Transparency" panel button (visible when 1+ nodes selected). Single undo step. |
| `load_swatch_library` | Load a predefined color swatch library into the document palette. Libraries: web (16 HTML colors), material (16 Material Design 500 tones), pastels (12), earth_tones (12), neon (12), grayscale (11). Skips duplicates by name. GUI: Library dropdown + Load button in Color Swatches panel. |
| `define_graphic_style` | Save a named graphic style (fill + stroke + opacity preset). Capture from a node by passing node_id, or define explicitly with fill_hex/stroke_hex/stroke_width/opacity. Overwrites on name conflict. GUI: "Save Style" text input + button in Graphic Styles panel. |
| `list_graphic_styles` | List all named graphic styles in the document. |
| `apply_graphic_style` | Apply a named graphic style (fill, stroke, opacity) to one or more nodes. GUI: "Apply" button per style row. Single undo step. |
| `delete_graphic_style` | Delete a named graphic style. Does not alter existing node appearances. GUI: "✕" button per style row. |
| `define_width_profile` | Define a named variable-width stroke profile from an array of width samples (≥2 values, all non-negative). Overwrites on name conflict. GUI: "Save Profile" text input + button in Width Profiles panel. |
| `list_width_profiles` | List all named width profiles in the document. |
| `apply_width_profile` | Apply a named width profile (as the average stroke width) to one or more nodes. GUI: "Apply" button per profile row. Single undo step. |
| `delete_width_profile` | Delete a named width profile. Does not alter existing node stroke widths. GUI: "✕" button per profile row. |
| `detect_rhythms` | Detect visual rhythm patterns across visible nodes: horizontal/vertical spacing intervals, uniform widths, geometric size progressions, and rotational symmetry. Returns structured JSON patterns with descriptions and extension suggestions. Read-only. GUI: "Detect Rhythms" button in Composition Analysis panel. |
| `define_action` | Define (or overwrite) a named action set — a replayable sequence of MCP tool calls. Accepts an ordered array of `{tool, args}` steps. Stored in the document model. GUI: Actions panel with Play/Delete per action row. |
| `list_actions` | List all named action sets in the document with step counts. |
| `play_action` | Replay a named action set, executing each step in order. Optional `substitutions` map replaces node IDs/names from the recording with current values. Stops at first error and reports which step failed. |
| `delete_action` | Delete a named action set. GUI: "✕" button per action row. |
| `measure_distances` | Measure edge-to-edge gaps (horizontal and vertical), center-to-center distance, and alignment offsets between two or more nodes. For ≤6 nodes reports all pairs; for larger sets reports consecutive pairs. Read-only. GUI: Distances panel (visible when 2+ nodes selected) with "Measure Selected" button. |
| `define_grammar_rule` | Define (or update) a named design grammar rule. Five rule types: `palette_includes` (a specific fill color must exist), `max_colors` (palette size cap), `min_text_size` (font size floor), `required_layer` (named layer must exist), `max_node_count` (total node limit). Stored in document model. GUI: Document Grammar panel form. |
| `list_grammar_rules` | List all named design grammar rules in the document. |
| `check_grammar` | Validate the document against its grammar rules. Returns per-rule pass/fail results with descriptive messages. Read-only. GUI: "Check Grammar" button in Document Grammar panel. |
| `delete_grammar_rule` | Delete a named design grammar rule. GUI: "✕" button per rule row. |
| `apply_flex_layout` | Redistribute children of a group in a flex-like row or column arrangement with configurable gap, cross-axis alignment, and padding. Children are sorted by current position along the main axis before redistribution. GUI: "Apply Flex (row/column, gap=8)" buttons in Group Operations section (visible when group with 2+ children selected). Single undo step. |
| `apply_stack_layout` | Stack all children of a group at the same position (Z-stack), aligning each child within the union bounding box. Configurable align_h (left/center/right) and align_v (top/center/bottom). GUI: "Stack (center)" button in Group Operations panel. |
| `set_document_bleed` | Set the print bleed and/or slug margins (in mm) for the document. Values persist in the .photon file. Supports partial updates — pass only the field you want to change. GUI: "Print Settings" collapsing panel in Document Info section with Bleed/Slug DragValues and Apply button. |
| `get_document_bleed` | Return the current document bleed and slug values in millimetres. Read-only. |
| `list_history` | Return the most recent edit history entries from the undo stack, newest first. Returns step index and description for each entry. Useful for AI agents auditing what was changed and for deciding which node to revert with `undo_node`. Read-only. GUI: "Edit History" collapsing panel with ⟳ refresh button. |
| `jump_to_history` | Jump the document state to any undo-stack index by issuing the required sequence of undo or redo operations. Index 0 is the oldest recorded state; current undo depth is the present state. GUI: "Jump to step" DragValue slider + Jump button in the Edit History panel. |
| `fit_to_margins` | Scale and center nodes to fill the artboard safe area (bounds minus set margins). Uniform mode preserves aspect ratio. GUI: "Fit to Margins" button in the Artboard Margins panel (enabled when margins are set). |
| `add_dimension` | Add a dimension annotation showing the distance between two nodes. Stores from/to node centers plus axis (x/y/diagonal) and label_offset. Rendered as orange tick-mark lines with distance labels in canvas overlay when guides are visible; stripped from exports. GUI: Add H/V/Diagonal buttons in "Dimension Annotations" panel (visible when 2 nodes selected). |
| `list_dimensions` | List all dimension annotations in the document (id, node refs, axis, distance, offset). Read-only. |
| `remove_dimension` | Remove a dimension annotation by ID. GUI: ✕ button per entry in the Dimension Annotations panel. |
| `rotate_copies` | Create N evenly-spaced rotational copies of a node around its bounding-box center (or a specified cx/cy). Optionally groups all copies into a single node. GUI: "Radial Copies" DragValue (2–64) + Apply button in the node Transform panel. |
| `copy_appearance` | Copy fill, stroke, and/or opacity from a source node to one or more target nodes (eyedropper-style). Each attribute toggled independently. GUI: "Copy Appearance" CollapsingHeader with Fill/Stroke/Opacity checkboxes + Apply Eyedropper button (visible when 2+ nodes selected). |
| `apply_grid_layout` | Arrange children of a group in a uniform grid: left-to-right, top-to-bottom with configurable column count, horizontal gap, vertical gap, and padding. Column width and row height are set to the maximum child dimensions. GUI: "Grid (3 cols)" and "Grid (4 cols)" buttons in Group Operations panel (visible when group with 2+ children selected). Single undo step. |
| `add_construction_line` | Add an infinite non-printing angled reference line through any document point. Parameters: x, y origin, angle_degrees (0=horizontal, 90=vertical), optional hex color. Renders as an overlay in the editor and is excluded from all exports. GUI: "Construction Lines" collapsing panel in Document Info section with X/Y/angle DragValues and quick H/V/45° buttons. |
| `set_artboard_margins` | Set the artboard safe-area margins (top, right, bottom, left) in document units. Values persist in the .photon file. Supports partial updates. GUI: "Artboard Margins" collapsing panel in Document Info section with T/R/B/L DragValues, Apply and Reset buttons. Blue margin rectangle rendered as canvas overlay when guides are visible. |
| `get_artboard_margins` | Return the current artboard safe-area margin values in document units. Read-only. |
| `link_text_frames` | Link two text nodes as a threaded text chain so content overflow flows from the upstream frame into the downstream frame. Stores next_frame/prev_frame references on TextNode. Supports undo. GUI: "Text Frame Threading" collapsing panel in text node properties with Link/Unlink buttons (active when two text nodes co-selected). |
| `unlink_text_frames` | Remove a text node from its thread chain, severing both upstream and downstream frame links. Supports undo. |
| `set_paragraph_options` | Set paragraph-level options on a text node: spacing_before, spacing_after (document units added before/after each paragraph), and indent (first-line indent, negative for hanging). All fields optional — partial updates supported. Supports undo. GUI: "¶ Before / After / Indent" DragValues inline in Text Operations panel. |
| `set_tab_stops` | Set explicit tab stop positions (in document units) on a text node. Stops are auto-sorted ascending and replace all existing stops. Stored as Vec<f64> on TextNode. Supports undo. GUI: "Tab Stops" collapsing panel with current stop list, Add-stop DragValue, and Clear All button. |
| `clear_tab_stops` | Remove all custom tab stops from a text node, restoring default tab spacing. Supports undo. |
| `set_text_decoration` | Set the text decoration on a text node: underline, line-through (strikethrough), overline, or none. Stored in text_decoration field on TextNode. Supports undo. GUI: U/S/O toggle buttons in Text Operations panel. |
| `set_opentype_features` | Set, add, or remove OpenType feature tags (liga, calt, frac, smcp, sups, subs, ordn, swsh, dlig, onum, tnum, zero) on a text node. Mode "set" replaces all features, "add" appends, "remove" removes. Stored as Vec<String> on TextNode. Supports undo. GUI: "OpenType Features" collapsing panel with checkboxes for 12 common tags. |
| `get_opentype_features` | Return the active OpenType feature tags on a text node. Read-only. |
| `register_event_trigger` | Map a document lifecycle event (on_open, on_save, on_node_create, on_selection_change) to a named action set. The action fires automatically when the event occurs. Validates that the event name is known and the action exists. |
| `list_event_triggers` | List all registered script event triggers in the document. Returns event name and action name for each entry. Read-only. |
| `remove_event_trigger` | Remove one or all event triggers for a given event. If action_name is provided only that specific mapping is removed; otherwise all triggers for the event are cleared. GUI: "Event Triggers" collapsing panel with ComboBox selectors for event and action and ✕ per-row remove buttons. |
| `undo_node` | Revert a specific node to its state N edits ago without touching any other nodes. Scans the undo stack for UpdateNode history entries targeting the node and applies the N-th pre-mutation snapshot as a new undoable command (so the revert itself is reversible). GUI: "↩ Revert Last Edit" and "↩↩ Revert 3 Edits" buttons in Properties panel. |

### Document & Layers
| Tool | Description |
|---|---|
| `create_layer` | Create a new layer at an optional position |
| `collect_in_new_layer` | Move a set of nodes into a newly created layer as a single undoable step; group children are resolved to their top-level ancestor; optional `name` and `position` for the new layer |
| `release_to_layers` | Move each node into its own newly created layer (inverse of `collect_in_new_layer`); group children resolved to top-level ancestors; optional `name_prefix`; single undo step |
| `merge_layers` | Merge two or more layers into one; all nodes from source layers are moved into the target layer (bottom-most in stack order); empty source layers are removed; optional `target_name`; single undo step |
| `flatten_artwork` | Merge all layers in the document into one; the bottom-most layer survives; all other layers are dissolved into it and removed; optional `target_name`; single undo step |
| `update_layer` | Rename a layer, toggle visibility/lock, set a color tag, or mark as a template layer (locked, dimmed reference). Only supplied fields are changed. Setting `is_template: true` also locks the layer. GUI: "T" button per layer in Layers panel. Single undo step. |
| `export_svg` | Export full document as SVG string with semantic `id` attributes on all nodes/layers (slugified names, deduplicated); versioned output (`<!-- photonic-svg-v1 -->`); `inner_only`, `semantic_ids`, and `precision` options |
| `export_selection_as_svg` | Export specific nodes (or current selection) as clean minimal SVG with tight viewBox, semantic `id` attributes, no artboard background; optional React component wrapper |
| `export_raster` | Export the current canvas as PNG, JPEG, WebP, GIF, or TIFF with optional width/height resize and JPEG/WebP quality. Returns base64-encoded image data. TIFF is lossless with full RGBA support. |
| `export_pdf` | Export the document as a single-page vector PDF. Args: `path` (also write to disk), `outline_text` (convert every glyph to a vector path — zero font dependencies), `marks` (render trim + registration marks in the bleed/slug band), `color_mode` (`rgb`/`cmyk`; defaults to the document's mode), `profile` (CMYK ICC path; defaults to bundled FOGRA39), `background`. A CMYK export is emitted as a **print-ready PDF/X-1a:2001** file: PDF 1.3, DeviceCMYK, embedded `GTS_PDFX` OutputIntent, MediaBox/TrimBox/BleedBox from the document bleed, physical page size at the document DPI. Returns base64. |
| `set_document_color_mode` / `get_document_color_mode` | Set or read the document colour model (`rgb`/`cmyk`). CMYK marks the document for print separation on export. GUI: Print Settings panel. |
| `export_design_tokens` | Extract the document's design vocabulary (solid fill colors, stroke colors, font families, font sizes, stroke widths) and return them as structured tokens in `json`, `css`, `tailwind`, or `style-dictionary` format |
| `get_css_preview` | Return the CSS equivalent of a node's visual properties for developer handoff: `background-color`/`background` (fill), `outline` (stroke), `opacity`, `mix-blend-mode`, `transform`; text nodes also include `color`, `font-family`, `font-size`, `font-weight`, `text-align`. Width/height from world bounding box. Fluid/mesh gradients approximate as solid. Read-only. |

### History & Checkpoints
| Tool | Description |
|---|---|
| `undo` | Undo with configurable step count |
| `redo` | Redo with configurable step count |
| `create_checkpoint` | Named snapshot of current document state |
| `list_checkpoints` | List all stored checkpoints |
| `restore_checkpoint` | Restore document to a named checkpoint |
| `diff_checkpoints` | Compare two checkpoints; returns structured JSON diff of added/removed/modified nodes and layers |

### Annotations
| Tool | Description |
|---|---|
| `add_annotation` | Attach a non-printing comment or design note to a node or the document; supports optional `author` identity for multi-agent workflows |
| `list_annotations` | List unresolved annotations (optionally filtered by `node_id`); pass `include_resolved: true` for full history |
| `resolve_annotation` | Mark an annotation as resolved; it is retained in the file but hidden from future listings |

### Audit Log
| Tool | Description |
|---|---|
| `list_audit_log` | Return the most recent N MCP tool calls (default 50) with timestamp, tool name, bounded argument summaries, result summary, and duration; newest first; response is byte-capped |
| `export_audit_log` | Export the retained in-memory audit log as a JSON array (oldest first); bounded argument summaries and a 256 KiB response cap apply |

### Clipboard History
| Tool | Description |
|---|---|
| `copy_nodes_to_clipboard` | Snapshot one or more nodes (with all descendants) into the session clipboard ring; ring holds up to 20 entries; optional user-defined label |
| `get_clipboard_history` | List all clipboard entries (index, id, label, node count, timestamp); use index with `paste_from_history` |
| `paste_from_history` | Paste nodes from any clipboard ring entry; all pasted nodes get fresh UUIDs; optional pixel offset; single undoable step |

### Canvas
| Tool | Description |
|---|---|
| `screenshot` | Capture canvas as PNG with optional scale downsampling |

---

## Shape Types

| Shape | Notes |
|---|---|
| Rectangle | Configurable width, height |
| Ellipse | Configurable rx, ry |
| Polygon | Configurable side count (≥ 3) |
| Star | Configurable point count (≥ 2), inner radius ratio |
| Line | Straight stroked path |

---

## Fill Types

| Fill | Notes |
|---|---|
| None | Transparent / no fill |
| Solid | Single color with opacity |
| Linear Gradient | Two-point gradient with color stops |
| Radial Gradient | Center + focal point + radius with color stops |
| Fluid Gradient | IDW interpolation from free-placed control points |
| Mesh Gradient | Bilinear grid (rows × cols) with per-vertex colors |

---

## Stroke Options

| Property | Options |
|---|---|
| Width | Any value in document units |
| Color & Opacity | Full RGBA |
| Line Cap | Butt, Round, Square |
| Line Join | Miter (with miter limit), Round, Bevel |
| Alignment | Center, Inside, Outside |
| Dash Array | Configurable dash/gap pattern |
| Miter Limit | Configurable |

---

## Blend Modes (16)

Normal, Multiply, Screen, Overlay, Darken, Lighten, Color Dodge, Color Burn, Hard Light, Soft Light, Difference, Exclusion, Hue, Saturation, Color, Luminosity

---

## Node Types

| Type | Description |
|---|---|
| Path | Vector shape with fill and stroke |
| Group | Container with children; optional `clip_children` flag |
| Text | Rasterized text with typography properties |

---

## Text Properties

| Property | Options |
|---|---|
| Font family | Any installed system font |
| Font size | Document units |
| Font weight | 100–900 |
| Alignment | Left, Center, Right |
| Fill | Full style support |
| Stroke | Full style support |

---

## Export Formats

| Format | Details |
|---|---|
| PNG | Transparent or artboard background; custom output dimensions |
| JPEG | Lossy export with configurable quality (1–100); alpha composited onto white |
| WebP | Lossless export with transparency support |
| GIF | GIF export via image crate encoder |
| ICO | Windows icon; selectable sizes (16, 32, 48, 256 px) |
| SVG | Full document vector export; `inner_only` option |

**Export options:** transparent background, crop to content bounding box, custom dimensions.

---

## GUI Tools (10)

| Tool | Function |
|---|---|
| Select | Select and move nodes |
| Direct Select | Edit individual anchor points on paths |
| Pan | Navigate the canvas |
| Rectangle | Draw rectangles |
| Ellipse | Draw ellipses |
| Polygon | Draw N-sided polygons |
| Star | Draw stars with configurable points and inner radius |
| Pen | Place anchor points to draw freeform paths |
| Shape Builder | Drag to union/subtract overlapping shapes interactively |
| Text | Place text nodes |

Shape tools (Rectangle, Ellipse, Polygon, Star) are grouped under a single toolbar button with a hover popover.

---

## GUI Panels

| Panel | Description |
|---|---|
| Toolbar | Document name, zoom level display |
| Tools Panel | Vertical tool selector with grouped shape sub-menu |
| Properties / Styles | Fill, stroke, opacity, blend mode editing for selection; path nodes show "Add Anchor Points", "Reverse Direction", "Outline Stroke" (when stroke is enabled), "Average Anchors", and "Convert to Grayscale" buttons; text nodes show a "Find / Replace…" button; all node types show a "Lock" / "Unlock" toggle button that prevents canvas selection |
| Layers Panel | Layer management, visibility toggle, lock toggle |
| History Panel | Undo/redo stack with named checkpoint snapshots; each checkpoint has a "Diff" button to highlight canvas changes (green=added, yellow=modified, red=removed); active diff shows a "✕ Clear Diff" button in the toolbar |
| Console Panel | Dual tabs: Lua REPL and Claude chat interface |
| Welcome Screen | New document form and recent files list |
| Export Dialog | Format selection (PNG/ICO/SVG) with background and crop options |
| Preferences | Dark/light theme toggle |
| Audit Log Panel | Floating window showing recent MCP tool calls; filterable by tool name; color-coded by success/error; toggled via the "Audit" toolbar button |

---

## Radial Wheel (Right-Click Context Menu)

Context-sensitive actions with scroll-wheel pagination (8 items per page):

| Context | Actions |
|---|---|
| Empty canvas | Create shape shortcuts |
| Single node selected | Duplicate, Delete, Bring to Front, Send to Back, Bring Forward, Send Backward, Copy as SVG, Invert, Grayscale; path nodes also show Add Anchors, Simplify, Outline Stroke, Reverse, and Average |
| Multiple nodes selected | Group, Delete All, Union, Subtract, Intersect, Exclude, Copy as SVG, Invert All, Grayscale All |

---

## Document Model

| Feature | Detail |
|---|---|
| Multi-layer | Ordered layer stack per document |
| Node IDs | Unique UUID per node |
| Semantic tags | Arbitrary string tags on any node for querying |
| Selection tracking | Active selection state maintained by document controller |
| Visibility | Per-node show/hide |
| Lock | Per-node lock state |

---

## History & Undo

| Feature | Detail |
|---|---|
| Undo / Redo stacks | Default 200-step capacity |
| Atomic command batching | Multiple operations grouped as a single undo step |
| Named checkpoints | Full document clone stored under a user-defined name |
| MCP auto-checkpoint | Automatic checkpoint created on a 60-second debounce during MCP sessions |

---

## Path Operations

| Feature | Detail |
|---|---|
| SVG path parsing | Import SVG `d` attribute strings to document paths |
| SVG path generation | Export paths back to SVG `d` attribute strings |
| Bounding box | World-space bounding box computation for any path |
| Boolean operations | Union, Subtract, Intersect, Exclude via `geo` crate |
| Bézier math | Curve handling via `kurbo` |

---

## Import / Export

| Format | Direction |
|---|---|
| `.photon` | Native JSON document format — read/write |
| `.svg` | Vector — import and export |
| `.png` | Raster — export |
| `.ico` | Windows icon — export |
| Recent files | Stored for quick access on the welcome screen |

---

## Rendering

| Feature | Detail |
|---|---|
| GPU rendering | wgpu WebGPU backend |
| Tessellation | lyon path tessellation to GPU triangles |
| Canvas pan/zoom | Smooth interactive navigation |
| Headless renderer | Off-screen PNG/ICO export without a visible window |
| Frame capture | MCP screenshot captures the next rendered frame as PNG bytes |

---

## MCP Server

| Feature | Detail |
|---|---|
| Protocol | JSON-RPC 2.0 over HTTP POST `/mcp` |
| Port | `localhost:7842` |
| Events | SSE stream at `/mcp/events` for real-time document change notifications |
| Architecture | Actor model — GUI and MCP send typed commands via `tokio::sync::mpsc`; no shared mutex |

---

## Scripting

| Feature | Detail |
|---|---|
| Lua REPL | Interactive Lua scripting via `mlua` in the Console panel |
| Claude chat | Direct Claude interaction panel in the Console |

---

*Last updated 2026-04-01. Copy Appearance (`copy_appearance`) MCP tool + GUI "Copy Appearance" CollapsingHeader — eyedropper-style attribute transfer from first selected node to all others; fill/stroke/opacity each independently toggleable; undo-safe via Command::Batch. Rotate Copies (`rotate_copies`) MCP tool + GUI "Radial Copies" DragValue + Apply button — creates N evenly-spaced rotational copies of a node around its bounding-box center; optional grouping; undo-safe via Command::Batch. Fit to Margins (`fit_to_margins`) MCP tool + GUI "Fit to Margins" button in Artboard Margins panel — scales and centers selected (or all) nodes to fill the artboard safe area; uniform aspect-ratio mode default; `padding` parameter for additional inset; single undo step via Command::Batch. Dimension Annotations (`add_dimension`, `list_dimensions`, `remove_dimension`) MCP tools + GUI "Dimension Annotations" panel — measurement lines with tick marks and distance labels rendered as canvas overlay (orange) between node pairs; supports x/y/diagonal axes; stripped from all exports. History Timeline Scrubber (`jump_to_history`) MCP tool + GUI "Jump to step" DragValue slider in Edit History panel — navigates the undo stack to any absolute index by issuing the required undo/redo operations. Stack Group (`apply_stack_layout`) MCP tool + GUI "Stack (center)" button — repositions all group children to the same center point (Z-stack) with configurable alignment. Symbol Libraries (`load_symbol_library`) MCP tool + GUI "Load Library…" panel — loads arrows/shapes/ui preset symbols as hidden off-canvas master nodes, skipping duplicates. Tab Stops (`set_tab_stops`, `clear_tab_stops`) MCP tools + GUI "Tab Stops" CollapsingHeader in text properties — custom tab stop positions stored as Vec<f64> on TextNode; GUI shows stop list with Add/Clear All. TIFF Export — `export_raster { format: "tiff" }` MCP tool extension + GUI Export dialog "TIFF" option — lossless RGBA TIFF output via the image crate; full width/height/background controls. Dynamic Symbols (`set_symbol_override`, `clear_symbol_overrides`) MCP tools + GUI "Symbol Override" panel — per-instance fill/stroke overrides on symbol instances. Symbol Sprayer (`spray_symbol_instances`) MCP tool + GUI Symbol Sprayer panel — scatters N symbol instances around a point using golden-angle distribution. Custom Workspaces (`save_workspace`, `load_workspace`, `list_workspaces`, `delete_workspace`) MCP tools + GUI Workspaces panel — named panel filter presets; load applies search_query to switch panel layout. Expand Blend (`expand_blend`) MCP tool + GUI "Expand Blend" button — converts blend group into individual discrete objects. Reverse Blend Spine (`reverse_blend_spine`) MCP tool + GUI "Reverse Spine" button in Blend Spine panel — reverses the spine path direction of a blend group. Blend Spine (`set_blend_spine`, `clear_blend_spine`) MCP tools + GUI Blend Spine CollapsingHeader in Group Operations panel — assigns a custom path node as the spine for a blend group; stores `blend_spine_id` on `GroupNode` with serde default. Paragraph Options (`set_paragraph_options`) MCP tool + GUI DragValues — paragraph_spacing_before/after and text_indent on TextNode. Text Decoration (`set_text_decoration`) MCP tool + GUI U/S/O toggle buttons — underline/strikethrough/overline stored as text_decoration String on TextNode. OpenType Features (`set_opentype_features`, `get_opentype_features`) MCP tools + GUI checkboxes — 12 standard OTF feature tags stored on TextNode; mode set/add/remove. Script Event Manager (`register_event_trigger`, `list_event_triggers`, `remove_event_trigger`) MCP tools + GUI Event Triggers panel — maps document lifecycle events to named action sets stored in event_triggers Vec on document model. Text Frame Threading (`link_text_frames`, `unlink_text_frames`) MCP tools + GUI Text Frame Threading panel — chain text nodes so overflow flows from upstream to downstream; stores next_frame/prev_frame on TextNode; bidirectional unlink. Artboard Margins (`set_artboard_margins`, `get_artboard_margins`) MCP tools + GUI panel — four-sided safe-area margins stored on document model; blue rectangle canvas overlay when guides visible. Construction Lines (`add_construction_line`) MCP tool + GUI panel — angled infinite reference lines through any point, rendered as canvas overlay and excluded from exports; extends Guide struct with angle_degrees/position_x/position_y. Document Bleed (`set_document_bleed`, `get_document_bleed`) MCP tools + GUI Print Settings panel — stores bleed/slug margins on document model. Edit History Inspector (`list_history`) MCP tool + GUI History panel — returns undo stack entries newest first. Grid Layout (`apply_grid_layout`) MCP tool + GUI buttons — arranges group children in uniform N-column grid. Per-Object Undo (`undo_node`) MCP tool + GUI "↩ Revert" buttons — revert a node N edits back without affecting other nodes; uses `revert_node_steps` on CommandHistory. Flex Layout (`apply_flex_layout`) MCP tool + GUI Group Operations panel — redistributes group children in row/column flex arrangement with gap, alignment, and padding. Print-Ready PDF/X-1a Export (`export_pdf` extended with `path`/`outline_text`/`marks`/`color_mode`/`profile`; `set_document_color_mode`/`get_document_color_mode`) — a CMYK export is emitted as a commercial-print PDF/X-1a:2001 file: native Rust vector PDF (pdf-writer), text outlined to paths (photonic-render glyph outliner, zero font deps), RGB→DeviceCMYK ICC conversion (moxcms + bundled Coated FOGRA39), embedded `GTS_PDFX` OutputIntent, PDF 1.3, MediaBox/TrimBox/BleedBox from `set_document_bleed`, trim + registration marks, bleed-aware background, and DPI-correct physical page size (`Document::new_print`/`from_preset`, business-card presets). Verified with `scripts/preflight-pdfx.sh` (deterministic X-1a invariant checker) and `scripts/svg-to-pdfx.sh` (interim SVG→PDF/X pipeline).

Last updated 2026-03-31. Actions (`define_action`, `list_actions`, `play_action`, `delete_action`) MCP tools + GUI Actions panel — define named MCP tool sequences, replay with node substitutions. Measure Distances (`measure_distances`) MCP tool + GUI Distances panel — reports edge-to-edge gaps and center-to-center distance between node pairs. Document Grammar (`define_grammar_rule`, `list_grammar_rules`, `check_grammar`, `delete_grammar_rule`) MCP tools + GUI Document Grammar panel — store named design rules (palette, color count, text size, layer, node count constraints) and validate the document against them. Rhythm & Repetition Detection (`detect_rhythms`) MCP tool + "Detect Rhythms" button in Composition Analysis panel — detects spacing, size, and rotation rhythms with structured JSON findings. Variable Width Profiles (`define_width_profile`, `list_width_profiles`, `apply_width_profile`, `delete_width_profile`) MCP tools + GUI Width Profiles panel — named stroke-width sample arrays; applied as averaged stroke width. Graphic Styles (`define_graphic_style`, `list_graphic_styles`, `apply_graphic_style`, `delete_graphic_style`) MCP tools + GUI Graphic Styles panel — save fill+stroke+opacity as named presets, apply to any node. Swatch Libraries (`load_swatch_library`) MCP tool + Color Swatches panel dropdown — loads 6 predefined palettes (web, material, pastels, earth_tones, neon, grayscale). Flatten Transparency (`flatten_transparency`) MCP tool + GUI panel — bakes node and fill opacity into color alpha values for print-ready output. Spot Colors (`define_spot_color`, `list_spot_colors`, `apply_spot_color`, `delete_spot_color`) MCP tools + GUI Spot Colors panel — named inks with overprint flag; applied as solid fills. Named Branches (`branch_create`, `branch_list`, `branch_switch`, `branch_delete`) MCP tools + GUI Branches panel — fork document state, restore or delete branches; session-scoped like checkpoints. Composition Advisor (`analyze_composition`) — read-only MCP tool checking quadrant balance, density, overlaps, color contrast, palette size, and off-canvas objects; returns structured JSON findings with severity levels. GUI: Composition Analysis panel with "Analyze Canvas" button. Gradient Swatches (`save_gradient_swatch`, `list_gradient_swatches`, `apply_gradient_swatch`, `delete_gradient_swatch`) MCP + GUI Gradient Swatches panel — save any gradient fill as a named swatch, apply to multiple nodes, delete. Navigator Panel (`get_canvas_overview`) MCP + GUI miniature thumbnail with selected node highlighted. Font Style & Weight (`set_font_style`, `set_font_weight`) MCP tools + B/I toggle buttons in GUI Text panel; also fixes font weight rendering (was stored but ignored by renderer). Symbols Panel (`define_symbol`, `list_symbols`, `place_symbol`, `break_link_to_symbol`, `delete_symbol`) MCP + GUI Symbols panel with Define/Place/Break Link/Delete. Variables Panel data-driven design (7 tools: define, list, set, delete, apply, bind, unbind). Area Type Tool (`set_text_area`/`clear_text_area`) MCP + GUI. Vertical Type (`set_text_direction`) MCP + GUI toggle. Type on a Path (`set_text_path`/`clear_text_path`) MCP + GUI spine panel. Clipping Mask (`make_clipping_mask`/`release_clipping_mask`) MCP + GUI Make/Release buttons for Group nodes. Global Color Swatches (`add_color_swatch`, `list_color_swatches`, `apply_color_swatch`, `update_color_swatch` with propagation, `delete_color_swatch`) MCP + GUI Color Swatches panel. Document Templates (`get_document_template`/`apply_document_template`) MCP + GUI clipboard button. Pencil Tool (`create_freehand_path`) MCP + GUI drag-to-draw. Isolation Mode (`enter_isolation_mode`/`exit_isolation_mode`) MCP + double-click GUI. Group Selection (`select_inside_group`) MCP + Alt+click GUI. Recently used colors tracking (`get_recent_colors`) MCP + GUI swatches. Lasso selection tool (`lasso_select`) MCP + GUI freehand drag. Ruler guides (`add_guide`, `remove_guide`, `list_guides`, `clear_guides`) MCP + GUI canvas overlay (Ctrl+; toggle). Rotation DragValue in properties panel. Copy/Paste/Paste-in-Place (Ctrl+C/V/Shift+V). Outline Mode (Ctrl+Y wireframe view). W/H editable DragValues in properties panel (world-space resize). Pathfinder Merge (`pathfinder_merge`) MCP + GUI — trim then merge same-color faces. Cursor coordinate overlay (live X/Y in canvas space). Arrow-key nudge with configurable distance (Shift×10). Divide Objects Below (`divide_objects_below`) MCP + GUI. Pathfinder Divide (`pathfinder_divide`) MCP + GUI. Editable X/Y position DragValues in properties panel. Object Hide/Show eye toggle in properties panel. Align to Artboard GUI panel (single-node canvas alignment). Recolor Artwork: `recolor_artwork` MCP + GUI panel. Distribute on Path. Snap to Pixel. Polar Grid, Rectangular Grid, Arc Tool, Dash corner alignment, Document Info, Template layers, Color Guide, Dashed stroke, Line Segment Tool, Shear transform.*
