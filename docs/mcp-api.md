# Photonic MCP API Reference

<!-- GENERATED FILE — do not edit by hand. -->
<!-- Regenerate with: cargo run -p photonic-mcp --bin dump_tools | python3 tools/gen-mcp-docs.py > docs/mcp-api.md -->

This document lists all **329** MCP tools exposed by `photonic-mcp`, generated directly from `server::tool_list()` so it cannot drift from the implementation.

## Tools

[`add_anchor_points`](#add-anchor-points), [`add_annotation`](#add-annotation), [`add_artboard`](#add-artboard), [`add_color_swatch`](#add-color-swatch), [`add_construction_line`](#add-construction-line), [`add_dimension`](#add-dimension), [`add_dimension_line`](#add-dimension-line), [`add_drop_shadow`](#add-drop-shadow), [`add_export_profile`](#add-export-profile), [`add_guide`](#add-guide), [`adjust_colors`](#adjust-colors), [`align_nodes`](#align-nodes), [`analyze_composition`](#analyze-composition), [`apply_adjustment`](#apply-adjustment), [`apply_character_style`](#apply-character-style), [`apply_color_swatch`](#apply-color-swatch), [`apply_document_template`](#apply-document-template), [`apply_filter`](#apply-filter), [`apply_flex_layout`](#apply-flex-layout), [`apply_gradient_swatch`](#apply-gradient-swatch), [`apply_graphic_style`](#apply-graphic-style), [`apply_grid_layout`](#apply-grid-layout), [`apply_paragraph_style`](#apply-paragraph-style), [`apply_pattern_fill`](#apply-pattern-fill), [`apply_spot_color`](#apply-spot-color), [`apply_stack_layout`](#apply-stack-layout), [`apply_transform`](#apply-transform), [`apply_variables`](#apply-variables), [`apply_width_profile`](#apply-width-profile), [`auto_name_nodes`](#auto-name-nodes), [`average_anchor_points`](#average-anchor-points), [`bind_text_variable`](#bind-text-variable), [`blend_colors`](#blend-colors), [`blend_objects`](#blend-objects), [`boolean_operation`](#boolean-operation), [`branch_create`](#branch-create), [`branch_delete`](#branch-delete), [`branch_list`](#branch-list), [`branch_switch`](#branch-switch), [`break_link_to_symbol`](#break-link-to-symbol), [`brush_stroke`](#brush-stroke), [`bucket_fill`](#bucket-fill), [`build_shape_from_points`](#build-shape-from-points), [`center_on_canvas`](#center-on-canvas), [`check_grammar`](#check-grammar), [`check_style_continuity`](#check-style-continuity), [`clean_up`](#clean-up), [`clear_blend_spine`](#clear-blend-spine), [`clear_guides`](#clear-guides), [`clear_layer_mask`](#clear-layer-mask), [`clear_symbol_overrides`](#clear-symbol-overrides), [`clear_tab_stops`](#clear-tab-stops), [`clear_text_area`](#clear-text-area), [`clear_text_path`](#clear-text-path), [`collect_in_new_layer`](#collect-in-new-layer), [`color_guide`](#color-guide), [`convert_anchor_points`](#convert-anchor-points), [`convert_to_grayscale`](#convert-to-grayscale), [`copy_appearance`](#copy-appearance), [`copy_nodes_to_clipboard`](#copy-nodes-to-clipboard), [`create_adjustment_layer`](#create-adjustment-layer), [`create_array`](#create-array), [`create_arrow_shape`](#create-arrow-shape), [`create_bar_chart`](#create-bar-chart), [`create_character_style`](#create-character-style), [`create_cross`](#create-cross), [`create_curvature_path`](#create-curvature-path), [`create_donut`](#create-donut), [`create_flare`](#create-flare), [`create_freehand_path`](#create-freehand-path), [`create_gear`](#create-gear), [`create_grid`](#create-grid), [`create_heart`](#create-heart), [`create_layer`](#create-layer), [`create_line_chart`](#create-line-chart), [`create_paragraph_style`](#create-paragraph-style), [`create_parametric_shape`](#create-parametric-shape), [`create_path`](#create-path), [`create_pie_chart`](#create-pie-chart), [`create_polar_grid`](#create-polar-grid), [`create_qr_code`](#create-qr-code), [`create_radar_chart`](#create-radar-chart), [`create_raster_layer`](#create-raster-layer), [`create_scatter_plot`](#create-scatter-plot), [`create_shape`](#create-shape), [`create_speech_bubble`](#create-speech-bubble), [`create_spiral`](#create-spiral), [`create_stacked_bar_chart`](#create-stacked-bar-chart), [`create_sunburst`](#create-sunburst), [`create_text`](#create-text), [`create_truchet_tiling`](#create-truchet-tiling), [`create_vectors_from_css`](#create-vectors-from-css), [`create_vectors_from_react`](#create-vectors-from-react), [`create_wave_pattern`](#create-wave-pattern), [`crystallize_path`](#crystallize-path), [`define_action`](#define-action), [`define_grammar_rule`](#define-grammar-rule), [`define_graphic_style`](#define-graphic-style), [`define_pattern`](#define-pattern), [`define_spot_color`](#define-spot-color), [`define_symbol`](#define-symbol), [`define_variable`](#define-variable), [`define_width_profile`](#define-width-profile), [`delete_action`](#delete-action), [`delete_anchor_point`](#delete-anchor-point), [`delete_character_style`](#delete-character-style), [`delete_color_swatch`](#delete-color-swatch), [`delete_gradient_swatch`](#delete-gradient-swatch), [`delete_grammar_rule`](#delete-grammar-rule), [`delete_graphic_style`](#delete-graphic-style), [`delete_layer`](#delete-layer), [`delete_nodes`](#delete-nodes), [`delete_paragraph_style`](#delete-paragraph-style), [`delete_pattern`](#delete-pattern), [`delete_spot_color`](#delete-spot-color), [`delete_symbol`](#delete-symbol), [`delete_variable`](#delete-variable), [`delete_width_profile`](#delete-width-profile), [`delete_workspace`](#delete-workspace), [`deselect_all`](#deselect-all), [`detect_rhythms`](#detect-rhythms), [`diff_checkpoints`](#diff-checkpoints), [`distribute_no_overlap`](#distribute-no-overlap), [`distribute_on_path`](#distribute-on-path), [`divide_objects_below`](#divide-objects-below), [`duplicate_artboard`](#duplicate-artboard), [`duplicate_layer`](#duplicate-layer), [`duplicate_nodes`](#duplicate-nodes), [`enter_isolation_mode`](#enter-isolation-mode), [`exit_isolation_mode`](#exit-isolation-mode), [`expand_blend`](#expand-blend), [`export_artboards`](#export-artboards), [`export_audit_log`](#export-audit-log), [`export_design_tokens`](#export-design-tokens), [`export_icon_set`](#export-icon-set), [`export_pdf`](#export-pdf), [`export_raster`](#export-raster), [`export_selection_as_svg`](#export-selection-as-svg), [`export_svg`](#export-svg), [`export_tagged_assets`](#export-tagged-assets), [`find_nodes`](#find-nodes), [`find_replace_style`](#find-replace-style), [`find_replace_text`](#find-replace-text), [`fit_to_canvas`](#fit-to-canvas), [`fit_to_margins`](#fit-to-margins), [`flatten_artwork`](#flatten-artwork), [`flatten_group`](#flatten-group), [`flatten_transparency`](#flatten-transparency), [`flip_nodes`](#flip-nodes), [`get_artboard_margins`](#get-artboard-margins), [`get_canvas_overview`](#get-canvas-overview), [`get_clipboard_history`](#get-clipboard-history), [`get_css_preview`](#get-css-preview), [`get_document_bleed`](#get-document-bleed), [`get_document_color_mode`](#get-document-color-mode), [`get_document_dpi`](#get-document-dpi), [`get_document_info`](#get-document-info), [`get_document_state`](#get-document-state), [`get_document_template`](#get-document-template), [`get_node`](#get-node), [`get_node_prompts`](#get-node-prompts), [`get_opentype_features`](#get-opentype-features), [`get_raster_info`](#get-raster-info), [`get_recent_colors`](#get-recent-colors), [`get_selection`](#get-selection), [`gradient_fill`](#gradient-fill), [`group_nodes`](#group-nodes), [`hatch_fill`](#hatch-fill), [`import_design_tokens`](#import-design-tokens), [`inspect_node`](#inspect-node), [`invert_colors`](#invert-colors), [`join_paths`](#join-paths), [`jump_to_history`](#jump-to-history), [`lasso_select`](#lasso-select), [`layout_nodes`](#layout-nodes), [`link_text_frames`](#link-text-frames), [`liquify`](#liquify), [`list_actions`](#list-actions), [`list_annotations`](#list-annotations), [`list_artboards`](#list-artboards), [`list_audit_log`](#list-audit-log), [`list_character_styles`](#list-character-styles), [`list_checkpoints`](#list-checkpoints), [`list_color_swatches`](#list-color-swatches), [`list_constraints`](#list-constraints), [`list_dimensions`](#list-dimensions), [`list_event_triggers`](#list-event-triggers), [`list_export_profiles`](#list-export-profiles), [`list_gradient_swatches`](#list-gradient-swatches), [`list_grammar_rules`](#list-grammar-rules), [`list_graphic_styles`](#list-graphic-styles), [`list_guides`](#list-guides), [`list_history`](#list-history), [`list_paragraph_styles`](#list-paragraph-styles), [`list_patterns`](#list-patterns), [`list_spot_colors`](#list-spot-colors), [`list_symbols`](#list-symbols), [`list_variables`](#list-variables), [`list_width_profiles`](#list-width-profiles), [`list_workspaces`](#list-workspaces), [`load_swatch_library`](#load-swatch-library), [`load_symbol_library`](#load-symbol-library), [`load_workspace`](#load-workspace), [`magic_wand_select`](#magic-wand-select), [`make_clipping_mask`](#make-clipping-mask), [`make_compound_path`](#make-compound-path), [`make_live_boolean`](#make-live-boolean), [`measure_distance`](#measure-distance), [`measure_distances`](#measure-distances), [`measure_nodes`](#measure-nodes), [`measure_path`](#measure-path), [`merge_layers`](#merge-layers), [`mirror_copy`](#mirror-copy), [`move_artboard`](#move-artboard), [`move_to_layer`](#move-to-layer), [`noise_deform`](#noise-deform), [`offset_path`](#offset-path), [`outline_stroke`](#outline-stroke), [`paste_from_history`](#paste-from-history), [`pathfinder_crop`](#pathfinder-crop), [`pathfinder_divide`](#pathfinder-divide), [`pathfinder_merge`](#pathfinder-merge), [`pathfinder_minus_back`](#pathfinder-minus-back), [`pathfinder_minus_front`](#pathfinder-minus-front), [`pathfinder_outline`](#pathfinder-outline), [`pathfinder_trim`](#pathfinder-trim), [`pin_object_guides`](#pin-object-guides), [`place_image`](#place-image), [`place_symbol`](#place-symbol), [`play_action`](#play-action), [`point_on_path`](#point-on-path), [`preview_selection`](#preview-selection), [`proportional_move_anchor`](#proportional-move-anchor), [`pucker_bloat`](#pucker-bloat), [`randomize_colors`](#randomize-colors), [`recolor_artwork`](#recolor-artwork), [`redo`](#redo), [`register_event_trigger`](#register-event-trigger), [`release_clipping_mask`](#release-clipping-mask), [`release_compound_path`](#release-compound-path), [`release_to_layers`](#release-to-layers), [`remove_artboard`](#remove-artboard), [`remove_background`](#remove-background), [`remove_constraint`](#remove-constraint), [`remove_dimension`](#remove-dimension), [`remove_event_trigger`](#remove-event-trigger), [`remove_export_profile`](#remove-export-profile), [`remove_fill`](#remove-fill), [`remove_guide`](#remove-guide), [`remove_stroke`](#remove-stroke), [`reorder_layers`](#reorder-layers), [`reorder_node`](#reorder-node), [`resize_canvas`](#resize-canvas), [`resolve_annotation`](#resolve-annotation), [`restore_checkpoint`](#restore-checkpoint), [`retouch`](#retouch), [`reverse_blend_spine`](#reverse-blend-spine), [`reverse_node_order`](#reverse-node-order), [`reverse_path_direction`](#reverse-path-direction), [`rotate_copies`](#rotate-copies), [`roughen_path`](#roughen-path), [`round_corners`](#round-corners), [`run_export_profile`](#run-export-profile), [`sample_color_at`](#sample-color-at), [`save_document`](#save-document), [`save_gradient_swatch`](#save-gradient-swatch), [`save_workspace`](#save-workspace), [`scallop_path`](#scallop-path), [`scatter_copies`](#scatter-copies), [`scissors_cut`](#scissors-cut), [`screenshot`](#screenshot), [`select_all`](#select-all), [`select_by_kind`](#select-by-kind), [`select_inside_group`](#select-inside-group), [`select_same`](#select-same), [`select_similar`](#select-similar), [`set_active_artboard`](#set-active-artboard), [`set_active_layer`](#set-active-layer), [`set_artboard_margins`](#set-artboard-margins), [`set_blend_mode`](#set-blend-mode), [`set_blend_spine`](#set-blend-spine), [`set_character_metrics`](#set-character-metrics), [`set_constraint`](#set-constraint), [`set_document_bleed`](#set-document-bleed), [`set_document_color_mode`](#set-document-color-mode), [`set_document_dpi`](#set-document-dpi), [`set_font_style`](#set-font-style), [`set_font_weight`](#set-font-weight), [`set_layer_mask`](#set-layer-mask), [`set_locked`](#set-locked), [`set_node_prompt`](#set-node-prompt), [`set_node_size`](#set-node-size), [`set_opacity`](#set-opacity), [`set_opentype_features`](#set-opentype-features), [`set_paint`](#set-paint), [`set_paragraph_options`](#set-paragraph-options), [`set_selection`](#set-selection), [`set_symbol_override`](#set-symbol-override), [`set_tab_stops`](#set-tab-stops), [`set_text_area`](#set-text-area), [`set_text_decoration`](#set-text-decoration), [`set_text_direction`](#set-text-direction), [`set_text_path`](#set-text-path), [`set_variable_value`](#set-variable-value), [`set_visibility`](#set-visibility), [`simplify_path`](#simplify-path), [`smooth_path`](#smooth-path), [`snap_to_pixel`](#snap-to-pixel), [`split_into_grid`](#split-into-grid), [`spray_symbol_instances`](#spray-symbol-instances), [`stipple_fill`](#stipple-fill), [`style_transfer`](#style-transfer), [`swap_fill_stroke`](#swap-fill-stroke), [`tag_node_for_export`](#tag-node-for-export), [`tag_nodes`](#tag-nodes), [`transform_copies`](#transform-copies), [`transform_image`](#transform-image), [`twirl_path`](#twirl-path), [`unbind_text_variable`](#unbind-text-variable), [`undo`](#undo), [`undo_node`](#undo-node), [`ungroup_nodes`](#ungroup-nodes), [`unlink_text_frames`](#unlink-text-frames), [`update_artboard`](#update-artboard), [`update_color_swatch`](#update-color-swatch), [`update_layer`](#update-layer), [`update_node`](#update-node), [`warp_envelope`](#warp-envelope), [`zig_zag_path`](#zig-zag-path)

---

## `add_anchor_points`

Insert a new anchor point at the midpoint of every segment in the selected path node(s). Each pass doubles the anchor count. Non-path nodes are silently skipped.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | IDs of path nodes to subdivide |
| `passes` | integer | no | Number of subdivision passes (default 1, max 8) |

## `add_annotation`

Attach a non-printing text comment to a node or to the document as a whole.

Annotations are stored in the `.photon` file but are completely invisible in all export formats (SVG, PNG, ICO). They are not part of the undo/redo history.

Use cases:
- AI agents recording *why* a design decision was made: "Chose this radius because the brief said 'approachable'."
- Human reviewers leaving redline feedback: "This stroke weight should match the header."
- Cross-session notes that survive save/reload.

Returns the new `annotation_id` UUID.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `text` | string | yes | The comment or design note (required, non-empty). |
| `author` | string | no | Optional author identity, e.g. "claude" or "design-reviewer". |
| `node_id` | string | no | UUID of the node to annotate. Omit to create a document-level annotation. |

## `add_artboard`

Create a new artboard (a named crop/export rectangle) at the given top-left (x, y) with width×height in document units, and make it the active artboard. Returns the new artboard_id. Use for multi-artboard layouts (e.g. responsive frames, icon sizes, print pages side by side).

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `height` | number | yes | Height in document units (> 0). |
| `width` | number | yes | Width in document units (> 0). |
| `x` | number | yes | Top-left X in document units. |
| `y` | number | yes | Top-left Y in document units. |
| `name` | string | no | Optional name. Default: 'Artboard N'. |

## `add_color_swatch`

Add a named color swatch to the document palette. Swatches can be applied to any node's fill with apply_color_swatch.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `color_hex` | string | yes | CSS hex color e.g. #FF5733. |
| `name` | string | yes | Unique swatch name. |

## `add_construction_line`

Add an angled construction line — an infinite non-printing reference line through a specified point at any angle. Unlike ruler guides (horizontal/vertical only), construction lines can be at any angle. Stored in the document's guide list and stripped from all exports. Visible when guides are shown (Ctrl+;).

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `angle_degrees` | number | yes | Angle in degrees. 0° = horizontal, 90° = vertical, 45° = diagonal. |
| `x` | number | yes | X coordinate (document units) of the line's origin point. |
| `y` | number | yes | Y coordinate (document units) of the line's origin point. |
| `color` | string | no | Optional hex color (e.g. '#FF8800'). Default: orange. |

## `add_dimension`

Add a dimension annotation showing the distance between two nodes. The annotation is rendered as an arrow line with a distance label in the canvas overlay (visible when guides are shown). Strips from all exports. Use list_dimensions to see all annotations and remove_dimension to delete one.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `from_node_id` | string | yes | UUID or name of the first node. |
| `to_node_id` | string | yes | UUID or name of the second node. |
| `axis` | enum (`x`, `y`, `diagonal`) | no | Measurement axis. 'x' = horizontal only, 'y' = vertical only, 'diagonal' = Euclidean. Default: 'diagonal'. |
| `label_offset` | number | no | Perpendicular visual offset of the line from the node centers in document units. Default: 20. |

## `add_dimension_line`

Add a technical dimension annotation between two points. Creates a grouped set of elements: extension lines, dimension line with arrowheads, and a distance text label.

Useful for technical illustrations, architectural drawings, and precision documentation.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `x1` | number | yes | Start X |
| `x2` | number | yes | End X |
| `y1` | number | yes | Start Y |
| `y2` | number | yes | End Y |
| `color` | string | no | Color hex (default: #666666) |
| `font_size` | number | no | Label font size (default: 12) |
| `layer_id` | string | no |  |
| `offset` | number | no | Distance of dimension line from measured points (default: 20) |

## `add_drop_shadow`

Add a drop shadow behind one or more nodes. Creates a duplicate of each node, offset and recolored to the shadow color, placed behind the original.

The shadow copy has its fill replaced with the shadow color and stroke removed. For groups, child colors are preserved as a solid-color silhouette. Works with paths, text, and groups.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Node IDs to add shadows to |
| `color` | string | no | Shadow color hex (default: #000000) |
| `offset_x` | number | no | Shadow X offset (default: 5) |
| `offset_y` | number | no | Shadow Y offset (default: 5) |
| `opacity` | number | no | Shadow opacity 0–1 (default: 0.4) |

## `add_export_profile`

Save a named export configuration to the document. Profiles store format and quality settings so you can re-export with consistent settings using run_export_profile. If a profile with the same name exists it is replaced.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `format` | enum (`svg`, `png`, `jpeg`, `webp`) | yes | Target export format. |
| `name` | string | yes | Unique profile name. |
| `height` | integer | no | Raster-only: output pixel height. |
| `precision` | integer | no | SVG-only: coordinate decimal precision (default: 4). |
| `semantic_ids` | boolean | no | SVG-only: emit semantic id attributes (default: true). |
| `width` | integer | no | Raster-only: output pixel width. |

## `add_guide`

Add a ruler guide (horizontal or vertical reference line) at a precise document-unit position. Guides are visible in the editor and stripped from all export formats.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `orientation` | enum (`horizontal`, `vertical`) | yes | Guide orientation. 'horizontal' creates a fixed-Y line; 'vertical' creates a fixed-X line. |
| `position` | number | yes | Position in document units. Y coordinate for horizontal guides; X coordinate for vertical guides. |
| `color` | array<number> | no | Optional RGBA color override as [R, G, B, A] in [0, 1] range. Omit to use the default cyan. |

## `adjust_colors`

Shift RGB(A) channel values across selected path nodes. Each delta is added to the corresponding channel and clamped to [0, 1]. Works on solid fills, gradient stops, fluid/mesh gradient points, and stroke colors. If node_ids is omitted, all path nodes in the document are adjusted. Single undo step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `delta_a` | number | no | Alpha channel delta (−1.0 to 1.0). Default 0. |
| `delta_b` | number | no | Blue channel delta (−1.0 to 1.0). Default 0. |
| `delta_g` | number | no | Green channel delta (−1.0 to 1.0). Default 0. |
| `delta_r` | number | no | Red channel delta (−1.0 to 1.0). Default 0. |
| `node_ids` | array<string> | no | UUIDs of path nodes to adjust. Omit to adjust all path nodes in the document. |

## `align_nodes`

Align or distribute multiple nodes by their bounding boxes. Alignment snaps each node's edge or center to a reference (selection bounds, canvas, or a key object). Distribution evenly spaces nodes along an axis — by default the two extreme nodes stay fixed; supply `spacing` to use an exact pixel gap instead. Groups are not supported (no bounds).

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | IDs of the nodes to align or distribute |
| `operation` | enum (`left`, `center_horizontal`, `right`, `top`, `center_vertical`, `bottom`, `distribute_horizontal`, `distribute_vertical`) | yes | left/center_horizontal/right — snap to horizontal reference edge or center. top/center_vertical/bottom — snap to vertical reference edge or center. distribute_horizontal/distribute_vertical — evenly space gaps between nodes along the axis (or use exact `spacing`). |
| `anchor` | enum (`selection`, `canvas`, `key_object`) | no | Reference for alignment. selection (default) = combined bounding box of all specified nodes. canvas = document dimensions. key_object = use the bounding box of the node given in key_object_id as the fixed reference; the key object itself is not moved. |
| `key_object_id` | string | no | When anchor is key_object, the ID of the node to use as the fixed alignment reference. Must be one of the node_ids. |
| `spacing` | number | no | Exact pixel gap between adjacent node edges when using distribute_horizontal or distribute_vertical. The first node (leftmost / topmost) stays fixed; subsequent nodes are placed at prev_edge + spacing. Omit for equal-spacing mode (default). |

## `analyze_composition`

Analyze the visual composition of the current document and return advisory findings. Checks balance (quadrant distribution), density, object overlaps, color contrast, palette size, and off-canvas objects. Read-only — does not modify the document.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | no | Optional list of node UUIDs or names to restrict the analysis to. Defaults to all visible nodes. |

## `apply_adjustment`

Apply a non-spatial color adjustment to a raster node (Photoshop Image > Adjustments). `adjustment` selects the operation; `params` carries its arguments; optional `selection` confines the edit to a region.

adjustment ∈ {brightness_contrast(brightness,contrast -1..1), levels(in_black,in_white,gamma,out_black,out_white), curves(points:[[x,y],...] 0..1), exposure(stops), hue_saturation(hue deg,saturation -1..1,lightness -1..1), color_balance(shadows/midtones/highlights:[r,g,b] -1..1), vibrance(amount -1..1), desaturate, black_and_white(weights:[r,g,b]), invert, posterize(levels), threshold(level 0..1), photo_filter(color:[r,g,b] 0..1,density 0..1,preserve_luminosity), channel_mixer(red/green/blue:[r,g,b]), gradient_map(stops:[{pos,color}]), selective_color(target:[r,g,b],adjust:[r,g,b],range), shadows_highlights(shadows 0..1,highlights 0..1), gamma(gamma), auto_contrast, auto_levels}.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `adjustment` | string | yes | Adjustment name (see description) |
| `node_id` | string | yes | Raster node id or name |
| `params` | object | no | Adjustment-specific parameters |
| `selection` | object | no | Optional selection mask (see make-selection schema in apply_filter) |

## `apply_character_style`

Apply a named character style to one or more text nodes. Only attributes defined in the style are changed; unset attributes are left as-is.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `style_name` | string | yes | Name of the style to apply. |
| `node_ids` | array<string> | no | Text node UUIDs or names. Uses current selection if empty. |

## `apply_color_swatch`

Apply a named color swatch to the fill of one or more nodes.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Node IDs (UUID or name) to apply the swatch to. |
| `swatch_name` | string | yes | Name of the swatch to apply. |

## `apply_document_template`

Apply a previously captured document template to the current document. Canvas size, guides, and export profiles from the template are merged in non-destructively. New layers from the template are added only if no layer with the same name already exists. Existing nodes are never removed.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `template_json` | string | yes | Template JSON string as returned by get_document_template. |

## `apply_filter`

Apply a spatial filter to a raster node (Photoshop Filter menu). `filter` selects the operation; `params` carries its arguments; optional `selection` confines the edit.

filter ∈ {gaussian_blur(radius), box_blur(radius), motion_blur(angle deg,distance px), sharpen(amount), unsharp_mask(radius,amount,threshold 0..255), median(radius), add_noise(amount 0..1,monochrome bool,seed), emboss, find_edges, mosaic(block px), high_pass(radius), surface_blur(radius,threshold), lens_blur(radius), smart_sharpen(amount,radius,threshold), reduce_noise(strength), clarity(amount), vignette(amount,feather), chromatic_aberration(amount)}.

selection (shared by adjustment/filter/brush): {kind:'rect'|'ellipse'|'polygon'|'wand'|'color_range'|'whole', x,y,w,h | points:[[x,y]] | tolerance,color | feather,invert,grow,contract}.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `filter` | string | yes | Filter name (see description) |
| `node_id` | string | yes |  |
| `params` | object | no |  |
| `selection` | object | no |  |

## `apply_flex_layout`

Redistribute the direct children of a Group node in a flex-like arrangement. Children are sorted by their current position along the main axis, then repositioned sequentially with a fixed gap between them. Cross-axis alignment ('start', 'center', 'end') aligns shorter children relative to the tallest/widest. Optional padding offsets the origin. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `group_id` | string | yes | Group node ID (UUID or name) whose children will be laid out. |
| `align` | enum (`start`, `center`, `end`) | no | Cross-axis alignment. Default: 'center'. |
| `direction` | enum (`row`, `column`) | no | Layout direction. 'row' arranges children left-to-right, 'column' top-to-bottom. Default: 'row'. |
| `gap` | number | no | Gap in document units between consecutive children. Default: 8.0. |
| `padding` | number | no | Offset from origin before placing the first child. Default: 0.0. |

## `apply_gradient_swatch`

Apply a named gradient swatch to one or more path nodes, replacing their current fill. Undo-safe (one step per node).

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the gradient swatch to apply. |
| `node_ids` | array<string> | yes | Path node IDs (UUIDs or names) to apply the swatch to. |

## `apply_graphic_style`

Apply a named graphic style (fill, stroke, opacity) to one or more nodes. Undo-safe batch command.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the graphic style to apply. |
| `node_ids` | array<string> | yes | Node UUIDs or names to apply the style to. |

## `apply_grid_layout`

Arrange the direct children of a Group node in a CSS-grid-style layout: left-to-right, top-to-bottom, with uniform column width (max child width) and row height (max child height). `columns` controls how many children appear per row before wrapping. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `group_id` | string | yes | Group node ID (UUID or name) whose children will be laid out. |
| `columns` | integer | no | Number of columns per row. Default: 3. |
| `gap_x` | number | no | Horizontal gap between columns in document units. Default: 8.0. |
| `gap_y` | number | no | Vertical gap between rows in document units. Default: 8.0. |
| `padding` | number | no | Offset from origin before placing the first cell. Default: 0.0. |

## `apply_paragraph_style`

Apply a named paragraph style to one or more text nodes. Only defined attributes are changed.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `style_name` | string | yes |  |
| `node_ids` | array<string> | no | Text node UUIDs or names. Uses selection if empty. |

## `apply_pattern_fill`

Apply a registry pattern as the fill of one or more path nodes (undo-safe batch). A clone of the pattern's tile is embedded on each node, with optional per-application transform overrides. The pattern transform is independent of the node transform — moving the shape scrolls the artwork under the pattern.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Node UUIDs or names to fill with the pattern. |
| `pattern` | string | yes | Name or UUID of the pattern to apply. |
| `offset` | array<number> | no | Override the pattern [x, y] offset for this application. |
| `rotation_degrees` | number | no | Override the pattern rotation (degrees) for this application. |
| `scale` | number | no | Override the pattern scale for this application. |
| `spacing` | number | no | Override the inter-tile spacing for this application. |
| `tile_type` | enum (`grid`, `brick_by_row`, `brick_by_column`, `hex`) | no | Override the tile layout for this application. |

## `apply_spot_color`

Apply a named spot color as a solid fill to one or more nodes. The node's fill becomes the spot color's hex value.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the spot color to apply. |
| `node_ids` | array<string> | yes | Node UUIDs or names to apply the spot color to. |

## `apply_stack_layout`

Stack all children of a Group node at the same position, creating a Z-stack (like CSS `position: absolute` on all children). Each child is repositioned to align its anchor point with the group's union bounding box. Useful for layered compositions, badge overlays, or icon-over-background patterns. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `group_id` | string | yes | Group node ID (UUID or name) whose children will be stacked. |
| `align_h` | enum (`left`, `center`, `right`) | no | Horizontal alignment anchor. Default: center. |
| `align_v` | enum (`top`, `center`, `bottom`) | no | Vertical alignment anchor. Default: center. |

## `apply_transform`

Apply a geometric transform to nodes

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `operation` | enum (`translate`, `rotate`, `scale`, `matrix`, `reflect_horizontal`, `reflect_vertical`, `shear`) | yes |  |
| `matrix` | array<number> | no |  |
| `node_ids` | array<string> | no |  |
| `rotate` | object | no |  |
| `scale` | object | no |  |
| `shear` | object | no |  |
| `translate` | object | no |  |

## `apply_variables`

Apply all document variables — replaces the text content of every bound text node with its variable's current value. This is the main dispatch step for data-driven design. Supports undo (single batch command).

_No parameters._

## `apply_width_profile`

Apply a named width profile to path nodes — sets stroke.width to the profile average. Undo-safe batch command.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the width profile to apply. |
| `node_ids` | array<string> | yes | Node UUIDs or names to apply the profile to. |

## `auto_name_nodes`

Rename nodes with descriptive, human-readable names derived from their content and geometry.

- Text nodes → first 24 chars of content: "text: hello world"
- Group nodes → child count: "group (3 items)"
- Path nodes → fill colour + bounding-box shape: "blue medium square", "red large wide bar", "gradient shape"

By default only renames nodes with generic auto-generated names (e.g. 'rectangle', 'path', 'group'). Pass overwrite:true to rename all targeted nodes. Use dry_run:true to preview proposed names without applying them.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `dry_run` | boolean | no | If true, return proposed renames without applying them. Default: false. |
| `overwrite` | boolean | no | If true, rename nodes even if they already have non-generic names. Default: false. |
| `scope` | enum (`selection`, `document`) | no | Which nodes to rename: 'selection' (active selection only) or 'document' (all nodes). Default: document. |

## `average_anchor_points`

Reposition all on-curve anchor points in each selected path node to their average position on the chosen axis. 'horizontal' equalises X-coordinates, 'vertical' equalises Y-coordinates, 'both' (default) moves all anchors to the centroid. Bézier control handles shift with their owning anchor so local curve shape is preserved. Non-path nodes are silently skipped.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | IDs of path nodes to average |
| `axis` | enum (`horizontal`, `vertical`, `both`) | no | Which axis to average (default: both) |

## `bind_text_variable`

Bind a text node to a document variable. When apply_variables is called, this node's content will be replaced by the variable's current value. The variable must exist (use define_variable first). Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | Text node ID (UUID or name). |
| `variable_name` | string | yes | Variable name to bind to. |

## `blend_colors`

Distribute fill colors linearly across 2 or more path nodes. The first and last nodes keep their existing solid fill colors; all intermediate nodes receive interpolated colors at evenly spaced positions between them. Optionally sort the nodes along an axis before blending: 'horizontal' (left→right by bounding-box center X), 'vertical' (top→bottom by center Y), or 'depth' (bottom→top by z-order). Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Ordered UUIDs of path nodes to blend. At minimum 2 required; 3+ produces visible interpolation on intermediate nodes. |
| `direction` | enum (`horizontal`, `vertical`, `depth`) | no | Optional sort axis. 'horizontal' sorts by bounding-box center X, 'vertical' by center Y, 'depth' by z-order. Omit to use the supplied order as-is. |

## `blend_objects`

Generate intermediate path nodes that interpolate between two paths in both shape (geometry) and fill color. Both source paths must have the same number of BezPath elements — use add_anchor_points to equalize if needed.

Three step-count modes:
- `steps` (default): fixed number of intermediate steps (default: 5)
- `smooth_color: true`: auto-compute steps so each step changes color by ≤ 1/255 (Smooth Color mode)
- `spacing`: steps = ceil(center_distance / spacing) — Specified Distance mode

Each intermediate node has: geometry (linear interp), fill color (linear interp, solid fills only), opacity (interpolated), and position (translation interpolated).

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id_a` | string | yes | First (start) path node UUID or name |
| `node_id_b` | string | yes | Second (end) path node UUID or name |
| `smooth_color` | boolean | no | Auto-compute steps so each step changes color by ≤ 1/255. When true, steps is ignored. |
| `spacing` | number | no | Specified Distance mode: space blend steps by this many pixels. Steps = ceil(dist / spacing). When set, overrides steps and smooth_color. |
| `steps` | integer | no | Number of intermediate steps to generate (default: 5, min: 1). Ignored when smooth_color or spacing is set. |

## `boolean_operation`

Combine two path nodes using a boolean operation. The result inherits fill and stroke from the target node and is placed at the target's z-position. By default both originals are removed.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `operation` | enum (`union`, `subtract`, `intersect`, `exclude`) | yes | union = merge shapes; subtract = cut tool from target; intersect = keep overlap; exclude = remove overlap |
| `target_id` | string | yes | Base shape — result inherits its style |
| `tool_id` | string | yes | Cutting/combining shape (relevant for subtract: tool is subtracted FROM target) |
| `keep_originals` | boolean | no | Keep original nodes alongside the result (default: false) |

## `branch_create`

Save the current document state as a named branch. If a branch with the same name already exists it is overwritten. Branches are stored in-memory and do not persist to disk.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Branch name (e.g. 'main', 'experiment-a'). |

## `branch_delete`

Delete a named branch. The live document is not affected.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the branch to delete. |

## `branch_list`

List all named document branches saved in the current session.

_No parameters._

## `branch_switch`

Restore the document to a previously saved named branch. Clears the undo/redo history. Equivalent to checking out a branch in version control.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the branch to restore. |

## `break_link_to_symbol`

Break the link between an instance node and its symbol master, converting it to an independent editable node. The symbol registry is unaffected.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | Instance node ID (UUID or name) to detach. |

## `brush_stroke`

Paint (or erase) a brush stroke along a polyline on a raster node. Points are in the image's local pixel space. mode 'paint' (default) composites `color`; mode 'erase' removes alpha.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes |  |
| `points` | array<array<number>> | yes | [[x,y],...] in image pixel space |
| `color` | any | no | Brush color: hex or [r,g,b,a] |
| `flow` | number | no | Per-dab build-up 0..1 (default 1) |
| `hardness` | number | no | 0 soft .. 1 hard (default 0.8) |
| `mode` | enum (`paint`, `erase`) | no | Default paint |
| `opacity` | number | no | Stroke opacity 0..1 (default 1) |
| `radius` | number | no | Brush radius in px (default 10) |
| `selection` | object | no |  |

## `bucket_fill`

Flood-fill a contiguous region of a raster node from a seed pixel, by color tolerance (paint bucket).

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `color` | any | yes | Fill color: hex or [r,g,b,a] |
| `node_id` | string | yes |  |
| `x` | integer | yes |  |
| `y` | integer | yes |  |
| `tolerance` | number | no | 0..1 color tolerance (default 0) |

## `build_shape_from_points`

Place any number of [x,y] points and connect them in any order to build a filled/stroked shape. Use connection_order to specify a custom vertex sequence; omit it to connect in the order given.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `points` | array<array<number>> | yes | Array of [x, y] coordinate pairs (the vertices) |
| `closed` | boolean | no | Close the path back to the start (default: true) |
| `connection_order` | array<integer> | no | Indices into 'points' defining connection sequence. Omit for sequential order. |
| `fill` | object | no | Fill — solid: {"type":"solid","color":"#rrggbb"} \| none: {"type":"none"} \| linear: {"type":"gradient","gradient_type":"linear","colors":["#hex1","#hex2"],"coords":[x0,y0,x1,y1]} \| radial: {"type":"gradient","gradient_type":"radial","colors":["#hex1","#hex2"],"coords":[cx,cy,r]} \| fluid: {"type":"fluid_gradient","points":[{"x":100,"y":50,"color":"#ff0000"},...],"power":2.0} \| mesh: {"type":"mesh_gradient","rows":2,"cols":2,"vertices":[{"x":0,"y":0,"color":"#ff0000"},...]} |
| `layer_id` | string | no |  |
| `name` | string | no |  |
| `stroke` | object | no | Stroke outline. Fields: color (#RRGGBB), width (number), enabled (bool), opacity (0-1), line_cap (butt\|round\|square), line_join (miter\|round\|bevel), align (center\|inside\|outside), dash_array ([dash,gap,...] up to 6 values), dash_offset (number). Example: {"color":"#000000","width":2,"enabled":true,"dash_array":[8,4]} |
| `tags` | array<string> | no |  |

## `center_on_canvas`

Center selected nodes on the canvas without scaling. Translates all nodes so their combined bounding box is centered. Supports horizontal-only or vertical-only centering.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `horizontal` | boolean | no | Center horizontally (default: true) |
| `node_ids` | array<string> | no | Node IDs. Empty = use selection. |
| `vertical` | boolean | no | Center vertically (default: true) |

## `check_grammar`

Check the document against its grammar rules. Returns per-rule pass/fail with descriptive messages. Read-only.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `rule_names` | array<string> | no | Optional subset of rule names to check. Defaults to all rules. |

## `check_style_continuity`

Analyse style consistency across the document or a node subset. Flags outliers — nodes whose fill color, stroke width, opacity, or font family deviate from the dominant values used by the rest of the selection. Returns a structured report; makes no changes to the document.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `checks` | array<enum (`fill`, `stroke`, `opacity`, `font`)> | no | Which property groups to check. Defaults to all four when omitted. |
| `node_ids` | array<string> | no | UUIDs of nodes to analyse. Omit or pass empty array to analyse the entire document. |
| `outlier_threshold` | integer | no | Minimum occurrences for a value to be considered 'dominant'. Nodes whose value appears fewer than this many times are flagged. |

## `clean_up`

Remove degenerate content: stray points (paths with no drawing segments), unpainted objects (no visible fill or stroke), and empty text nodes. Use dry_run:true to preview without deleting.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `dry_run` | boolean | no | Preview what would be removed without deleting (default false) |
| `remove_empty_text` | boolean | no | Remove text nodes with empty or whitespace-only content (default true) |
| `remove_stray_points` | boolean | no | Remove paths with no drawing segments (default true) |
| `remove_unpainted` | boolean | no | Remove paths with no visible fill and no visible stroke (default true) |

## `clear_blend_spine`

Remove the blend spine assignment from a group node, reverting it to default straight-line interpolation. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `group_id` | string | yes | UUID or name of the group node whose blend spine should be cleared. |

## `clear_guides`

Remove all unlocked ruler guides from the document. Locked guides are preserved.

_No parameters._

## `clear_layer_mask`

Remove the layer mask from a raster node (fully reveal).

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes |  |

## `clear_symbol_overrides`

Clear all per-instance color overrides on a symbol instance node, reverting it to the master's fill and stroke. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | UUID or name of the symbol instance node to reset. |

## `clear_tab_stops`

Remove all custom tab stops from a text node, restoring default tab spacing (every 4 em widths). Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | Text node ID or name. |

## `clear_text_area`

Remove the area boundary from a text node, reverting it to normal point text. The former area path node is unaffected. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `text_node_id` | string | yes | Text node ID (UUID or name) with an active area path. |

## `clear_text_path`

Remove the path spine from a text node, reverting it to normal positioned text. The former spine path node is unaffected. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `text_node_id` | string | yes | Text node ID (UUID or name) currently on a path. |

## `collect_in_new_layer`

Move a set of nodes into a newly created layer as a single undoable step. Group children are automatically resolved to their top-level ancestor.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | IDs of nodes to collect (group children resolve to their top-level ancestor) |
| `name` | string | no | Name for the new layer (default: "Collected Layer") |
| `position` | integer | no | Position in layer stack (0 = top/front; 1 = just below top; omit to add at top) |

## `color_guide`

Generate a color harmony palette from a base color using classic harmony rules. Supply a hex color directly or omit base_color to use the solid fill of the first selected node. Returns an array of colors including the base. Read-only — does not modify the document.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `base_color` | string | no | Hex color string (#RRGGBB or #RRGGBBAA). If omitted, uses the solid fill of the first selected node. |
| `rule` | enum (`complementary`, `analogous`, `triadic`, `split_complementary`, `tetradic`, `monochromatic`) | no | Color harmony rule. Default: 'complementary'. |

## `convert_anchor_points`

Convert all cubic bezier anchor points in the selected path nodes to smooth joins (handles made collinear through each interior anchor) or corner joins (handles retracted to the anchor, producing straight-line segments). Non-path nodes are skipped. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | IDs of path nodes to convert. |
| `mode` | enum (`smooth`, `corner`) | no | smooth: makes junction handles collinear (smooth bezier curve). corner: retracts cubic handles to their anchors (straight lines / cusps). Default: smooth. |

## `convert_to_grayscale`

Convert all color values (fill and stroke) on selected path nodes to grayscale using the ITU-R BT.601 luminance formula (0.299R + 0.587G + 0.114B). Works on solid fills, linear/radial gradient stops, fluid gradient points, and mesh gradient vertices. Alpha is preserved. If node_ids is omitted, all path nodes in the document are converted. Single undo step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | no | UUIDs of path nodes to convert. Omit to convert all path nodes in the document. |

## `copy_appearance`

Copy fill, stroke, and/or opacity from one source node to one or more target nodes (eyedropper-style). Each attribute can be toggled independently. Targets that are not path nodes will still have their opacity updated if copy_opacity is true. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `source_id` | string | yes | UUID or name of the node to copy appearance from. |
| `target_ids` | array<string> | yes | UUIDs or names of nodes to apply the appearance to. |
| `copy_fill` | boolean | no | Copy fill. Default: true. |
| `copy_opacity` | boolean | no | Copy opacity. Default: true. |
| `copy_stroke` | boolean | no | Copy stroke. Default: true. |

## `copy_nodes_to_clipboard`

Copy one or more nodes (and all their descendants) into the session clipboard ring.

The clipboard ring holds up to 20 entries. Copying always pushes to index 0 (most recent); older entries shift down. The clipboard is session-scoped — it is not persisted when Photonic closes.

Useful for AI workflows where you want to save a node or group for later reuse within the same session without modifying the document. Combine with `paste_from_history` to place saved nodes anywhere.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | IDs of the nodes to copy. Groups include all descendants automatically. |
| `label` | string | no | Optional human-readable label for this clipboard entry. Defaults to "N node(s)". |

## `create_adjustment_layer`

Create a NON-DESTRUCTIVE adjustment layer — a Photoshop adjustment layer. It carries no pixels; its adjustment is re-applied to the composite of every layer beneath it on each render, and the layer `opacity` controls the adjustment strength. `adjustment` and `params` use the exact same vocabulary as `apply_adjustment`. Move/reorder/hide it like any node; delete it to fully revert.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `adjustment` | string | yes | Adjustment name (see apply_adjustment) |
| `layer_id` | string | no |  |
| `name` | string | no |  |
| `opacity` | number | no | Adjustment strength 0..1 (default 1) |
| `params` | object | no | Adjustment-specific parameters |

## `create_array`

Repeat a node in a structured pattern — grid or radial. The source node stays in place; new copies are created around it in a single undoable step. Great for tile patterns, mandalas, icon grids, clock faces, and any repeating motif.

Grid mode: source is cell (0,0); `rows × cols - 1` copies fill the remaining cells. Copies are translated by (col × col_stride, row × row_stride).

Radial mode: source is instance 0; `count - 1` copies are placed at evenly-spaced angles around (center_x, center_y). Each copy is the source rotated around that centre by its angle, so the visual count (source + copies) = count.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `mode` | enum (`grid`, `radial`) | yes | Layout mode |
| `node_id` | string | yes | ID of the source node to repeat |
| `center_x` | number | no | (radial) X of rotation centre. Default 0. |
| `center_y` | number | no | (radial) Y of rotation centre. Default 0. |
| `col_stride` | number | no | (grid) Horizontal distance between column centres in px. Default 100. |
| `cols` | integer | no | (grid) Number of columns — source is col 0. Default 2. Total grid size must not exceed 10000 cells. |
| `count` | integer | no | (radial) Total instances including source (min 2, default 6). Creates count-1 new copies. |
| `group_result` | boolean | no | Wrap source + all copies into a new group node. Default false. |
| `layer_id` | string | no | Target layer UUID. Defaults to source node's layer. |
| `name_prefix` | string | no | Name prefix for copies, e.g. 'Petal' → 'Petal 1', 'Petal 2'. Defaults to the source node's name. |
| `row_stride` | number | no | (grid) Vertical distance between row centres in px. Default 100. |
| `rows` | integer | no | (grid) Number of rows — source is row 0. Default 2. Total grid size must not exceed 10000 cells. |
| `start_angle_degrees` | number | no | (radial) Clockwise angle in degrees for the first copy relative to the source. Default 0 (evenly distributed). |

## `create_arrow_shape`

Create a block arrow shape (chevron) with configurable dimensions and direction. The arrow has a triangular head and rectangular shaft.

Useful for flowcharts, infographics, directional indicators, and UI elements.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `x` | number | yes | Arrow tip X coordinate |
| `y` | number | yes | Arrow tip Y coordinate |
| `direction` | number | no | Direction in degrees, 0 = right (default: 0) |
| `fill` | object | no |  |
| `head_depth` | number | no | Head depth as fraction of length (default: 0.4) |
| `head_width` | number | no | Arrow head width (default: 40) |
| `layer_id` | string | no |  |
| `length` | number | no | Total arrow length (default: 100) |
| `shaft_width` | number | no | Shaft width (default: 16) |
| `stroke` | object | no |  |

## `create_bar_chart`

Create a bar chart from data values. Bars are proportional to their values. Supports vertical (default) and horizontal orientation, configurable gap, colors, and labels. Bars are grouped.

For vertical charts, y is the baseline (bottom) and bars grow upward. For horizontal, x is the left edge and bars grow rightward.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `values` | array<number> | yes | Data values |
| `x` | number | yes | Left X (vertical) or baseline X (horizontal) |
| `y` | number | yes | Bottom Y (vertical) or top Y (horizontal) |
| `colors` | array<string> | no | Bar colors hex (cycles) |
| `gap` | number | no | Gap between bars as fraction of bar width (default: 0.2) |
| `height` | number | no | Chart height (default: 200) |
| `horizontal` | boolean | no | Horizontal bars (default: false) |
| `labels` | array<string> | no | Bar labels |
| `layer_id` | string | no |  |
| `width` | number | no | Chart width (default: 300) |

## `create_character_style`

Save a named character style to the document. Capture from a source text node or specify attributes explicitly. Styles can be applied to any text node with apply_character_style.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Unique style name. |
| `fill_hex` | string | no | Fill color as CSS hex e.g. #FF5733. |
| `font_family` | string | no |  |
| `font_size` | number | no |  |
| `font_weight` | integer | no | 100–900. 400=regular, 700=bold. |
| `letter_spacing` | number | no |  |
| `line_height` | number | no | Multiplier e.g. 1.5 = 150%. |
| `source_node_id` | string | no | Capture font/color from this text node (UUID or name). Explicit args override captured values. |

## `create_cross`

Create a cross/plus shape. A 12-point polygon with configurable size, arm thickness, rotation, and style. Set rotation to 45° for an X shape.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `cx` | number | yes | Center X |
| `cy` | number | yes | Center Y |
| `fill` | object | no |  |
| `layer_id` | string | no |  |
| `rotation` | number | no | Rotation in degrees (default: 0, use 45 for X) |
| `size` | number | no | Total size (default: 60) |
| `stroke` | object | no |  |
| `thickness` | number | no | Arm thickness (default: 20) |

## `create_curvature_path`

Create a smooth curve that passes through all specified points using Catmull-Rom interpolation. Unlike create_path (which requires manual SVG path data with bezier control points), this tool automatically computes smooth bezier handles from just the on-curve points.

Use this when you want a smooth flowing curve through a set of coordinates without manually calculating control points. Optionally close the path to form a smooth closed shape.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `points` | array<array<number>> | yes | Ordered [x, y] points the curve passes through. Minimum 2 points. |
| `closed` | boolean | no | Close the path smoothly back to the first point (default: false) |
| `fill` | object | no | Fill style (see create_path for format) |
| `layer_id` | string | no | Target layer UUID (default: active layer) |
| `stroke` | object | no | Stroke style (see create_path for format) |

## `create_donut`

Create a donut (ring/annulus) shape with configurable inner and outer radius. Supports full rings and partial arc segments (e.g., a pie chart slice with a hole).

Full donuts use compound path with even-odd fill rule. Partial donuts create a closed wedge-shaped ring segment.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `cx` | number | yes | Center X |
| `cy` | number | yes | Center Y |
| `end_angle` | number | no | End angle in degrees (default: 360 = full ring) |
| `fill` | object | no | Fill style |
| `inner_radius` | number | no | Inner radius / hole size (default: 25) |
| `layer_id` | string | no |  |
| `outer_radius` | number | no | Outer radius (default: 50) |
| `start_angle` | number | no | Start angle in degrees for partial arcs (default: 0) |
| `stroke` | object | no | Stroke style |

## `create_flare`

Create a procedural lens flare vector effect at the specified position. Generates a grouped set of paths: a semi-transparent halo circle, radiating ray triangles, and concentric stroke rings.

All parts are grouped as 'Lens Flare'. Useful for light effects, sparkle decorations, and sci-fi/fantasy illustrations.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `cx` | number | yes | Center X coordinate |
| `cy` | number | yes | Center Y coordinate |
| `halo_color` | string | no | Halo color as hex (default: #fffbe6) |
| `halo_radius` | number | no | Halo circle radius (default: 50) |
| `layer_id` | string | no | Target layer UUID (default: active layer) |
| `ray_count` | integer | no | Number of radiating rays (default: 12). Total flare nodes, including halo and group, may not exceed 10000. |
| `ray_length` | number | no | Length of rays beyond the halo (default: 80) |
| `ray_opacity` | number | no | Ray opacity 0–1 (default: 0.3) |
| `ring_count` | integer | no | Number of concentric rings (default: 3). Total flare nodes, including halo and group, may not exceed 10000. |

## `create_freehand_path`

Create a freehand polyline path from an ordered list of canvas-space [x, y] points. Equivalent to using the Pencil tool by dragging. The path is open (no auto-close). Optionally specify fill and stroke styles.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `points` | array<array<number>> | yes | Ordered canvas-space points [x, y]. Minimum 2 required. |
| `fill` | object | no | Optional fill. |
| `name` | string | no | Node name (default: 'Pencil'). |
| `stroke` | object | no | Optional stroke. |

## `create_gear`

Create a gear/cog shape with configurable tooth count, inner/outer radius, and center hole. Useful for mechanical icons, settings symbols, and technical illustrations.

The gear is a compound path with even-odd fill rule for the center hole.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `cx` | number | yes | Center X |
| `cy` | number | yes | Center Y |
| `fill` | object | no |  |
| `hole_radius` | number | no | Center hole radius (default: 10, 0 = no hole) |
| `inner_radius` | number | no | Base of teeth radius (default: 35) |
| `layer_id` | string | no |  |
| `outer_radius` | number | no | Tip of teeth radius (default: 50) |
| `stroke` | object | no |  |
| `teeth` | integer | no | Number of teeth (default: 12) |

## `create_grid`

Create a rectangular grid of lines. Specify position, size, and the number of rows and columns. The grid is drawn as a single path of open line subpaths.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `height` | number | yes | Total grid height in document units |
| `width` | number | yes | Total grid width in document units |
| `x` | number | yes | X coordinate of the top-left corner |
| `y` | number | yes | Y coordinate of the top-left corner |
| `cols` | integer | no | Number of columns (default: 4) |
| `fill` | object | no |  |
| `layer_id` | string | no |  |
| `name` | string | no |  |
| `rows` | integer | no | Number of rows (default: 4) |
| `stroke` | object | no |  |

## `create_heart`

Create a heart shape using smooth cubic bezier curves. Defaults to red fill if no style specified. The cy coordinate is the bottom tip of the heart.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `cx` | number | yes | Center X |
| `cy` | number | yes | Bottom tip Y |
| `fill` | object | no |  |
| `layer_id` | string | no |  |
| `size` | number | no | Heart width (default: 60) |
| `stroke` | object | no |  |

## `create_layer`

Create a new layer in the document

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes |  |
| `position` | integer | no | Position in layer stack (0 = top/front; 1 = just below top; omit to add at top) |

## `create_line_chart`

Create a line chart from one or more data series. Lines can be smooth (Catmull-Rom) or straight. Supports area fill under lines. Multiple series overlaid on the same axes.

Data is auto-scaled to fit the chart area. Y axis grows upward from the baseline.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `series` | array<array<number>> | yes | Data series — each is an array of values |
| `x` | number | yes | Left X |
| `y` | number | yes | Baseline Y (bottom) |
| `colors` | array<string> | no | Line colors hex |
| `fill_area` | boolean | no | Fill area under lines (default: false) |
| `height` | number | no | Chart height (default: 200) |
| `layer_id` | string | no |  |
| `smooth` | boolean | no | Smooth with Catmull-Rom (default: true) |
| `stroke_width` | number | no | Line width (default: 2) |
| `width` | number | no | Chart width (default: 300) |

## `create_paragraph_style`

Save a named paragraph style (alignment, line height, letter spacing, font) to the document. Capture from a source text node or specify attributes directly.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Unique style name. |
| `align` | enum (`left`, `center`, `right`) | no | Text alignment. |
| `font_family` | string | no |  |
| `font_size` | number | no |  |
| `letter_spacing` | number | no |  |
| `line_height` | number | no | Line height multiplier e.g. 1.5. |
| `source_node_id` | string | no | Capture layout from this text node (UUID or name). |

## `create_parametric_shape`

Create a closed path from a parametric mathematical equation. Five shape types:
- `lissajous`: x = A·sin(a·t + δ), y = B·sin(b·t) — elegant figure-8 and knot curves
- `superellipse`: |x/a|^n + |y/b|^n = 1 — from astroid (n=0.5) through ellipse (n=2) to squircle (n=4)
- `rose`: r = cos(k·θ) — flower-like petals (odd k → k petals, even k → 2k petals)
- `hypotrochoid`: rolling circle inside a larger circle — spirograph patterns
- `epicycloid`: rolling circle outside a larger circle — epicycloid petals and curves

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `cx` | number | yes | Center X |
| `cy` | number | yes | Center Y |
| `shape_type` | enum (`lissajous`, `superellipse`, `rose`, `hypotrochoid`, `epicycloid`) | yes | Which parametric curve to generate |
| `delta_deg` | number | no | Lissajous: phase offset δ in degrees (default: 90) |
| `exponent` | number | no | Superellipse: exponent n (default: 2.5; 2=ellipse, >2=squircle, <2=astroid-like) |
| `fill` | object | no |  |
| `freq_a` | number | no | Lissajous: x-frequency a (default: 3) |
| `freq_b` | number | no | Lissajous: y-frequency b (default: 2) |
| `inner_ratio` | number | no | Hypotrochoid/Epicycloid: rolling circle radius as fraction of outer radius (default: 0.4) |
| `layer_id` | string | no |  |
| `pen_ratio` | number | no | Hypotrochoid/Epicycloid: pen distance as fraction of rolling radius (default: 1.0) |
| `petals` | number | no | Rose: petal factor k (default: 5; odd k → k petals, even k → 2k petals) |
| `points` | integer | no | Sample points for the polyline path (default: 360, max: 4096) |
| `radius` | number | no | Overall scale / outer radius (default: 80) |
| `ratio_x` | number | no | X semi-axis ratio (Lissajous/Superellipse, default: 1.0) |
| `ratio_y` | number | no | Y semi-axis ratio (Lissajous/Superellipse, default: 1.0) |
| `stroke` | object | no |  |

## `create_path`

Create a vector path from SVG path data (M/L/C/Q/Z commands)

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `path_data` | string | yes | SVG path data, e.g. 'M 0 0 L 100 0 L 100 100 Z' |
| `fill` | object | no | Fill — solid: {"type":"solid","color":"#rrggbb"} \| none: {"type":"none"} \| linear: {"type":"gradient","gradient_type":"linear","colors":["#hex1","#hex2"],"coords":[x0,y0,x1,y1]} \| radial: {"type":"gradient","gradient_type":"radial","colors":["#hex1","#hex2"],"coords":[cx,cy,r]} \| fluid: {"type":"fluid_gradient","points":[{"x":100,"y":50,"color":"#ff0000"},...],"power":2.0} \| mesh: {"type":"mesh_gradient","rows":2,"cols":2,"vertices":[{"x":0,"y":0,"color":"#ff0000"},...]} |
| `layer_id` | string | no |  |
| `name` | string | no |  |
| `stroke` | object | no | Stroke outline. Fields: color (#RRGGBB), width (number), enabled (bool), opacity (0-1), line_cap (butt\|round\|square), line_join (miter\|round\|bevel), align (center\|inside\|outside), dash_array ([dash,gap,...] up to 6 values), dash_offset (number). Example: {"color":"#000000","width":2,"enabled":true,"dash_array":[8,4]} |
| `tags` | array<string> | no |  |

## `create_pie_chart`

Create a pie chart from data values. Each slice is proportional to its value. Supports solid pie and donut style (with inner_radius). Slices are grouped. Default Tableau-10 color palette.

Useful for data visualization, infographics, and dashboards.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `cx` | number | yes | Center X |
| `cy` | number | yes | Center Y |
| `values` | array<number> | yes | Data values — slice sizes are proportional |
| `colors` | array<string> | no | Slice colors as hex (cycles if fewer than values) |
| `inner_radius` | number | no | Inner radius for donut chart (default: 0 = solid pie) |
| `labels` | array<string> | no | Slice labels (optional) |
| `layer_id` | string | no |  |
| `radius` | number | no | Outer radius (default: 80) |

## `create_polar_grid`

Create a polar (radial) grid centered at a point. Draws concentric circles and radial spokes.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `outer_radius` | number | yes | Outer radius in document units |
| `x` | number | yes | X coordinate of the center |
| `y` | number | yes | Y coordinate of the center |
| `fill` | object | no |  |
| `inner_radius` | number | no | Inner radius (0 = full disk, default: 0) |
| `layer_id` | string | no |  |
| `name` | string | no |  |
| `rings` | integer | no | Number of concentric rings (default: 4) |
| `sectors` | integer | no | Number of radial sectors/spokes (default: 8) |
| `stroke` | object | no |  |

## `create_qr_code`

Generate a native, resolution-independent QR code as a group of vectors (a compound path of all dark modules, plus an optional background). Encodes a URL/text and lets you style the modules while staying scannable — the three finder 'eyes' are always rendered recognizably so stylized codes still scan.

Styles: square (classic), rounded (rounded squares), dot (circles), connected ('blob' — neighbouring modules merge with smooth joins). Dark modules take any fill (solid or gradient). Keep good dark-on-light contrast and use a higher error-correction level (q/h) when heavily stylized.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `data` | string | yes | Content to encode (URL or text) |
| `background` | string | no | Background behind the code: a #rrggbb hex (default white) or 'none' for transparent |
| `ecc` | enum (`l`, `m`, `q`, `h`) | no | Error-correction level (default: m). Higher = more robust but denser. |
| `fill` | object | no | Fill for the dark modules — solid or gradient (default solid black) |
| `layer_id` | string | no |  |
| `module_shape` | enum (`square`, `rounded`, `dot`, `connected`) | no | Module style (default: square) |
| `quiet_zone` | integer | no | Quiet-zone margin in modules (default 4; keep >= 2 to stay scannable) |
| `radius` | number | no | Corner radius as a fraction of one module, 0..0.5 (default 0.4). Used by rounded/connected. |
| `size` | number | no | Total artwork size in document units incl. quiet zone (default 200) |
| `x` | number | no | Top-left X (default 0) |
| `y` | number | no | Top-left Y (default 0) |

## `create_radar_chart`

Create a radar (spider) chart from multi-dimensional data. Each axis represents one dimension; each series is drawn as a polygon scaled to its values per axis. Supports filled semi-transparent overlays, configurable grid rings, and multiple overlapping series. Default Tableau-10 color palette.

Useful for comparing profiles (skills, stats, attributes) across multiple subjects.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `cx` | number | yes | Center X |
| `cy` | number | yes | Center Y |
| `series` | array<array<number>> | yes | Data series. Each series is an array of values, one per axis. All series must have the same length (≥ 3). |
| `colors` | array<string> | no | Series fill/stroke colors as hex (cycles if fewer than series) |
| `fill_area` | boolean | no | Fill series polygons with semi-transparent color (default: true) |
| `grid_rings` | number | no | Number of concentric grid rings (default: 4) |
| `labels` | array<string> | no | Axis labels, one per axis (optional) |
| `layer_id` | string | no |  |
| `radius` | number | no | Outer radius (default: 100) |
| `series_names` | array<string> | no | Series names for node labeling (optional) |
| `stroke_width` | number | no | Stroke width for series polygons (default: 1.5) |

## `create_raster_layer`

Create a blank raster (pixel) layer of a given size — a transparent canvas to paint on, or filled with a solid color. Edit with brush_stroke, gradient_fill, apply_filter, etc. Dimensions are limited to 16384 pixels per side and 67108864 pixels total.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `height` | integer | yes | Height in pixels |
| `width` | integer | yes | Width in pixels |
| `fill` | any | no | Optional fill color: hex string (#rrggbb / #rrggbbaa) or [r,g,b,a] (0-255). Default: transparent. |
| `layer_id` | string | no |  |
| `name` | string | no |  |
| `x` | number | no | X position in document space (default 0) |
| `y` | number | no | Y position in document space (default 0) |

## `create_scatter_plot`

Create a scatter plot from X/Y data points. Points are auto-scaled to fit the plot area. Each point rendered as a filled circle.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `points` | array<array<number>> | yes | Data points as [x, y] pairs |
| `x` | number | yes | Plot area left X |
| `y` | number | yes | Plot area bottom Y |
| `color` | string | no | Dot color hex (default: #4E79A7) |
| `dot_radius` | number | no | Dot radius (default: 4) |
| `height` | number | no | Plot height (default: 300) |
| `layer_id` | string | no |  |
| `width` | number | no | Plot width (default: 300) |

## `create_shape`

Create a primitive shape (rectangle, rounded_rect, ellipse, arc, polygon, star, line). For arc: x,y,width,height define the bounding box; arc_start_angle and arc_end_angle set the sweep in degrees (0=3 o'clock); arc_open=true for open arc, false for closed pie sector.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `height` | number | yes |  |
| `shape_type` | enum (`rectangle`, `rounded_rect`, `ellipse`, `arc`, `polygon`, `star`, `line`) | yes |  |
| `width` | number | yes |  |
| `x` | number | yes |  |
| `y` | number | yes |  |
| `arc_end_angle` | number | no | Arc end angle in degrees. Default: 270 (¾ circle). |
| `arc_open` | boolean | no | If true, draw open arc stroke only. If false (default), close back to center (pie sector). |
| `arc_start_angle` | number | no | Arc start angle in degrees (0=3 o'clock, 90=6 o'clock). Default: 0. |
| `color` | string | no | Shorthand for a solid fill colour, e.g. "#2277ff". Ignored when "fill" is also provided. |
| `corner_radius` | number | no | Corner radius for rounded_rect shapes in document units (default: 10.0). Clamped to half the shortest side. |
| `fill` | object | no | Fill — solid: {"type":"solid","color":"#rrggbb"} \| none: {"type":"none"} \| linear: {"type":"gradient","gradient_type":"linear","colors":["#hex1","#hex2"],"coords":[x0,y0,x1,y1]} \| radial: {"type":"gradient","gradient_type":"radial","colors":["#hex1","#hex2"],"coords":[cx,cy,r]} \| fluid: {"type":"fluid_gradient","points":[{"x":100,"y":50,"color":"#ff0000"},...],"power":2.0} \| mesh: {"type":"mesh_gradient","rows":2,"cols":2,"vertices":[{"x":0,"y":0,"color":"#ff0000"},...]} |
| `inner_radius` | number | no | Inner radius ratio (star, 0–1) |
| `layer_id` | string | no |  |
| `name` | string | no |  |
| `rx` | number | no | Reserved |
| `sides` | integer | no | Sides (polygon/star) |
| `stroke` | object | no | Stroke outline. Fields: color (#RRGGBB), width (number), enabled (bool), opacity (0-1), line_cap (butt\|round\|square), line_join (miter\|round\|bevel), align (center\|inside\|outside), dash_array ([dash,gap,...] up to 6 values), dash_offset (number). Example: {"color":"#000000","width":2,"enabled":true,"dash_array":[8,4]} |
| `tags` | array<string> | no |  |

## `create_speech_bubble`

Create a speech bubble shape — a rounded rectangle with a triangular tail pointing to a specified location. Defaults to white fill with black stroke. Tail position is configurable.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `cx` | number | yes | Bubble center X |
| `cy` | number | yes | Bubble center Y |
| `corner_radius` | number | no | Corner radius (default: 15) |
| `fill` | object | no |  |
| `height` | number | no | Bubble height (default: 60) |
| `layer_id` | string | no |  |
| `stroke` | object | no |  |
| `tail_width` | number | no | Tail base width (default: 20) |
| `tail_x` | number | no | Tail tip X (default: below-left of center) |
| `tail_y` | number | no | Tail tip Y |
| `width` | number | no | Bubble width (default: 120) |

## `create_spiral`

Create an Archimedean spiral path. Specify center, outer/inner radius, and number of turns.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `outer_radius` | number | yes | Maximum (outer) radius in document units |
| `x` | number | yes | X coordinate of spiral center |
| `y` | number | yes | Y coordinate of spiral center |
| `fill` | object | no |  |
| `inner_radius` | number | no | Minimum (inner) radius. Use 0 for a true center spiral (default: 0) |
| `layer_id` | string | no |  |
| `name` | string | no |  |
| `segments_per_turn` | integer | no | Bézier segments per revolution for smoothness (default: 16). The rounded total across all turns may not exceed 10000 segments. |
| `stroke` | object | no |  |
| `turns` | number | no | Number of full revolutions (default: 3) |

## `create_stacked_bar_chart`

Create a stacked bar or column chart from multiple data series. Each series is stacked on top of the previous one within each position. Useful for showing part-to-whole relationships across categories. Default Tableau-10 color palette.

For vertical charts (default), x/y is the bottom-left corner and bars grow upward. For horizontal, bars grow rightward.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `series` | array<array<number>> | yes | Data series. Each series is one dataset. All series must have the same length (one value per stack position). |
| `x` | number | yes | Left X |
| `y` | number | yes | Bottom Y (vertical) or top Y (horizontal) |
| `colors` | array<string> | no | Series colors as hex (one per series, cycles) |
| `gap` | number | no | Gap between stacks as fraction of bar width (default: 0.2) |
| `height` | number | no | Chart height (default: 200) |
| `horizontal` | boolean | no | Horizontal bars (default: false = vertical columns) |
| `labels` | array<string> | no | Labels for each stack position (column/bar) |
| `layer_id` | string | no |  |
| `series_names` | array<string> | no | Series names for node labeling |
| `width` | number | no | Chart width (default: 300) |

## `create_sunburst`

Create a radial sunburst pattern — alternating filled wedges radiating from a center point. Classic retro/vintage effect.

Wedges are created as a single compound path with smooth arc edges. Configurable ray count, inner/outer radius, and color.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `cx` | number | yes | Center X |
| `cy` | number | yes | Center Y |
| `color` | string | no | Wedge fill color hex (default: #FFD700 gold) |
| `inner_radius` | number | no | Inner radius (default: 20). Set to 0 for no hole. |
| `layer_id` | string | no |  |
| `outer_radius` | number | no | Outer radius (default: 100) |
| `rays` | integer | no | Number of rays — half are filled (default: 24) |

## `create_text`

Create a text node at a position. Use update_node to change content, font, size, or color after creation.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `content` | string | yes | The text to display |
| `x` | number | yes | X position in document space |
| `y` | number | yes | Y position in document space |
| `align` | enum (`left`, `center`, `right`) | no | Text alignment (default: left) |
| `fill` | object | no | Fill colour — e.g. {"type":"solid","color":"#000000"} |
| `font_family` | string | no | Font family name (default: sans-serif) |
| `font_size` | number | no | Font size in document units (default: 16) |
| `font_weight` | integer | no | Font weight 100–900 (default: 400) |
| `layer_id` | string | no |  |
| `letter_spacing` | number | no | Letter spacing in document units (default: 0). Positive = wider. |
| `line_height` | number | no | Line height multiplier (default: 1.2). 1.0 = tight, 2.0 = double-spaced. |
| `name` | string | no |  |
| `stroke` | object | no | Stroke outline |
| `tags` | array<string> | no |  |

## `create_truchet_tiling`

Generate a Truchet tiling — a grid of algorithmically arranged tiles where each tile is one of two orientations, chosen randomly from a seed. Creates organic, labyrinthine, or kaleidoscopic patterns depending on the tile style.

Styles:
- "arcs" (default): two quarter-circle arcs per tile — classic Truchet pattern
- "diagonals": a straight diagonal line per tile — creates maze-like cross-hatch patterns
- "triangles": a filled triangle per tile — creates woven/checkerboard-like patterns

All tiles are grouped into a single node.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `x` | number | yes | Top-left X of the tiling area |
| `y` | number | yes | Top-left Y of the tiling area |
| `background` | string | no | Background fill color as hex; if absent no background is added |
| `color` | string | no | Stroke/fill color for tiles as hex (default: #1a1a2e) |
| `height` | number | no | Height of the tiling area (default: 200) |
| `layer_id` | string | no |  |
| `seed` | number | no | Random seed for reproducible patterns (default: 42) |
| `stroke_width` | number | no | Stroke width for arc/diagonal tiles (default: 2.0) |
| `style` | enum (`arcs`, `diagonals`, `triangles`) | no | Tile pattern style (default: arcs) |
| `tile_size` | number | no | Side length of each tile in px (default: 40, min: 4) |
| `width` | number | no | Width of the tiling area (default: 200) |

## `create_vectors_from_css`

Compile a bounded CSS snippet into editable Photonic groups and vector paths. Strict mode (the default) rejects unsupported/lossy CSS without mutation; dry_run returns the deterministic plan without changing the document.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `css` | string | yes | Inline declaration block or supported selector tree. |
| `dry_run` | boolean | no |  |
| `group_name` | string | no |  |
| `layer_id` | string | no |  |
| `origin` | object | no |  |
| `selector` | string | no | Optional virtual root selector. |
| `strict` | boolean | no |  |
| `viewport` | object | no |  |

## `create_vectors_from_react`

Import a bounded, static local JSX fragment or an allowlisted local React snapshot into editable Photonic groups and vector paths. The snapshot resolver reads only explicitly bounded local modules and assets; it never executes JavaScript, fetches network resources, follows arbitrary imports, or loads external Tailwind configuration. Unsupported or dynamic input is rejected before the document changes.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `dry_run` | boolean | no |  |
| `dynamic_content` | object | no | Pinned bounded wrapper branches, such as backgroundImage:null and enableInactivity:false. |
| `export_name` | string | no | Named React export to import. |
| `group_name` | string | no |  |
| `interaction_policy` | enum (`reject`, `strip`) | no | Reject rendered event handlers, or strip them with warnings/provenance. JavaScript is never executed. |
| `jsx` | string | no | Static JSX fragment of intrinsic layout elements. |
| `layer_id` | string | no |  |
| `module_roots` | array<string> | no | Approved local roots for source and bounded imports. |
| `origin` | object | no |  |
| `props` | object | no | JSON-only static props snapshot. |
| `source_path` | string | no | Allowlisted local module path; it and every resolved module/asset must be beneath module_roots. |
| `strict` | boolean | no |  |
| `theme_tokens` | object | no | Optional hex overrides for source-resolved light theme tokens. |
| `viewport` | object | no |  |

## `create_wave_pattern`

Generate a decorative wave/sine pattern as a compound stroke path. Creates multiple parallel sine waves with configurable wavelength, amplitude, and line count.

Useful for water effects, hair/fur textures, topographic maps, decorative borders, and abstract backgrounds.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `height` | number | yes | Pattern height |
| `width` | number | yes | Pattern width |
| `x` | number | yes | Left edge X |
| `y` | number | yes | Top edge Y |
| `amplitude` | number | no | Wave amplitude (default: 10) |
| `layer_id` | string | no |  |
| `lines` | integer | no | Number of wave lines (default: 8) |
| `stroke` | object | no | Stroke style |
| `wavelength` | number | no | Wavelength in document units (default: 40) |

## `crystallize_path`

Add sharp outward spike detail to path segments, creating star-like, crystal, or frost-like edges. Each segment is replaced with triangular spikes pointing outward from the path.

Configurable spike height (size) and number of spikes per segment (count). Destructive — modifies path data. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Path node IDs to crystallize |
| `count` | integer | no | Number of spikes per original segment (default: 3, min: 1) |
| `size` | number | no | Height of each spike in document units (default: 10) |

## `define_action`

Define (or overwrite) a named action set — a replayable sequence of MCP tool calls. Use to record multi-step workflows that can be replayed in one call. Node IDs in steps can be substituted at play time.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Unique action name. |
| `steps` | array<object> | yes | Ordered list of tool steps. |

## `define_grammar_rule`

Define (or update) a named design grammar rule. Rules constrain the document: palette_includes (a specific color must appear), max_colors (palette size limit), min_text_size (minimum font size), required_layer (a named layer must exist), max_node_count (total node limit). Run check_grammar to validate.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Unique rule name (used as key in check results). |
| `params` | object | yes | Rule parameters: palette_includes={color_hex}, max_colors={count}, min_text_size={px}, required_layer={name or prefix}, max_node_count={count}. |
| `rule_type` | enum (`palette_includes`, `max_colors`, `min_text_size`, `required_layer`, `max_node_count`) | yes | Rule type discriminator. |

## `define_graphic_style`

Define (or overwrite) a named graphic style — a reusable appearance preset storing fill, stroke, and opacity. Capture style from an existing node by passing node_id, or define it explicitly with fill_hex, stroke_hex, stroke_width, and opacity. Apply later with apply_graphic_style.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Unique style name. |
| `fill_hex` | string | no | Fill color as hex (e.g. '#ff0000'). Used when node_id is not provided. |
| `node_id` | string | no | Capture fill, stroke, and opacity from this node (UUID or name). Omit to use explicit parameters. |
| `opacity` | number | no | Node opacity 0.0–1.0. Default 1.0. |
| `stroke_hex` | string | no | Stroke color as hex. Used when node_id is not provided. |
| `stroke_width` | number | no | Stroke width in px. Used when node_id is not provided. |

## `define_pattern`

Define (or overwrite) a named tiled raster pattern in the document pattern registry. Supply the tile via `path` (a file on disk) or `data_base64` (inline image bytes). Patterns are applied to nodes with apply_pattern_fill; the applied fill embeds a clone of the tile so it renders self-contained on canvas, headless, and SVG.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Unique pattern name. |
| `data_base64` | string | no | Base64-encoded tile image bytes (alternative to `path`). |
| `offset` | array<number> | no | Document-space [x, y] offset of the pattern origin (default [0, 0]). |
| `path` | string | no | Path to a tile image file (PNG/JPEG/WebP/…). |
| `rotation_degrees` | number | no | Pattern rotation in degrees (default 0). |
| `scale` | number | no | Uniform pattern scale (default 1.0). |
| `spacing` | number | no | Inter-tile gutter in tile pixels; samples as transparent (default 0). |
| `tile_type` | enum (`grid`, `brick_by_row`, `brick_by_column`, `hex`) | no | Tile layout (default 'grid'). |

## `define_spot_color`

Define (or update) a named spot color. Spot colors are named inks with optional overprint behavior. Unlike regular color swatches, they carry print-production semantics and can be applied as solid fills.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `hex` | string | yes | Hex color value (e.g. '#FF2400'). Leading # is optional. |
| `name` | string | yes | Unique spot color name (e.g. 'Pantone 485 C'). |
| `overprint` | boolean | no | When true, ink overprints underlying colors (print production). Default: false. |

## `define_symbol`

Designate a node as a named symbol master. Any node can be a symbol: paths, groups, text. Instances placed with place_symbol are independent copies that carry a symbol_ref tag identifying their origin symbol.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Unique symbol name. |
| `node_id` | string | yes | Node ID (UUID or name) to designate as the symbol master. |

## `define_variable`

Define (or update) a named document variable. Variables are key-value string pairs that can be bound to text nodes and applied in batch with apply_variables. Useful for data-driven design: names, prices, dates, labels.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Unique variable name. |
| `value` | string | yes | String value. |

## `define_width_profile`

Define (or overwrite) a named variable-width stroke profile. Widths are sampled at even t intervals along the path (t=0 start, t=1 end). When applied, the average width is used for uniform stroke rendering — the profile is stored for future variable-width rendering support.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Unique profile name. |
| `widths` | array<number> | yes | Width values (≥2) in document units, from path start to end. E.g. [1, 4, 1] = thin ends, thick middle. |

## `delete_action`

Delete a named action set.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the action to delete. |

## `delete_anchor_point`

Remove specific anchor points from a path node by their zero-based BezPath element indices. The path is rebuilt with the specified elements removed. Use inspect_node to discover anchor count, or the Direct Select tool in the GUI to visually identify anchor indices. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `anchor_indices` | array<integer> | yes | Zero-based indices of BezPath elements to remove |
| `node_id` | string | yes | Path node UUID or name |

## `delete_character_style`

Delete a named character style from the document.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the style to delete. |

## `delete_color_swatch`

Remove a named color swatch from the document palette. Does not alter existing node fill colors.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the swatch to delete. |

## `delete_gradient_swatch`

Delete a named gradient swatch from the document registry. Does not affect nodes that were already painted with this gradient.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the gradient swatch to delete. |

## `delete_grammar_rule`

Delete a named design grammar rule.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the rule to delete. |

## `delete_graphic_style`

Delete a named graphic style from the document. Existing nodes are not affected.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the graphic style to delete. |

## `delete_layer`

Delete a layer. By default, nodes are moved to the first remaining layer. Set delete_nodes=true to also remove all nodes. Cannot delete the last layer.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `layer_id` | string | yes | Layer UUID or name |
| `delete_nodes` | boolean | no | Also delete all nodes on the layer (default: false — moves them) |

## `delete_nodes`

Delete one or more nodes by ID

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes |  |

## `delete_paragraph_style`

Delete a named paragraph style from the document.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes |  |

## `delete_pattern`

Delete a named pattern from the registry. Does not affect nodes already filled with it.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the pattern to delete. |

## `delete_spot_color`

Delete a named spot color from the document. Does not alter existing node fills.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the spot color to delete. |

## `delete_symbol`

Remove a named symbol from the registry. Existing instances are converted to standalone nodes (symbol_ref cleared). The master node itself is not deleted.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Symbol name to delete. |

## `delete_variable`

Delete a named document variable. Does not unbind existing text nodes — their binding name is retained but will no longer update.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Variable name to delete. |

## `delete_width_profile`

Delete a named width profile. Does not affect existing node stroke widths.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the profile to delete. |

## `delete_workspace`

Delete a named workspace preset from the document.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the workspace to delete. |

## `deselect_all`

Clear the selection (deselect all nodes).

_No parameters._

## `detect_rhythms`

Detect visual rhythm patterns in the document: evenly-spaced objects (horizontal/vertical), uniform widths, geometric size progressions, and rotational symmetry. Returns structured findings with descriptions and extension suggestions. Read-only.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `min_count` | integer | no | Minimum number of nodes required to form a pattern (default: 3). |
| `node_ids` | array<string> | no | Optional list of node UUIDs or names to restrict the analysis to. Defaults to all visible leaf nodes. |

## `diff_checkpoints`

Compare two checkpoint snapshots and return a structured JSON diff of added, removed, and modified nodes and layers. Use list_checkpoints first to get checkpoint IDs.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `from_id` | string | yes | UUID of the baseline (older) checkpoint |
| `to_id` | string | yes | UUID of the target (newer) checkpoint |

## `distribute_no_overlap`

Push nodes apart until none of their bounding boxes overlap, using iterative pairwise repulsion. Nodes are nudged along the axis with the smallest overlap at each step.

Useful for:
- Spreading a pile of overlapping objects
- Auto-spacing labels, icons, or stickers
- Resolving collisions after a bulk paste or array creation

Node positions are updated in a single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `max_iterations` | number | no | Maximum resolution iterations (default: 100, max: 500) |
| `node_ids` | array<string> | no | IDs of nodes to distribute. Uses current selection if empty. |
| `padding` | number | no | Minimum gap between bounding boxes in px (default: 4) |

## `distribute_on_path`

Place evenly-spaced copies of one or more nodes along a guide path. Each source node is cloned at arc-length-equidistant positions along the first subpath of the path node. Optionally rotates each copy to align with the path's tangent direction.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | IDs of source nodes to clone and distribute. Nodes are cycled if count > node_ids.length. |
| `path_node_id` | string | yes | ID of the path node to use as the distribution guide |
| `align_to_path` | boolean | no | Rotate each copy to face along the path's tangent direction. Default: false. |
| `count` | integer | no | Number of copies to place. Defaults to the number of source nodes. |
| `layer_id` | string | no | Target layer for the copies. Defaults to the guide path's layer. |

## `divide_objects_below`

Use a selected path as a cutting edge to divide all path nodes beneath it in z-order. Each overlapping node below is split into two face nodes (inside the cutter, outside the cutter). Non-overlapping nodes are untouched. The cutter is always removed. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | UUID of the cutting path node (must be a path; will be removed after cutting) |

## `duplicate_artboard`

Duplicate an artboard together with all content/nodes inside it, as one undoable step. The copy becomes active. By default it is placed to the right of the original with a 40-unit gutter (offset_x = original width + 40, offset_y = 0). Get the artboard_id from list_artboards.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `artboard_id` | string | yes | UUID of the artboard to duplicate (from list_artboards). |
| `new_name` | string | no | Optional name for the copy. Default: '{original name} copy'. |
| `offset_x` | number | no | Horizontal offset for the copy in document units. Default: original width + 40. |
| `offset_y` | number | no | Vertical offset for the copy in document units. Default: 0. |

## `duplicate_layer`

Duplicate a layer with all its nodes. Creates a copy of the layer and deep-clones every node with new IDs. Single undoable batch.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `layer_id` | string | yes | Layer UUID or name to duplicate |
| `name` | string | no | Name for the copy (default: '<original> Copy') |

## `duplicate_nodes`

Deep-clone one or more nodes, creating N offset copies. Groups are duplicated with all their descendants — every node in the subtree gets a fresh ID. All copies land in one undoable batch. Returns the IDs of the new root nodes.

Use cases: repeating elements (stars, petals, grid cells), creating variations, building patterns without re-specifying styles.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | IDs of the nodes to duplicate |
| `count` | integer | no | Number of copies to create per source node. Copy N is offset by N × {offset}. |
| `layer_id` | string | no | Target layer for the copies. Defaults to the source node's own layer. |
| `offset` | object | no | Position shift applied per copy. Copy 1 shifts by 1×offset, copy 2 by 2×offset, etc. Default: {x: 10, y: 10}. |

## `enter_isolation_mode`

Enter Isolation Mode for a group: select all direct children of the group, restricting further edits to those children. Equivalent to double-clicking a group in Illustrator. In the GUI, only children of the group are clickable until Escape is pressed.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `group_id` | string | yes | The UUID of the group node to isolate. |

## `exit_isolation_mode`

Exit Isolation Mode: clear the current selection and return to normal editing. Equivalent to pressing Escape in Illustrator's isolation mode.

_No parameters._

## `expand_blend`

Expand a blend group into individual discrete objects. Dissolves the group wrapper and places all child objects as standalone nodes at the parent layer position — equivalent to Illustrator's Object > Blend > Expand. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `group_id` | string | yes | UUID or name of the blend group to expand. |

## `export_artboards`

Export one or more artboards to raster images — one image per artboard. Returns base64 by default, OR — when `path` is given — writes each image to disk and returns only a small result (path + dimensions per artboard), keeping full-resolution PNGs off the socket. Unlike export_raster (which snapshots the live editor viewport), each artboard's rectangle is rendered off-screen at its own aspect ratio, so results are exact regardless of the current zoom/scroll. Select artboards by (precedence): all=true → every artboard; range=[start,end] → 1-based inclusive index range; artboards=[...] → a list of UUIDs, exact names, or 1-based index strings; otherwise the active artboard. Get ids/names/indices from list_artboards. Set bleed=true to include the document print bleed around each artboard (for print upload). Max 24 artboards per call; each side capped at 8192 px.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `all` | boolean | no | Export every artboard in document order. Overrides range/artboards. |
| `artboards` | array<string> | no | Specific artboards to export, each a UUID, an exact name, or a 1-based index string ("1", "2", …). Exported in the given order. |
| `bleed` | boolean | no | Expand each artboard's rectangle by the document print bleed (bleed_mm, at the export scale/DPI) on all four sides, so the output includes the bleed area for print upload. Output dimensions become artboard px + 2×bleed px. Default false (trim only). |
| `format` | enum (`png`, `jpeg`, `webp`, `gif`, `tiff`) | no | Output format (default: png). |
| `path` | string | no | Write each artboard's image to disk (parent dirs created) and return a small result (path + dimensions) instead of base64. With multiple artboards, include a {name} or {index} placeholder (or an extension, before which -{index} is inserted); a single artboard is written to `path` verbatim. Omit to return base64 inline. |
| `quality` | integer | no | JPEG/WebP quality 1–100 (default: 90 for JPEG, 80 for WebP). Ignored for PNG. |
| `range` | array<integer> | no | Inclusive 1-based index range [start, end] (e.g. [2, 5] exports artboards 2–5). |
| `scale` | number | no | Output pixels per document unit (default 1.0). A 400×300 artboard at scale 2 exports 800×600 px. Clamped to 0.05–8.0. |
| `transparent` | boolean | no | Transparent background instead of the artboard fill (default false). Ignored by JPEG (always opaque). |

## `export_audit_log`

Export the retained in-memory MCP audit log as a JSON array (oldest first). Argument summaries are bounded, and the formatted response is capped at 256 KiB; when the retained buffer does not fit, only the oldest entries that fit are returned.

_No parameters._

## `export_design_tokens`

Extract the document's design vocabulary — unique solid fill colors, stroke colors, font families, font sizes, and stroke widths — and return them as structured design tokens.

Useful for generating a CSS variable sheet, a Tailwind theme extension, a Style Dictionary token file, or raw JSON for any downstream tooling. Only solid-color fills are tokenised; gradient fills are skipped (they don't map to a single value).

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `format` | enum (`json`, `css`, `tailwind`, `style-dictionary`) | no | Output format (default: json). css → :root { --color-1: … }, tailwind → theme.extend block, style-dictionary → W3C Design Token format with $type annotations. |

## `export_icon_set`

Batch-export a set of icons to normalized .svg files in a single call — the canonical icon-pipeline workflow. Given a list of {name, node_ids} entries (or, if omitted, every top-level group in the document named by its group name), each icon is exported with a uniform square viewBox (so the whole set renders at a consistent scale) and compact rounded path data. If `out_dir` is given the .svg files are written to disk (one per icon, filenames slugified + de-duplicated); otherwise each icon's SVG string is returned inline. Replaces the drop-out-to-a-Python-script post-pass for uniform icon sizing.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `icons` | array<object> | no | Icons to export. Omit to export every top-level group in the document. |
| `normalize` | enum (`tight`, `square`) | no | viewBox framing (default: 'square' for uniform icon sizing). |
| `out_dir` | string | no | Directory to write the .svg files into. If omitted, SVGs are returned inline instead of written to disk. |
| `pad` | number | no | Padding fraction for square normalization (default: 0.1). |
| `precision` | integer | no | Decimal places for coordinates and path data (default: 4). |

## `export_pdf`

Export the document as a vector PDF, at its physical size for the document DPI. Returns the PDF bytes as base64 in `data_base64`; also writes to `path` when given.

Vector paths with solid colours, transforms and nesting (gradients approximate to their first stop). With `color_mode: "cmyk"` the output is a print-ready PDF/X-1a:2001 file: DeviceCMYK via an ICC profile, embedded GTS_PDFX OutputIntent, PDF 1.3, and MediaBox/TrimBox/BleedBox from the document bleed. `outline_text` converts every glyph to a vector path (zero font dependencies); `marks` renders trim + registration marks.

PER-ARTBOARD / MULTI-PAGE: set `all`, `range`, or `artboards` to emit one page per artboard, each clipped to its rectangle + bleed with its own page boxes and marks — a single multi-page PDF by default, or one file per artboard with `separate_files: true` (needs a `path` template). This is the native card-batch path; without any selector the whole canvas is one page. Note: documents that use layer opacity/blend still emit transparency (not yet flattened for strict X-1a).

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `all` | boolean | no | Export every artboard, one page each (overrides range/artboards). |
| `artboards` | array<string> | no | Per-artboard export: specific artboards, each a UUID, an exact name, or a 1-based index string ("1", "2"). One clipped page per artboard, in this order. Get ids/names from list_artboards. |
| `background` | string | no | Optional page background colour, e.g. "#ffffff". Omit for an unpainted (white-in-viewers) page. |
| `color_mode` | enum (`rgb`, `cmyk`) | no | Export colour model. "rgb" (default) preserves on-screen colours. "cmyk" separates to DeviceCMYK via the ICC profile and emits a PDF/X-1a file. When omitted, the document's own color_mode is used. |
| `marks` | boolean | no | Render trim and registration marks in the bleed/slug area. Default false. |
| `outline_text` | boolean | no | Convert text nodes to vector outlines (zero font dependencies). Default false. |
| `path` | string | no | Also write the exported PDF to this filesystem path. The base64 payload is still returned. |
| `profile` | string | no | Path to a CMYK ICC profile for colour conversion and the OutputIntent (defaults to the bundled Coated FOGRA39 when color_mode is cmyk). |
| `range` | array<integer> | no | Export a 1-based inclusive artboard index range [start, end], one page each. |
| `separate_files` | boolean | no | With a multi-artboard selection, write one file PER artboard instead of a single multi-page PDF. Requires `path` with a {name} or {index} placeholder (or a filename, before whose extension -{index} is inserted). |

## `export_raster`

Export the current canvas as a raster image (PNG, JPEG, WebP, GIF, or TIFF). Returns the image data as a base64-encoded string, OR — when `path` is given — writes the image to disk and returns only a small result (path + dimensions), keeping full-resolution PNGs off the socket.

PNG is lossless with optional transparency. JPEG is lossy with configurable quality (1–100) and always has a white background. WebP is lossy with transparency support and configurable quality. TIFF is lossless with full RGBA support, suitable for print workflows. Use this to obtain a file-ready raster export without the GUI file menu.

Optionally specify width/height to resize the output. Each supplied dimension is limited to 16384 pixels per side and a paired resize is limited to 67108864 pixels total. If omitted, the capture uses the current canvas dimensions.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `format` | enum (`png`, `jpeg`, `webp`, `gif`, `tiff`) | no | Output format (default: png) |
| `height` | integer | no | Output height in pixels (maximum 16384; paired resize maximum 67108864 pixels). Omit to use current canvas height. |
| `path` | string | no | Write the encoded image to this filesystem path (parent dirs created) and return a small result (path + width/height) instead of base64. Omit to return base64 inline. Use this for full-resolution PNGs, whose base64 is too large for the MCP socket. |
| `quality` | integer | no | JPEG/WebP quality 1–100 (default: 90 for JPEG, 80 for WebP). Ignored for PNG. |
| `width` | integer | no | Output width in pixels (maximum 16384; paired resize maximum 67108864 pixels). Omit to use current canvas width. |

## `export_selection_as_svg`

Export specific nodes (or the current selection) as a clean, minimal SVG. Each node's name is slugified into the SVG id attribute, making the output immediately pasteable into HTML or React. Identical gradient/pattern paints are deduplicated into a single shared <defs> entry. Coordinates and path data are rounded to `precision` decimals (default 4) so exports stay compact. Use `normalize: "square"` to frame the icon in a uniform centered square viewBox (so a set of icons all export at the same aspect ratio and scale). Optionally wrap the output in a React functional component.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `as_react_component` | boolean | no | Wrap the SVG in a TypeScript React functional component (default: false) |
| `component_name` | string | no | Component name when as_react_component is true (default: 'SvgIcon') |
| `node_ids` | array<string> | no | Node IDs to export. If omitted or empty, uses the current document selection. |
| `normalize` | enum (`tight`, `square`) | no | viewBox framing. 'tight' (default) = union bounding box; 'square' = uniform centered square so a set of icons frames identically. |
| `pad` | number | no | Padding as a fraction of the square side when normalize='square' (e.g. 0.1 = 10%). Default 0. |
| `precision` | integer | no | Decimal places for coordinates and path data (default: 4). Prevents 15-decimal path bloat. |

## `export_svg`

Export the entire document as an SVG string. Returns the raw SVG markup that can be saved as a .svg file or pasted directly into any SVG-aware tool.

The output starts with <!-- photonic-svg-v1 --> for pipeline stability. By default, every node and layer element receives an id attribute derived from its name (slugified, deduplicated), making the SVG immediately usable in CSS, JavaScript, and developer handoff.

Use this to:
- Verify exactly what the canvas looks like as markup after a sequence of drawing operations
- Get export-ready SVG without using the GUI file menu
- Inspect gradient definitions, path data, transforms, and layer structure as emitted XML

The returned SVG reflects all visible layers in draw order with correct transforms. Hidden layers and nodes with visible=false are omitted.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `inner_only` | boolean | no | Return only the inner SVG body without the outer <svg> wrapper (default: false) |
| `precision` | integer | no | Decimal places for SVG dimension and viewBox values, clamped 1–6 (default: 4). Use 2 for smaller output, 6 for maximum fidelity. |
| `semantic_ids` | boolean | no | Emit slugified node/layer names as id attributes (default: true). Set to false to suppress id attributes on all elements. |

## `export_tagged_assets`

Export all nodes tagged via tag_node_for_export. SVG assets are returned inline; raster assets return metadata (name, node_id, scale) for use with export_raster.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `filter` | string | no | Only export assets whose name contains this string. |

## `find_nodes`

Query nodes by tag, name, type, layer, visibility, or world-space region. All filters are optional and combine with AND. Empty call returns all nodes up to limit. Results are unordered. 'count' = nodes returned; check 'truncated' if limit was reached.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `in_region` | object | no | World AABB filter. Path nodes whose transformed bounding box intersects this rect are included. Groups/text always pass. |
| `include_details` | boolean | no | Return full node JSON (default: false = minimal {id,name,type,tags,layer_id,visible}) |
| `layer_id` | string | no | Restrict to this layer UUID |
| `limit` | integer | no | Max results (default: 200) |
| `name_contains` | string | no | Case-insensitive substring match on node name |
| `node_type` | enum (`path`, `group`, `text`) | no | Filter by node type |
| `tags` | array<string> | no | Node must have ALL these tags |
| `tags_any` | array<string> | no | Node must have ANY of these tags |
| `visible_only` | boolean | no | Exclude invisible nodes (default: false) |

## `find_replace_style`

Search every node for a matching fill or stroke color and replace those colors — and optionally node-level opacity — in a single undoable batch.

This is the 'Find & Replace' for color. Instead of calling get_document_state → iterating nodes → calling update_node for each match (N round-trips, N undo steps), a single find_replace_style call handles the entire document atomically.

Typical use cases:
- Brand refresh: swap old brand color for new across the whole file in one call
- Design audit: dry_run=true to see every node using a given color before committing
- Bulk opacity change: set all red fills to 50% opacity
- Near-match cleanup: use color_tolerance=0.05 to catch slightly off-brand colors

Gradient support: matching checks solid fills AND individual stop colors inside linear, radial, fluid, and mesh gradients. Only matching stops are replaced; others are untouched.

Requires at least one search criterion (fill_color or stroke_color) and at least one replacement (new_fill_color, new_stroke_color, or new_opacity). Returns the list of changed nodes and exactly what changed on each.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `color_tolerance` | number | no | Match threshold. 0.0 = exact match (default). 0.05 = visually near-identical. 1.0 = any color. Normalized Euclidean distance in linear RGB. |
| `dry_run` | boolean | no | When true, return what would change without mutating the document. Use before large batch operations to confirm scope. Default: false. |
| `fill_color` | string | no | Hex color to search for in fills — solid color or any gradient stop. e.g. '#FF0000' |
| `layer_id` | string | no | Restrict the search to nodes on this layer UUID. Omit to search the entire document. |
| `new_fill_color` | string | no | Replace every matched fill color (solid or gradient stop) with this hex color. |
| `new_opacity` | number | no | Set node-level opacity to this value for every matched node. |
| `new_stroke_color` | string | no | Replace every matched stroke color with this hex color. |
| `node_ids` | array<string> | no | Restrict the search to these specific node IDs. Useful for scoped updates without touching the rest of the file. |
| `stroke_color` | string | no | Hex color to search for in enabled strokes. e.g. '#000000' |

## `find_replace_text`

Search and replace text content across text nodes. Supports plain-string and regular-expression matching with optional case sensitivity. Use dry_run: true to preview matches without applying changes. Returns a list of changed nodes with their old and new content.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `find` | string | yes | Text to search for. Plain string by default; treated as a regex when regex: true. |
| `replace` | string | yes | Replacement string. When regex: true, capture group back-references ($1, $2, …) are supported. |
| `case_sensitive` | boolean | no | Case-sensitive match. Default: true. |
| `dry_run` | boolean | no | Preview matches without applying changes. Default: false. |
| `node_ids` | array<string> | no | Scope to specific text node UUIDs. Omit to search all text nodes in the document. |
| `regex` | boolean | no | Treat find as a regular expression. Default: false. |

## `fit_to_canvas`

Scale and center artwork to fit within the canvas bounds. Applies a uniform scale (never scales up) and centers the result. Useful after importing SVGs or when artwork extends beyond the artboard.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | no | Nodes to fit. Empty = all. |
| `padding` | number | no | Padding around edges (default: 10) |

## `fit_to_margins`

Scale and position nodes to fill the artboard safe area (artboard bounds minus the set margins). By default preserves aspect ratio (uniform=true) and centers content in the safe area. Requires margins to be set with set_artboard_margins. GUI: 'Fit to Margins' button in the Artboard Margins panel (visible when selection exists and any margin > 0).

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | no | Node UUIDs or names to fit. Omit to fit all visible nodes. |
| `padding` | number | no | Additional inset inside the margin rectangle in document units. Default: 0. |
| `uniform` | boolean | no | Preserve aspect ratio while scaling. Default: true. |

## `flatten_artwork`

Merge all layers in the document into one. The bottom-most layer becomes the target; all other layers are dissolved into it and removed. No-ops on a single-layer document. Optional target_name renames the surviving layer. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `target_name` | string | no | Optional new name for the surviving layer. Defaults to the bottom-most layer's existing name. |

## `flatten_group`

Recursively ungroup all nested groups into flat nodes on the parent layer. Unlike ungroup_nodes (single level), this flattens the entire group hierarchy. Useful for simplifying complex imported SVGs.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | no | Group node IDs. Empty = use selection. |

## `flatten_transparency`

Bake node opacity and fill/stroke opacity into color alpha values for print-ready output. After flattening, all processed nodes have opacity=1.0 with colors premultiplied. Group opacity is not baked (children are processed individually). Irreversible — uses a single undoable batch command.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | no | Node UUIDs or names to process. Defaults to all nodes in the document. |

## `flip_nodes`

Flip/mirror nodes horizontally or vertically around their bounding box center. Paths are flipped geometrically; text and groups are flipped via transform scale.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `axis` | enum (`horizontal`, `vertical`) | yes | Flip axis |
| `node_ids` | array<string> | no | Node IDs. Empty = use selection. |

## `get_artboard_margins`

Return the current artboard safe-area margin values (top, right, bottom, left in document units). Read-only.

_No parameters._

## `get_canvas_overview`

Return a compact spatial map of all visible nodes: bounding box, layer, kind, and fill color for each node, plus the overall canvas bounds. Faster than get_document_state for layout queries. Useful for AI agents to understand spatial composition before placing or adjusting elements.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `include_hidden` | boolean | no | When true, include hidden nodes in the overview. Default: false. |

## `get_clipboard_history`

Return a summary of all entries currently in the clipboard ring.

Each entry shows its index (0 = most recent), id, label, the number of root nodes copied, and the timestamp. Use the index with `paste_from_history` to paste a specific entry.

_No parameters._

## `get_css_preview`

Return the CSS equivalent of a node's visual properties for developer handoff. Shows background/color, outline (stroke), opacity, mix-blend-mode, transform, and — for text nodes — font-family, font-size, font-weight, and text-align. Width and height are derived from the node's world bounding box. Read-only — does not modify the document.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `id` | string | no | Node UUID or name. If omitted, the first node in document order is used. |

## `get_document_bleed`

Return the current document bleed and slug values in millimetres. Read-only.

_No parameters._

## `get_document_color_mode`

Return the current document color mode ('rgb' or 'cmyk'). Read-only.

_No parameters._

## `get_document_dpi`

Return the current document DPI and the physical page size (pt and inches) it implies for export. Read-only.

_No parameters._

## `get_document_info`

Get a compact summary of the document: canvas dimensions, layer list (name, visibility, node count, template status), node counts by kind (path/text/group), unique font names, and unique solid fill colors. Faster than get_document_state for overview queries.

_No parameters._

## `get_document_state`

Get the full document tree: layers, nodes, styles, transforms

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `include_path_data` | boolean | no |  |
| `layer_id` | string | no |  |

## `get_document_template`

Capture the current document as a reusable template — preserving canvas size, layer structure, guides, and export profiles while stripping all node content. Use the returned template_json with apply_document_template to stamp these settings onto a different document.

_No parameters._

## `get_node`

Get full details of a node by ID or name

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | no |  |
| `node_id` | string | no |  |

## `get_node_prompts`

Return the full prompt history for a node — the chronological list of AI prompts that created or modified it. Returns empty history message if none recorded.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | UUID or name of the node. |

## `get_opentype_features`

Return the active OpenType feature tags on a text node. Read-only.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | Text node ID or name. |

## `get_raster_info`

Read-only: report a raster node's dimensions, whether it has a layer mask, its source file, and a 16-bucket luma histogram.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes |  |

## `get_recent_colors`

Return the list of recently used fill and stroke colors for this document, ordered most-recently-used first (up to 20 entries). Useful for quickly re-applying a palette or building color suggestions.

_No parameters._

## `get_selection`

Return the current selection — list of selected node IDs with name, kind, visibility, and lock state. Read-only.

_No parameters._

## `gradient_fill`

Fill a raster node with a linear gradient from (x0,y0) to (x1,y1), optionally confined to a selection.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `color0` | any | yes | Start color: hex or [r,g,b,a] |
| `color1` | any | yes | End color: hex or [r,g,b,a] |
| `node_id` | string | yes |  |
| `x0` | number | yes |  |
| `x1` | number | yes |  |
| `y0` | number | yes |  |
| `y1` | number | yes |  |
| `selection` | object | no |  |

## `group_nodes`

Group two or more nodes into a single group node. All nodes must belong to the same layer. The group is inserted at the z-position of the bottom-most child.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | IDs of nodes to group |
| `layer_id` | string | no | Optional layer override |
| `name` | string | no | Name for the new group (default: 'Group') |

## `hatch_fill`

Fill a path shape with parallel hatching lines clipped to the path boundary. Supports single-direction hatching or cross-hatching (two angles).

Useful for engraving style, technical drawing shading, woodcut effects, and decorative fills. Lines are created as a separate stroke-only path on the same layer.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Path node IDs to fill with hatching |
| `angle` | number | no | Angle of hatch lines in degrees (default: 45) |
| `color` | string | no | Line color hex (default: uses path fill color) |
| `cross_angle` | number | no | Second angle for cross-hatching. Omit for single-direction. |
| `spacing` | number | no | Spacing between lines (default: 5) |
| `stroke_width` | number | no | Line width (default: 1) |

## `import_design_tokens`

Counterpart to export_design_tokens: register named color swatches from a design-tokens payload so brand colors can be referenced by name (via apply_color_swatch) instead of hard-coded into every fill/gradient. Accepts CSS custom properties (:root { --brand-primary: #2f56cf }), flat or nested JSON, and Style Dictionary ({ "value": "#hex" }) — auto-detected. Hex values are normalized; existing swatches with the same name are updated. When the brand palette shifts, re-importing re-themes everything referencing those swatches.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `clear_existing` | boolean | no | Clear all existing swatches before importing (default: false). |
| `content` | string | no | Inline tokens text (CSS, JSON, or style-dictionary). Provide this OR `path`. |
| `format` | enum (`auto`, `css`, `json`) | no | Parse hint (default: auto — CSS if it doesn't start with { or [, else JSON). |
| `path` | string | no | Path to a tokens file to read. Provide this OR `content`. |
| `prefix` | string | no | Optional prefix prepended to every imported swatch name (e.g. 'brand/'). |

## `inspect_node`

Return computed geometry and structure metrics for a single node — values that go beyond what get_node provides.

**Path nodes:** world-space and local bounding box, perimeter length, enclosed area, centroid (world-space center of bounding box), and anchor-point count.

**Group nodes:** direct child count, total descendant count, sum of all anchor points across descendant paths, and sorted lists of unique solid fill and stroke colors (hex strings) used anywhere in the group hierarchy.

**Text nodes:** line count (split by newlines), character count, font family, font size, and font weight.

All node types include a `world_bounds` object with `x`, `y`, `width`, and `height` in document (world) space. Returns an error if no node matches the provided ID or name.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `id` | string | yes | Node ID (UUID string) or node name. UUID is matched first; falls back to name search if parsing fails. |

## `invert_colors`

Invert all color values (fill and stroke) on selected path nodes. Each RGB channel becomes (1 − value); alpha is preserved. Works on solid fills, linear/radial gradient stops, fluid gradient points, and mesh gradient vertices. If node_ids is omitted, all path nodes in the document are inverted. Single undo step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | no | UUIDs of path nodes to invert. Omit to invert all path nodes in the document. |

## `join_paths`

Close or join path nodes. With 1 node_id: appends ClosePath to every open subpath in the node (i.e. closes the path). With 2 node_ids: merges both paths into one by connecting their nearest open endpoints with a straight line segment; the result replaces the first node and the second node is deleted. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | 1 or 2 path node IDs. 1 = close open subpaths; 2 = join the two paths into one. |

## `jump_to_history`

Jump to a specific position in the document's edit history by undoing or redoing the required number of steps. index=0 is the empty-document state; index equal to the current undo_depth means no change. Values beyond the maximum (undo_depth + redo_depth) are clamped. Use list_history to see the current depth and available steps.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `index` | integer | yes | Target history depth. 0 = undo all; undo_depth() = current state. |

## `lasso_select`

Select all visible nodes whose bounding-box centroid (or any corner, in non-centroid mode) lies inside a closed polygon defined by canvas-space coordinates. Equivalent to the Lasso Selection tool. Useful for selecting nodes within an irregular region without needing their IDs.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `points` | array<array<number>> | yes | Polygon boundary in canvas (document) coordinates. Each element is [x, y]. Minimum 3 points. The polygon is automatically closed. |
| `additive` | boolean | no | When true, add to the existing selection rather than replacing it. Default false. |
| `centroid_mode` | boolean | no | When true (default), select nodes whose bounding-box centroid is inside the polygon. When false, select nodes with any AABB corner inside the polygon. |

## `layout_nodes`

Rearrange a set of existing nodes using a spatial layout algorithm — no manual coordinate math required.

Four layouts are available:
- `grid` — pack nodes into a uniform grid. Columns default to ceil(sqrt(N)); cell size defaults to the widest × tallest node. Nodes are centred inside their cell.
- `circle` — distribute nodes evenly around a circle at a given centre and radius.
- `stack_horizontal` — place nodes left-to-right with a gap, with optional cross-axis alignment (top / centre / bottom).
- `stack_vertical` — place nodes top-to-bottom with a gap, with optional cross-axis alignment (left / centre / right).

All layout origins default to the current top-left corner of the combined selection so the group stays in place unless you explicitly move it.

This complements `create_array` (which duplicates one node) and `align_nodes` (which distributes along a single axis). Use `layout_nodes` whenever you have N *existing* nodes that need 2-D spatial organisation.

Returns the number of nodes moved. The operation is a single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `layout` | enum (`grid`, `circle`, `stack_horizontal`, `stack_vertical`) | yes | Layout algorithm to apply. |
| `node_ids` | array<string> | yes | IDs of the nodes to rearrange. Order determines placement (left-to-right, top-to-bottom for grid/stack; clockwise from start_angle for circle). |
| `align` | enum (`start`, `center`, `end`) | no | (stack_horizontal / stack_vertical) Cross-axis alignment. For stack_horizontal: top/centre/bottom. For stack_vertical: left/centre/right. Default: start. |
| `cell_height` | number | no | (grid) Fixed cell height in pixels. Defaults to the tallest node's height. |
| `cell_width` | number | no | (grid) Fixed cell width in pixels. Defaults to the widest node's width. |
| `columns` | integer | no | (grid) Number of columns. Defaults to ceil(sqrt(N)). |
| `cx` | number | no | (circle) X of the circle centre. Defaults to the combined bounding-box centre. |
| `cy` | number | no | (circle) Y of the circle centre. Defaults to the combined bounding-box centre. |
| `gap` | number | no | (stack_horizontal / stack_vertical) Gap between successive nodes in pixels. Default: 20. |
| `gap_x` | number | no | (grid) Horizontal gap between cells in pixels. Default: 20. |
| `gap_y` | number | no | (grid) Vertical gap between cells in pixels. Default: 20. |
| `radius` | number | no | (circle) Radius in pixels. Default: 200. |
| `start_angle` | number | no | (circle) Angle in degrees for the first node, measured clockwise from the positive X axis. Default: 0 (rightmost point). |
| `x` | number | no | X coordinate of the layout origin. Defaults to the left edge of the current selection. |
| `y` | number | no | Y coordinate of the layout origin. Defaults to the top edge of the current selection. |

## `link_text_frames`

Link two text nodes as a threaded text chain so that content overflow from the upstream frame flows into the downstream frame. Both nodes must be text nodes. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `from_id` | string | yes | ID or name of the upstream text node (overflow flows out from here). |
| `to_id` | string | yes | ID or name of the downstream text node (overflow flows into here). |

## `liquify`

Liquify / distortion on a raster node. op ∈ {push(cx,cy,dx,dy,radius,strength — forward warp), twirl(cx,cy,radius,angle), pucker(cx,cy,radius,amount), bloat(cx,cy,radius,amount), pinch(amount), spherize(amount), ripple(amplitude,wavelength), perspective(corners:[[x,y]×4] TL,TR,BR,BL)}.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes |  |
| `op` | string | yes | Liquify/distort operation (see description) |
| `params` | object | no |  |
| `selection` | object | no |  |

## `list_actions`

List all named action sets in the document.

_No parameters._

## `list_annotations`

Return all annotations on the document, optionally filtered by node or resolved status.

By default only unresolved annotations are returned. Pass `include_resolved: true` to see the full history. Results are sorted by creation time (oldest first).

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `include_resolved` | boolean | no | When true, include annotations that have already been resolved. Default: false. |
| `node_id` | string | no | Filter to annotations attached to this specific node UUID. Omit to list all annotations. |

## `list_artboards`

List every artboard (named crop/export rectangle) in the document, with id, name, position (x, y = top-left), size (width, height), and which one is active. Artboards live in the shared document coordinate space; a fresh document has one 'Artboard 1' at the origin. Read-only. Use before update_artboard / remove_artboard / set_active_artboard to get ids. Distinct from the document canvas size (get_document_info) and from artboard safe-area margins (get_artboard_margins).

_No parameters._

## `list_audit_log`

Return the most recent MCP tool calls recorded since the server started.

Each entry includes: `id` (sequential), `timestamp` (ISO 8601), `tool_name`, `args` (a bounded structural summary), `result_summary` (first 200 chars of result text), `duration_ms`, and `is_error`. Responses are capped at a fixed byte budget.

Useful for multi-agent accountability: see what was called, by whom (if the calling agent passes an `author` in its args), and with which bounded parameters.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `limit` | integer | no | Maximum number of entries to return, newest first. Default: 50, maximum: 1000. |

## `list_character_styles`

List all named character styles saved in the document.

_No parameters._

## `list_checkpoints`

List all saved document checkpoints, including their IDs, names, and creation times. Use these IDs with diff_checkpoints or restore_checkpoint.

_No parameters._

## `list_color_swatches`

List all named color swatches saved in the document palette.

_No parameters._

## `list_constraints`

List all live property constraints with their current evaluated values.

_No parameters._

## `list_dimensions`

List all dimension annotations in the document, including their IDs, node references, axis, and measured distance. Read-only.

_No parameters._

## `list_event_triggers`

List all registered script event triggers in the document. Read-only.

_No parameters._

## `list_export_profiles`

List all named export profiles stored in the document.

_No parameters._

## `list_gradient_swatches`

List all named gradient swatches saved in the document.

_No parameters._

## `list_grammar_rules`

List all named design grammar rules in the document.

_No parameters._

## `list_graphic_styles`

List all named graphic styles saved in the document.

_No parameters._

## `list_guides`

List all ruler guides in the document with their orientation, position, lock state, and optional color. Read-only.

_No parameters._

## `list_history`

Return the most recent edit history entries from the undo stack, newest first. Useful for understanding what an AI agent has done to a document, auditing changes, or deciding which node to revert with `undo_node`. Read-only.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `limit` | integer | no | Maximum entries to return. Default: 20. |

## `list_paragraph_styles`

List all named paragraph styles saved in the document.

_No parameters._

## `list_patterns`

List all named patterns in the document pattern registry, with their tile dimensions and transform.

_No parameters._

## `list_spot_colors`

List all named spot colors defined in the document.

_No parameters._

## `list_symbols`

List all named symbols defined in the document, including master node names and IDs.

_No parameters._

## `list_variables`

List all named document variables and their current values.

_No parameters._

## `list_width_profiles`

List all named variable-width stroke profiles in the document.

_No parameters._

## `list_workspaces`

List all saved workspace presets in the document. Read-only.

_No parameters._

## `load_swatch_library`

Load a predefined color swatch library into the document. Available libraries: web (16 named HTML colors), material (16 Material Design 500 tones), pastels (12 soft pastel shades), earth_tones (12 warm earthy tones), neon (12 bright neon colors), grayscale (11-step neutral ramp). Skips swatches already present by name. Set clear_existing=true to replace all existing swatches first.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `library` | enum (`web`, `material`, `pastels`, `earth_tones`, `neon`, `grayscale`) | yes | Library name to load. |
| `clear_existing` | boolean | no | Remove all existing swatches before loading. Default false (append). |

## `load_symbol_library`

Load a built-in symbol library into the document. Each library adds a set of named symbols (as hidden off-canvas master nodes) ready to be placed with place_symbol or spray_symbol_instances. Available libraries: 'arrows' (6 directional arrows), 'shapes' (diamond, hexagon, pentagon, star-5pt, cross, checkmark), 'ui' (checkbox-empty, checkbox-checked, radio-empty, close-x, menu-lines, plus-icon). Skips symbols that are already defined. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `library_name` | enum (`arrows`, `shapes`, `ui`) | yes | Name of the built-in library to load. |

## `load_workspace`

Load a saved workspace preset, returning its search_query for the GUI to apply. Read-only (does not mutate document state).

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the workspace to load. |

## `magic_wand_select`

Click at a canvas coordinate to select the topmost node at that point, then expand the selection to all nodes sharing the specified attribute (fill color, stroke color, stroke weight, opacity, blend mode, or object type). Equivalent to the Magic Wand tool in vector editors.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `canvas_x` | number | yes | X coordinate in document (canvas) space to click. |
| `canvas_y` | number | yes | Y coordinate in document (canvas) space to click. |
| `attribute` | enum (`fill_color`, `stroke_color`, `stroke_weight`, `opacity`, `blend_mode`, `object_type`) | no | Which attribute to match. Defaults to fill_color. |
| `tolerance` | number | no | How close two values must be to count as matching. For colors: Euclidean RGBA distance in [0,1] space. For stroke weight / opacity: absolute difference. Ignored for blend_mode and object_type. Defaults to 0.01. |

## `make_clipping_mask`

Create a clipping mask on a group node. The topmost child (last in the group's child order) becomes the clip path; all other children are masked to that shape. The clip path node is preserved in the group but rendered only as a mask. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `group_id` | string | yes | Group node ID (UUID or name). Must contain at least 2 children. |

## `make_compound_path`

Combine two or more path nodes into a single compound path. Overlapping subpaths create holes via the even-odd fill rule (like Illustrator's Object > Compound Path > Make). The bottommost selected node's fill, stroke, and position are kept; all other source nodes are removed. All transforms are baked before merging. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | IDs of the path nodes to combine. Must be at least 2 top-level path nodes. |
| `name` | string | no | Optional name for the resulting compound path node. |

## `make_live_boolean`

Group two or more path nodes into a NON-DESTRUCTIVE live boolean. Unlike boolean_operation / make_compound_path, the operands stay individually editable: the group renders and exports as the single resolved path (styled by the bottom child), and editing any operand re-computes the boolean live. Operators: union, intersect, subtract, exclude, divide. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | IDs of the path nodes to combine (at least 2 top-level path nodes). |
| `operation` | enum (`union`, `intersect`, `subtract`, `exclude`, `divide`) | yes | The boolean operator to apply across the operands. |
| `name` | string | no | Optional name for the live-boolean group. |

## `measure_distance`

Measure the distance between two points or two nodes. Returns distance, delta X/Y, and angle.

Each target can be an [x, y] coordinate pair or a node UUID/name (uses the node's bounding box center).

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `from` | any | yes | Start: [x, y] array or node ID string |
| `to` | any | yes | End: [x, y] array or node ID string |

## `measure_distances`

Measure edge-to-edge gaps, center-to-center distances, and alignment offsets between two or more nodes. For ≤6 nodes reports all pairs; for larger sets reports consecutive pairs. Read-only. Useful for verifying layout spacing.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | UUIDs or names of at least 2 nodes to measure between. |

## `measure_nodes`

Return the world-space bounding box and center of each node after applying its transform. Also returns the combined bounding box of all specified nodes. When exactly two nodes are provided, includes pairwise center-to-center distance and angle (0° = right, 90° = down).

Use this whenever you need to know WHERE something actually is on canvas — e.g. before placing a new element next to an existing one, checking alignment, or computing spacing. Groups and text nodes return null bounds; use path nodes or pass children individually.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | IDs of the nodes to measure |

## `measure_path`

Measure a path's total arc length, anchor count, segment count, bounding box, and open/closed status. Read-only — does not modify the document.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | Path node UUID or name |

## `merge_layers`

Merge two or more layers into one. All nodes from source layers are moved into the target layer (the first layer among those selected in document stack order). Empty source layers are then removed. Optional target_name renames the surviving layer. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `layer_ids` | array<string> | yes | IDs of the layers to merge. The bottom-most layer in document order becomes the target; all others are merged into it and removed. |
| `target_name` | string | no | Optional new name for the surviving layer. Defaults to its existing name. |

## `mirror_copy`

Duplicate each selected node and flip the copy across its bounding-box center, producing a mirrored twin. The original is unchanged; the new copy is added to the same layer and can be repositioned freely. Uses current selection when node_ids is empty.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `axis` | enum (`horizontal`, `vertical`) | no | "horizontal" flips left-right (default); "vertical" flips top-bottom. |
| `node_ids` | array<string> | no | UUIDs or names of nodes to mirror. Uses current selection if empty. |

## `move_artboard`

Move an artboard together with all content/nodes inside it by (dx, dy), as one undoable step. Unlike update_artboard, which repositions only the frame, this tool also moves the artboard's contents. Get the artboard_id from list_artboards.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `artboard_id` | string | yes | UUID of the artboard to move (from list_artboards). |
| `dx` | number | yes | Horizontal movement in document units. |
| `dy` | number | yes | Vertical movement in document units. |

## `move_to_layer`

Move nodes to a different layer. Nodes are appended to the top of the target layer's z-order. Undoable.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `target_layer` | string | yes | Target layer UUID or name |
| `node_ids` | array<string> | no | Node IDs. Empty = use selection. |

## `noise_deform`

Apply smooth sinusoidal displacement to all anchor and control points in the selected path nodes, producing organic wave-like deformation. Uses two-octave sinusoidal noise — unlike roughen_path (random per-point jitter), noise_deform produces flowing, rhythmic distortion.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | UUIDs or names of path nodes to deform. |
| `amplitude` | number | no | Maximum displacement in document units (default: 8.0). |
| `axis` | enum (`both`, `x`, `y`) | no | Which axis to deform (default: "both"). |
| `frequency` | number | no | Spatial frequency in cycles/px — higher = tighter waves (default: 0.05). |
| `seed` | number | no | Phase offset seed to shift the wave pattern (default: 0.0). |

## `offset_path`

Create a parallel copy of one or more paths inset or outset by a fixed distance. Positive distance expands the path outward (outset); negative distance contracts it inward (inset). By default a new offset node is added above the original (create_copy: true); set create_copy to false to replace the original in place. Corner style is configurable. Non-path nodes are silently skipped. Single undoable step per call.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `distance` | number | yes | Offset distance in document units. Positive = outset (expand outward), negative = inset (contract inward). |
| `node_ids` | array<string> | yes | UUIDs of path nodes to offset |
| `create_copy` | boolean | no | If true (default), add the offset result as a new node above the original. If false, replace the original node with the offset result. |
| `join_style` | enum (`miter`, `round`, `bevel`) | no | Corner join style for the offset path. Default: miter. |

## `outline_stroke`

Convert the stroke on each selected path node into a new filled closed path that traces the stroke outline (center-aligned). The new node inherits the stroke color and opacity as its solid fill; its stroke is disabled. The original node's stroke is disabled. Useful for turning hairline strokes into editable geometry for boolean operations, export, or further styling. Dash patterns are ignored — the solid stroke shape is always outlined. Single undoable step per call.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | UUIDs of path nodes to outline. Each must be a path node with an enabled stroke. |
| `keep_original` | boolean | no | Unused — reserved for future use. The original node is always retained with its stroke disabled. Default: false. |

## `paste_from_history`

Paste nodes from a clipboard history entry into the document.

All pasted nodes receive fresh UUIDs — the original clipboard snapshot is preserved and can be pasted again. The paste is a single undoable step.

An optional pixel offset shifts the pasted nodes relative to their original positions; useful when pasting multiple times to avoid exact overlap.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `index` | integer | yes | Zero-based index into the clipboard ring (0 = most recently copied). |
| `layer_id` | string | no | Target layer UUID. Defaults to the document's active layer. |
| `offset_x` | number | no | Horizontal offset in pixels applied to pasted nodes. Default: 0. |
| `offset_y` | number | no | Vertical offset in pixels applied to pasted nodes. Default: 0. |

## `pathfinder_crop`

Clip all selected path nodes to the boundary of the frontmost selected node (highest z-order). Each back node is replaced by the intersection of its path with the frontmost path; the frontmost node is then removed. Useful for masking artwork to a crop shape without creating a clipping mask. All node transforms are baked into path coordinates before the operation. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Two or more path node IDs. The frontmost (highest z-order) is the crop boundary. |

## `pathfinder_divide`

Divide two overlapping path nodes at every edge where they intersect, producing up to three distinct colored face nodes: the region only in the back shape, the overlapping region, and the region only in the front shape. Both originals are removed and replaced by the face nodes. Transforms are baked before the operation. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Exactly two path node IDs: [back_node_id, front_node_id] |
| `layer_id` | string | no | Layer for result nodes (default: back node's layer) |

## `pathfinder_merge`

Trim all selected path nodes of hidden areas, then merge (union) any nodes that share the same solid fill color into a single combined shape (Illustrator's Merge). Like Trim, each node has the regions covered by nodes above it subtracted; unlike Trim, nodes with matching solid fill colors are then unioned together. Non-solid fills remain separate. Original nodes are replaced by the merged result nodes. Strokes are disabled on all results. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Two or more path node IDs (z-order resolved automatically) |
| `layer_id` | string | no | Layer for result nodes (default: backmost source node's layer) |

## `pathfinder_minus_back`

Subtract all back nodes from the frontmost selected path node (Illustrator's Minus Back). The frontmost node (highest z-order) has each back node's shape subtracted from it in sequence; the back nodes are removed. The frontmost node's fill/stroke style is preserved. All node transforms are baked before the operation. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Two or more path node IDs. All nodes except the frontmost are subtracted from the frontmost. |

## `pathfinder_minus_front`

Subtract the frontmost selected path from every back node (Illustrator's Minus Front). The frontmost node (highest z-order) punches a hole in each back node; each back node is updated with back_path − front_path. The frontmost node is removed. Each back node's fill/stroke style is preserved. All transforms baked before the operation. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Two or more path node IDs. The frontmost is the cutter; all others have the front subtracted from them. |

## `pathfinder_outline`

Convert selected filled path nodes to stroked outlines (Illustrator's Outline). For each node: the solid fill color is moved to the stroke and the fill is set to none. Gradient fills fall back to black. Existing stroke width is preserved (or defaults to 1 pt). Path data is unchanged. Non-path nodes are silently skipped. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | One or more path node IDs to convert to outlines. |

## `pathfinder_trim`

Remove hidden portions of each selected path node by subtracting all paths above it in z-order (Illustrator's Trim). Nodes are processed back-to-front; each node's path becomes its_path − union(all_paths_above). Strokes are disabled on all result nodes; fills are preserved. No nodes are removed. All transforms are baked before the operation. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Two or more path node IDs to trim. |

## `pin_object_guides`

Create persistent ruler guides at the edges and/or center of selected nodes. Guides remain visible across editing sessions and serve as precision alignment references. Deduplicates — existing guides within 0.5 px are not duplicated.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `edges` | string | no | Which edges to pin: "all" (default), "center" (center_h + center_v), "edges" (top+bottom+left+right), or comma-separated from: top, bottom, left, right, center_h, center_v. |
| `node_ids` | array<string> | no | UUIDs or names of nodes. Uses current selection if empty. |

## `place_image`

Place a raster (pixel) image as a new layer — the Photoshop-style bitmap import. Supply either `path` (a file on disk: PNG/JPEG/WebP/…) or `data_base64` (base64-encoded image bytes). The image becomes a Raster node positioned at (x,y); edit it afterward with apply_adjustment / apply_filter / brush_stroke / transform_image.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `data_base64` | string | no | Base64-encoded image bytes (alternative to path) |
| `layer_id` | string | no |  |
| `name` | string | no |  |
| `path` | string | no | Path to an image file on disk |
| `x` | number | no | X position in document space (default 0) |
| `y` | number | no | Y position in document space (default 0) |

## `place_symbol`

Place an instance of a named symbol at the given position. The instance is a clone of the master node with a symbol_ref tag. Edit the master to see design intent; use break_link_to_symbol to detach an instance for independent editing.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `symbol_name` | string | yes | Symbol name to instantiate. |
| `x` | number | no | X position (document units). Default: 0. |
| `y` | number | no | Y position (document units). Default: 0. |

## `play_action`

Play a named action set, executing each recorded step in order. Optional substitutions replace node IDs or names from the recording with new values for the current run. Stops at first error.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the action set to play. |
| `substitutions` | object | no | Optional map of recorded node UUID/name → current node UUID/name. |

## `point_on_path`

Sample one or more points along a path at specified fractions (0.0 = start, 1.0 = end). Returns the (x, y) coordinates and tangent angle at each position.

Useful for:
- Positioning elements at precise locations along curves
- Computing tangent directions for text-on-path or object alignment
- Measuring intermediate distances along a path

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | Path node UUID or name |
| `t` | array<number> | yes | Position fractions along the path (0.0–1.0). Single value or array. |

## `preview_selection`

Render the selection (or given nodes) at target display sizes over light AND dark backgrounds, returned as a single contact-sheet PNG (base64 in data.data_base64). Icons live or die at their real size and against their real surface — this closes that loop without round-tripping through an app. Rows = backgrounds, columns = sizes; each icon is centered in a square cell (aspect ratio preserved). Only the selected nodes are drawn (others hidden), so neighbors don't bleed in.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `backgrounds` | array<string> | no | Background swatch hex colors to composite over (default: ["#ffffff", "#0b0b12"]). |
| `node_ids` | array<string> | no | Nodes to preview. If omitted or empty, uses the current selection. |
| `pad` | number | no | Padding as a fraction of the icon's square size (default: 0.15). |
| `sizes` | array<integer> | no | Target pixel sizes (default: [24, 32, 48]). |

## `proportional_move_anchor`

Move a path's anchor point(s) and pull nearby anchors along a falloff — Blender-style proportional editing on real vector anchors (topology is preserved; only positions change). The primary anchors move by the full (dx, dy); every other anchor within `spread` follows by a weighted fraction shaped by the falloff `curve`. Coordinates and `spread` are in the path's local space, matching the anchor positions from inspect_node. Destructive to geometry; single undoable step.

Use anchor element indices from inspect_node / get_node. Great for organically reshaping, tapering, or bulging a run of points without dragging each one by hand.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `anchor_indices` | array<integer> | yes | Element indices of the primary anchor(s) to move directly (weight 1.0), from inspect_node/get_node |
| `dx` | number | yes | Primary displacement in the path's local X (path units) |
| `dy` | number | yes | Primary displacement in the path's local Y (path units) |
| `node_id` | string | yes | Path node ID (UUID or name) to edit |
| `curve` | number | no | Falloff curve exponent: 1 = linear, >1 = sharp, <1 = soft/broad. Default: 2 |
| `spread` | number | no | Falloff radius in path units — neighbours within this distance follow proportionally. Default: 120 |

## `pucker_bloat`

Distort path nodes by displacing all anchor and control points radially from a center point.

Positive strength = bloat (expand outward, like inflating). Negative strength = pucker (contract inward, like pulling toward center). Strength of 0.5 expands each point 50% further from center; -0.5 pulls each point 50% closer.

Center defaults to the path's centroid. Useful for organic deformations, icon styling, and decorative effects. Destructive — modifies path data. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Path node IDs to distort |
| `center_x` | number | no | X coordinate of distortion center (default: path centroid) |
| `center_y` | number | no | Y coordinate of distortion center (default: path centroid) |
| `strength` | number | no | Distortion strength: positive = bloat, negative = pucker (default: 0.5) |

## `randomize_colors`

Assign random colors to selected nodes from a palette. If no palette provided, generates random vibrant colors. Useful for color exploration, generative art, and rapid prototyping.

Different from recolor_artwork (which maps existing colors to nearest palette match). randomize_colors assigns completely random picks from the palette.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `fill` | boolean | no | Randomize fills (default: true) |
| `node_ids` | array<string> | no | Node IDs. Empty = use selection. |
| `palette` | array<string> | no | Color palette as hex strings. Empty = auto-generate. |
| `seed` | integer | no | Random seed (default: 42) |
| `stroke` | boolean | no | Randomize strokes (default: false) |

## `recolor_artwork`

Map every unique solid fill in the selected nodes to the nearest color in a target palette (Euclidean RGB distance). Useful for applying brand palettes or reducing color count. Gradient fills are skipped. Single undoable batch step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `palette` | array<string> | yes | Target palette as hex strings, e.g. ["#FF0000","#00FF00","#0000FF"]. Each node's fill is replaced with the closest palette color. |
| `node_ids` | array<string> | no | IDs of nodes to recolor. If empty, all path nodes in the document are processed. |

## `redo`

Redo previously undone operation(s)

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `steps` | integer | no |  |

## `register_event_trigger`

Register a script event trigger: map a document lifecycle event to a named action that executes automatically when the event fires. Valid events: on_open, on_save, on_node_create, on_selection_change. The named action must already exist.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `action_name` | string | yes | Name of the action set to execute when the event fires. |
| `event` | enum (`on_open`, `on_save`, `on_node_create`, `on_selection_change`) | yes | Document lifecycle event to listen for. |

## `release_clipping_mask`

Release the clipping mask from a group node. All children revert to normal visible objects; the former clip path node remains in the group as a regular object. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `group_id` | string | yes | Group node ID (UUID or name) that currently has a clipping mask. |

## `release_compound_path`

Release a compound path back into its individual subpaths (Illustrator's Object > Compound Path > Release). Each subpath becomes a separate path node sharing the compound path's fill and stroke. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | ID of the compound path node to release. |

## `release_to_layers`

Move each node into its own newly created layer — the inverse of collect_in_new_layer. One layer is created per node; group children are resolved to their top-level ancestor before release. Layer names default to 'Layer 1', 'Layer 2', … but can be customised with name_prefix. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | IDs of nodes to release. Each top-level node goes into its own new layer. |
| `name_prefix` | string | no | Prefix for new layer names. Layers are named '<prefix> 1', '<prefix> 2', … Default: 'Layer'. |

## `remove_artboard`

Delete an artboard by id. Refuses to remove the last remaining artboard (a document must keep at least one). If the removed artboard was active, the first remaining one becomes active. Get the artboard_id from list_artboards.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `artboard_id` | string | yes | UUID of the artboard to remove (from list_artboards). |

## `remove_background`

Detect the subject of a raster layer with a local on-device matting model (U²-Net-p via ONNX Runtime; one-time ~5 MB download, then offline) and apply the result as a non-destructive foreground layer mask — the background is hidden, not erased, and the edit is undoable. Intersects with any existing mask.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | Raster node id or name |

## `remove_constraint`

Remove a property constraint by its id.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `constraint_id` | string | yes | UUID of the constraint to remove. |

## `remove_dimension`

Remove a dimension annotation by its ID. Use list_dimensions to find the ID.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `id` | string | yes | UUID of the dimension annotation to remove. |

## `remove_event_trigger`

Remove one or all event triggers for a given event. If action_name is omitted, all triggers for the event are removed.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `event` | string | yes | Event name to remove triggers for. |
| `action_name` | string | no | Optional: only remove the trigger pointing to this action name. |

## `remove_export_profile`

Delete a named export profile from the document.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the profile to remove. |

## `remove_fill`

Remove the fill from selected nodes (set to none/transparent).

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | no | Node IDs. Empty = use selection. |

## `remove_guide`

Remove a specific ruler guide by its UUID. Returns an error if the guide is locked.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `guide_id` | string | yes | UUID of the guide to remove. Obtain from list_guides. |

## `remove_stroke`

Remove the stroke from selected nodes (set to none).

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | no | Node IDs. Empty = use selection. |

## `reorder_layers`

Change the stacking order of layers. Provide the complete layer order as an array of layer UUIDs (bottom to top). All existing layers must be included.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `layer_order` | array<string> | yes | New layer order (bottom to top) |

## `reorder_node`

Change the z-order (stacking position) of a node within its layer. Use send_to_back / bring_to_front for absolute positioning, send_backward / bring_forward to step one place, or move_above / move_below with a relative_id to position relative to another node.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | ID of the node to reorder |
| `operation` | enum (`send_to_back`, `bring_to_front`, `send_backward`, `bring_forward`, `move_above`, `move_below`) | yes | send_to_back = lowest z; bring_to_front = highest z; move_above/move_below require relative_id |
| `relative_id` | string | no | Required for move_above / move_below — the reference node |

## `resize_canvas`

Resize the document canvas (artboard) to new dimensions. Does not scale existing artwork — only changes the canvas boundary. Undoable.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `height` | number | yes | New canvas height |
| `width` | number | yes | New canvas width |

## `resolve_annotation`

Mark an annotation as resolved. The annotation is retained in the file (for audit purposes) but is excluded from future `list_annotations` calls unless `include_resolved: true` is passed.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `annotation_id` | string | yes | UUID of the annotation to resolve. |

## `restore_checkpoint`

Restore the document to a saved checkpoint by ID, clearing undo/redo history. This mutates the document.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `checkpoint_id` | string | yes | UUID of the checkpoint to restore |

## `retouch`

Photoshop retouching tools on a raster node. op ∈ {healing_brush(cx,cy,radius,src_dx,src_dy — frequency-separation seamless clone), spot_healing(cx,cy,radius — auto-heal a blemish from its surroundings), content_aware_fill(requires `selection` — inpaint the selected region from surrounding pixels), red_eye(cx,cy,radius), dust_and_scratches(radius,threshold)}.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes |  |
| `op` | string | yes | Retouch operation (see description) |
| `params` | object | no |  |
| `selection` | object | no | Required for content_aware_fill |

## `reverse_blend_spine`

Reverse the direction of the blend spine path in a group node. This inverts the order of blend interpolation from start-to-end to end-to-start. The group must have a blend spine assigned. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `group_id` | string | yes | UUID or name of the group node whose blend spine should be reversed. |

## `reverse_node_order`

Reverse the front-to-back stacking order of children within each selected group node. The topmost child becomes the bottommost and vice versa. Useful for flipping blend results or layered artwork. Single undoable step. Uses current selection if node_ids is empty.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | no | UUIDs or names of group nodes. Uses current selection if empty. |

## `reverse_path_direction`

Reverse the winding direction of one or more path nodes. For open paths this flips the travel direction; for closed paths it toggles the fill rule winding (relevant for self-intersecting shapes, brushes, and type-on-path). Non-path nodes are silently skipped.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | IDs of path nodes to reverse |

## `rotate_copies`

Create N evenly-spaced rotational copies of a node around a center point, producing a radial symmetry arrangement. The original node is counted in the total — count=6 means the original plus 5 copies at 60° increments. Optionally wraps all copies in a Group. Useful for mandalas, snowflakes, icons, and any N-fold symmetric composition.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `count` | integer | yes | Total number of copies including the original (e.g. 6 = original + 5 copies at 60° steps). |
| `node_id` | string | yes | UUID or name of the source node. |
| `cx` | number | no | X of rotation center in document units. Defaults to the node's bounding-box center. |
| `cy` | number | no | Y of rotation center in document units. Defaults to the node's bounding-box center. |
| `group` | boolean | no | When true, wrap all copies (including the original) in a new Group node. Default: false. |

## `roughen_path`

Displace path anchor and control points by random amounts to create a hand-drawn, organic, or grunge effect. Configurable maximum displacement (size), optional subdivision for extra detail, and deterministic seed for reproducible results.

Use detail > 0 to add intermediate points before roughening — this creates finer texture on long straight segments. Destructive — modifies path data. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Path node IDs to roughen |
| `detail` | integer | no | Subdivision passes before roughening — adds points for finer texture (default: 0) |
| `seed` | integer | no | Random seed for reproducible results (default: 42) |
| `size` | number | no | Maximum displacement in document units (default: 5) |

## `round_corners`

Round sharp corners of path nodes by replacing each corner with a smooth quadratic bezier arc. The radius is automatically clamped so adjacent fillets never overlap.

Different from smooth_path (Chaikin): round_corners inserts precise arc fillets at corners while preserving straight segments. Useful for UI element shapes, rounded rectangles, and softening angular artwork. Destructive — modifies path data. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Path node IDs to round |
| `radius` | number | no | Corner radius in document units (default: 10) |

## `run_export_profile`

Execute a named export profile and return the export data. For SVG profiles, returns the SVG markup. For raster profiles, returns base64-encoded image data.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the profile to run. |

## `sample_color_at`

Sample the fill and stroke color of the topmost visible node at a canvas coordinate. Returns the node ID, fill color hex, stroke color hex, and opacity. Like an eyedropper for the MCP side.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `x` | number | yes | Canvas X coordinate |
| `y` | number | yes | Canvas Y coordinate |

## `save_document`

Save the current in-memory document in Photonic's native .photon format, including its undo/history tree. Pass path to save-as and make it the current MCP document path; omit path to save to that current path. Parent directories are created automatically. Returns the absolute path and bytes written.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `path` | string | no | Optional destination path for save-as. Required when the document has no current path. |

## `save_gradient_swatch`

Save the gradient fill of a node as a named gradient swatch. Works with linear, radial, fluid, and mesh gradients. If a swatch with the same name already exists it is updated. Use apply_gradient_swatch to reuse it on other nodes.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Unique name for the swatch. |
| `node_id` | string | yes | Path/text node ID (UUID or name) whose gradient fill to save. |

## `save_workspace`

Save the current properties-panel filter query as a named workspace preset. Pass search_query to define which panel sections are visible (e.g. 'text font' shows typography sections). Overwrites any existing workspace with the same name. Stored on document.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name for the workspace (e.g. 'Typography', 'Drawing'). |
| `search_query` | string | no | Panel search filter to save. Empty string shows all panels. |

## `scallop_path`

Replace each path segment with smooth inward-curving scallop arcs. Creates decorative scalloped edges, cloud-like shapes, and ornamental borders.

Positive depth curves inward (toward the interior); negative depth curves outward. Multiple arcs per segment create finer scalloping. Destructive — modifies path data. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Path node IDs to scallop |
| `count` | integer | no | Number of scallop arcs per original segment (default: 1, min: 1) |
| `depth` | number | no | Depth of each scallop arc in document units (default: 10). Positive = inward. |

## `scatter_copies`

Randomly scatter copies of a node within a rectangular area. Each copy gets a random position, optional random rotation and scale variation. Deterministic seed for reproducibility.

Useful for confetti, stars, foliage, particle effects, and random textures.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `height` | number | yes | Area height |
| `node_id` | string | yes | Source node to copy |
| `width` | number | yes | Area width |
| `x` | number | yes | Area left X |
| `y` | number | yes | Area top Y |
| `count` | integer | no | Number of copies (default: 20; maximum: 10000) |
| `rotation_range` | number | no | Random rotation range in degrees (default: 0) |
| `scale_range` | number | no | Scale variation range (default: 0) |
| `seed` | integer | no | Random seed (default: 42) |

## `scissors_cut`

Cut a path node at the point on it nearest to the specified canvas coordinates, splitting it into two open path nodes. The original node is removed; both halves inherit the original's fill, stroke, transform, opacity, and blend mode. Useful for splitting paths at intersections or at specific positions along an edge.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `canvas_x` | number | yes | X coordinate in document (canvas) space of the desired cut point. |
| `canvas_y` | number | yes | Y coordinate in document (canvas) space of the desired cut point. |
| `node_id` | string | yes | UUID of the path node to cut. |

## `screenshot`

Capture the current canvas as a PNG for visual inspection

_No parameters._

## `select_all`

Select all nodes in the document, or all nodes on a specific layer.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `layer_id` | string | no | Only select nodes on this layer (UUID or name). Omit for all layers. |

## `select_by_kind`

Select all nodes of a specified type. kind can be: 'path' (all path/shape nodes), 'text' (all text nodes), 'group' (all group nodes), or 'same_layer' (all nodes on the active layer). Optionally additive to extend the current selection.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `additive` | boolean | no | When true, add to the current selection instead of replacing it. Default false. |
| `kind` | enum (`path`, `text`, `group`, `same_layer`) | no | Which object type to select. Defaults to 'path'. |

## `select_inside_group`

Replace the current selection with the direct children of a group node. Equivalent to Alt+clicking into a group in Illustrator's Group Selection tool. Use to select individual objects inside a group without ungrouping it.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `group_id` | string | yes | The UUID of the group node whose children should be selected. |
| `additive` | boolean | no | When true, add the group's children to the existing selection instead of replacing it. Default false. |

## `select_same`

Select all document nodes that share a specific attribute value with the reference node. Updates the active selection. Useful for selecting all objects with the same fill color, stroke weight, opacity, etc. For color/weight/opacity comparisons a configurable tolerance is applied (default 0.01).

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `attribute` | enum (`fill_color`, `stroke_color`, `stroke_weight`, `opacity`, `blend_mode`, `object_type`) | yes | Which attribute to match |
| `node_id` | string | yes | UUID of the reference node whose attribute value is matched |
| `include_self` | boolean | no | Include the reference node itself in results (default true) |
| `tolerance` | number | no | Allowed difference for numeric/color comparisons (default 0.01) |

## `select_similar`

Select all nodes in the document whose visual attributes match those of the reference node(s). Implements Illustrator-style 'Select > Same > …' and Global Edit. match_by accepts a comma-separated list of: fill_color, stroke_color, stroke_width, kind, opacity, tags. Default match_by: fill_color.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `additive` | boolean | no | When true, add matches to the existing selection instead of replacing it. Default: false. |
| `match_by` | string | no | Comma-separated match criteria: fill_color, stroke_color, stroke_width, kind, opacity, tags. Default: fill_color. |
| `node_ids` | array<string> | no | Reference node UUIDs or names. Uses current selection if empty. |
| `tolerance` | integer | no | Color match tolerance 0–255 per channel. Default: 5. |

## `set_active_artboard`

Make an artboard the active one. The active artboard is the default target for artboard-relative operations and export. Soft view state — not an undo step. Get the artboard_id from list_artboards.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `artboard_id` | string | yes | UUID of the artboard to activate (from list_artboards). |

## `set_active_layer`

Set the active layer. New nodes created without an explicit layer_id will be placed on the active layer.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `layer_id` | string | yes | Layer UUID or name |

## `set_artboard_margins`

Set the artboard safe-area margins (top, right, bottom, left) in document units. Margins define the inner content area; content should stay within these guides. Pass only the fields you want to change.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `bottom` | number | no | Bottom margin in document units. Default: unchanged. |
| `left` | number | no | Left margin in document units. Default: unchanged. |
| `right` | number | no | Right margin in document units. Default: unchanged. |
| `top` | number | no | Top margin in document units. Default: unchanged. |

## `set_blend_mode`

Set blend mode on multiple nodes at once. All 16 blend modes supported: normal, multiply, screen, overlay, darken, lighten, color_dodge, color_burn, hard_light, soft_light, difference, exclusion, hue, saturation, color, luminosity.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `blend_mode` | enum (`normal`, `multiply`, `screen`, `overlay`, `darken`, `lighten`, `color_dodge`, `color_burn`, `hard_light`, `soft_light`, `difference`, `exclusion`, `hue`, `saturation`, `color`, `luminosity`, `linear_dodge`, `linear_burn`, `subtract`, `divide`, `vivid_light`, `linear_light`, `pin_light`, `hard_mix`, `darker_color`, `lighter_color`) | yes | Blend mode |
| `node_ids` | array<string> | no | Node IDs. Empty = use selection. |

## `set_blend_spine`

Assign a path node (child of the group) as the blend spine for a group node. The spine path guides interpolation between objects in a blend group. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `group_id` | string | yes | UUID or name of the group node to configure as a blend. |
| `path_id` | string | yes | UUID or name of the path node to use as the blend spine. |

## `set_character_metrics`

Set advanced node-level character metrics on a text node: baseline shift and super/subscript position. baseline_shift is in document units (positive raises text above the baseline, negative lowers it). script_position is 'normal', 'superscript' (renders smaller and raised), or 'subscript' (renders smaller and lowered). Both fields are optional — pass only those you want to change. Applies to the whole node (per-character ranges are not yet supported). Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | Text node ID or name. |
| `baseline_shift` | number | no | Baseline shift in document units (positive = up). Default: unchanged. |
| `script_position` | enum (`normal`, `superscript`, `subscript`) | no | Script position. Default: unchanged. |

## `set_constraint`

Create a live property constraint binding a node property to an arithmetic expression over other nodes' properties (e.g. 'nodes[\'logo\'].x + 20'). Re-evaluated after every edit. Target property must be one of x, y, opacity, font_size. Cycles are rejected.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `expression` | string | yes | Arithmetic expression; may reference nodes['<id-or-name>'].<prop>. |
| `node_id` | string | yes | Target node UUID or name. |
| `property` | enum (`x`, `y`, `opacity`, `font_size`) | yes | Target property to drive. |

## `set_document_bleed`

Set the print bleed and/or slug margins for the document. Bleed is the extra artwork bled past the trim edge (typically 3 mm) to prevent white borders after cutting. Slug is the additional area outside bleed reserved for printer marks and file info. Values persist in the .photon file. Provide only the fields you want to change.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `bleed_mm` | number | no | Bleed in millimetres (all four sides). Typical values: 3.0 (EU) or 3.175 (US 0.125 in). Default: unchanged. |
| `slug_mm` | number | no | Slug area in millimetres outside the bleed. Default: unchanged. |

## `set_document_color_mode`

Set the document color mode to RGB or CMYK. CMYK is required for print-production PDF/X output. The mode persists in the .photon file and is used as the default color space when exporting PDF without an explicit color_mode override.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `mode` | enum (`rgb`, `cmyk`) | yes | Color mode. 'rgb' for screen/web; 'cmyk' for print production. |

## `set_document_dpi`

Set the document resolution (DPI) — the honored physical-size property on export: exported PDF/raster physical size = pixel size / dpi × 72 pt. A 1050×600 px document at 300 DPI exports at 252×144 pt (3.5×2 in); the same pixels at 72 DPI export at 1050×600 pt. Presets set this automatically (e.g. 300 for print). Default is 72 (px ≡ pt). Persists in the .photon file.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `dpi` | number | yes | Dots per inch. Common print value: 300. |

## `set_font_style`

Set the font style (normal, italic, or oblique) on a text node. Italic uses a true italic face if the font provides one; oblique synthesizes slant. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | Text node ID (UUID or name). |
| `style` | enum (`normal`, `italic`, `oblique`) | yes | Font style to apply. |

## `set_font_weight`

Set the font weight (100–900) on a text node. Common values: 400 = Regular, 700 = Bold. Clamped to valid range. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | Text node ID (UUID or name). |
| `weight` | integer | yes | Font weight (100=Thin, 400=Regular, 700=Bold, 900=Black). |

## `set_layer_mask`

Attach a non-destructive layer mask to a raster node, built from a selection spec (rect/ellipse/polygon/wand/color_range, with feather/invert/grow/contract). The mask gates the node's compositing without altering its pixels.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes |  |
| `selection` | object | yes | Selection spec defining the mask |

## `set_locked`

Lock or unlock nodes. Locked nodes cannot be selected or modified in the GUI. Omit `locked` to toggle current state.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `locked` | boolean | no | Set locked. Omit to toggle. |
| `node_ids` | array<string> | no | Node IDs. Empty = use selection. |

## `set_node_prompt`

Record an AI prompt on a node's prompt history for creative provenance tracking. Each entry is chronological — the full history shows which prompts shaped the node's appearance. This enables 'intent-preserving edit' workflows where an agent understands why a node looks the way it does.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | UUID or name of the node to annotate. |
| `prompt` | string | yes | The prompt text to record. |
| `mode` | enum (`append`, `prepend`, `replace`) | no | "append" (default) adds to end; "prepend" adds to start; "replace" clears history first. |

## `set_node_size`

Resize a node to exact pixel dimensions in a single undoable step — no manual scale-factor arithmetic required.

Internally this tool:
1. Computes the node's current world-space bounding box (equivalent to `measure_nodes`)
2. Derives the x/y scale factors needed to reach the requested dimensions
3. Composes those scales onto the existing node transform, anchored at the chosen corner or edge

This eliminates the common two-round-trip pattern of `measure_nodes` → compute → `apply_transform`.

Tips:
- Omit `height` and pass only `width` (with `maintain_aspect_ratio: true`) to scale proportionally
- Use `anchor: "center"` when you want the shape to grow or shrink symmetrically
- Use `anchor: "top_left"` (the default) when you want the position to stay fixed
- Works on any path node; groups and text nodes with no geometry return an error

Returns the previous and new dimensions and the scale factors applied.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | ID of the node to resize |
| `anchor` | enum (`top_left`, `top_center`, `top_right`, `left_center`, `center`, `right_center`, `bottom_left`, `bottom_center`, `bottom_right`) | no | The point on the bounding box that stays fixed while the rest of the shape scales. Default: "top_left". |
| `height` | number | no | Target height in pixels (must be > 0). Omit to derive from width when maintain_aspect_ratio is true. |
| `maintain_aspect_ratio` | boolean | no | When true and both dimensions given: fit inside the requested box without distortion (uses the smaller scale factor). When true and only one dimension given: scale the other axis proportionally. Default: false. |
| `width` | number | no | Target width in pixels (must be > 0). Omit to derive from height when maintain_aspect_ratio is true. |

## `set_opacity`

Set opacity on multiple nodes at once. More efficient than calling update_node individually for each.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `opacity` | number | yes | Opacity 0.0–1.0 |
| `node_ids` | array<string> | no | Node IDs. Empty = use selection. |

## `set_opentype_features`

Set, add, or remove OpenType feature tags on a text node. Common tags: liga (ligatures), calt (contextual alternates), frac (fractions), smcp (small caps), sups (superscript), subs (subscript), ordn (ordinals), swsh (swashes), dlig (discretionary ligatures). Mode 'set' replaces all features, 'add' appends unique entries, 'remove' removes listed entries. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `features` | array<string> | yes | OpenType feature tag strings (4-letter codes). |
| `node_id` | string | yes | Text node ID or name. |
| `mode` | enum (`set`, `add`, `remove`) | no | How to apply. Default: set. |

## `set_paint`

Apply ONE paint to MANY nodes in a single undoable call — each node re-fit to its own bounding box. The paint uses the same object shape as `fill` (solid / linear / radial / fluid / mesh / pattern). For gradients, set `paint.units` to "bbox" with 0–1 `coords` to reuse one relative gradient (e.g. left→right blue→purple) across an entire icon set with zero per-node coordinate math — the coords are resolved to each node's bounding box automatically. `target` chooses the fill (default) or stroke slot; strokes accept gradient/pattern paints too (#201), so a whole set of gradient-stroked line icons is one call — no outline_stroke→delete→refill workaround.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Node IDs or names to paint. |
| `paint` | object | yes | The paint (same shape as `fill`). For a bbox-relative gradient: { "type": "gradient", "gradient_type": "linear", "colors": ["#2f56cf","#7c3aed"], "coords": [0,0,1,0], "units": "bbox" }. |
| `target` | enum (`fill`, `stroke`) | no | Which paint slot to set (default: fill). |

## `set_paragraph_options`

Set paragraph-level text options on a text node: spacing before paragraphs, spacing after paragraphs, and first-line indent. All fields are optional — pass only those you want to change. Values are in document units. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | Text node ID or name. |
| `indent` | number | no | First-line indent in document units. Default: unchanged. |
| `spacing_after` | number | no | Space after each paragraph in document units. Default: unchanged. |
| `spacing_before` | number | no | Space before each paragraph in document units. Default: unchanged. |

## `set_selection`

Set the active selection to specific node IDs. Replaces current selection unless additive=true. Empty node_ids clears selection.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `additive` | boolean | no | Add to existing selection (default: false = replace) |
| `node_ids` | array<string> | no | Node IDs to select |

## `set_symbol_override`

Set per-instance fill and/or stroke color overrides on a symbol instance node. Overrides apply to this instance only; the master symbol is unaffected. Pass fill_hex and/or stroke_hex as '#rrggbb' strings. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | UUID or name of the symbol instance node. |
| `fill_hex` | string | no | Fill color override as '#rrggbb'. Omit to leave unchanged. |
| `stroke_hex` | string | no | Stroke color override as '#rrggbb'. Omit to leave unchanged. |

## `set_tab_stops`

Set explicit tab stop positions on a text node. Stops are specified in document units and are automatically sorted ascending. Replaces all existing tab stops. Use clear_tab_stops to revert to default tab spacing. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | Text node ID or name. |
| `stops` | array<number> | yes | Tab stop positions in document units. |

## `set_text_area`

Flow a text node inside a closed path boundary (Area Type). The text reflows to fill the area defined by the given path node. The path node remains a separate visible object; hide it if not needed. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `area_path_id` | string | yes | Closed path node ID (UUID or name) defining the text boundary. |
| `text_node_id` | string | yes | Text node ID (UUID or name) to flow inside the area. |

## `set_text_decoration`

Set the text decoration on a text node: underline, line-through (strikethrough), overline, or none. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `decoration` | enum (`none`, `underline`, `line-through`, `overline`) | yes | Decoration to apply. |
| `node_id` | string | yes | Text node ID or name. |

## `set_text_direction`

Set the layout direction of a text node. When vertical is true, characters are stacked top-to-bottom (Vertical Type). When false (default), text flows left-to-right normally. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | Text node ID (UUID or name). |
| `vertical` | boolean | yes | true = vertical top-to-bottom, false = normal horizontal. |

## `set_text_path`

Place a text node along a path spine (Type on a Path). The text flows along the curve starting at `offset` document units from the path start. The path node remains visible as a separate object; hide it manually if not needed. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `path_node_id` | string | yes | Path node ID (UUID or name) to use as the text spine. |
| `text_node_id` | string | yes | Text node ID (UUID or name) to place on the path. |
| `offset` | number | no | Start offset along the path in document units. Default: 0.0. |

## `set_variable_value`

Update the value of an existing document variable. Use apply_variables to propagate the change to all bound text nodes.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Variable name to update. |
| `value` | string | yes | New string value. |

## `set_visibility`

Show or hide nodes. Omit `visible` to toggle current state. Hidden nodes are not rendered but remain in the document.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | no | Node IDs. Empty = use selection. |
| `visible` | boolean | no | Set visible. Omit to toggle. |

## `simplify_path`

Reduce the anchor-point count of a path using Ramer-Douglas-Peucker simplification. Bézier curves are first sampled to line segments, then redundant points are removed. Supports dry_run to preview the reduction without applying. The result is a polygonal path with fewer vertices.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | UUID of the path node to simplify |
| `tolerance` | number | yes | RDP tolerance in document coordinates. Larger values remove more points. Typical: 0.5–5.0 for screen work, 0.1–1.0 for precise technical illustration. |
| `dry_run` | boolean | no | If true, return before/after point counts without modifying the document. Default false. |

## `smooth_path`

Smooth jagged or polygonal paths using Chaikin's corner-cutting algorithm. Converts sharp LineTo segments into smooth cubic Bézier curves. Applies to the specified node IDs or the current selection. factor (0–0.5) controls rounding strength; 0.25 is the classic value. iterations (1–8) controls how many passes are applied.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `factor` | number | no | Smoothing strength [0, 0.5]. 0.25 is the classic Chaikin value; higher values produce rounder curves. Default 0.25. |
| `iterations` | integer | no | Number of smoothing passes (1–8). More passes = smoother result. Default 2. |
| `node_ids` | array<string> | no | UUIDs of path nodes to smooth. If empty, uses the current selection. |

## `snap_to_pixel`

Round the position (translation) of one or more nodes to the nearest integer coordinates. Useful for pixel-perfect screen design. Bundled as a single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | IDs of nodes to snap to integer pixel coordinates |

## `split_into_grid`

Divide a path node's bounding box into a rows×cols grid of separate rectangle nodes, each inheriting the source node's fill, stroke, opacity, and blend mode. Optional horizontal (gutter_x) and vertical (gutter_y) gutters are subtracted from the total area before dividing. The source node is deleted by default; set keep_original to true to retain it. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `cols` | integer | yes | Number of columns in the grid (≥ 1). Total rows × cols may not exceed 10000 cells. |
| `node_id` | string | yes | UUID of the source path node whose bounding box defines the grid area. |
| `rows` | integer | yes | Number of rows in the grid (≥ 1). Total rows × cols may not exceed 10000 cells. |
| `gutter_x` | number | no | Horizontal gutter width in document units between columns. Default: 0. |
| `gutter_y` | number | no | Vertical gutter height in document units between rows. Default: 0. |
| `keep_original` | boolean | no | When true, keep the source node after splitting. Default: false (source is deleted). |
| `layer_id` | string | no | UUID of the layer to place new nodes in. Defaults to the source node's layer. |

## `spray_symbol_instances`

Spray multiple instances of a named symbol scattered around a center point using a golden-angle spiral distribution for natural-looking placement. Like Illustrator's Symbol Sprayer tool. Supports undo per instance.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `count` | integer | yes | Number of instances to place (1–200). |
| `symbol_name` | string | yes | Name of the symbol to spray. |
| `x` | number | yes | Center X coordinate of the spray area. |
| `y` | number | yes | Center Y coordinate of the spray area. |
| `spread` | number | no | Scatter radius in document units. Default: 100. |

## `stipple_fill`

Fill a path shape with randomly placed dots (stipple effect). Uses rejection sampling to place dots inside the path boundary.

The original path is preserved — dots are added as a separate path on the same layer. Useful for halftone textures, pointillism, sand/grain effects, and decorative fills.

Dot color defaults to the path's solid fill color. Deterministic seed ensures reproducible results.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Path node IDs to fill with stipple dots |
| `color` | string | no | Dot color hex (default: uses path fill color) |
| `count` | integer | no | Number of dots (default: 200) |
| `dot_radius` | number | no | Dot radius in document units (default: 1.5) |
| `seed` | integer | no | Random seed for reproducibility (default: 42) |

## `style_transfer`

Copy the visual style (fill, stroke, opacity, blend_mode) from one source node onto any number of target nodes in a single undoable step.

Use cases: applying a reference palette to many shapes at once, making a set of icons consistent, pasting a complex gradient or stroke style without re-specifying it per node.

fill and stroke only transfer when both source and target are path nodes. opacity and blend_mode transfer to all node types. Use the `properties` filter to copy a subset (e.g. fill only, or opacity+blend_mode only).

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `source_id` | string | yes | ID of the node whose style to copy |
| `target_ids` | array<string> | yes | IDs of the nodes that will receive the style |
| `properties` | array<enum (`fill`, `stroke`, `opacity`, `blend_mode`)> | no | Which style properties to copy. Omit or pass an empty array to copy all four. Example: ["fill", "stroke"] copies only colour and outline, leaving opacity and blend_mode untouched. |

## `swap_fill_stroke`

Swap the fill and stroke colors on selected nodes. The fill color becomes the stroke color and vice versa. Works with paths and text. Solid fills only — gradient fills become stroke black.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | no | Node IDs. Empty = use selection. |

## `tag_node_for_export`

Tag a node for inclusion in batch asset exports (Asset Export Panel equivalent). Set name to an empty string to remove the tag. Supports per-scale raster exports.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Base asset name (without extension). Pass empty string to remove the tag. |
| `node_id` | string | yes | UUID or name of the node to tag. |
| `format` | enum (`svg`, `png`, `jpeg`, `jpg`, `webp`) | no | Export format. Default: svg. |
| `scales` | array<number> | no | Scale multipliers for raster exports (e.g. [1,2,3] → @1x @2x @3x). Ignored for SVG. Default: [1]. |

## `tag_nodes`

Batch add or remove tags on nodes. Tags are arbitrary strings used for querying with find_nodes.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `add` | array<string> | no | Tags to add |
| `node_ids` | array<string> | no | Node IDs. Empty = use selection. |
| `remove` | array<string> | no | Tags to remove |

## `transform_copies`

Create N copies of a node with cumulative transform offsets. Each copy has the previous copy's transform plus the specified translation, rotation, and scale increments.

Perfect for:
- Radial patterns: rotate=30°, copies=11 → 12-spoke pattern
- Step-and-repeat: translate_x=50, copies=9 → 10-column grid
- Spiral scaling: rotate=20°, scale=0.9, copies=15 → shrinking spiral
- Fade trails: opacity_step=0.8 → each copy 80% of previous opacity

All copies are placed on the same layer as the source.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | Source node UUID or name to copy |
| `copies` | integer | no | Number of copies (default: 5) |
| `opacity_step` | number | no | Opacity multiplier per copy (default: 1.0). 0.8 = fade 20% each. |
| `rotate` | number | no | Rotation per copy in degrees (default: 0) |
| `scale` | number | no | Scale factor per copy (default: 1.0). 0.9 = shrink 10% each. |
| `translate_x` | number | no | X offset per copy in document units (default: 0) |
| `translate_y` | number | no | Y offset per copy in document units (default: 0) |

## `transform_image`

Geometric op on a raster node's pixels. op ∈ {crop(x,y,width,height), resize(width,height,filter:'nearest'|'bilinear'|'lanczos3'), resize_canvas(width,height,offset_x,offset_y), rotate90, rotate180, rotate270, flip_h, flip_v, rotate(angle deg)}.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes |  |
| `op` | string | yes | Operation name (see description) |
| `params` | object | no |  |

## `twirl_path`

Rotate path points around a center with a spiral falloff — points near the center rotate more, creating a twirl/vortex effect. Useful for decorative spirals, logo flourishes, and abstract patterns.

The rotation angle decreases linearly from full at the center to zero at the outermost point. Add anchor points first for smoother results on paths with few segments. Destructive — modifies path data. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Path node IDs to twirl |
| `angle` | number | no | Rotation angle in degrees (positive = counter-clockwise). Default: 90 |
| `center_x` | number | no | X coordinate of twirl center (default: path centroid) |
| `center_y` | number | no | Y coordinate of twirl center (default: path centroid) |

## `unbind_text_variable`

Remove the variable binding from a text node. The node's current text content is preserved. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | Text node ID (UUID or name). |

## `undo`

Undo the last operation(s)

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `steps` | integer | no |  |

## `undo_node`

Revert a specific node to its state N edits ago without undoing anything else in the document. Scans the undo history for UpdateNode commands targeting the given node and applies the N-th-most-recent pre-mutation snapshot as a new undoable command — so the revert itself can be undone with a global Ctrl+Z.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | UUID or name of the node to revert. |
| `steps` | integer | no | How many node-specific edits to revert. Default: 1. |

## `ungroup_nodes`

Dissolve a group node, returning its children to the layer at the group's former z-position.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `group_id` | string | yes | ID of the group node to dissolve |

## `unlink_text_frames`

Remove a text node from its thread chain, severing both the previous and next frame links while preserving adjacent nodes. Supports undo.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes | ID or name of the text node to remove from its thread chain. |

## `update_artboard`

Edit an existing artboard's name, position (x, y = top-left) and/or size (width, height). Only the fields you pass change; others are left as-is. Repositioning with this tool moves only the artboard frame, not its contents. One undoable step. Get the artboard_id from list_artboards.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `artboard_id` | string | yes | UUID of the artboard to edit (from list_artboards). |
| `height` | number | no | New height in document units (> 0). Default: unchanged. |
| `name` | string | no | New name. Default: unchanged. |
| `width` | number | no | New width in document units (> 0). Default: unchanged. |
| `x` | number | no | New top-left X in document units. Default: unchanged. |
| `y` | number | no | New top-left Y in document units. Default: unchanged. |

## `update_color_swatch`

Rename or change the color of an existing swatch. Optionally propagates the color change to all nodes currently using the old swatch color.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Name of the swatch to update. |
| `new_color_hex` | string | no | New color as CSS hex (optional, omit to keep current color). |
| `new_name` | string | no | New name (optional, omit to keep current name). |
| `propagate` | boolean | no | When true (default), update all nodes whose fill matches the old color. Set false to update only the swatch record. |

## `update_layer`

Update mutable metadata on a layer: rename it, change visibility, lock/unlock, set a color tag, mark as a template layer, or set the layer's opacity and blend mode. Only the fields you supply are changed; omitted fields keep their current values. Layer opacity and blend mode composite the whole layer as a unit against the layers beneath (like Photoshop/Illustrator). Template layers are locked reference layers used for tracing over (dimmed in the GUI). Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `layer_id` | string | yes | UUID of the layer to update. |
| `blend_mode` | enum (`normal`, `multiply`, `screen`, `overlay`, `darken`, `lighten`, `color_dodge`, `color_burn`, `hard_light`, `soft_light`, `difference`, `exclusion`, `hue`, `saturation`, `color`, `luminosity`, `linear_dodge`, `linear_burn`, `subtract`, `divide`, `vivid_light`, `linear_light`, `pin_light`, `hard_mix`, `darker_color`, `lighter_color`) | no | Layer blend mode: normal, multiply, screen, overlay, darken, lighten, color_dodge, color_burn, hard_light, soft_light, difference, exclusion, hue, saturation, color, luminosity. |
| `color` | any | no | Color tag as [r,g,b,a] floats 0.0–1.0. Pass null to clear. |
| `is_template` | boolean | no | Mark as a template layer (locked, dimmed reference for tracing over artwork). Setting true also locks the layer. |
| `locked` | boolean | no | Lock or unlock the layer. |
| `name` | string | no | New name for the layer. |
| `opacity` | number | no | Layer opacity 0.0–1.0; the layer composites as a unit at this opacity. |
| `print` | boolean | no | Include the layer in export/print output. false = visible on canvas but excluded from exports. |
| `visible` | boolean | no | Show or hide the layer. |

## `update_node`

Update properties of an existing node by ID. Text nodes also accept: content, font_family, font_size, font_weight, text_align.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_id` | string | yes |  |
| `blend_mode` | string | no |  |
| `content` | string | no | New text content (text nodes only) |
| `fill` | object | no | Fill — solid: {"type":"solid","color":"#rrggbb"} \| none: {"type":"none"} \| linear: {"type":"gradient","gradient_type":"linear","colors":["#hex1","#hex2"],"coords":[x0,y0,x1,y1]} \| radial: {"type":"gradient","gradient_type":"radial","colors":["#hex1","#hex2"],"coords":[cx,cy,r]} \| fluid: {"type":"fluid_gradient","points":[{"x":100,"y":50,"color":"#ff0000"},...],"power":2.0} \| mesh: {"type":"mesh_gradient","rows":2,"cols":2,"vertices":[{"x":0,"y":0,"color":"#ff0000"},...]} |
| `font_family` | string | no | Font family (text nodes only) |
| `font_size` | number | no | Font size in document units (text nodes only) |
| `font_weight` | integer | no | Font weight 100–900 (text nodes only) |
| `locked` | boolean | no | Lock the node so it cannot be selected or moved in the canvas |
| `name` | string | no |  |
| `opacity` | number | no |  |
| `stroke` | object | no | Stroke outline. Fields: color (#RRGGBB), width (number), enabled (bool), opacity (0-1), line_cap (butt\|round\|square), line_join (miter\|round\|bevel), align (center\|inside\|outside), dash_array ([dash,gap,...] up to 6 values), dash_offset (number). Example: {"color":"#000000","width":2,"enabled":true,"dash_array":[8,4]} |
| `tags` | array<string> | no |  |
| `text_align` | enum (`left`, `center`, `right`) | no | Text alignment (text nodes only) |
| `visible` | boolean | no |  |

## `warp_envelope`

Apply an envelope warp distortion to path nodes using named presets. The path is deformed according to a mathematical envelope function.

Presets:
- arc: bend along a circular arc
- bulge: expand from center outward
- wave: sinusoidal wave deformation
- flag: wave that increases from left to right
- squeeze: compress horizontally in the middle
- inflate: expand everything from center (softer than bulge)
- fisheye: fisheye lens distortion

For best results, add_anchor_points first for smoother warping on low-polygon paths. Destructive — modifies path data. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Path node IDs to warp |
| `warp_type` | enum (`arc`, `arc_lower`, `arc_upper`, `arch`, `bulge`, `wave`, `flag`, `squeeze`, `inflate`, `fisheye`, `shell_lower`, `shell_upper`, `fish`, `rise`, `twist`) | yes | Warp preset name |
| `bend` | number | no | Primary bend amount, roughly -1 to 1 (default: 0.5). Negative reverses direction. |
| `distort_h` | number | no | Horizontal distortion, roughly -1 to 1 (default: 0). Only affects some presets. |
| `distort_v` | number | no | Vertical distortion, roughly -1 to 1 (default: 0). Only affects some presets. |

## `zig_zag_path`

Replace each segment of a path with a zig-zag (sharp corners) or smooth wave (bezier curves) pattern. Configurable amplitude and ridge count per segment. Useful for decorative borders, electrical symbols, water/wave effects, and organic textures. Destructive — modifies the path data directly. Single undoable step.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `node_ids` | array<string> | yes | Path node IDs to apply zig-zag to |
| `ridges_per_segment` | integer | no | Number of peaks per original path segment (default: 4, min: 1) |
| `size` | number | no | Peak-to-peak amplitude in document units (default: 10) |
| `smooth` | boolean | no | Use smooth bezier waves instead of sharp zigzag corners (default: false) |
